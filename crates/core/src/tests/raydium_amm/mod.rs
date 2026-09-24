//! Raydium AMM v4 integration tests. They fetch real mainnet accounts, so they need a network
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

const AMM_V4_PROGRAM: Pubkey =
    Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");

const SOL_USDC: Pubkey = Pubkey::from_str_const("58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2");
const RAY_USDC: Pubkey = Pubkey::from_str_const("6UmmUiYoBjSrhakAobJw8BvkmJtDVxaeBtbt7rxWo1mg");
const RAY_SOL: Pubkey = Pubkey::from_str_const("AVs9TA4nWDzfPJE9gGVNJMVhcQy3V9PGazuz33BfG2RA");

/// `AmmInfo` is repr(C, packed) of u64/u128/Pubkey only, so Borsh lands on the same offsets.
const STATUS: usize = 0;
const SWAP_FEE_NUMERATOR: usize = 176;
const TARGET_ORDERS: usize = 592;

const AMM_INFO_LEN: usize = 752;
const TARGET_ORDERS_LEN: usize = 2208;

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
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => {
                assert_eq!(
                    account.owner, AMM_V4_PROGRAM,
                    "{address} is not owned by AMM v4 any more"
                );
                account
            }
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

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

#[tokio::test]
async fn real_mainnet_pools_round_trip_unchanged() {
    let pools = [SOL_USDC, RAY_USDC, RAY_SOL];
    let accounts = fetch(&pools).await;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let idl = &registry
        .get("raydium-amm-pool-state")
        .expect("template")
        .idl;
    let pubkey = Pubkey::new_unique();

    for (address, account) in pools.iter().zip(&accounts) {
        let data = &account.data;
        assert_eq!(
            data.len(),
            AMM_INFO_LEN,
            "{address} is not an AmmInfo-sized account"
        );

        let forged = surfnet_svm
            .get_forged_account_data(&pubkey, data, idl, &HashMap::new())
            .unwrap_or_else(|e| {
                panic!("live pool {address} failed to decode/re-encode with the bundled IDL: {e}")
            });
        assert_eq!(
            forged, *data,
            "live pool {address} was altered by a no-op round-trip"
        );
    }
}

#[tokio::test]
async fn a_live_target_orders_account_resolves_to_its_own_type() {
    let pools = fetch(&[SOL_USDC]).await;
    let target_orders =
        Pubkey::try_from(&pools[0].data[TARGET_ORDERS..TARGET_ORDERS + 32]).unwrap();

    let accounts = fetch(&[target_orders]).await;
    let data = &accounts[0].data;
    assert_eq!(
        data.len(),
        TARGET_ORDERS_LEN,
        "{target_orders} is not a TargetOrders-sized account"
    );

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let idl = &registry
        .get("raydium-amm-pool-state")
        .expect("template")
        .idl;

    let forged = surfnet_svm
        .get_forged_account_data(&Pubkey::new_unique(), data, idl, &HashMap::new())
        .expect("a 2208-byte account must resolve to TargetOrders");
    assert_eq!(
        forged, *data,
        "the live TargetOrders was altered by a no-op round-trip"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_materializer_touch_only_their_own_bytes() {
    let before = fetch(&[SOL_USDC]).await.remove(0);

    let new_status: u64 = if read_u64(&before.data, STATUS) == 0 {
        1
    } else {
        0
    };
    let new_fee = read_u64(&before.data, SWAP_FEE_NUMERATOR) + 1;

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm.set_account(&SOL_USDC, before.clone()).unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let registry = TemplateRegistry::new();
    let template = registry
        .get("raydium-amm-pool-state")
        .expect("raydium-amm-pool-state template should exist");

    let pool_override = OverrideInstance::new(
        template.id.clone(),
        1,
        AccountAddress::Pubkey(SOL_USDC.to_string()),
    )
    .with_values(HashMap::from([
        ("status".to_string(), serde_json::json!(new_status)),
        (
            "fees.swap_fee_numerator".to_string(),
            serde_json::json!(new_fee),
        ),
    ]))
    .with_label("Raydium AMM v4 status and fee override".to_string());

    let mut scenario = Scenario::new(
        "Raydium AMM v4 Override".to_string(),
        "Change a live AmmInfo's status and swap fee through the materializer.".to_string(),
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
        before.data.len(),
        "override changed account size"
    );
    let outside: Vec<usize> = diff_indices(&before.data, &after.data)
        .into_iter()
        .filter(|i| {
            !(STATUS..STATUS + 8).contains(i)
                && !(SWAP_FEE_NUMERATOR..SWAP_FEE_NUMERATOR + 8).contains(i)
        })
        .collect();
    assert!(
        outside.is_empty(),
        "bytes outside status and swap_fee_numerator changed: {outside:?}"
    );
    assert_eq!(read_u64(&after.data, STATUS), new_status);
    assert_eq!(read_u64(&after.data, SWAP_FEE_NUMERATOR), new_fee);
}
