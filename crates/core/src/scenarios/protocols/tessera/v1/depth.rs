use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

use super::TesseraMarket;

pub fn build_tessera_depth_scenario(
    market: Pubkey,
    account: &Account,
    sell_remaining_bps: u16,
    buy_remaining_bps: u16,
) -> SurfpoolResult<Scenario> {
    if [sell_remaining_bps, buy_remaining_bps]
        .iter()
        .any(|bps| !(1..=10_000).contains(bps))
    {
        return Err(SurfpoolError::internal(
            "Remaining depth must be 1..10000 basis points; 1000 keeps 10%, 10000 leaves a side unchanged",
        ));
    }
    TesseraMarket::mint_addresses(account)?;
    let registry = TemplateRegistry::new();
    let template = registry
        .get("tessera-depth")
        .expect("compiled Tessera template");
    let mut values = HashMap::new();
    for property in &template.properties {
        let bps = if property.path.starts_with("sell_levels.") {
            sell_remaining_bps
        } else {
            buy_remaining_bps
        };
        let offset = property.offset.expect("Tessera capacity offset");
        // The enabled flag follows capacity and price factor in each 24-byte level.
        if bps == 10_000 || account.data[offset + 16] == 0 {
            continue;
        }
        let current = u64::from_le_bytes(account.data[offset..offset + 8].try_into().unwrap());
        let scaled = (u128::from(current) * u128::from(bps) / 10_000) as u64;
        if scaled == 0 {
            return Err(SurfpoolError::internal(format!(
                "{} would have zero capacity while enabled; retain more depth",
                property.path
            )));
        }
        values.insert(property.path.clone(), serde_json::json!(scaled.to_string()));
    }
    if values.is_empty() {
        return Err(SurfpoolError::internal(
            "No enabled levels selected for depth reduction",
        ));
    }
    let percent = |bps: u16| format!("{}.{:02}%", bps / 100, bps % 100);
    let mut scenario = Scenario::new(
        "Tessera depth stress".to_string(),
        format!(
            "Keep {} of sell depth and {} of buy depth on market {market}, preserving prices and keeping quotes fresh.",
            percent(sell_remaining_bps),
            percent(buy_remaining_bps)
        ),
    );
    let target = AccountAddress::Pubkey(market.to_string());
    scenario.add_override(
        OverrideInstance::new(template.id.clone(), 0, target.clone())
            .with_values(values)
            .with_label("Reduce Tessera depth".to_string()),
    );
    scenario.add_override(
        OverrideInstance::new("tessera-freshness".to_string(), 0, target)
            .with_values(HashMap::from([(
                "last_update_slot".to_string(),
                serde_json::Value::Null,
            )]))
            .with_label("Keep Tessera quote fresh".to_string())
            .with_persist(true),
    );
    scenario.tags = vec![
        "tessera".to_string(),
        "pmm".to_string(),
        "depth-stress".to_string(),
    ];
    Ok(scenario)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::protocols::tessera::v1::{TESSERA_PROGRAM_ID, fair_value::MARKET_LAYOUT};

    fn market() -> Account {
        let mut data = vec![0; MARKET_LAYOUT.account_size];
        let magic = MARKET_LAYOUT.magic.as_ref().unwrap();
        data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        data[24..56].copy_from_slice(Pubkey::new_unique().as_ref());
        data[56..88].copy_from_slice(Pubkey::new_unique().as_ref());
        for (offset, amount, enabled) in [(160, u64::MAX, 1), (184, 101, 0), (640, 12345, 1)] {
            data[offset..offset + 8].copy_from_slice(&amount.to_le_bytes());
            data[offset + 16] = enabled;
        }
        Account {
            data,
            owner: TESSERA_PROGRAM_ID,
            ..Account::default()
        }
    }

    #[test]
    fn scales_exactly_and_preserves_unselected_bytes() {
        let account = market();
        let address = Pubkey::new_unique();
        for (sell, buy, expected_sell, expected_buy) in [
            (5000, 10000, 9223372036854775807u64, 12345u64),
            (1000, 2500, 1844674407370955161u64, 3086),
        ] {
            let scenario = build_tessera_depth_scenario(address, &account, sell, buy).unwrap();
            assert_eq!(scenario.overrides.len(), 2);
            let mut actual = account.data.clone();
            let mut expected = actual.clone();
            expected[160..168].copy_from_slice(&expected_sell.to_le_bytes());
            expected[640..648].copy_from_slice(&expected_buy.to_le_bytes());
            expected[120..128].copy_from_slice(&42u64.to_le_bytes());
            let registry = TemplateRegistry::new();
            for instance in &scenario.overrides {
                assert_eq!(
                    instance.account,
                    AccountAddress::Pubkey(address.to_string())
                );
                assert_eq!(instance.scenario_relative_slot, 0);
                assert!(!instance.fetch_before_use);
                let template = registry.get(&instance.template_id).unwrap();
                actual = template
                    .raw_layout
                    .as_ref()
                    .unwrap()
                    .materialize(&actual, &template.properties, &instance.values, 42)
                    .unwrap();
            }
            assert_eq!(actual, expected);
            assert!(!scenario.overrides[0].persist);
            assert!(scenario.overrides[1].persist);
            assert!(
                scenario.overrides[0]
                    .values
                    .values()
                    .all(|value| value.is_string())
            );
        }
    }

    #[test]
    fn rejects_invalid_reductions_and_accounts() {
        let mut account = market();
        let address = Pubkey::new_unique();
        for (sell, buy) in [(0, 10000), (10000, 10001), (10000, 10000)] {
            assert!(build_tessera_depth_scenario(address, &account, sell, buy).is_err());
        }
        account.data[160..168].copy_from_slice(&1u64.to_le_bytes());
        assert!(build_tessera_depth_scenario(address, &account, 1000, 10000).is_err());
        account.owner = Pubkey::new_unique();
        assert!(build_tessera_depth_scenario(address, &account, 1000, 1000).is_err());
        account.owner = TESSERA_PROGRAM_ID;
        account.data.truncate(100);
        assert!(build_tessera_depth_scenario(address, &account, 1000, 1000).is_err());
    }
}
