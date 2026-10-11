//! Raydium CPMM integration tests. They fetch real mainnet accounts, so they need a network
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

const POOL_STATE_TEMPLATE: &str = "raydium-cpmm-pool-state";
const AMM_CONFIG_TEMPLATE: &str = "raydium-cpmm-amm-config";

const PROGRAM_ID: Pubkey = Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
const WSOL_USELESS_POOL: Pubkey =
    Pubkey::from_str_const("Q2sPHPdUWFMg7M7wwrQKLrn619cAucfRsmhVJffodSp");
const SOL_USDC_POOL: Pubkey =
    Pubkey::from_str_const("47hq28mcL7q5GhBg7epyGF2dnuJd4MKFt8QhT7CzYUp4");
const AMM_CONFIG_INDEX_0: Pubkey =
    Pubkey::from_str_const("D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2");

const POOL_STATE_LEN: usize = 637;
const AMM_CONFIG_LEN: usize = 236;
const AMM_CONFIG_RANGE: std::ops::Range<usize> = 8..40;
const VAULT_0_RANGE: std::ops::Range<usize> = 72..104;
const VAULT_1_RANGE: std::ops::Range<usize> = 104..136;
const MINT_0_RANGE: std::ops::Range<usize> = 168..200;
const MINT_1_RANGE: std::ops::Range<usize> = 200..232;
const STATUS_OFFSET: usize = 329;
const OPEN_TIME_RANGE: std::ops::Range<usize> = 373..381;
const TRADE_FEE_RATE_RANGE: std::ops::Range<usize> = 12..20;

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

fn pubkey_at(data: &[u8], range: std::ops::Range<usize>) -> Pubkey {
    Pubkey::try_from(&data[range]).unwrap()
}

#[tokio::test]
async fn real_mainnet_accounts_round_trip_unchanged() {
    let accounts = fetch(&[WSOL_USELESS_POOL, SOL_USDC_POOL, AMM_CONFIG_INDEX_0]).await;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    for (account, template_id, len) in [
        (&accounts[0], POOL_STATE_TEMPLATE, POOL_STATE_LEN),
        (&accounts[1], POOL_STATE_TEMPLATE, POOL_STATE_LEN),
        (&accounts[2], AMM_CONFIG_TEMPLATE, AMM_CONFIG_LEN),
    ] {
        assert_eq!(account.owner, PROGRAM_ID, "{template_id} fixture owner");
        assert_eq!(account.data.len(), len, "{template_id} fixture size");
        let template = registry.get(template_id).expect("template should exist");
        let forged = surfnet_svm
            .get_forged_account_data(
                &Pubkey::new_unique(),
                &account.data,
                template.idl(),
                &HashMap::new(),
            )
            .unwrap_or_else(|e| panic!("live {template_id} account failed to round-trip: {e}"));
        assert_eq!(
            forged, account.data,
            "live {template_id} account was altered by a no-op round-trip"
        );
    }
}

/// The pair lookup in the templates' `llm_context` reads the mints at 168 and 200, the vaults at
/// 72 and 104 and the fee tier at 8; each vault is the program's PDA over its pool and mint.
#[tokio::test]
async fn pool_lookup_offsets_hold_the_mints_vaults_and_fee_tier() {
    let pool = fetch(&[WSOL_USELESS_POOL]).await.remove(0).data;

    assert_eq!(pubkey_at(&pool, AMM_CONFIG_RANGE), AMM_CONFIG_INDEX_0);
    for (vault, mint) in [(VAULT_0_RANGE, MINT_0_RANGE), (VAULT_1_RANGE, MINT_1_RANGE)] {
        let (expected, _) = Pubkey::find_program_address(
            &[
                b"pool_vault",
                WSOL_USELESS_POOL.as_ref(),
                pubkey_at(&pool, mint).as_ref(),
            ],
            &PROGRAM_ID,
        );
        assert_eq!(pubkey_at(&pool, vault), expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_materializer_touch_only_their_own_bytes() {
    let mut fetched = fetch(&[WSOL_USELESS_POOL, AMM_CONFIG_INDEX_0]).await;
    let config_before = fetched.pop().unwrap();
    let pool_before = fetched.pop().unwrap();

    let new_status: u8 = if pool_before.data[STATUS_OFFSET] == 0 {
        4
    } else {
        0
    };
    let new_open_time =
        u64::from_le_bytes(pool_before.data[OPEN_TIME_RANGE].try_into().unwrap()) + 3600;
    let new_trade_fee_rate =
        u64::from_le_bytes(config_before.data[TRADE_FEE_RATE_RANGE].try_into().unwrap()) + 7500;

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm
        .set_account(&WSOL_USELESS_POOL, pool_before.clone())
        .unwrap();
    surfnet_svm
        .set_account(&AMM_CONFIG_INDEX_0, config_before.clone())
        .unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let mut scenario = Scenario::new(
        "Raydium CPMM Overrides".to_string(),
        "Halt a live pool, move its open time and raise its fee tier through the materializer."
            .to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            POOL_STATE_TEMPLATE.to_string(),
            1,
            AccountAddress::Pubkey(WSOL_USELESS_POOL.to_string()),
        )
        .with_values(HashMap::from([
            ("status".to_string(), serde_json::json!(new_status)),
            ("open_time".to_string(), serde_json::json!(new_open_time)),
        ]))
        .with_label("Raydium CPMM pool override".to_string()),
    );
    scenario.add_override(
        OverrideInstance::new(
            AMM_CONFIG_TEMPLATE.to_string(),
            1,
            AccountAddress::Pubkey(AMM_CONFIG_INDEX_0.to_string()),
        )
        .with_values(HashMap::from([(
            "trade_fee_rate".to_string(),
            serde_json::json!(new_trade_fee_rate),
        )]))
        .with_label("Raydium CPMM fee tier override".to_string()),
    );
    locker.register_scenario(scenario, Some(0)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();

    let read = |address: Pubkey| {
        locker
            .with_svm_reader(|svm| svm.get_account(&address))
            .unwrap()
            .expect("account should still exist after materialization")
    };
    let pool_after = read(WSOL_USELESS_POOL);
    let config_after = read(AMM_CONFIG_INDEX_0);

    let pool_outside: Vec<usize> = diff_indices(&pool_before.data, &pool_after.data)
        .into_iter()
        .filter(|i| *i != STATUS_OFFSET && !OPEN_TIME_RANGE.contains(i))
        .collect();
    assert!(
        pool_outside.is_empty(),
        "pool bytes outside status and open_time changed: {pool_outside:?}"
    );
    assert_eq!(pool_after.data[STATUS_OFFSET], new_status);
    assert_eq!(
        u64::from_le_bytes(pool_after.data[OPEN_TIME_RANGE].try_into().unwrap()),
        new_open_time
    );

    let config_outside: Vec<usize> = diff_indices(&config_before.data, &config_after.data)
        .into_iter()
        .filter(|i| !TRADE_FEE_RATE_RANGE.contains(i))
        .collect();
    assert!(
        config_outside.is_empty(),
        "AmmConfig bytes outside trade_fee_rate changed: {config_outside:?}"
    );
    assert_eq!(
        u64::from_le_bytes(config_after.data[TRADE_FEE_RATE_RANGE].try_into().unwrap()),
        new_trade_fee_rate
    );
}
