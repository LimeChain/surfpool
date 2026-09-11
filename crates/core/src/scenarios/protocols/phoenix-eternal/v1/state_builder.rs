use core::mem::size_of;
use std::collections::HashMap;

use phoenix_rise_accounts::{
    PhoenixAccount, PhoenixAccountDecodeError,
    global_config::GlobalConfig,
    perp_asset_map::{
        MarkPrice, PerpAssetMap, PerpPriceComponent, PriceComponent, SpotPriceComponent,
        TicksAtSlot,
    },
};
use phoenix_rise_math::quantities::Ticks;
use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};
use thiserror::Error;

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

pub const PHOENIX_ETERNAL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih");
pub const PHOENIX_GLOBAL_CONFIG: Pubkey =
    Pubkey::from_str_const("2zskx2iyCvb6Stg7RBZkt1f6MrF4dpYtMG3yMvKwqtUZ");

pub fn ensure_phoenix_account_owner(
    account_address: impl core::fmt::Display,
    account: &Account,
) -> SurfpoolResult<()> {
    if account.owner == PHOENIX_ETERNAL_PROGRAM_ID {
        return Ok(());
    }

    Err(SurfpoolError::invalid_account_owner(
        account_address,
        Some(format!(
            "expected {PHOENIX_ETERNAL_PROGRAM_ID}, got {}",
            account.owner
        )),
    ))
}

pub fn is_phoenix_trader_account(data: &[u8]) -> bool {
    phoenix_account_kind(data) == Some(PhoenixAccount::Trader)
}

pub fn is_phoenix_perp_asset_map_account(data: &[u8]) -> bool {
    phoenix_account_kind(data) == Some(PhoenixAccount::PerpAssetMap)
}

fn phoenix_account_kind(data: &[u8]) -> Option<PhoenixAccount> {
    PhoenixAccount::from_discriminant(data.get(..8)?.try_into().unwrap())
}

const COLLATERAL_FIELD: &str = "traderState.quoteLotCollateral";
const DIRECT_MARK_SYMBOL_FIELD: &str = "symbol";
const DIRECT_MARK_TICKS_FIELD: &str = "target_ticks";
const REFERENCE_SPOT_TICKS_FIELD: &str = "spot_ticks";
const REFERENCE_PERP_TICKS_FIELD: &str = "perp_ticks";
const PREPARATION_SLOT: u64 = 0;
const MARK_PRICE_RANGE: core::ops::Range<usize> = 16..32;

pub fn phoenix_market_symbols(
    perp_asset_map: Pubkey,
    account: &Account,
) -> SurfpoolResult<Vec<String>> {
    ensure_phoenix_account_owner(perp_asset_map, account)?;

    let map = PerpAssetMap::try_from_account_bytes(&account.data).map_err(|error| {
        SurfpoolError::invalid_account_data(
            perp_asset_map,
            "Expected a valid Phoenix Eternal PerpAssetMap account",
            Some(error),
        )
    })?;
    let mut symbols = map
        .iter()
        .map(|entry| entry.map(|entry| entry.symbol.as_str().to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            SurfpoolError::invalid_account_data(
                perp_asset_map,
                "Expected a valid Phoenix Eternal PerpAssetMap account",
                Some(error),
            )
        })?;
    symbols.sort_unstable();

    Ok(symbols)
}

#[derive(Debug, Error, PartialEq, Eq)]
enum PhoenixPricePatchError {
    #[error("invalid Phoenix PerpAssetMap account: {0}")]
    InvalidPerpAssetMap(#[from] PhoenixAccountDecodeError),
    #[error("Phoenix market {symbol} was not found")]
    MarketNotFound { symbol: String },
    #[error("price ticks {ticks} exceed the Phoenix u32 tick range")]
    InvalidTicks { ticks: u64 },
    #[error("selected Phoenix market metadata does not occur exactly once")]
    InvalidMetadataLocation,
    #[error("price patch changed byte {offset} outside the requested price fields")]
    UnexpectedByteChange { offset: usize },
}

fn changed_byte_outside(
    original: &[u8],
    patched: &[u8],
    allowed: &[core::ops::Range<usize>],
) -> Option<usize> {
    original
        .iter()
        .zip(patched)
        .enumerate()
        .find(|(offset, (before, after))| {
            before != after && !allowed.iter().any(|range| range.contains(offset))
        })
        .map(|(offset, _)| offset)
}

fn patch_direct_mark(
    data: &[u8],
    symbol: &str,
    target_ticks: u64,
    mark_slot: u64,
) -> Result<Vec<u8>, PhoenixPricePatchError> {
    let target_ticks =
        Ticks::new_checked(target_ticks).map_err(|_| PhoenixPricePatchError::InvalidTicks {
            ticks: target_ticks,
        })?;
    patch_price_component(data, symbol, &[MARK_PRICE_RANGE], |price| {
        price.mark_price.price.slot = mark_slot;
        price.mark_price.price.ticks = target_ticks;
    })
}

fn patch_reference_prices(
    data: &[u8],
    symbol: &str,
    spot_ticks: u64,
    perp_ticks: u64,
    reference_slot: u64,
) -> Result<Vec<u8>, PhoenixPricePatchError> {
    let spot_ticks = Ticks::new_checked(spot_ticks)
        .map_err(|_| PhoenixPricePatchError::InvalidTicks { ticks: spot_ticks })?;
    let perp_ticks = Ticks::new_checked(perp_ticks)
        .map_err(|_| PhoenixPricePatchError::InvalidTicks { ticks: perp_ticks })?;
    patch_price_component(data, symbol, &reference_value_ranges(), |price| {
        for value in &mut price
            .mark_price
            .spot_price_component
            .last_exchange_spot_price
        {
            value.slot = reference_slot;
            value.ticks = spot_ticks;
        }
        for value in &mut price
            .mark_price
            .perp_price_component
            .last_exchange_perp_price
        {
            value.slot = reference_slot;
            value.ticks = perp_ticks;
        }
    })
}

fn patch_price_component(
    data: &[u8],
    symbol: &str,
    allowed: &[core::ops::Range<usize>],
    update: impl FnOnce(&mut PriceComponent),
) -> Result<Vec<u8>, PhoenixPricePatchError> {
    let map = PerpAssetMap::try_from_account_bytes(data)?;
    let entry =
        map.find_by_symbol(symbol)?
            .ok_or_else(|| PhoenixPricePatchError::MarketNotFound {
                symbol: symbol.to_string(),
            })?;
    let metadata_bytes = entry.metadata.as_bytes();
    let metadata_offset = unique_subslice_offset(data, metadata_bytes)
        .ok_or(PhoenixPricePatchError::InvalidMetadataLocation)?;
    let price_len = size_of::<PriceComponent>();
    let mut price = bytemuck::pod_read_unaligned::<PriceComponent>(&metadata_bytes[..price_len]);
    update(&mut price);

    let mut patched = data.to_vec();
    patched[metadata_offset..metadata_offset + price_len]
        .copy_from_slice(bytemuck::bytes_of(&price));
    PerpAssetMap::try_from_account_bytes(&patched)?;
    let allowed: Vec<_> = allowed
        .iter()
        .map(|range| metadata_offset + range.start..metadata_offset + range.end)
        .collect();
    if let Some(offset) = changed_byte_outside(data, &patched, &allowed) {
        return Err(PhoenixPricePatchError::UnexpectedByteChange { offset });
    }
    Ok(patched)
}

fn reference_value_ranges() -> Vec<core::ops::Range<usize>> {
    let mark_offset = core::mem::offset_of!(PriceComponent, mark_price);
    let spot_offset = mark_offset
        + core::mem::offset_of!(MarkPrice, spot_price_component)
        + core::mem::offset_of!(SpotPriceComponent, last_exchange_spot_price);
    let perp_offset = mark_offset
        + core::mem::offset_of!(MarkPrice, perp_price_component)
        + core::mem::offset_of!(PerpPriceComponent, last_exchange_perp_price);
    [spot_offset, perp_offset]
        .into_iter()
        .flat_map(|component_offset| {
            (0..5).map(move |index| {
                let value_start = component_offset + index * size_of::<TicksAtSlot>();
                value_start..value_start + size_of::<TicksAtSlot>()
            })
        })
        .collect()
}

fn unique_subslice_offset(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > data.len() {
        return None;
    }

    let mut match_offset = None;
    let mut search_start = 0;
    while search_start + needle.len() <= data.len() {
        let Some(relative_offset) = data[search_start..=data.len() - needle.len()]
            .iter()
            .position(|byte| *byte == needle[0])
        else {
            break;
        };
        let offset = search_start + relative_offset;
        if data[offset..].starts_with(needle) {
            if match_offset.is_some() {
                return None;
            }
            match_offset = Some(offset);
        }
        search_start = offset + 1;
    }
    match_offset
}

pub fn forge_phoenix_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &HashMap<String, serde_json::Value>,
    materialization_slot: u64,
) -> SurfpoolResult<Vec<u8>> {
    ensure_phoenix_account_owner(account_pubkey, account)?;
    let wants_direct_mark = account_values.contains_key(DIRECT_MARK_TICKS_FIELD);
    let wants_reference = account_values.contains_key(REFERENCE_SPOT_TICKS_FIELD)
        || account_values.contains_key(REFERENCE_PERP_TICKS_FIELD);
    let fields: &[&str] = match (wants_direct_mark, wants_reference) {
        (true, false) => &[DIRECT_MARK_SYMBOL_FIELD, DIRECT_MARK_TICKS_FIELD],
        (false, true) => &[
            DIRECT_MARK_SYMBOL_FIELD,
            REFERENCE_SPOT_TICKS_FIELD,
            REFERENCE_PERP_TICKS_FIELD,
        ],
        _ => {
            return Err(SurfpoolError::internal(
                "Phoenix map overrides accept exactly one value group: \
             symbol + target_ticks, or symbol + spot_ticks + perp_ticks",
            ));
        }
    };
    if account_values.len() != fields.len()
        || fields
            .iter()
            .any(|field| !account_values.contains_key(*field))
    {
        return Err(SurfpoolError::internal(format!(
            "Phoenix map overrides accept only {}",
            fields.join(", ")
        )));
    }
    let symbol = account_values[DIRECT_MARK_SYMBOL_FIELD]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SurfpoolError::internal("symbol must be a non-empty string"))?;
    let patched = if wants_direct_mark {
        patch_direct_mark(
            &account.data,
            symbol,
            parse_unsigned_ticks(
                &account_values[DIRECT_MARK_TICKS_FIELD],
                DIRECT_MARK_TICKS_FIELD,
            )?,
            materialization_slot,
        )
    } else {
        patch_reference_prices(
            &account.data,
            symbol,
            parse_unsigned_ticks(
                &account_values[REFERENCE_SPOT_TICKS_FIELD],
                REFERENCE_SPOT_TICKS_FIELD,
            )?,
            parse_unsigned_ticks(
                &account_values[REFERENCE_PERP_TICKS_FIELD],
                REFERENCE_PERP_TICKS_FIELD,
            )?,
            materialization_slot,
        )
    };
    patched.map_err(|error| {
        SurfpoolError::invalid_account_data(
            account_pubkey,
            "Expected a valid Phoenix Eternal PerpAssetMap account",
            Some(error),
        )
    })
}

pub enum PhoenixOverridePreparation {
    GenericIdl { is_trader: bool },
    Patched(Vec<u8>),
}

pub fn prepare_phoenix_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &mut HashMap<String, serde_json::Value>,
    materialization_slot: u64,
) -> SurfpoolResult<PhoenixOverridePreparation> {
    if account.owner != PHOENIX_ETERNAL_PROGRAM_ID {
        return Ok(PhoenixOverridePreparation::GenericIdl { is_trader: false });
    }

    if is_phoenix_perp_asset_map_account(&account.data) {
        return forge_phoenix_override(
            account_pubkey,
            account,
            account_values,
            materialization_slot,
        )
        .map(PhoenixOverridePreparation::Patched);
    }

    let is_trader = is_phoenix_trader_account(&account.data);
    if is_trader {
        super::collateral::trader_header(account_pubkey, account)?;
        if let Some(field) = account_values.keys().find(|field| {
            field.as_str() == "traderState"
                || (field.starts_with("traderState.")
                    && field.as_str() != "traderState.quoteLotCollateral")
        }) {
            return Err(SurfpoolError::internal(format!(
                "Phoenix TraderState field '{field}' is unsupported; only \
                 traderState.quoteLotCollateral has consistent hot-Trader mirroring"
            )));
        }
        if let Some(value) = account_values.get_mut("traderState.quoteLotCollateral") {
            *value = super::collateral::collateral_override_value(value)?;
        }
    }

    Ok(PhoenixOverridePreparation::GenericIdl { is_trader })
}

pub fn build_phoenix_collateral_scenario(
    trader: Pubkey,
    trader_account: &Account,
    target_quote_lots: &str,
    global_trader_index: Option<&Account>,
) -> SurfpoolResult<Scenario> {
    let target_quote_lots = parse_quote_lot_collateral(target_quote_lots)?;
    let header = super::collateral::trader_header(&trader, trader_account)?;

    // Raising collateral needs a real deposit into the global vault.
    let current_quote_lots = super::collateral::effective_collateral(&header, global_trader_index)?;
    if target_quote_lots > current_quote_lots {
        return Err(SurfpoolError::internal(format!(
            "Phoenix collateral stress can only lower collateral: {current_quote_lots} quote lots \
             are backed by the global vault, {target_quote_lots} would not be. Deposit first to \
             raise it."
        )));
    }

    let template = TemplateRegistry::new()
        .get("phoenix-trader-collateral-stress")
        .cloned()
        .ok_or_else(|| SurfpoolError::internal("Phoenix collateral template is unavailable"))?;
    let values = HashMap::from([(
        COLLATERAL_FIELD.to_string(),
        serde_json::json!(target_quote_lots.to_string()),
    )]);
    let collateral_override = OverrideInstance::new(
        template.id,
        PREPARATION_SLOT,
        AccountAddress::Pubkey(trader.to_string()),
    )
    .with_values(values)
    .with_label("Phoenix Trader collateral stress".to_string());

    let mut scenario = Scenario::new(
        "Phoenix Trader Collateral Stress".to_string(),
        "Set exact signed quote-lot collateral on a Phoenix Trader and its effective index entry."
            .to_string(),
    );
    scenario.tags = vec![
        "phoenix-eternal".to_string(),
        "collateral".to_string(),
        "risk".to_string(),
    ];
    scenario.add_override(collateral_override);

    Ok(scenario)
}

pub fn phoenix_perp_asset_map_address(global_account: &Account) -> SurfpoolResult<Pubkey> {
    Ok(Pubkey::new_from_array(
        phoenix_global_config(global_account)?.perp_asset_map_key(),
    ))
}

pub fn phoenix_global_trader_index_address(global_account: &Account) -> SurfpoolResult<Pubkey> {
    Ok(Pubkey::new_from_array(
        phoenix_global_config(global_account)?.global_trader_index_header_key(),
    ))
}

fn phoenix_global_config(global_account: &Account) -> SurfpoolResult<GlobalConfig> {
    ensure_phoenix_account_owner(PHOENIX_GLOBAL_CONFIG, global_account)?;
    let global = GlobalConfig::try_from_account_bytes(&global_account.data).map_err(|error| {
        SurfpoolError::invalid_account_data(
            PHOENIX_GLOBAL_CONFIG,
            "Expected a valid Phoenix Eternal GlobalConfig account",
            Some(error),
        )
    })?;
    if Pubkey::new_from_array(global.account_key()) != PHOENIX_GLOBAL_CONFIG {
        return Err(SurfpoolError::invalid_account_data(
            PHOENIX_GLOBAL_CONFIG,
            "GlobalConfig account_key does not match its address",
            None::<String>,
        ));
    }
    Ok(global)
}

fn parse_unsigned_ticks(value: &serde_json::Value, field: &str) -> SurfpoolResult<u64> {
    value
        .as_str()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            SurfpoolError::internal(format!(
                "{field} must be an unsigned 64-bit integer encoded as a string"
            ))
        })
}

fn parse_quote_lot_collateral(value: &str) -> SurfpoolResult<i64> {
    value.parse::<i64>().map_err(|_| {
        SurfpoolError::internal(
            "quote_lot_collateral must be a signed 64-bit integer encoded as a string",
        )
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use base64::{Engine, prelude::BASE64_STANDARD};
    use phoenix_rise_accounts::{PhoenixAccount, trader::TraderHeader};
    use solana_account::Account;

    use super::*;

    const TRADER_HEADER_LEN: usize = size_of::<TraderHeader>();
    const COLLATERAL_BYTE_RANGE: core::ops::Range<usize> = 88..96;
    const POSITION_MAP_PREFIX_LEN: usize = 16;
    const POSITION_ENTRY_LEN: usize = 40;
    const PERP_ASSET_MAP_LEN: usize = 1_622_064;
    const SOL_PERP_ASSET_MAP_PREFIX_B64: &str = "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";

    fn trader_fixture(collateral: i64, len: u64, capacity: u64) -> Vec<u8> {
        let capacity = usize::try_from(capacity).expect("fixture capacity");
        let mut data =
            vec![0_u8; TRADER_HEADER_LEN + POSITION_MAP_PREFIX_LEN + capacity * POSITION_ENTRY_LEN];
        data[..8].copy_from_slice(&PhoenixAccount::Trader.discriminant());
        data[COLLATERAL_BYTE_RANGE].copy_from_slice(&collateral.to_le_bytes());
        data[112..116].copy_from_slice(&(capacity as u32).to_le_bytes());
        data[TRADER_HEADER_LEN..TRADER_HEADER_LEN + 8].copy_from_slice(&len.to_le_bytes());
        data[TRADER_HEADER_LEN + 8..TRADER_HEADER_LEN + 16]
            .copy_from_slice(&(capacity as u64).to_le_bytes());
        if len > 0 && capacity > 0 {
            data[TRADER_HEADER_LEN + POSITION_MAP_PREFIX_LEN
                ..TRADER_HEADER_LEN + POSITION_MAP_PREFIX_LEN + 8]
                .copy_from_slice(&42_u64.to_le_bytes());
        }
        data
    }

    fn trader_account() -> Account {
        Account {
            lamports: 1,
            data: trader_fixture(0, 1, 2),
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn trader_account_for(trader: Pubkey, collateral: i64) -> Account {
        let mut account = Account {
            data: trader_fixture(collateral, 1, 2),
            ..trader_account()
        };
        account.data[24..56].copy_from_slice(trader.as_array());
        account
    }

    pub(crate) fn perp_asset_map_fixture() -> Vec<u8> {
        let prefix = BASE64_STANDARD
            .decode(SOL_PERP_ASSET_MAP_PREFIX_B64)
            .unwrap();
        let mut data = vec![0_u8; PERP_ASSET_MAP_LEN];
        data[..prefix.len()].copy_from_slice(&prefix);
        data[24..26].copy_from_slice(&1_u16.to_le_bytes());
        data[32..36].copy_from_slice(&1_u32.to_le_bytes());
        data[36..40].copy_from_slice(&0_u32.to_le_bytes());
        data
    }

    #[test]
    fn builder_rejects_an_account_that_is_not_a_trader() {
        let mut account = trader_account();
        account.data[..8].fill(0);

        let error = build_phoenix_collateral_scenario(Pubkey::new_unique(), &account, "1", None)
            .unwrap_err();

        assert!(error.to_string().contains("Trader"));
    }

    #[test]
    fn builder_rejects_collateral_outside_the_signed_64_bit_range() {
        let error = build_phoenix_collateral_scenario(
            Pubkey::new_unique(),
            &trader_account(),
            "9223372036854775808",
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("signed 64-bit integer"));
    }

    #[test]
    fn builds_one_editable_collateral_override_for_the_requested_trader() {
        let trader = Pubkey::new_unique();
        let preparation = build_phoenix_collateral_scenario(
            trader,
            &trader_account_for(trader, 0),
            "-9007199254740993",
            None,
        )
        .unwrap();

        assert_eq!(preparation.overrides.len(), 1);

        let collateral_override = &preparation.overrides[0];
        assert_eq!(
            collateral_override.template_id,
            "phoenix-trader-collateral-stress"
        );
        assert_eq!(
            collateral_override.account,
            AccountAddress::Pubkey(trader.to_string())
        );
        assert_eq!(
            collateral_override.values[COLLATERAL_FIELD],
            serde_json::json!("-9007199254740993")
        );
        assert_eq!(collateral_override.scenario_relative_slot, PREPARATION_SLOT);
        assert!(!collateral_override.fetch_before_use);
    }

    #[test]
    fn builder_refuses_to_raise_collateral_above_its_vault_backing() {
        let trader = Pubkey::new_unique();
        let funded = trader_account_for(trader, 500);

        let raised = build_phoenix_collateral_scenario(trader, &funded, "501", None).unwrap_err();
        assert!(raised.to_string().contains("can only lower collateral"));

        let lowered = build_phoenix_collateral_scenario(trader, &funded, "499", None).unwrap();
        assert_eq!(lowered.overrides[0].values[COLLATERAL_FIELD], "499");
        let held = build_phoenix_collateral_scenario(trader, &funded, "500", None).unwrap();
        assert_eq!(held.overrides[0].values[COLLATERAL_FIELD], "500");
    }

    #[test]
    fn builder_rejects_a_non_phoenix_account() {
        let trader = Pubkey::new_unique();
        let mut account = trader_account();
        account.owner = Pubkey::new_unique();

        let error = build_phoenix_collateral_scenario(trader, &account, "1", None).unwrap_err();

        assert!(error.to_string().contains("invalid account owner"));
    }

    #[test]
    fn lists_active_market_symbols_from_the_perp_asset_map() {
        let perp_asset_map = Pubkey::new_unique();
        let account = Account {
            lamports: 1,
            data: perp_asset_map_fixture(),
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        };

        assert_eq!(
            phoenix_market_symbols(perp_asset_map, &account).unwrap(),
            vec!["SOL"]
        );
    }

    #[test]
    fn patches_only_the_selected_mark_ticks_and_slot_in_an_official_market_entry() {
        let data = perp_asset_map_fixture();
        let map = PerpAssetMap::try_from_account_bytes(&data).unwrap();
        let before = map.find_by_symbol("SOL").unwrap().unwrap();
        let metadata_offset = unique_subslice_offset(&data, before.metadata.as_bytes()).unwrap();

        let patched = patch_direct_mark(&data, "SOL", 1, 123).unwrap();
        let map = PerpAssetMap::try_from_account_bytes(&patched).unwrap();
        let after = map.find_by_symbol("SOL").unwrap().unwrap();
        assert_eq!(after.metadata.oracle_price().mark_price.price.slot, 123);

        assert_eq!(
            after
                .metadata
                .oracle_price()
                .mark_price
                .price
                .ticks
                .as_inner(),
            1
        );
        assert_eq!(patched.len(), data.len());
        assert!(
            data.iter()
                .zip(&patched)
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .all(|(offset, _)| {
                    (metadata_offset + MARK_PRICE_RANGE.start
                        ..metadata_offset + MARK_PRICE_RANGE.end)
                        .contains(&offset)
                })
        );
    }

    #[test]
    fn reference_price_patch_supports_both_divergence_directions_and_preserves_mark() {
        let data = perp_asset_map_fixture();
        let before_map = PerpAssetMap::try_from_account_bytes(&data).unwrap();
        let before = before_map.find_by_symbol("SOL").unwrap().unwrap();
        let before_mark = before
            .metadata
            .oracle_price()
            .mark_price
            .price
            .ticks
            .as_inner();

        for (spot_ticks, perp_ticks) in [(8_000, 7_000), (7_000, 8_000)] {
            let patched =
                patch_reference_prices(&data, "SOL", spot_ticks, perp_ticks, 123).unwrap();
            let map = PerpAssetMap::try_from_account_bytes(&patched).unwrap();
            let entry = map.find_by_symbol("SOL").unwrap().unwrap();
            let price = entry.metadata.oracle_price();

            assert_eq!(price.mark_price.price.ticks.as_inner(), before_mark);
            assert!(
                price
                    .mark_price
                    .spot_price_component
                    .last_exchange_spot_price
                    .iter()
                    .all(|value| value.ticks.as_inner() == spot_ticks && value.slot == 123)
            );
            assert!(
                price
                    .mark_price
                    .perp_price_component
                    .last_exchange_perp_price
                    .iter()
                    .all(|value| value.ticks.as_inner() == perp_ticks && value.slot == 123)
            );
            assert_eq!(patched.len(), data.len());
        }
    }

    #[test]
    fn direct_mark_rejects_unknown_markets_and_out_of_range_ticks() {
        let data = perp_asset_map_fixture();

        assert!(matches!(
            patch_direct_mark(&data, "BTC", 1, 123),
            Err(PhoenixPricePatchError::MarketNotFound { .. })
        ));
        assert!(matches!(
            patch_direct_mark(&data, "SOL", u64::from(u32::MAX) + 1, 123,),
            Err(PhoenixPricePatchError::InvalidTicks { .. })
        ));
    }

    #[test]
    fn direct_mark_override_requires_exact_string_fields() {
        let account = Account {
            lamports: 1,
            data: perp_asset_map_fixture(),
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        };
        let values = HashMap::from([
            (
                DIRECT_MARK_SYMBOL_FIELD.to_string(),
                serde_json::json!("SOL"),
            ),
            (DIRECT_MARK_TICKS_FIELD.to_string(), serde_json::json!("1")),
        ]);

        let patched =
            forge_phoenix_override(&Pubkey::new_unique(), &account, &values, 123).unwrap();
        let map = PerpAssetMap::try_from_account_bytes(&patched).unwrap();
        let price = map
            .find_by_symbol("SOL")
            .unwrap()
            .unwrap()
            .metadata
            .oracle_price()
            .mark_price
            .price;
        assert_eq!(price.ticks.as_inner(), 1);
        assert_eq!(price.slot, 123);

        let mut numeric_ticks = values;
        numeric_ticks.insert(DIRECT_MARK_TICKS_FIELD.to_string(), serde_json::json!(1));
        assert!(
            forge_phoenix_override(&Pubkey::new_unique(), &account, &numeric_ticks, 123)
                .unwrap_err()
                .to_string()
                .contains("encoded as a string")
        );
    }

    #[test]
    fn reference_override_patches_prices_and_requires_string_fields() {
        let account = Account {
            lamports: 1,
            data: perp_asset_map_fixture(),
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        };
        let values = HashMap::from([
            (
                DIRECT_MARK_SYMBOL_FIELD.to_string(),
                serde_json::json!("SOL"),
            ),
            (
                REFERENCE_SPOT_TICKS_FIELD.to_string(),
                serde_json::json!("8000"),
            ),
            (
                REFERENCE_PERP_TICKS_FIELD.to_string(),
                serde_json::json!("7000"),
            ),
        ]);

        let patched =
            forge_phoenix_override(&Pubkey::new_unique(), &account, &values, 123).unwrap();
        let map = PerpAssetMap::try_from_account_bytes(&patched).unwrap();
        let entry = map.find_by_symbol("SOL").unwrap().unwrap();
        let price = entry.metadata.oracle_price();

        assert!(
            price
                .mark_price
                .spot_price_component
                .last_exchange_spot_price
                .iter()
                .all(|value| value.slot == 123 && value.ticks.as_inner() == 8_000)
        );
        assert!(
            price
                .mark_price
                .perp_price_component
                .last_exchange_perp_price
                .iter()
                .all(|value| value.slot == 123 && value.ticks.as_inner() == 7_000)
        );
        let mut numeric = values;
        numeric.insert(
            REFERENCE_SPOT_TICKS_FIELD.to_string(),
            serde_json::json!(8000),
        );
        assert!(
            forge_phoenix_override(&Pubkey::new_unique(), &account, &numeric, 100)
                .unwrap_err()
                .to_string()
                .contains("encoded as a string")
        );
    }

    #[test]
    fn forge_rejects_no_value_group() {
        let error = forge_phoenix_override(
            &Pubkey::new_unique(),
            &trader_account(),
            &HashMap::new(),
            100,
        )
        .unwrap_err();

        assert!(error.to_string().contains("exactly one value group"));
    }

    #[test]
    fn forge_rejects_mixed_value_groups() {
        let values = HashMap::from([
            (DIRECT_MARK_TICKS_FIELD.to_string(), serde_json::json!("1")),
            (
                REFERENCE_SPOT_TICKS_FIELD.to_string(),
                serde_json::json!("2"),
            ),
        ]);

        let error = forge_phoenix_override(&Pubkey::new_unique(), &trader_account(), &values, 100)
            .unwrap_err();

        assert!(error.to_string().contains("exactly one value group"));
    }
}
