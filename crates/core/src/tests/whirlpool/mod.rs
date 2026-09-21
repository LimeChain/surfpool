//! Whirlpool integration tests.
//!
//! These fetch the real accounts from mainnet rather than embedding captured copies, so they need
//! a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests whirlpool -- --test-threads=1
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! What these cover that the unit tests cannot: a synthetic account is built *by* the bundled IDL,
//! so it can never disagree with it. Real accounts carry live enum discriminants and populated
//! reward arrays, so an IDL that has drifted from the on-chain layout shows up as a byte diff here
//! and nowhere else.

use std::collections::HashMap;

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const WHIRLPOOL_PROGRAM_ID: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const WHIRLPOOLS_CONFIG: &str = "2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ";

/// The audited SOL/USDC pool: verified live on 2026-09-20 to have `whirlpools_config` ==
/// [`WHIRLPOOLS_CONFIG`], tick_spacing 64, and to reproduce its own address from the standard
/// 5-seed derivation.
const SOL_USDC: &str = "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ";

/// A second, distinct pool (SOL/USDC, tick spacing 4) for tests that need more than one address.
const SOL_USDC_TS4: &str = "Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE";

/// Offsets into a live `Whirlpool` account, including the 8-byte discriminator.
const FEE_RATE_OFFSET: usize = 45;
const SQRT_PRICE_OFFSET: usize = 65;
const SQRT_PRICE_LEN: usize = 16;
const CONFIG_OFFSET: usize = 8;
const CONFIG_LEN: usize = 32;

/// Fetches the accounts in one request, so every account returned is from the same slot. Returns
/// `None` entries as-is so callers can distinguish "does not exist" from "network error".
async fn fetch(addresses: &[Pubkey]) -> Vec<Option<(Vec<u8>, Pubkey)>> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );

    client
        .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .map(|result| match result {
            GetAccountResult::FoundAccount(_, account, _)
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => {
                Some((account.data, account.owner))
            }
            GetAccountResult::None(_) => None,
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

fn derive_tick_array(whirlpool: &Pubkey, start_index: i32) -> Pubkey {
    let program_id = Pubkey::from_str_const(WHIRLPOOL_PROGRAM_ID);
    let (pda, _bump) = Pubkey::find_program_address(
        &[
            b"tick_array",
            whirlpool.as_ref(),
            start_index.to_string().as_bytes(),
        ],
        &program_id,
    );
    pda
}

fn tick_array_start_index(tick_current_index: i32, tick_spacing: u16) -> i32 {
    let ticks_in_array = tick_spacing as i32 * 88;
    tick_current_index.div_euclid(ticks_in_array) * ticks_in_array
}

/// A failure here means the bundled IDL disagrees with the live on-chain layout.
#[tokio::test]
async fn real_mainnet_whirlpool_round_trips_unchanged() {
    let pubkey = Pubkey::from_str_const(SOL_USDC);
    let fetched = fetch(&[pubkey]).await;
    let (data, owner) = fetched[0].clone().unwrap_or_else(|| {
        panic!("{SOL_USDC} no longer exists on mainnet; the test needs a new address")
    });

    assert_eq!(
        owner,
        Pubkey::from_str_const(WHIRLPOOL_PROGRAM_ID),
        "the SOL/USDC pool must still be owned by the Whirlpool program"
    );
    assert_eq!(data.len(), 653, "Whirlpool account size must be 653 bytes");

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get("whirlpool-custom")
        .expect("whirlpool-custom template");

    let account_def = template
        .idl
        .accounts
        .iter()
        .find(|a| a.name == "Whirlpool")
        .expect("Whirlpool not in the IDL");
    assert_eq!(
        &data[..8],
        account_def.discriminator.as_slice(),
        "Whirlpool discriminator does not match the IDL - wrong account type?"
    );

    let forged = surfnet_svm
        .get_forged_account_data(&pubkey, &data, &template.idl, &HashMap::new())
        .expect("live mainnet Whirlpool should decode/re-encode with the bundled IDL");

    assert_eq!(
        forged.len(),
        data.len(),
        "Whirlpool changed size on round-trip"
    );
    let diffs = diff_indices(&forged, &data);
    assert!(
        diffs.is_empty(),
        "live mainnet Whirlpool was altered by a no-op round-trip at {} byte(s), first at {:?}",
        diffs.len(),
        diffs.first()
    );
}

/// Catches collateral damage against real reward-array padding and live enum discriminants that a
/// synthetic account cannot exercise, and proves the two documented economic levers only move
/// their own bytes.
#[tokio::test]
async fn fee_rate_and_sqrt_price_overrides_touch_only_target_bytes() {
    let pubkey = Pubkey::from_str_const(SOL_USDC);
    let fetched = fetch(&[pubkey]).await;
    let (data, _owner) = fetched[0].clone().unwrap_or_else(|| {
        panic!("{SOL_USDC} no longer exists on mainnet; the test needs a new address")
    });

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get("whirlpool-custom")
        .expect("whirlpool-custom template");

    // fee_rate: one u16 at a known offset.
    let original_fee_rate = u16::from_le_bytes(
        data[FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let new_fee_rate = original_fee_rate.wrapping_add(500);

    let forged = surfnet_svm
        .get_forged_account_data(
            &pubkey,
            &data,
            &template.idl,
            &HashMap::from([("fee_rate".to_string(), serde_json::json!(new_fee_rate))]),
        )
        .expect("fee_rate override on live Whirlpool");

    assert_eq!(
        diff_indices(&forged, &data),
        vec![FEE_RATE_OFFSET, FEE_RATE_OFFSET + 1],
        "exactly the two fee_rate bytes should change"
    );
    assert_eq!(
        u16::from_le_bytes(
            forged[FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2]
                .try_into()
                .unwrap()
        ),
        new_fee_rate
    );

    // sqrt_price: one u128 at a known offset.
    let original_sqrt_price = u128::from_le_bytes(
        data[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + SQRT_PRICE_LEN]
            .try_into()
            .unwrap(),
    );
    let new_sqrt_price = original_sqrt_price / 2;

    let forged = surfnet_svm
        .get_forged_account_data(
            &pubkey,
            &data,
            &template.idl,
            &HashMap::from([(
                "sqrt_price".to_string(),
                serde_json::json!(new_sqrt_price.to_string()),
            )]),
        )
        .expect("sqrt_price override on live Whirlpool");

    let diffs = diff_indices(&forged, &data);
    assert!(!diffs.is_empty(), "sqrt_price should have changed");
    assert!(
        diffs
            .iter()
            .all(|i| (SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + SQRT_PRICE_LEN).contains(i)),
        "only the 16 bytes of sqrt_price should change, got {diffs:?}"
    );
    assert_eq!(
        u128::from_le_bytes(
            forged[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + SQRT_PRICE_LEN]
                .try_into()
                .unwrap()
        ),
        new_sqrt_price
    );
}

/// The `whirlpools-config` template's literal address is the singleton the live SOL/USDC pool was
/// created under (VERIFY-15: compared against the live field, not re-derived).
#[tokio::test]
async fn whirlpools_config_singleton_matches_a_live_pool() {
    let registry = TemplateRegistry::new();
    let pubkey = Pubkey::from_str_const(SOL_USDC);

    let config_template = registry
        .get("whirlpools-config")
        .expect("whirlpools-config template");
    let config_literal = config_template
        .address
        .resolve_simple()
        .expect("whirlpools-config address should resolve");
    assert_eq!(config_literal.to_string(), WHIRLPOOLS_CONFIG);

    let fetched = fetch(&[pubkey]).await;
    let (data, _owner) = fetched[0].clone().unwrap_or_else(|| {
        panic!("{SOL_USDC} no longer exists on mainnet; the test needs a new address")
    });
    assert_eq!(
        &data[CONFIG_OFFSET..CONFIG_OFFSET + CONFIG_LEN],
        config_literal.as_ref(),
        "{pubkey}'s live whirlpools_config field must equal the whirlpools-config template's \
         literal address, not merely re-derive the same constant"
    );
}

/// The TickArray covering the pool's current tick must exist for a swap to find liquidity there.
#[tokio::test]
async fn tick_array_for_current_tick_exists_on_two_pools() {
    let pools = [
        Pubkey::from_str_const(SOL_USDC),
        Pubkey::from_str_const(SOL_USDC_TS4),
    ];
    let fetched = fetch(&pools).await;

    let mut tick_arrays = Vec::new();
    for (pubkey, entry) in pools.iter().zip(fetched.iter()) {
        let (data, _owner) = entry
            .clone()
            .unwrap_or_else(|| panic!("{pubkey} no longer exists on mainnet"));
        let tick_spacing = u16::from_le_bytes(data[41..43].try_into().unwrap());
        let tick_current_index = i32::from_le_bytes(data[81..85].try_into().unwrap());
        let start_index = tick_array_start_index(tick_current_index, tick_spacing);
        tick_arrays.push(derive_tick_array(pubkey, start_index));
    }

    let fetched_arrays = fetch(&tick_arrays).await;
    let program_id = Pubkey::from_str_const(WHIRLPOOL_PROGRAM_ID);
    for (tick_array, entry) in tick_arrays.iter().zip(fetched_arrays.iter()) {
        let (data, owner) = entry
            .clone()
            .unwrap_or_else(|| panic!("TickArray {tick_array} should exist for the current tick"));
        assert_eq!(
            owner, program_id,
            "TickArray {tick_array} must be owned by the Whirlpool program"
        );
        assert_eq!(
            data.len(),
            9988,
            "TickArray {tick_array} must be 9988 bytes"
        );
    }
}
