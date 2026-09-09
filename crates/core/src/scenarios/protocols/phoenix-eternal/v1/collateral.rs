use std::{collections::HashSet, ops::Range};

use phoenix_rise_accounts::{
    PhoenixAccount, PhoenixAccountDecodeError, multi_arena::MultiArenaHeader, trader::TraderHeader,
};
use solana_account::Account;
use solana_pubkey::Pubkey;

use super::state_builder::PHOENIX_ETERNAL_PROGRAM_ID;
use crate::error::{SurfpoolError, SurfpoolResult};

pub fn trader_header(trader: &Pubkey, account: &Account) -> SurfpoolResult<TraderHeader> {
    if account.owner != PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(SurfpoolError::invalid_account_owner(
            *trader,
            None::<PhoenixAccountDecodeError>,
        ));
    }
    TraderHeader::try_read_from_account_bytes(&account.data).map_err(|error| {
        SurfpoolError::invalid_account_data(
            trader,
            "Expected a valid Phoenix Eternal Trader account",
            Some(error),
        )
    })
}

pub fn index_trader_state_range(
    index: &Account,
    trader_key: &[u8; 32],
) -> SurfpoolResult<Range<usize>> {
    let invalid = || SurfpoolError::internal("Invalid Phoenix GlobalTraderIndex tree");
    if index.owner != PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(SurfpoolError::internal(
            "Expected a Phoenix-owned GlobalTraderIndex account",
        ));
    }
    let header = MultiArenaHeader::try_from_account_bytes(
        "GlobalTraderIndex",
        &index.data,
        PhoenixAccount::GlobalTraderIndexHeader.discriminant(),
    )
    .map_err(|error| {
        SurfpoolError::internal(format!("Invalid Phoenix GlobalTraderIndex: {error}"))
    })?;
    if header.num_arenas() != 1 || header.superblock().num_active_arenas() != 1 {
        return Err(SurfpoolError::internal(
            "Phoenix collateral overrides currently require a single-arena GlobalTraderIndex",
        ));
    }
    // MultiArenaHeader (48), superblock (32), tree root and padding (16), then
    // 1-based Sokoban nodes: four u32 registers, a 32-byte key, and IDL TraderState.
    const NODES_START: usize = 96;
    const NODE_LEN: usize = 64;
    let data = &index.data;
    if data.len() < NODES_START || !(data.len() - NODES_START).is_multiple_of(NODE_LEN) {
        return Err(invalid());
    }
    let capacity = (data.len() - NODES_START) / NODE_LEN;
    let read_u32 = |offset| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let mut pending = vec![read_u32(80)];
    let mut visited = HashSet::new();
    let mut found = None;
    while let Some(node) = pending.pop() {
        if node == 0 {
            continue;
        }
        if node >= header.superblock().bump_index()
            || node as usize > capacity
            || !visited.insert(node)
        {
            return Err(invalid());
        }
        let start = NODES_START + (node as usize - 1) * NODE_LEN;
        pending.extend([read_u32(start), read_u32(start + 4)]);
        if data[start + 16..start + 48] == trader_key[..] {
            if found.is_some() {
                return Err(invalid());
            }
            found = Some(start + 48..start + 64);
        }
    }
    if visited.len() != header.superblock().size() as usize {
        return Err(invalid());
    }
    found.ok_or_else(|| {
        SurfpoolError::internal("Hot Phoenix Trader has no reachable GlobalTraderIndex entry")
    })
}

pub fn effective_collateral(header: &TraderHeader, index: Option<&Account>) -> SurfpoolResult<i64> {
    if !header.trader_state.is_hot() {
        return Ok(header.trader_state.quote_lot_collateral.as_inner());
    }
    let index = index.ok_or_else(|| {
        SurfpoolError::internal("Hot Phoenix Trader requires its GlobalTraderIndex account")
    })?;
    let range = index_trader_state_range(index, &header.key)?;
    Ok(i64::from_le_bytes(
        index.data[range.start..range.start + 8].try_into().unwrap(),
    ))
}

pub fn collateral_override_value(value: &serde_json::Value) -> SurfpoolResult<serde_json::Value> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
        .map(serde_json::Value::from)
        .ok_or_else(|| {
            SurfpoolError::internal("Phoenix collateral must be a signed 64-bit integer")
        })
}

#[cfg(test)]
mod tests {
    use phoenix_rise_accounts::trader::TRADER_CAPABILITY_HOT;

    use super::*;
    use crate::scenarios::protocols::phoenix_eternal::v1::state_builder::build_phoenix_collateral_scenario;

    const FIRST_KEY: [u8; 32] = [11; 32];
    const SECOND_KEY: [u8; 32] = [22; 32];

    #[test]
    fn collateral_values_preserve_signed_integer_precision() {
        for value in [i64::MIN, -9_007_199_254_740_993, 0, i64::MAX] {
            assert_eq!(
                collateral_override_value(&serde_json::json!(value.to_string())).unwrap(),
                serde_json::json!(value)
            );
            assert_eq!(
                collateral_override_value(&serde_json::json!(value)).unwrap(),
                serde_json::json!(value)
            );
        }
        for value in [
            serde_json::json!("9223372036854775808"),
            serde_json::json!(-1.5),
            serde_json::json!(null),
        ] {
            assert!(collateral_override_value(&value).is_err());
        }
    }

    fn write_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn index_account() -> Account {
        let mut data = vec![0; 96 + 3 * 64];
        data[..8].copy_from_slice(&PhoenixAccount::GlobalTraderIndexHeader.discriminant());
        write_u32(&mut data, 48, 2);
        data[52..54].copy_from_slice(&1_u16.to_le_bytes());
        data[54..56].copy_from_slice(&1_u16.to_le_bytes());
        write_u32(&mut data, 56, 3);
        write_u32(&mut data, 60, 4);
        write_u32(&mut data, 64, 3);
        write_u32(&mut data, 80, 2);
        write_u32(&mut data, 160, 1);
        write_u32(&mut data, 104, 2);
        for (slot, key, collateral) in [
            (0, FIRST_KEY, 111_i64),
            (1, SECOND_KEY, 222_i64),
            (2, FIRST_KEY, 999_i64),
        ] {
            let start = 96 + slot * 64;
            data[start + 16..start + 48].copy_from_slice(&key);
            data[start + 48..start + 56].copy_from_slice(&collateral.to_le_bytes());
            write_u32(&mut data, start + 56, TRADER_CAPABILITY_HOT);
        }
        Account {
            data,
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            ..Account::default()
        }
    }

    fn trader_account(key: [u8; 32], collateral: i64, hot: bool) -> Account {
        let mut data = vec![0; core::mem::size_of::<TraderHeader>() + 16];
        data[..8].copy_from_slice(&PhoenixAccount::Trader.discriminant());
        data[24..56].copy_from_slice(&key);
        data[88..96].copy_from_slice(&collateral.to_le_bytes());
        if hot {
            write_u32(&mut data, 96, TRADER_CAPABILITY_HOT);
        }
        Account {
            data,
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            ..Account::default()
        }
    }

    #[test]
    fn index_lookup_selects_reachable_keys_and_ignores_freed_duplicate() {
        let index = index_account();
        assert_eq!(
            index_trader_state_range(&index, &FIRST_KEY).unwrap(),
            144..160
        );
        assert_eq!(
            index_trader_state_range(&index, &SECOND_KEY).unwrap(),
            208..224
        );

        let mut only_freed_match = index.clone();
        only_freed_match.data[112..144].copy_from_slice(&[33; 32]);
        assert!(index_trader_state_range(&only_freed_match, &FIRST_KEY).is_err());
    }

    #[test]
    fn index_lookup_rejects_cycles_wrong_size_and_missing_key() {
        let mut cycle = index_account();
        write_u32(&mut cycle.data, 96, 2);
        assert!(index_trader_state_range(&cycle, &FIRST_KEY).is_err());

        let mut wrong_size = index_account();
        write_u32(&mut wrong_size.data, 48, 3);
        assert!(index_trader_state_range(&wrong_size, &FIRST_KEY).is_err());
        assert!(index_trader_state_range(&index_account(), &[44; 32]).is_err());
    }

    #[test]
    fn index_lookup_rejects_invalid_node_addresses_and_duplicate_reachable_keys() {
        let mut out_of_bounds = index_account();
        write_u32(&mut out_of_bounds.data, 60, 100);
        write_u32(&mut out_of_bounds.data, 164, 4);
        assert!(index_trader_state_range(&out_of_bounds, &FIRST_KEY).is_err());

        let mut unallocated = index_account();
        write_u32(&mut unallocated.data, 60, 2);
        assert!(index_trader_state_range(&unallocated, &FIRST_KEY).is_err());

        let mut duplicate = index_account();
        duplicate.data[176..208].copy_from_slice(&FIRST_KEY);
        assert!(index_trader_state_range(&duplicate, &FIRST_KEY).is_err());
    }

    #[test]
    fn index_lookup_rejects_wrong_owner_discriminator_and_truncated_layouts() {
        let mut wrong_owner = index_account();
        wrong_owner.owner = Pubkey::new_unique();
        assert!(index_trader_state_range(&wrong_owner, &FIRST_KEY).is_err());

        let mut wrong_discriminator = index_account();
        wrong_discriminator.data[..8].fill(0);
        assert!(index_trader_state_range(&wrong_discriminator, &FIRST_KEY).is_err());

        for len in [0, 79, 95, 96 + 3 * 64 - 1] {
            let mut truncated = index_account();
            truncated.data.truncate(len);
            assert!(index_trader_state_range(&truncated, &FIRST_KEY).is_err());
        }
    }

    #[test]
    fn index_lookup_rejects_unsupported_arena_counts() {
        for (arenas, active) in [(0_u16, 1_u16), (2, 1), (1, 0), (1, 2)] {
            let mut index = index_account();
            index.data[52..54].copy_from_slice(&arenas.to_le_bytes());
            index.data[54..56].copy_from_slice(&active.to_le_bytes());
            assert!(index_trader_state_range(&index, &FIRST_KEY).is_err());
        }
    }

    #[test]
    fn hot_trader_uses_effective_index_collateral_and_requires_index() {
        let trader = Pubkey::new_from_array(FIRST_KEY);
        let account = trader_account(FIRST_KEY, 9_999, true);
        let header = trader_header(&trader, &account).unwrap();
        assert_eq!(
            effective_collateral(&header, Some(&index_account())).unwrap(),
            111
        );
        assert!(effective_collateral(&header, None).is_err());
    }

    #[test]
    fn collateral_builder_bounds_hot_targets_by_effective_state() {
        let trader = Pubkey::new_from_array(FIRST_KEY);
        let index = index_account();
        let stale_high = trader_account(FIRST_KEY, 9_999, true);
        assert!(
            build_phoenix_collateral_scenario(trader, &stale_high, "112", Some(&index)).is_err()
        );
        assert!(build_phoenix_collateral_scenario(trader, &stale_high, "1", None).is_err());

        let stale_low = trader_account(FIRST_KEY, 1, true);
        let prepared =
            build_phoenix_collateral_scenario(trader, &stale_low, "100", Some(&index)).unwrap();
        assert_eq!(
            prepared.overrides[0].values["traderState.quoteLotCollateral"],
            "100"
        );
        assert_eq!(index, index_account());
        assert_eq!(stale_low, trader_account(FIRST_KEY, 1, true));
    }

    #[test]
    fn cold_trader_keeps_standalone_collateral_and_increase_guard() {
        let trader = Pubkey::new_from_array(FIRST_KEY);
        let account = trader_account(FIRST_KEY, 50, false);
        let header = trader_header(&trader, &account).unwrap();
        assert_eq!(effective_collateral(&header, None).unwrap(), 50);
        assert_eq!(
            effective_collateral(&header, Some(&index_account())).unwrap(),
            50
        );
        assert!(build_phoenix_collateral_scenario(trader, &account, "51", None).is_err());
        assert!(build_phoenix_collateral_scenario(trader, &account, "50", None).is_ok());
    }

    fn global_account(index: &Pubkey) -> Account {
        use super::super::state_builder::PHOENIX_GLOBAL_CONFIG;

        let mut data = vec![0; 2_560];
        data[..8].copy_from_slice(&PhoenixAccount::GlobalConfiguration.discriminant());
        data[8..40].copy_from_slice(PHOENIX_GLOBAL_CONFIG.as_ref());
        data[392..424].copy_from_slice(index.as_ref());
        Account {
            lamports: 1,
            data,
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            ..Account::default()
        }
    }

    #[tokio::test]
    async fn materialization_patches_selected_hot_trader_and_index_record_only() {
        use super::super::state_builder::PHOENIX_GLOBAL_CONFIG;
        use crate::surfnet::svm::SurfnetSvm;

        for (key, other_key, collateral_offset, field, target) in [
            (
                FIRST_KEY,
                SECOND_KEY,
                144,
                "traderState.quoteLotCollateral",
                1_i64,
            ),
            (
                SECOND_KEY,
                FIRST_KEY,
                208,
                "traderState.quoteLotCollateral",
                -9_007_199_254_740_993,
            ),
            (FIRST_KEY, SECOND_KEY, 144, "quote_lot_collateral", 1),
            (
                SECOND_KEY,
                FIRST_KEY,
                208,
                "quote_lot_collateral",
                -9_007_199_254_740_993,
            ),
        ] {
            let trader = Pubkey::new_from_array(key);
            let other_trader = Pubkey::new_from_array(other_key);
            let index_key = Pubkey::new_unique();
            let mut before_trader = trader_account(key, 9_999, true);
            before_trader.lamports = 1;
            let mut before_other = trader_account(other_key, 8_888, true);
            before_other.lamports = 1;
            let mut before_index = index_account();
            before_index.lamports = 1;
            let global = global_account(&index_key);
            let mut scenario = build_phoenix_collateral_scenario(
                trader,
                &before_trader,
                &target.to_string(),
                Some(&before_index),
            )
            .unwrap();
            scenario.overrides[0].values = std::collections::HashMap::from([(
                field.to_string(),
                serde_json::json!(target.to_string()),
            )]);
            let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
            svm.set_account(&trader, before_trader.clone()).unwrap();
            svm.set_account(&other_trader, before_other.clone())
                .unwrap();
            svm.set_account(&index_key, before_index.clone()).unwrap();
            svm.set_account(&PHOENIX_GLOBAL_CONFIG, global.clone())
                .unwrap();
            svm.register_scenario(scenario, Some(100)).unwrap();

            assert_eq!(svm.get_account(&trader).unwrap().unwrap(), before_trader);
            assert_eq!(svm.get_account(&index_key).unwrap().unwrap(), before_index);
            svm.materialize_overrides_for_slot(&None, 100)
                .await
                .unwrap();

            let mut expected_trader = before_trader;
            expected_trader.data[88..96].copy_from_slice(&target.to_le_bytes());
            let mut expected_index = before_index;
            expected_index.data[collateral_offset..collateral_offset + 8]
                .copy_from_slice(&target.to_le_bytes());
            assert_eq!(svm.get_account(&trader).unwrap().unwrap(), expected_trader);
            assert_eq!(
                svm.get_account(&index_key).unwrap().unwrap(),
                expected_index
            );
            assert_eq!(
                svm.get_account(&other_trader).unwrap().unwrap(),
                before_other
            );
            assert_eq!(
                svm.get_account(&PHOENIX_GLOBAL_CONFIG).unwrap().unwrap(),
                global
            );
        }
    }

    #[tokio::test]
    async fn materialization_skips_invalid_or_missing_index_without_partial_trader_write() {
        use super::super::state_builder::PHOENIX_GLOBAL_CONFIG;
        use crate::surfnet::svm::SurfnetSvm;

        for failure in [
            "cycle",
            "missing key",
            "missing account",
            "conflicting fields",
        ] {
            let trader = Pubkey::new_from_array(FIRST_KEY);
            let index_key = Pubkey::new_unique();
            let mut before_trader = trader_account(FIRST_KEY, 9_999, true);
            before_trader.lamports = 1;
            let mut before_index = index_account();
            before_index.lamports = 1;
            let mut scenario =
                build_phoenix_collateral_scenario(trader, &before_trader, "1", Some(&before_index))
                    .unwrap();
            match failure {
                "conflicting fields" => {
                    scenario.overrides[0]
                        .values
                        .insert("quote_lot_collateral".to_string(), serde_json::json!("2"));
                }
                "cycle" => write_u32(&mut before_index.data, 96, 2),
                "missing key" => before_index.data[112..144].copy_from_slice(&[33; 32]),
                _ => {}
            }
            let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
            svm.set_account(&trader, before_trader.clone()).unwrap();
            svm.set_account(&PHOENIX_GLOBAL_CONFIG, global_account(&index_key))
                .unwrap();
            if failure != "missing account" {
                svm.set_account(&index_key, before_index.clone()).unwrap();
            }
            svm.register_scenario(scenario, Some(100)).unwrap();

            // A rejected override is skipped, never an error out of the batch: that error
            // would abort block production.
            svm.materialize_overrides_for_slot(&None, 100)
                .await
                .unwrap_or_else(|error| panic!("{failure}: {error}"));
            assert_eq!(
                svm.get_account(&trader).unwrap().unwrap(),
                before_trader,
                "{failure}"
            );
            let after_index = svm.get_account(&index_key).unwrap();
            if failure == "missing account" {
                assert!(after_index.is_none());
            } else {
                assert_eq!(after_index.unwrap(), before_index, "{failure}");
            }
        }
    }
}
