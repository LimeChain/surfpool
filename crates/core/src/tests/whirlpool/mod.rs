//! Whirlpool integration tests. They fetch real mainnet accounts, so they need a network
//! connection and `--features integration-tests`. `SURFPOOL_TEST_RPC_URL` overrides the endpoint.

use std::collections::HashMap;

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::whirlpool::v1::price_shock_builder::{
            build_price_shock_scenario, plan_price_shock, tick_array_start_index,
        },
    },
    surfnet::{
        GetAccountResult, locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm,
    },
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const SOL_USDC: &str = "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ";
const SOL_USDC_TS4: &str = "Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE";

const CONFIG_OFFSET: usize = 8;
const FEE_RATE_OFFSET: usize = 45;
const SQRT_PRICE_OFFSET: usize = 65;

/// Fetches the accounts in one request (same slot) and panics if any is missing.
async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
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
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
            GetAccountResult::None(pubkey) => {
                panic!("{pubkey} does not exist on mainnet; the test needs a new address")
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
async fn real_mainnet_whirlpool_round_trips_unchanged() {
    let pubkey = Pubkey::from_str_const(SOL_USDC);
    let data = fetch(&[pubkey]).await.remove(0).data;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get("whirlpool-pool-state")
        .expect("whirlpool-pool-state template");

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

#[tokio::test]
async fn whirlpools_config_singleton_matches_a_live_pool() {
    let registry = TemplateRegistry::new();
    let config = registry
        .get("whirlpools-config")
        .expect("whirlpools-config template")
        .address
        .resolve_simple()
        .expect("whirlpools-config address should resolve");

    let data = fetch(&[Pubkey::from_str_const(SOL_USDC)])
        .await
        .remove(0)
        .data;
    assert_eq!(
        &data[CONFIG_OFFSET..CONFIG_OFFSET + 32],
        config.as_ref(),
        "the live pool's whirlpools_config field must equal the whirlpools-config template's address"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_the_materializer_touch_only_their_own_bytes() {
    let pubkey = Pubkey::from_str_const(SOL_USDC_TS4);
    let account = fetch(&[pubkey]).await.remove(0);
    let data = account.data.clone();

    let new_fee_rate = u16::from_le_bytes(
        data[FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2]
            .try_into()
            .unwrap(),
    )
    .wrapping_add(500);
    let new_sqrt_price = u128::from_le_bytes(
        data[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16]
            .try_into()
            .unwrap(),
    ) / 2;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(surfnet_svm);
    locker
        .with_svm_writer(|svm| svm.set_account(&pubkey, account))
        .expect("seeding the live pool into the local SVM should succeed");

    let pool_override = OverrideInstance::new(
        "whirlpool-pool-state".to_string(),
        1,
        AccountAddress::Pubkey(pubkey.to_string()),
    )
    .with_values(HashMap::from([
        ("fee_rate".to_string(), serde_json::json!(new_fee_rate)),
        (
            "sqrt_price".to_string(),
            serde_json::json!(new_sqrt_price.to_string()),
        ),
    ]))
    .with_label("Whirlpool fee_rate and sqrt_price override".to_string());

    let mut scenario = Scenario::new(
        "Whirlpool byte-diff".to_string(),
        "Override a live pool's fee_rate and sqrt_price through the materializer.".to_string(),
    );
    scenario.add_override(pool_override);
    locker.register_scenario(scenario, Some(0)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm| svm.get_account(&pubkey))
        .unwrap()
        .expect("account should still exist after materialization")
        .data;

    assert_eq!(after.len(), data.len(), "override changed account size");
    let allowed =
        (FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2).chain(SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16);
    let allowed: Vec<usize> = allowed.collect();
    let diffs = diff_indices(&data, &after);
    assert!(
        !diffs.is_empty() && diffs.iter().all(|i| allowed.contains(i)),
        "only fee_rate and sqrt_price bytes may change, touched {diffs:?}"
    );
    assert_eq!(
        u16::from_le_bytes(
            after[FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2]
                .try_into()
                .unwrap()
        ),
        new_fee_rate
    );
    assert_eq!(
        u128::from_le_bytes(
            after[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16]
                .try_into()
                .unwrap()
        ),
        new_sqrt_price
    );
}

#[tokio::test]
async fn price_shock_builds_against_live_pools_and_their_real_tick_arrays() {
    let pools = [
        Pubkey::from_str_const(SOL_USDC),
        Pubkey::from_str_const(SOL_USDC_TS4),
    ];
    let pool_accounts = fetch(&pools).await;

    let mut plans = Vec::new();
    for (pool, account) in pools.iter().zip(pool_accounts.iter()) {
        let probe = plan_price_shock(*pool, account, 1.0001)
            .unwrap_or_else(|e| panic!("{pool} should decode through the bundled IDL: {e}"));
        // Aim mid-array so the target stays on the array the pool already sits on, which must exist.
        let current_start =
            tick_array_start_index(probe.old_tick_current_index, probe.tick_spacing);
        let mut target_tick = current_start + 44 * i32::from(probe.tick_spacing);
        if target_tick == probe.old_tick_current_index {
            target_tick += 1;
        }
        let price_factor = 1.0001_f64.powi(target_tick - probe.old_tick_current_index);
        plans.push(plan_price_shock(*pool, account, price_factor).expect("plan"));
    }

    let tick_arrays: Vec<Pubkey> = plans.iter().map(|plan| plan.tick_array).collect();
    let tick_array_accounts = fetch(&tick_arrays).await;
    for (plan, tick_array) in plans.into_iter().zip(tick_array_accounts.iter()) {
        let scenario = build_price_shock_scenario(plan, Some(tick_array)).unwrap_or_else(|e| {
            panic!(
                "{}: derived tick array {} was rejected: {e}",
                plan.pool, plan.tick_array
            )
        });
        assert_eq!(scenario.overrides.len(), 1);
    }
}
