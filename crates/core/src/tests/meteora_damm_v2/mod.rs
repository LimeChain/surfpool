//! Meteora DAMM v2 integration tests. They fetch real mainnet accounts, so they need a network
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
    tests::helpers::diff_indices,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const POOL_STATE_TEMPLATE: &str = "meteora-damm-v2-pool-state";
const POOL_FEES_TEMPLATE: &str = "meteora-damm-v2-pool-fees";

const PROGRAM_ID: Pubkey = Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
/// SOL/USDC, one concentrated range.
const CONCENTRATED_POOL: Pubkey =
    Pubkey::from_str_const("8Pm2kZpnxD3hoMmt4bjStX2Pw2Z9abpbHzZxMPqxPmie");
/// USDC/SOL, compounding.
const COMPOUNDING_POOL: Pubkey =
    Pubkey::from_str_const("7g1ecbk6gtFdpJbq5vRA2Fu2JmNxPLB6Mbzv7PhK5WEp");

const BASE_FEE_INFO_PATH: &str = "pool_fees.base_fee.base_fee_info.data";

const POOL_LEN: usize = 1112;
const BASE_FEE_INFO_RANGE: std::ops::Range<usize> = 8..40;
const CLIFF_FEE_NUMERATOR_RANGE: std::ops::Range<usize> = 8..16;
const REDUCTION_FACTOR_RANGE: std::ops::Range<usize> = 32..40;
const MINT_A_RANGE: std::ops::Range<usize> = 168..200;
const MINT_B_RANGE: std::ops::Range<usize> = 200..232;
const VAULT_A_RANGE: std::ops::Range<usize> = 232..264;
const VAULT_B_RANGE: std::ops::Range<usize> = 264..296;
const SQRT_PRICE_REFERENCE_RANGE: std::ops::Range<usize> = 104..120;
const LIQUIDITY_RANGE: std::ops::Range<usize> = 360..376;
const SQRT_PRICE_RANGE: std::ops::Range<usize> = 456..472;
const POOL_STATUS_OFFSET: usize = 481;
const COLLECT_FEE_MODE_OFFSET: usize = 484;
const TOKEN_A_AMOUNT_RANGE: std::ops::Range<usize> = 680..688;
const TOKEN_B_AMOUNT_RANGE: std::ops::Range<usize> = 688..696;

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

fn pubkey_at(data: &[u8], range: std::ops::Range<usize>) -> Pubkey {
    Pubkey::try_from(&data[range]).unwrap()
}

fn u128_at(data: &[u8], range: std::ops::Range<usize>) -> u128 {
    u128::from_le_bytes(data[range].try_into().unwrap())
}

fn u64_at(data: &[u8], range: std::ops::Range<usize>) -> u64 {
    u64::from_le_bytes(data[range].try_into().unwrap())
}

#[tokio::test]
async fn real_mainnet_pools_round_trip_unchanged() {
    let pools = fetch(&[CONCENTRATED_POOL, COMPOUNDING_POOL]).await;
    assert_ne!(
        pools[0].data[COLLECT_FEE_MODE_OFFSET], 2,
        "fixture must be concentrated"
    );
    assert_eq!(
        pools[1].data[COLLECT_FEE_MODE_OFFSET], 2,
        "fixture must be compounding"
    );

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get(POOL_STATE_TEMPLATE)
        .expect("template should exist");
    for pool in &pools {
        assert_eq!(pool.owner, PROGRAM_ID);
        assert_eq!(pool.data.len(), POOL_LEN);
        let forged = surfnet_svm
            .get_forged_account_data(
                &Pubkey::new_unique(),
                &pool.data,
                template.idl(),
                &HashMap::new(),
            )
            .unwrap_or_else(|e| panic!("live DAMM v2 Pool failed to round-trip: {e}"));
        assert_eq!(
            forged, pool.data,
            "live Pool was altered by a no-op round-trip"
        );
    }
}

/// The pair lookup in the template's `llm_context` reads the mints at 168 and 200 and the vaults at
/// 232 and 264; each vault is the program's PDA over its mint and pool.
#[tokio::test]
async fn pool_lookup_offsets_hold_the_mints_and_vaults() {
    let pool = fetch(&[CONCENTRATED_POOL]).await.remove(0).data;

    for (vault, mint) in [(VAULT_A_RANGE, MINT_A_RANGE), (VAULT_B_RANGE, MINT_B_RANGE)] {
        let (expected, _) = Pubkey::find_program_address(
            &[
                b"token_vault",
                pubkey_at(&pool, mint).as_ref(),
                CONCENTRATED_POOL.as_ref(),
            ],
            &PROGRAM_ID,
        );
        assert_eq!(pubkey_at(&pool, vault), expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_materializer_touch_only_their_own_bytes() {
    let before = fetch(&[CONCENTRATED_POOL]).await.remove(0);

    let new_status: u8 = if before.data[POOL_STATUS_OFFSET] == 0 {
        1
    } else {
        0
    };
    let new_sqrt_price = u128_at(&before.data, SQRT_PRICE_RANGE) + 1;
    let new_liquidity = u128_at(&before.data, LIQUIDITY_RANGE) ^ 1;
    let new_token_a = u64_at(&before.data, TOKEN_A_AMOUNT_RANGE) + 1;
    let new_token_b = u64_at(&before.data, TOKEN_B_AMOUNT_RANGE) + 1;
    let new_cliff_fee_numerator = u64_at(&before.data, CLIFF_FEE_NUMERATOR_RANGE) + 1;
    let new_reduction_factor = u64_at(&before.data, REDUCTION_FACTOR_RANGE) ^ 1;
    let mut new_data = before.data.clone();
    new_data[CLIFF_FEE_NUMERATOR_RANGE].copy_from_slice(&new_cliff_fee_numerator.to_le_bytes());
    new_data[REDUCTION_FACTOR_RANGE].copy_from_slice(&new_reduction_factor.to_le_bytes());

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm
        .set_account(&CONCENTRATED_POOL, before.clone())
        .unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let mut scenario = Scenario::new(
        "Meteora DAMM v2 Pool Override".to_string(),
        "Change a live Pool's status, price, liquidity, tracked reserves and base fee through the materializer."
            .to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            POOL_STATE_TEMPLATE.to_string(),
            1,
            AccountAddress::Pubkey(CONCENTRATED_POOL.to_string()),
        )
        .with_values(HashMap::from([
            ("pool_status".to_string(), serde_json::json!(new_status)),
            (
                "sqrt_price".to_string(),
                serde_json::json!(new_sqrt_price.to_string()),
            ),
            (
                "liquidity".to_string(),
                serde_json::json!(new_liquidity.to_string()),
            ),
            ("token_a_amount".to_string(), serde_json::json!(new_token_a)),
            ("token_b_amount".to_string(), serde_json::json!(new_token_b)),
            (
                "pool_fees.dynamic_fee.sqrt_price_reference".to_string(),
                serde_json::json!(new_sqrt_price.to_string()),
            ),
        ]))
        .with_label("Meteora DAMM v2 pool override".to_string()),
    );
    scenario.add_override(
        OverrideInstance::new(
            POOL_FEES_TEMPLATE.to_string(),
            1,
            AccountAddress::Pubkey(CONCENTRATED_POOL.to_string()),
        )
        .with_values(HashMap::from([(
            BASE_FEE_INFO_PATH.to_string(),
            serde_json::json!(new_data[BASE_FEE_INFO_RANGE]),
        )]))
        .with_label("Meteora DAMM v2 fee override".to_string()),
    );
    let next_slot = locker.get_latest_absolute_slot() + 1;
    locker.register_scenario(scenario, Some(next_slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, next_slot + 1)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm| svm.get_account(&CONCENTRATED_POOL))
        .unwrap()
        .expect("account should still exist after materialization");

    assert_eq!(after.data.len(), before.data.len());
    let outside: Vec<usize> = diff_indices(&before.data, &after.data)
        .into_iter()
        .filter(|i| {
            *i != POOL_STATUS_OFFSET
                && !SQRT_PRICE_RANGE.contains(i)
                && !LIQUIDITY_RANGE.contains(i)
                && !TOKEN_A_AMOUNT_RANGE.contains(i)
                && !TOKEN_B_AMOUNT_RANGE.contains(i)
                && !SQRT_PRICE_REFERENCE_RANGE.contains(i)
                && !CLIFF_FEE_NUMERATOR_RANGE.contains(i)
                && !REDUCTION_FACTOR_RANGE.contains(i)
        })
        .collect();
    assert!(
        outside.is_empty(),
        "bytes outside the overridden fields changed: {outside:?}"
    );
    assert_eq!(after.data[POOL_STATUS_OFFSET], new_status);
    assert_eq!(u128_at(&after.data, SQRT_PRICE_RANGE), new_sqrt_price);
    assert_eq!(u128_at(&after.data, LIQUIDITY_RANGE), new_liquidity);
    assert_eq!(u64_at(&after.data, TOKEN_A_AMOUNT_RANGE), new_token_a);
    assert_eq!(u64_at(&after.data, TOKEN_B_AMOUNT_RANGE), new_token_b);
    assert_eq!(
        u128_at(&after.data, SQRT_PRICE_REFERENCE_RANGE),
        new_sqrt_price
    );
    assert_eq!(
        u64_at(&after.data, CLIFF_FEE_NUMERATOR_RANGE),
        new_cliff_fee_numerator
    );
    assert_eq!(
        u64_at(&after.data, REDUCTION_FACTOR_RANGE),
        new_reduction_factor
    );
}

/// A `data` write replaces all 32 base-fee bytes, so the caller sends the pool's own bytes back; the
/// scheduler bytes between the two numbers must survive.
#[tokio::test(flavor = "multi_thread")]
async fn base_fee_array_write_changes_only_the_cliff_fee_numerator() {
    let before = fetch(&[COMPOUNDING_POOL]).await.remove(0);
    let scheduler_bytes = CLIFF_FEE_NUMERATOR_RANGE.end..REDUCTION_FACTOR_RANGE.end;
    assert!(
        before.data[scheduler_bytes].iter().any(|byte| *byte != 0),
        "fixture must carry a fee schedule"
    );

    let new_cliff_fee_numerator = u64_at(&before.data, CLIFF_FEE_NUMERATOR_RANGE) + 1;
    let mut new_data = before.data.clone();
    new_data[CLIFF_FEE_NUMERATOR_RANGE].copy_from_slice(&new_cliff_fee_numerator.to_le_bytes());

    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm
        .set_account(&COMPOUNDING_POOL, before.clone())
        .unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let mut scenario = Scenario::new(
        "Meteora DAMM v2 Base Fee Bytes".to_string(),
        "Change a live Pool's base fee through its 32-byte array.".to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            POOL_FEES_TEMPLATE.to_string(),
            1,
            AccountAddress::Pubkey(COMPOUNDING_POOL.to_string()),
        )
        .with_values(HashMap::from([(
            BASE_FEE_INFO_PATH.to_string(),
            serde_json::json!(new_data[BASE_FEE_INFO_RANGE]),
        )]))
        .with_label("Meteora DAMM v2 base fee bytes".to_string()),
    );
    let next_slot = locker.get_latest_absolute_slot() + 1;
    locker.register_scenario(scenario, Some(next_slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, next_slot + 1)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm| svm.get_account(&COMPOUNDING_POOL))
        .unwrap()
        .expect("account should still exist after materialization");

    assert_eq!(after.data, new_data);
    let changed = diff_indices(&before.data, &after.data);
    assert!(!changed.is_empty(), "the base fee did not change");
    assert!(
        changed
            .iter()
            .all(|i| CLIFF_FEE_NUMERATOR_RANGE.contains(i)),
        "bytes outside cliff_fee_numerator changed: {changed:?}"
    );
}

/// Registers one fee override with `values` on the compounding pool, materializes it and returns
/// the pool's bytes.
async fn materialize_base_fee_values(
    before: &Account,
    values: HashMap<String, serde_json::Value>,
) -> Vec<u8> {
    let (mut surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    surfnet_svm
        .set_account(&COMPOUNDING_POOL, before.clone())
        .unwrap();
    let locker = SurfnetSvmLocker::new(surfnet_svm);

    let mut scenario = Scenario::new(
        "Meteora DAMM v2 Base Fee Elements".to_string(),
        "Change a live Pool's base fee one array element at a time.".to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            POOL_FEES_TEMPLATE.to_string(),
            1,
            AccountAddress::Pubkey(COMPOUNDING_POOL.to_string()),
        )
        .with_values(values)
        .with_label("Meteora DAMM v2 base fee elements".to_string()),
    );
    let next_slot = locker.get_latest_absolute_slot() + 1;
    locker.register_scenario(scenario, Some(next_slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, next_slot + 1)
        .await
        .unwrap();

    locker
        .with_svm_reader(|svm| svm.get_account(&COMPOUNDING_POOL))
        .unwrap()
        .expect("account should still exist after materialization")
        .data
}

fn element_path(index: usize) -> String {
    format!("{BASE_FEE_INFO_PATH}.{index}")
}

#[tokio::test(flavor = "multi_thread")]
async fn base_fee_single_element_writes_change_only_the_cliff_fee_numerator() {
    let before = fetch(&[COMPOUNDING_POOL]).await.remove(0);

    let new_cliff_fee_numerator = u64_at(&before.data, CLIFF_FEE_NUMERATOR_RANGE) + 1;
    let mut expected = before.data.clone();
    expected[CLIFF_FEE_NUMERATOR_RANGE].copy_from_slice(&new_cliff_fee_numerator.to_le_bytes());

    let values = new_cliff_fee_numerator
        .to_le_bytes()
        .into_iter()
        .enumerate()
        .map(|(index, byte)| (element_path(index), serde_json::json!(byte)))
        .collect();
    let after = materialize_base_fee_values(&before, values).await;

    assert_eq!(
        u64_at(&after, CLIFF_FEE_NUMERATOR_RANGE),
        new_cliff_fee_numerator
    );
    assert_eq!(after, expected, "bytes outside data.0..data.7 changed");
}

#[tokio::test(flavor = "multi_thread")]
async fn base_fee_non_adjacent_element_writes_land_in_one_override() {
    let before = fetch(&[COMPOUNDING_POOL]).await.remove(0);
    let first = BASE_FEE_INFO_RANGE.start;
    let twenty_fifth = BASE_FEE_INFO_RANGE.start + 24;

    let mut expected = before.data.clone();
    expected[first] ^= 1;
    expected[twenty_fifth] ^= 1;

    let after = materialize_base_fee_values(
        &before,
        HashMap::from([
            (element_path(0), serde_json::json!(expected[first])),
            (element_path(24), serde_json::json!(expected[twenty_fifth])),
        ]),
    )
    .await;

    assert_eq!(
        diff_indices(&before.data, &after),
        vec![first, twenty_fifth]
    );
    assert_eq!(after, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn base_fee_element_write_out_of_range_drops_the_whole_override() {
    let before = fetch(&[COMPOUNDING_POOL]).await.remove(0);
    let valid_first_byte = serde_json::json!(before.data[BASE_FEE_INFO_RANGE.start] ^ 1);

    let registry = TemplateRegistry::new();
    let template = registry
        .get(POOL_FEES_TEMPLATE)
        .expect("template should exist");
    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();

    for (bad, reason) in [
        (
            (element_path(32), serde_json::json!(1)),
            "Index 32 is out of bounds for array of length 32 in path \
             'pool_fees.base_fee.base_fee_info.data.32'",
        ),
        (
            (element_path(1), serde_json::json!(256)),
            "failed to encode field 'data': unable to encode value (256) as borsh u8",
        ),
    ] {
        let values = HashMap::from([(element_path(0), valid_first_byte.clone()), bad.clone()]);

        // The materializer logs this error as a warning and skips the override.
        let error = surfnet_svm
            .get_forged_account_data(&COMPOUNDING_POOL, &before.data, template.idl(), &values)
            .expect_err("an out-of-range element must not forge")
            .to_string();
        assert!(error.contains(reason), "{bad:?} failed with: {error}");

        let after = materialize_base_fee_values(&before, values).await;
        assert_eq!(
            after, before.data,
            "{bad:?} must drop the whole override, its valid data.0 included"
        );
    }
}
