//! Meteora DLMM integration tests. They fetch real mainnet accounts, so they need a network
//! connection and `--features integration-tests`. `SURFPOOL_TEST_RPC_URL` overrides the endpoint.

use std::collections::HashMap;

use anchor_lang_idl::types::{Idl, IdlArrayLen, IdlDefinedFields, IdlType, IdlTypeDefTy};
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::meteora::dlmm::v1::price_shock_builder::{
            build_price_shock_scenario, plan_price_shock,
        },
    },
    surfnet::{
        GetAccountResult, locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm,
    },
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const PROGRAM_ID: Pubkey = Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
const SOL_USDC: Pubkey = Pubkey::from_str_const("BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y");
const USDT_SOL: Pubkey = Pubkey::from_str_const("HToiT8XK8GHgAT4N3oGXadc7opdApPwsbCL9tFRYa3Rg");

const LB_PAIR_SIZE: usize = 904;
const BASE_FACTOR_OFFSET: usize = 8;
const ACTIVE_ID_OFFSET: usize = 76;
const BIN_STEP_OFFSET: usize = 80;
const STATUS_OFFSET: usize = 82;

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    client
        .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
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

/// Size of an IDL type in the account buffer; only the fixed-width kinds `LbPair` is built from are supported.
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

fn read_i32(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(data[offset..offset + 4].try_into().expect("4 bytes"))
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().expect("2 bytes"))
}

#[tokio::test]
async fn live_lb_pair_matches_the_bundled_layout() {
    let accounts = fetch(&[SOL_USDC, USDT_SOL]).await;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry.get("meteora-dlmm-pool-state").expect("template");
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
            idl.accounts
                .iter()
                .find(|a| a.name == "LbPair")
                .expect("LbPair in the IDL")
                .discriminator
                .as_slice(),
            "{address} is not an LbPair"
        );

        let forged = surfnet_svm
            .get_forged_account_data(address, &account.data, idl, &HashMap::new())
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

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_materializer_touch_only_their_own_bytes() {
    let before = fetch(&[SOL_USDC]).await.remove(0);
    let data = &before.data;

    let new_active_id = read_i32(data, ACTIVE_ID_OFFSET) + 1;
    let new_base_factor = read_u16(data, BASE_FACTOR_OFFSET).wrapping_add(1);
    let new_status: u8 = if data[STATUS_OFFSET] == 0 { 1 } else { 0 };

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm.set_account(&SOL_USDC, before.clone()).unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let pool_override = OverrideInstance::new(
        "meteora-dlmm-pool-state".to_string(),
        1,
        AccountAddress::Pubkey(SOL_USDC.to_string()),
    )
    .with_values(HashMap::from([
        ("active_id".to_string(), serde_json::json!(new_active_id)),
        (
            "parameters.base_factor".to_string(),
            serde_json::json!(new_base_factor),
        ),
        ("status".to_string(), serde_json::json!(new_status)),
    ]));
    let mut scenario = Scenario::new(
        "Meteora DLMM overrides".to_string(),
        "Override a live LbPair through the materializer.".to_string(),
    );
    scenario.add_override(pool_override);
    locker.register_scenario(scenario, Some(0)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm| svm.get_account(&SOL_USDC))
        .unwrap()
        .expect("account should still exist after materialization");
    assert_eq!(
        after.data.len(),
        data.len(),
        "override changed account size"
    );

    let allowed = [
        BASE_FACTOR_OFFSET..BASE_FACTOR_OFFSET + 2,
        ACTIVE_ID_OFFSET..ACTIVE_ID_OFFSET + 4,
        STATUS_OFFSET..STATUS_OFFSET + 1,
    ];
    let diffs = diff_indices(data, &after.data);
    assert!(
        diffs
            .iter()
            .all(|i| allowed.iter().any(|range| range.contains(i))),
        "bytes outside active_id, base_factor and status changed: {diffs:?}"
    );
    assert_eq!(read_i32(&after.data, ACTIVE_ID_OFFSET), new_active_id);
    assert_eq!(read_u16(&after.data, BASE_FACTOR_OFFSET), new_base_factor);
    assert_eq!(after.data[STATUS_OFFSET], new_status);
}

#[tokio::test]
async fn price_shock_decodes_live_pools_and_finds_their_bin_arrays() {
    let pools = [SOL_USDC, USDT_SOL];
    let accounts = fetch(&pools).await;

    let plans: Vec<_> = pools
        .iter()
        .zip(&accounts)
        .map(|(pool, account)| {
            let bin_step = read_u16(&account.data, BIN_STEP_OFFSET);
            let plan = plan_price_shock(*pool, account, 1.0 + f64::from(bin_step) / 10000.0)
                .unwrap_or_else(|e| panic!("live {pool} failed to plan: {e}"));
            assert_eq!(
                plan.old_active_id,
                read_i32(&account.data, ACTIVE_ID_OFFSET)
            );
            assert_eq!(plan.bin_step, bin_step);
            plan
        })
        .collect();

    let bin_arrays: Vec<Pubkey> = plans.iter().map(|plan| plan.bin_array).collect();
    for (plan, bin_array) in plans.iter().zip(fetch(&bin_arrays).await) {
        assert_eq!(
            i64::from_le_bytes(bin_array.data[8..16].try_into().expect("8 bytes")),
            plan.bin_array_index,
            "{} stores an index other than the one it was derived from",
            plan.bin_array
        );
        build_price_shock_scenario(*plan, Some(&bin_array)).unwrap_or_else(|e| {
            panic!(
                "bin array one bin away from live {} rejected: {e}",
                plan.pool
            )
        });
    }
}
