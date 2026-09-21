//! Raydium CLMM (v3) integration tests.
//!
//! These fetch real accounts from mainnet rather than embedding captured copies, so they need
//! a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests raydium_clmm -- --test-threads=1
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! What these cover that the unit tests cannot: `PoolState` is a packed zero-copy struct, not
//! Borsh by convention, so only a live account carries the padding and non-zero enum-adjacent
//! bytes that would expose an IDL drifted from the on-chain layout.

use std::collections::HashMap;

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

/// SOL/USDC at fee tier index 8, tick_spacing 1 - the pool named in the brief's verified facts.
const SOL_USDC_POOL: &str = "3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv";

/// Byte range of `PoolState::liquidity` (u128) within the full account, discriminator included.
const LIQUIDITY_RANGE: std::ops::Range<usize> = 237..253;
/// Byte range of `PoolState::tick_current` (i32) within the full account, discriminator included.
const TICK_CURRENT_RANGE: std::ops::Range<usize> = 269..273;

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[&str]) -> Vec<Vec<u8>> {
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
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account.data,
            GetAccountResult::None(_) => {
                panic!("{address} no longer exists on mainnet; the test needs a new address")
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

/// A failure here means the bundled IDL disagrees with the live on-chain `PoolState` layout.
#[tokio::test]
async fn real_mainnet_pool_round_trips_unchanged() {
    let data = fetch(&[SOL_USDC_POOL]).await.remove(0);

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get("raydium-clmm-pool-state")
        .expect("template raydium-clmm-pool-state should exist");

    let account_def = template
        .idl
        .accounts
        .iter()
        .find(|a| a.name == "PoolState")
        .expect("PoolState not in the IDL");
    assert_eq!(
        &data[..8],
        account_def.discriminator.as_slice(),
        "PoolState discriminator does not match the IDL - wrong account type?"
    );

    let pubkey = Pubkey::new_unique();
    let forged = surfnet_svm
        .get_forged_account_data(&pubkey, &data, &template.idl, &HashMap::new())
        .unwrap_or_else(|e| panic!("live mainnet PoolState failed to decode/re-encode: {e}"));

    assert_eq!(
        forged.len(),
        data.len(),
        "PoolState changed size on round-trip"
    );
    let diffs = diff_indices(&forged, &data);
    assert!(
        diffs.is_empty(),
        "live mainnet PoolState was altered by a no-op round-trip at {} byte(s), first at {:?}",
        diffs.len(),
        diffs.first()
    );
}

/// Catches collateral damage from the Borsh re-encode of a packed zero-copy struct against real
/// padding, which a synthetic account cannot exercise.
#[tokio::test]
async fn overrides_on_real_pool_touch_only_their_offsets() {
    let data = fetch(&[SOL_USDC_POOL]).await.remove(0);

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get("raydium-clmm-pool-state")
        .expect("template raydium-clmm-pool-state should exist");
    let pubkey = Pubkey::new_unique();

    let original_liquidity =
        u128::from_le_bytes(data[LIQUIDITY_RANGE].try_into().expect("16 bytes"));
    let new_liquidity = original_liquidity / 2;
    assert_ne!(
        new_liquidity, original_liquidity,
        "the live pool should have non-zero liquidity"
    );

    let forged = surfnet_svm
        .get_forged_account_data(
            &pubkey,
            &data,
            &template.idl,
            &HashMap::from([(
                "liquidity".to_string(),
                serde_json::json!(new_liquidity.to_string()),
            )]),
        )
        .expect("liquidity override on live pool");

    let diffs = diff_indices(&forged, &data);
    assert!(!diffs.is_empty(), "liquidity should have changed");
    assert!(
        diffs.iter().all(|i| LIQUIDITY_RANGE.contains(i)),
        "liquidity override touched bytes outside its own field: {diffs:?}"
    );
    assert_eq!(
        u128::from_le_bytes(forged[LIQUIDITY_RANGE].try_into().expect("16 bytes")),
        new_liquidity
    );

    let original_tick = i32::from_le_bytes(data[TICK_CURRENT_RANGE].try_into().expect("4 bytes"));
    let new_tick = original_tick - 1000;

    let forged = surfnet_svm
        .get_forged_account_data(
            &pubkey,
            &data,
            &template.idl,
            &HashMap::from([("tick_current".to_string(), serde_json::json!(new_tick))]),
        )
        .expect("tick_current override on live pool");

    let diffs = diff_indices(&forged, &data);
    assert!(!diffs.is_empty(), "tick_current should have changed");
    assert!(
        diffs.iter().all(|i| TICK_CURRENT_RANGE.contains(i)),
        "tick_current override touched bytes outside its own field: {diffs:?}"
    );
    assert_eq!(
        i32::from_le_bytes(forged[TICK_CURRENT_RANGE].try_into().expect("4 bytes")),
        new_tick
    );
}

/// Every `amm_config_index` option must both derive its documented address (static, already
/// covered in `registry.rs`) and correspond to a real 117-byte `AmmConfig` account on mainnet.
#[tokio::test]
async fn every_amm_config_index_option_is_a_live_amm_config_account() {
    let registry = TemplateRegistry::new();
    let template = registry
        .get("raydium-clmm-amm-config")
        .expect("template raydium-clmm-amm-config should exist");
    let amm_config_index = template
        .constants
        .get("amm_config_index")
        .expect("amm_config_index constant should exist");
    assert!(
        !amm_config_index.options.is_empty(),
        "the fee tiers are the fixture here"
    );

    let account_def = template
        .idl
        .accounts
        .iter()
        .find(|a| a.name == "AmmConfig")
        .expect("AmmConfig not in the IDL");

    let addresses: Vec<String> = amm_config_index
        .options
        .iter()
        .map(|option| {
            option
                .metadata
                .get("derived_address")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("option {} documents no derived_address", option.id))
                .to_string()
        })
        .collect();
    let address_refs: Vec<&str> = addresses.iter().map(String::as_str).collect();
    let accounts = fetch(&address_refs).await;

    for (option, data) in amm_config_index.options.iter().zip(&accounts) {
        assert_eq!(
            data.len(),
            117,
            "AmmConfig for option {} should be 117 bytes live, got {}",
            option.id,
            data.len()
        );
        assert_eq!(
            &data[..8],
            account_def.discriminator.as_slice(),
            "AmmConfig for option {} has the wrong discriminator live",
            option.id
        );
    }
}
