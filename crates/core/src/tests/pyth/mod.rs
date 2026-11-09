//! Pyth integration tests.
//!
//! These fetch a real price feed from mainnet, so they need a network connection and are compiled
//! only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests pyth
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.

use std::collections::HashMap;

use solana_clock::Clock;
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

const PYTH_CORE_RECEIVER: &str = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";
const CORE_SOL_USD_FEED: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
const PYTH_CORE_COMPATIBLE_RECEIVER: &str = "rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp";
const CORE_COMPATIBLE_SOL_USD_FEED: &str = "7AviUf9nL62mcxNbQGKm4nKDQnPjswo6c5MX4D57HmyE";
const SOL_USD_FEED_ID: &str = "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

async fn fetch(address: &str) -> solana_account::Account {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    match client
        .get_multiple_accounts(
            &[Pubkey::from_str_const(address)],
            CommitmentConfig::confirmed(),
        )
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {address} from mainnet: {e}"))
        .remove(0)
    {
        GetAccountResult::FoundAccount(_, account, _)
        | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
        GetAccountResult::None(_) => {
            panic!("{address} no longer exists on mainnet; the test needs a new address")
        }
    }
}

/// Runs the template through the scenario path on the account the caller names, so the IDL comes
/// from the account's owner.
#[tokio::test(flavor = "multi_thread")]
async fn core_compatible_price_feed_touches_only_price_fields() {
    let live = fetch(CORE_COMPATIBLE_SOL_USD_FEED).await;
    assert_eq!(live.owner.to_string(), PYTH_CORE_COMPATIBLE_RECEIVER);
    // A Partial verification level shifts every offset below by one.
    assert_eq!(live.data[40], 1, "verification level should be Full");
    let feed_id = hex::decode(SOL_USD_FEED_ID.trim_start_matches("0x")).unwrap();
    assert_eq!(&live.data[41..73], feed_id.as_slice(), "feed_id at 41");

    let registry = TemplateRegistry::new();
    let template = registry.get("pyth-core-compatible-price-feed-v2").unwrap();

    // (path, offset, width, value). Each value differs from the live one, so a skipped write fails.
    let rows: [(&str, usize, usize, i64); 8] = [
        ("price_message.price", 73, 8, 9_100_000_000_000),
        ("price_message.conf", 81, 8, 500_000_000),
        ("price_message.exponent", 89, 4, -10),
        ("price_message.publish_time", 93, 8, 1_800_000_000),
        ("price_message.prev_publish_time", 101, 8, 1_799_999_940),
        ("price_message.ema_price", 109, 8, 9_050_000_000_000),
        ("price_message.ema_conf", 117, 8, 450_000_000),
        ("posted_slot", 125, 8, 500_000_000),
    ];
    for path in template.property_paths() {
        assert!(
            rows.iter().any(|(row, ..)| *row == path),
            "{path} has no row"
        );
    }

    let target = Pubkey::from_str_const(CORE_COMPATIBLE_SOL_USD_FEED);
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let identity = svm
        .get_forged_account_data(&target, &live.data, template.idl(), &HashMap::new())
        .unwrap();
    assert_eq!(identity, live.data, "no-op round trip changed bytes");

    let values: HashMap<String, serde_json::Value> = rows
        .iter()
        .map(|(path, _, _, value)| (path.to_string(), serde_json::json!(value)))
        .collect();

    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm| svm.set_account(&target, live.clone()).unwrap());
    let mut scenario = Scenario::new(
        "Pyth Core Compatible SOL/USD".to_string(),
        "Move every price field of the live feed".to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            template.id.clone(),
            0,
            AccountAddress::Pubkey(CORE_COMPATIBLE_SOL_USD_FEED.to_string()),
        )
        .with_values(values),
    );
    locker.register_scenario(scenario, Some(100)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 100)
        .await
        .unwrap();

    let (forged, clock) = locker.with_svm_reader(|svm| {
        (
            svm.get_account(&target)
                .unwrap()
                .expect("the feed should exist"),
            svm.inner.get_sysvar::<Clock>(),
        )
    });
    assert_eq!(forged.owner, live.owner);
    assert_eq!(forged.lamports, live.lamports);
    assert_eq!(forged.data.len(), live.data.len());

    let diffs: Vec<usize> = (0..live.data.len())
        .filter(|&i| forged.data[i] != live.data[i])
        .collect();
    let written = |i: &usize| {
        rows.iter()
            .any(|(_, offset, width, _)| (*offset..offset + width).contains(i))
    };
    assert!(
        !diffs.is_empty() && diffs.iter().all(written),
        "changed {diffs:?}"
    );
    for (path, offset, width, value) in rows {
        // Keep-alive republishes the price after the override plays.
        let value = match path {
            "price_message.publish_time" => clock.unix_timestamp,
            "posted_slot" => clock.slot as i64,
            _ => value,
        };
        assert_eq!(
            &forged.data[offset..offset + width],
            &value.to_le_bytes()[..width],
            "{path}"
        );
    }
}

/// Keep-alive republishes a price account a scenario overrides every slot, as the Pyth publisher
/// does on mainnet, time travels included. Only the publication fields move with the Clock: the
/// price stays the scenario's.
#[tokio::test(flavor = "multi_thread")]
async fn price_feeds_stay_published_through_surfnet_svm() {
    const PRICE: i64 = 2_000_000_000;
    for (template_id, address, receiver) in [
        ("pyth-price-feed-v2", CORE_SOL_USD_FEED, PYTH_CORE_RECEIVER),
        (
            "pyth-core-compatible-price-feed-v2",
            CORE_COMPATIBLE_SOL_USD_FEED,
            PYTH_CORE_COMPATIBLE_RECEIVER,
        ),
    ] {
        let live = fetch(address).await;
        assert_eq!(live.owner.to_string(), receiver);
        // A Partial verification level shifts every offset below by one.
        assert_eq!(
            live.data[40], 1,
            "{address} verification level should be Full"
        );

        let target = Pubkey::from_str_const(address);
        let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        let locker = SurfnetSvmLocker::new(svm);
        locker.with_svm_writer(|svm| svm.set_account(&target, live.clone()).unwrap());
        let mut scenario = Scenario::new("Pyth SOL at $20".to_string(), "Price only".to_string());
        scenario.add_override(
            OverrideInstance::new(
                template_id.to_string(),
                0,
                AccountAddress::Pubkey(address.to_string()),
            )
            .with_values(HashMap::from([(
                "price_message.price".to_string(),
                serde_json::json!(PRICE),
            )])),
        );
        locker.register_scenario(scenario, None).unwrap();

        let assert_published = |when: &str| {
            let (data, clock) = locker.with_svm_reader(|svm| {
                (
                    svm.get_account(&target).unwrap().unwrap().data,
                    svm.inner.get_sysvar::<Clock>(),
                )
            });
            let word = |offset: usize| <[u8; 8]>::try_from(&data[offset..offset + 8]).unwrap();
            assert_eq!(
                i64::from_le_bytes(word(93)),
                clock.unix_timestamp,
                "{template_id} {when}: publish_time"
            );
            assert_eq!(
                u64::from_le_bytes(word(125)),
                clock.slot,
                "{template_id} {when}: posted_slot"
            );
            assert_eq!(i64::from_le_bytes(word(73)), PRICE, "{template_id} {when}");
            let diffs: Vec<usize> = (0..live.data.len())
                .filter(|&i| data[i] != live.data[i])
                .collect();
            assert!(
                diffs.iter().all(|i| [73..81, 93..101, 125..133]
                    .iter()
                    .any(|written| written.contains(i))),
                "{template_id} {when}: changed {diffs:?}"
            );
        };

        for slot in 1..=3 {
            locker.confirm_current_block(&None).await.unwrap();
            assert_published(&format!("slot {slot}"));
        }
        let clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar::<Clock>());
        locker
            .apply_time_travel(
                Clock {
                    slot: clock.slot + 1_000,
                    unix_timestamp: clock.unix_timestamp + 3_600,
                    ..clock
                },
                &None,
            )
            .await
            .unwrap();
        assert_published("after a time travel");
    }
}
