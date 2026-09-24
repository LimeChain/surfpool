//! Raydium CLMM integration tests. They fetch real mainnet accounts, so they need a network
//! connection and `--features integration-tests`. `SURFPOOL_TEST_RPC_URL` overrides the endpoint.

use std::collections::HashMap;

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::raydium::v3::price_shock_builder::{
            RAYDIUM, build_price_shock_scenario, plan_price_shock, tick_array_start_index,
        },
    },
    surfnet::{
        GetAccountResult, locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm,
    },
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const SOL_USDC_POOL: Pubkey =
    Pubkey::from_str_const("3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv");

const LIQUIDITY_RANGE: std::ops::Range<usize> = 237..253;
const TICK_CURRENT_RANGE: std::ops::Range<usize> = 269..273;
const STATUS_OFFSET: usize = 389;

const TRADE_FEE_RATE_OFFSET: usize = 47;
const TICK_SPACING_OFFSET: usize = 51;
const AMM_CONFIG_LEN: usize = 117;

const TICK_ARRAY_POOL_ID_RANGE: std::ops::Range<usize> = 8..40;
const TICK_ARRAY_START_INDEX_RANGE: std::ops::Range<usize> = 40..44;
const TICK_ARRAY_LEN: usize = 10240;

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
        .get(RAYDIUM.pool_state_template)
        .expect("pool state template should exist");

    let forged = surfnet_svm
        .get_forged_account_data(&Pubkey::new_unique(), &data, &template.idl, &HashMap::new())
        .unwrap_or_else(|e| panic!("live mainnet PoolState failed to decode/re-encode: {e}"));
    assert_eq!(
        forged, data,
        "live mainnet PoolState was altered by a no-op round-trip"
    );
}

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

    let addresses: Vec<Pubkey> = amm_config_index
        .options
        .iter()
        .map(|option| {
            let address = option
                .metadata
                .get("derived_address")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("option {} documents no derived_address", option.id));
            Pubkey::from_str_const(address)
        })
        .collect();
    let accounts = fetch(&addresses).await;

    for (option, account) in amm_config_index.options.iter().zip(&accounts) {
        let data = &account.data;
        assert_eq!(
            data.len(),
            AMM_CONFIG_LEN,
            "AmmConfig for option {} has the wrong size live",
            option.id
        );
        assert_eq!(
            &data[..8],
            account_def.discriminator.as_slice(),
            "AmmConfig for option {} has the wrong discriminator live",
            option.id
        );

        let tick_spacing = u16::from_le_bytes(
            data[TICK_SPACING_OFFSET..TICK_SPACING_OFFSET + 2]
                .try_into()
                .unwrap(),
        );
        let trade_fee_rate = u32::from_le_bytes(
            data[TRADE_FEE_RATE_OFFSET..TRADE_FEE_RATE_OFFSET + 4]
                .try_into()
                .unwrap(),
        );

        let expect_u64 = |field: &str| -> u64 {
            option
                .metadata
                .get(field)
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| panic!("option {} documents no {field} metadata", option.id))
        };

        assert_eq!(
            tick_spacing as u64,
            expect_u64("tick_spacing"),
            "option {} tick_spacing metadata disagrees with the live AmmConfig",
            option.id
        );
        assert_eq!(
            trade_fee_rate as u64,
            expect_u64("fee_rate_bps") * 100,
            "option {} live trade_fee_rate should be fee_rate_bps * 100",
            option.id
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_materializer_touch_only_their_own_bytes() {
    let before = fetch(&[SOL_USDC_POOL]).await.remove(0);

    let new_status: u8 = if before.data[STATUS_OFFSET] == 0 {
        31
    } else {
        0
    };
    let new_liquidity = u128::from_le_bytes(before.data[LIQUIDITY_RANGE].try_into().unwrap()) ^ 1;
    let new_tick = i32::from_le_bytes(before.data[TICK_CURRENT_RANGE].try_into().unwrap()) - 1000;

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm
        .set_account(&SOL_USDC_POOL, before.clone())
        .unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let registry = TemplateRegistry::new();
    let template = registry
        .get(RAYDIUM.pool_state_template)
        .expect("pool state template should exist");

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
        ("tick_current".to_string(), serde_json::json!(new_tick)),
    ]))
    .with_label("Raydium CLMM pool override".to_string());

    let mut scenario = Scenario::new(
        "Raydium CLMM Pool Override".to_string(),
        "Change a live PoolState's status, liquidity and tick through the materializer."
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
            *i != STATUS_OFFSET && !LIQUIDITY_RANGE.contains(i) && !TICK_CURRENT_RANGE.contains(i)
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
        i32::from_le_bytes(after.data[TICK_CURRENT_RANGE].try_into().unwrap()),
        new_tick
    );
}

#[tokio::test]
async fn price_shock_on_the_live_pool_finds_its_tick_array() {
    let pool_account = fetch(&[SOL_USDC_POOL]).await.remove(0);

    // Target a tick inside the pool's current array, so that array is guaranteed to exist.
    let probe = plan_price_shock(RAYDIUM, SOL_USDC_POOL, &pool_account, 1.0001)
        .expect("the live PoolState decodes through the IDL");
    let ticks_in_array = 60 * i32::from(probe.tick_spacing);
    let current_start = tick_array_start_index(probe.old_tick_current, probe.tick_spacing);
    let mut target_tick = current_start + ticks_in_array / 2;
    if target_tick == probe.old_tick_current {
        target_tick += 1;
    }
    let price_factor = 1.0001_f64.powi(target_tick - probe.old_tick_current);

    let plan = plan_price_shock(RAYDIUM, SOL_USDC_POOL, &pool_account, price_factor).expect("plan");
    assert_eq!(
        plan.tick_array_start_index, current_start,
        "the target tick was chosen to stay on the pool's current array"
    );

    let tick_array = fetch(&[plan.tick_array]).await.remove(0);
    assert_eq!(
        tick_array.owner, RAYDIUM.program_id,
        "the derived tick array is not owned by the CLMM program - PDA recipe is wrong"
    );
    assert_eq!(tick_array.data.len(), TICK_ARRAY_LEN);
    assert_eq!(
        &tick_array.data[TICK_ARRAY_POOL_ID_RANGE],
        SOL_USDC_POOL.as_ref(),
        "the derived tick array belongs to a different pool"
    );
    assert_eq!(
        i32::from_le_bytes(
            tick_array.data[TICK_ARRAY_START_INDEX_RANGE]
                .try_into()
                .expect("4 bytes")
        ),
        plan.tick_array_start_index,
        "the live array's start_tick_index disagrees with get_array_start_index"
    );

    build_price_shock_scenario(plan, Some(&tick_array)).expect("scenario should build");
}
