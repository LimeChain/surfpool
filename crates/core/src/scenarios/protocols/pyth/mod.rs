use solana_clock::{Clock, Slot};
use solana_pubkey::Pubkey;

use crate::{error::SurfpoolResult, surfnet::svm::SurfnetSvm};

/// Pyth's receiver and its Pro-compatible build; both own `PriceUpdateV2` accounts.
const RECEIVER_PROGRAMS: [Pubkey; 2] = [
    Pubkey::from_str_const("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ"),
    Pubkey::from_str_const("rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp"),
];

const PRICE_UPDATE_V2_DISCRIMINATOR: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];

/// Marks every `PriceUpdateV2` in the local VM whose `publish_time` is behind the clock as
/// published now, keeping its price. Nothing posts Pyth updates to the local VM, so a feed loaded
/// earlier would otherwise fail its readers' age checks once a scenario plays. Overrides applied
/// afterwards still write their own `publish_time`.
pub fn refresh_stale_price_updates(svm: &mut SurfnetSvm, slot: Slot) -> SurfpoolResult<usize> {
    let clock = svm.inner.get_sysvar::<Clock>();
    let mut refreshed = 0;
    for program in RECEIVER_PROGRAMS {
        for (pubkey, mut account) in svm.get_account_owned_by(&program)? {
            if mark_published_at(&mut account.data, clock.unix_timestamp) {
                svm.set_scenario_override_account(&pubkey, account, slot)?;
                refreshed += 1;
            }
        }
    }
    Ok(refreshed)
}

fn mark_published_at(data: &mut [u8], unix_timestamp: i64) -> bool {
    if data.get(..8) != Some(PRICE_UPDATE_V2_DISCRIMINATOR.as_slice()) {
        return false;
    }
    // After the write authority, `verification_level` is `Partial { num_signatures: u8 }` (0) or
    // `Full` (1); `publish_time` follows feed_id, price, conf and exponent in the price message.
    let publish_time = match data.get(40) {
        Some(0) => 42 + 52,
        Some(1) => 41 + 52,
        _ => return false,
    };
    let Some(field) = data.get_mut(publish_time..publish_time + 8) else {
        return false;
    };
    if i64::from_le_bytes(field.try_into().unwrap()) >= unix_timestamp {
        return false;
    }
    field.copy_from_slice(&unix_timestamp.to_le_bytes());
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use solana_account::Account;
    use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn price_update(verification_level: &[u8], publish_time: i64) -> Vec<u8> {
        let mut data = PRICE_UPDATE_V2_DISCRIMINATOR.to_vec();
        data.extend([7u8; 32]); // write_authority
        data.extend_from_slice(verification_level);
        data.extend([9u8; 32]); // feed_id
        data.extend(10_000_000_000i64.to_le_bytes()); // price
        data.extend(5_000_000u64.to_le_bytes()); // conf
        data.extend((-8i32).to_le_bytes()); // exponent
        data.extend(publish_time.to_le_bytes());
        data.extend((publish_time - 1).to_le_bytes()); // prev_publish_time
        data.extend(9_900_000_000i64.to_le_bytes()); // ema_price
        data.extend(4_000_000u64.to_le_bytes()); // ema_conf
        data.extend(123u64.to_le_bytes()); // posted_slot
        data.resize(134, 0);
        data
    }

    fn publish_time(data: &[u8]) -> i64 {
        let at = if data[40] == 1 { 41 + 52 } else { 42 + 52 };
        i64::from_le_bytes(data[at..at + 8].try_into().unwrap())
    }

    fn changed_bytes(before: &[u8], after: &[u8]) -> Vec<usize> {
        (0..before.len())
            .filter(|&i| before[i] != after[i])
            .collect()
    }

    #[test]
    fn a_stale_update_gets_the_clock_time_and_nothing_else() {
        for (level, at) in [(&[1u8][..], 93), (&[0u8, 13][..], 94)] {
            let before = price_update(level, NOW - 500);
            let mut data = before.clone();
            assert!(mark_published_at(&mut data, NOW));
            assert_eq!(publish_time(&data), NOW);
            let changed = changed_bytes(&before, &data);
            assert!(!changed.is_empty() && changed.iter().all(|i| (at..at + 8).contains(i)));
        }
    }

    #[test]
    fn current_and_future_updates_and_other_accounts_are_left_alone() {
        for publish in [NOW, NOW + 60] {
            let mut data = price_update(&[1], publish);
            assert!(!mark_published_at(&mut data, NOW));
            assert_eq!(publish_time(&data), publish);
        }
        let mut other = price_update(&[1], NOW - 500);
        other[0] ^= 1;
        assert!(!mark_published_at(&mut other, NOW));
        let mut truncated = price_update(&[1], NOW - 500)[..100].to_vec();
        assert!(!mark_published_at(&mut truncated, NOW));
    }

    #[tokio::test]
    async fn playing_a_scenario_refreshes_loaded_feeds_before_its_overrides_apply() {
        let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let mut clock = svm.inner.get_sysvar::<Clock>();
        clock.unix_timestamp = NOW;
        svm.inner.set_sysvar(&clock);

        let stale = NOW - 1_000;
        let [kept_stale, receiver_feed, pro_feed, not_pyth] = [0; 4].map(|_| Pubkey::new_unique());
        for (pubkey, owner) in [
            (kept_stale, RECEIVER_PROGRAMS[0]),
            (receiver_feed, RECEIVER_PROGRAMS[0]),
            (pro_feed, RECEIVER_PROGRAMS[1]),
            (not_pyth, Pubkey::new_unique()),
        ] {
            let account = Account {
                lamports: 1_000_000,
                data: price_update(&[1], NOW - 300),
                owner,
                executable: false,
                rent_epoch: 0,
            };
            svm.set_account(&pubkey, account).expect("set account");
        }

        // A scenario that makes a price stale on purpose must still get that time.
        let instance = OverrideInstance::new(
            "pyth-price-feed-v2".to_string(),
            0,
            AccountAddress::Pubkey(kept_stale.to_string()),
        )
        .with_values(HashMap::from([(
            "price_message.publish_time".to_string(),
            serde_json::json!(stale),
        )]));
        let scenario = Scenario {
            id: "stale-on-purpose".to_string(),
            name: "stale on purpose".to_string(),
            description: String::new(),
            tags: vec![],
            overrides: vec![instance],
        };
        let base_slot = svm.get_latest_absolute_slot();
        svm.register_scenario(scenario, None).expect("register");
        svm.materialize_overrides_for_slot(&None, base_slot)
            .await
            .expect("materialize");

        let read = |pubkey| publish_time(&svm.get_account(&pubkey).unwrap().unwrap().data);
        assert_eq!(read(kept_stale), stale);
        assert_eq!(read(receiver_feed), NOW);
        assert_eq!(read(pro_feed), NOW);
        assert_eq!(read(not_pyth), NOW - 300);
    }
}
