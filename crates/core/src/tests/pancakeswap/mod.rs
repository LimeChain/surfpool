//! PancakeSwap CLMM integration tests. They fetch real mainnet accounts, so they need a network
//! connection and `--features integration-tests`. `SURFPOOL_TEST_RPC_URL` overrides the endpoint.

use std::collections::HashMap;

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{
        GetAccountResult, locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm,
    },
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const POOL_STATE_TEMPLATE: &str = "pancakeswap-clmm-pool-state";
const AMM_CONFIG_TEMPLATE: &str = "pancakeswap-clmm-amm-config";

const PANCAKESWAP_CLMM_PROGRAM: Pubkey =
    Pubkey::from_str_const("HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq");
const SOL_USDC_POOL: Pubkey =
    Pubkey::from_str_const("DJNtGuBGEQiUCWE8F981M2C3ZghZt2XLD8f2sQdZ6rsZ");

const AMM_CONFIG_RANGE: std::ops::Range<usize> = 9..41;
const LIQUIDITY_RANGE: std::ops::Range<usize> = 237..253;
const SQRT_PRICE_X64_RANGE: std::ops::Range<usize> = 253..269;
const TICK_CURRENT_RANGE: std::ops::Range<usize> = 269..273;
const STATUS_OFFSET: usize = 389;

const POOL_STATE_LEN: usize = 1544;
const AMM_CONFIG_LEN: usize = 117;

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
                panic!("{address} no longer exists on mainnet; the test needs a new address")
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

#[tokio::test]
async fn real_mainnet_pool_round_trips_unchanged() {
    let data = fetch(&[SOL_USDC_POOL]).await.remove(0).data;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get(POOL_STATE_TEMPLATE)
        .expect("pool state template should exist");
    let account_def = template
        .idl()
        .accounts
        .iter()
        .find(|a| a.name == "PoolState")
        .expect("PoolState not in the IDL");
    assert_eq!(
        data.len(),
        POOL_STATE_LEN,
        "the SOL/USDC pool has the wrong size live"
    );
    assert_eq!(
        &data[..8],
        account_def.discriminator.as_slice(),
        "the SOL/USDC pool has the wrong discriminator live"
    );

    let forged = surfnet_svm
        .get_forged_account_data(
            &Pubkey::new_unique(),
            &data,
            template.idl(),
            &HashMap::new(),
        )
        .unwrap_or_else(|e| panic!("live mainnet PoolState failed to decode/re-encode: {e}"));
    assert_eq!(
        forged, data,
        "live mainnet PoolState was altered by a no-op round-trip"
    );
}

#[tokio::test]
async fn pool_amm_config_at_offset_9_is_a_live_amm_config_that_round_trips() {
    let pool = fetch(&[SOL_USDC_POOL]).await.remove(0).data;
    let config_address = Pubkey::try_from(&pool[AMM_CONFIG_RANGE]).expect("32 bytes");
    let config = fetch(&[config_address]).await.remove(0);

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get(AMM_CONFIG_TEMPLATE)
        .expect("amm config template should exist");
    let account_def = template
        .idl()
        .accounts
        .iter()
        .find(|a| a.name == "AmmConfig")
        .expect("AmmConfig not in the IDL");
    assert_eq!(
        config.owner, PANCAKESWAP_CLMM_PROGRAM,
        "the SOL/USDC pool's AmmConfig has the wrong owner live"
    );
    assert_eq!(
        config.data.len(),
        AMM_CONFIG_LEN,
        "the SOL/USDC pool's AmmConfig has the wrong size live"
    );
    assert_eq!(
        &config.data[..8],
        account_def.discriminator.as_slice(),
        "the SOL/USDC pool's AmmConfig has the wrong discriminator live"
    );

    let forged = surfnet_svm
        .get_forged_account_data(
            &config_address,
            &config.data,
            template.idl(),
            &HashMap::new(),
        )
        .unwrap_or_else(|e| panic!("live mainnet AmmConfig failed to decode/re-encode: {e}"));
    assert_eq!(
        forged, config.data,
        "live mainnet AmmConfig was altered by a no-op round-trip"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_materializer_touch_only_their_own_bytes() {
    let before = fetch(&[SOL_USDC_POOL]).await.remove(0);

    let registry = TemplateRegistry::new();
    let template = registry
        .get(POOL_STATE_TEMPLATE)
        .expect("pool state template should exist");
    let account_def = template
        .idl()
        .accounts
        .iter()
        .find(|a| a.name == "PoolState")
        .expect("PoolState not in the IDL");
    assert_eq!(
        before.data.len(),
        POOL_STATE_LEN,
        "the SOL/USDC pool has the wrong size live"
    );
    assert_eq!(
        &before.data[..8],
        account_def.discriminator.as_slice(),
        "the SOL/USDC pool has the wrong discriminator live"
    );

    let new_status: u8 = if before.data[STATUS_OFFSET] == 0 {
        31
    } else {
        0
    };
    let new_liquidity = u128::from_le_bytes(before.data[LIQUIDITY_RANGE].try_into().unwrap()) ^ 1;
    let new_sqrt_price_x64 =
        u128::from_le_bytes(before.data[SQRT_PRICE_X64_RANGE].try_into().unwrap()) + (1u128 << 64);
    let new_tick = i32::from_le_bytes(before.data[TICK_CURRENT_RANGE].try_into().unwrap()) - 1000;

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm
        .set_account(&SOL_USDC_POOL, before.clone())
        .unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let pool_override = OverrideInstance::new(
        template.id.clone(),
        1,
        AccountAddress::Pubkey(SOL_USDC_POOL.to_string()),
    )
    .with_values(HashMap::from([
        ("status".to_string(), serde_json::json!(new_status)),
        (
            "liquidity".to_string(),
            serde_json::json!(new_liquidity.to_string()),
        ),
        (
            "sqrt_price_x64".to_string(),
            serde_json::json!(new_sqrt_price_x64.to_string()),
        ),
        ("tick_current".to_string(), serde_json::json!(new_tick)),
    ]))
    .with_label("PancakeSwap CLMM pool override".to_string());

    let mut scenario = Scenario::new(
        "PancakeSwap CLMM Pool Override".to_string(),
        "Change a live PoolState's status, liquidity, price and tick through the materializer."
            .to_string(),
    );
    scenario.add_override(pool_override);
    locker.register_scenario(scenario, Some(0)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm| svm.get_account(&SOL_USDC_POOL))
        .unwrap()
        .expect("account should still exist after materialization");

    assert_eq!(
        after.data.len(),
        before.data.len(),
        "override changed account size"
    );
    let outside: Vec<usize> = diff_indices(&before.data, &after.data)
        .into_iter()
        .filter(|i| {
            *i != STATUS_OFFSET
                && !LIQUIDITY_RANGE.contains(i)
                && !SQRT_PRICE_X64_RANGE.contains(i)
                && !TICK_CURRENT_RANGE.contains(i)
        })
        .collect();
    assert!(
        outside.is_empty(),
        "bytes outside the overridden fields changed: {outside:?}"
    );
    assert_eq!(after.data[STATUS_OFFSET], new_status);
    assert_eq!(
        u128::from_le_bytes(after.data[LIQUIDITY_RANGE].try_into().unwrap()),
        new_liquidity
    );
    assert_eq!(
        u128::from_le_bytes(after.data[SQRT_PRICE_X64_RANGE].try_into().unwrap()),
        new_sqrt_price_x64
    );
    assert_eq!(
        i32::from_le_bytes(after.data[TICK_CURRENT_RANGE].try_into().unwrap()),
        new_tick
    );
}
