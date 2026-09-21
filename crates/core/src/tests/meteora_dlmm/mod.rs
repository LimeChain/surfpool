//! Meteora DLMM integration tests.
//!
//! These fetch the real accounts from mainnet rather than embedding captured copies, so they need
//! a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests meteora_dlmm -- --test-threads=1
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! `LbPair` is a zero-copy (bytemuck, repr C) account, so a Borsh round-trip that reproduces the
//! bytes only proves the two encodings agree on this layout. What pins it to the chain is the
//! size the IDL declares matching the size the account actually has, and each override landing on
//! the offset the IDL puts it at.

use std::collections::HashMap;

use anchor_lang_idl::types::{Idl, IdlArrayLen, IdlDefinedFields, IdlType, IdlTypeDefTy};
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const PROGRAM_ID: Pubkey = Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
const SOL_USDC: &str = "BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y";
const USDT_SOL: &str = "HToiT8XK8GHgAT4N3oGXadc7opdApPwsbCL9tFRYa3Rg";

const LB_PAIR_SIZE: usize = 904;
const BIN_ARRAY_SIZE: usize = 10136;
const BINS_PER_ARRAY: i32 = 70;
const BIN_SIZE: usize = 144;
const BIN_ARRAY_BINS_OFFSET: usize = 56;
const BIN_LIQUIDITY_SUPPLY_OFFSET: usize = 32;

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[&str]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let pubkeys: Vec<Pubkey> = addresses
        .iter()
        .map(|a| Pubkey::from_str_const(a))
        .collect();

    client
        .get_multiple_accounts(&pubkeys, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| match result {
            GetAccountResult::FoundAccount(_, account, _)
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
            GetAccountResult::None(_) => {
                panic!("{address} does not exist on mainnet; the test needs a new address")
            }
        })
        .collect()
}

/// Byte indices at which two buffers differ.
fn diff_indices(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.iter()
        .zip(b.iter())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i)
        .collect()
}

fn named_fields<'a>(idl: &'a Idl, type_name: &str) -> &'a Vec<anchor_lang_idl::types::IdlField> {
    let def = idl
        .types
        .iter()
        .find(|t| t.name == type_name)
        .unwrap_or_else(|| panic!("type {type_name} is not in the IDL"));
    match &def.ty {
        IdlTypeDefTy::Struct {
            fields: Some(IdlDefinedFields::Named(fields)),
        } => fields,
        _ => panic!("{type_name} is not a struct with named fields"),
    }
}

/// Size of an IDL type in the account buffer. Only the fixed-width kinds `LbPair` is built from
/// are supported: anything else means the layout has grown a variable part and the offsets this
/// module computes would be fiction.
fn type_size(idl: &Idl, ty: &IdlType) -> usize {
    match ty {
        IdlType::Bool | IdlType::U8 | IdlType::I8 => 1,
        IdlType::U16 | IdlType::I16 => 2,
        IdlType::U32 | IdlType::I32 | IdlType::F32 => 4,
        IdlType::U64 | IdlType::I64 | IdlType::F64 => 8,
        IdlType::U128 | IdlType::I128 => 16,
        IdlType::Pubkey => 32,
        IdlType::Array(inner, IdlArrayLen::Value(len)) => type_size(idl, inner) * len,
        IdlType::Defined { name, .. } => named_fields(idl, name)
            .iter()
            .map(|field| type_size(idl, &field.ty))
            .sum(),
        other => panic!("{other:?} has no fixed size; this account is not a flat layout"),
    }
}

/// Offset (from the start of the account, discriminator included) and size of a dot-separated
/// field path, taken from the IDL rather than from a hand-written constant.
fn field_offset(idl: &Idl, account_type: &str, path: &str) -> (usize, usize) {
    let mut offset = 8;
    let mut current = account_type.to_string();
    let mut segments = path.split('.').peekable();

    while let Some(segment) = segments.next() {
        let fields = named_fields(idl, &current);
        let mut found = None;
        for field in fields {
            if field.name == segment {
                found = Some(&field.ty);
                break;
            }
            offset += type_size(idl, &field.ty);
        }
        let ty = found.unwrap_or_else(|| panic!("{path}: {current} has no field {segment}"));
        if segments.peek().is_none() {
            return (offset, type_size(idl, ty));
        }
        match ty {
            IdlType::Defined { name, .. } => current = name.clone(),
            other => panic!("{path}: {segment} is {other:?}, so it has no fields"),
        }
    }
    unreachable!("an empty path cannot reach here")
}

fn discriminator(idl: &Idl, account_name: &str) -> Vec<u8> {
    idl.accounts
        .iter()
        .find(|a| a.name == account_name)
        .unwrap_or_else(|| panic!("{account_name} is not in the IDL"))
        .discriminator
        .clone()
}

fn read_i32(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(data[offset..offset + 4].try_into().expect("4 bytes"))
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().expect("2 bytes"))
}

/// The bin array holding `active_id`. The division floors, so bin -2222 lives in array -32.
fn bin_array_address(pool: &Pubkey, active_id: i32) -> (Pubkey, i64) {
    let index = active_id.div_euclid(BINS_PER_ARRAY) as i64;
    let (address, _) = Pubkey::find_program_address(
        &[b"bin_array", pool.as_ref(), &index.to_le_bytes()],
        &PROGRAM_ID,
    );
    (address, index)
}

/// A bundled IDL that has drifted from the on-chain layout shows up here: the declared size stops
/// matching the account, or the no-op re-encode moves bytes.
#[tokio::test]
async fn live_lb_pair_matches_the_bundled_layout() {
    let accounts = fetch(&[SOL_USDC, USDT_SOL]).await;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry.get("meteora-dlmm-custom").expect("template");
    let idl = &template.idl;

    let declared = 8 + type_size(
        idl,
        &IdlType::Defined {
            name: "LbPair".to_string(),
            generics: vec![],
        },
    );
    assert_eq!(
        declared, LB_PAIR_SIZE,
        "the IDL's LbPair fields no longer sum to the documented 904 bytes"
    );

    for (address, account) in [SOL_USDC, USDT_SOL].iter().zip(&accounts) {
        assert_eq!(
            account.owner, PROGRAM_ID,
            "{address} is not owned by the DLMM program"
        );
        assert_eq!(
            account.data.len(),
            declared,
            "{address} is {} bytes but the IDL declares {declared}; a zero-copy account and its \
             IDL must agree on size",
            account.data.len()
        );
        assert_eq!(
            &account.data[..8],
            discriminator(idl, "LbPair").as_slice(),
            "{address} is not an LbPair"
        );

        let forged = surfnet_svm
            .get_forged_account_data(
                &Pubkey::from_str_const(address),
                &account.data,
                idl,
                &HashMap::new(),
            )
            .unwrap_or_else(|e| panic!("live {address} failed to round-trip: {e}"));

        assert_eq!(forged.len(), account.data.len(), "{address} changed size");
        let diffs = diff_indices(&forged, &account.data);
        assert!(
            diffs.is_empty(),
            "a no-op round-trip altered {address} at {} byte(s), first at {:?}",
            diffs.len(),
            diffs.first()
        );
    }
}

/// Both levers are inside the zero-copy struct, where a Borsh re-encode that padded or reordered
/// anything would spill into a neighbour. The offsets come from the IDL, and the literals below
/// are what mainnet showed on 2026-09-20.
#[tokio::test]
async fn overrides_land_only_on_their_own_offsets() {
    let accounts = fetch(&[SOL_USDC]).await;
    let data = &accounts[0].data;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry.get("meteora-dlmm-custom").expect("template");
    let idl = &template.idl;
    let pool = Pubkey::from_str_const(SOL_USDC);

    let (active_id_offset, active_id_size) = field_offset(idl, "LbPair", "active_id");
    assert_eq!(
        (active_id_offset, active_id_size),
        (76, 4),
        "active_id moved in the IDL"
    );
    let (base_factor_offset, base_factor_size) =
        field_offset(idl, "LbPair", "parameters.base_factor");
    assert_eq!(
        (base_factor_offset, base_factor_size),
        (8, 2),
        "parameters.base_factor moved in the IDL"
    );

    let live_active_id = read_i32(data, active_id_offset);
    let new_active_id = live_active_id + 1;
    let forged = surfnet_svm
        .get_forged_account_data(
            &pool,
            data,
            idl,
            &HashMap::from([("active_id".to_string(), serde_json::json!(new_active_id))]),
        )
        .expect("active_id override on the live pool");
    let diffs = diff_indices(&forged, data);
    assert!(!diffs.is_empty(), "active_id should have changed");
    assert!(
        diffs
            .iter()
            .all(|i| (active_id_offset..active_id_offset + active_id_size).contains(i)),
        "only active_id's own bytes should change, got {diffs:?}"
    );
    assert_eq!(read_i32(&forged, active_id_offset), new_active_id);
    assert_eq!(
        read_u16(&forged, 80),
        read_u16(data, 80),
        "bin_step sits immediately after active_id and must not move"
    );

    let live_base_factor = read_u16(data, base_factor_offset);
    assert!(
        live_base_factor > 0,
        "the live pool should charge a base fee, got base_factor {live_base_factor}"
    );
    let new_base_factor = live_base_factor / 2;
    let forged = surfnet_svm
        .get_forged_account_data(
            &pool,
            data,
            idl,
            &HashMap::from([(
                "parameters.base_factor".to_string(),
                serde_json::json!(new_base_factor),
            )]),
        )
        .expect("base_factor override on the live pool");
    let diffs = diff_indices(&forged, data);
    assert!(!diffs.is_empty(), "base_factor should have changed");
    assert!(
        diffs
            .iter()
            .all(|i| (base_factor_offset..base_factor_offset + base_factor_size).contains(i)),
        "only parameters.base_factor's own bytes should change, got {diffs:?}"
    );
    assert_eq!(read_u16(&forged, base_factor_offset), new_base_factor);
}

/// An `active_id` override is only usable if the bin array covering that bin exists and holds
/// liquidity, so the template's `llm_context` tells callers to check it. This proves the recipe
/// that check uses, on both fixture pools.
#[tokio::test]
async fn active_bin_array_exists_and_holds_liquidity() {
    let pools = fetch(&[SOL_USDC, USDT_SOL]).await;

    let registry = TemplateRegistry::new();
    let idl = &registry.get("meteora-dlmm-custom").expect("template").idl;
    let (active_id_offset, _) = field_offset(idl, "LbPair", "active_id");
    let bin_array_discriminator = discriminator(idl, "BinArray");

    let derived: Vec<(Pubkey, i64, i32)> = [SOL_USDC, USDT_SOL]
        .iter()
        .zip(&pools)
        .map(|(address, account)| {
            let active_id = read_i32(&account.data, active_id_offset);
            let (bin_array, index) = bin_array_address(&Pubkey::from_str_const(address), active_id);
            (bin_array, index, active_id)
        })
        .collect();

    let addresses: Vec<String> = derived.iter().map(|(a, _, _)| a.to_string()).collect();
    let arrays = fetch(&addresses.iter().map(|a| a.as_str()).collect::<Vec<_>>()).await;

    for ((bin_array, index, active_id), account) in derived.iter().zip(&arrays) {
        assert_eq!(
            account.owner, PROGRAM_ID,
            "{bin_array} is not owned by the DLMM program"
        );
        assert_eq!(
            account.data.len(),
            BIN_ARRAY_SIZE,
            "{bin_array} is {} bytes, expected {BIN_ARRAY_SIZE}",
            account.data.len()
        );
        assert_eq!(
            &account.data[..8],
            bin_array_discriminator.as_slice(),
            "{bin_array} is not a BinArray"
        );
        assert_eq!(
            i64::from_le_bytes(account.data[8..16].try_into().expect("8 bytes")),
            *index,
            "{bin_array} stores an index other than the one it was derived from"
        );

        let slot = (active_id - (*index as i32) * BINS_PER_ARRAY) as usize;
        let bin = BIN_ARRAY_BINS_OFFSET + slot * BIN_SIZE + BIN_LIQUIDITY_SUPPLY_OFFSET;
        let liquidity =
            u128::from_le_bytes(account.data[bin..bin + 16].try_into().expect("16 bytes"));
        assert!(
            liquidity > 0,
            "bin {active_id} of {bin_array} is empty, so a swap moved there would return nothing"
        );
    }
}
