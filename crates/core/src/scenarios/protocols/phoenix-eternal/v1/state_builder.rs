use core::mem::size_of;
use std::collections::HashMap;

use phoenix_rise_accounts::{
    PhoenixAccountDecodeError,
    global_config::GlobalConfig,
    perp_asset_map::{
        MarkPrice, PerpAssetMap, PerpPriceComponent, PriceComponent, SpotPriceComponent,
        TicksAtSlot,
    },
    trader::{Trader, TraderHeader},
};
use phoenix_rise_math::quantities::{SignedQuoteLots, Ticks};
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

const TRADER_HEADER_LEN: usize = size_of::<TraderHeader>();
const COLLATERAL_BYTE_RANGE: core::ops::Range<usize> = 88..96;
const COLLATERAL_FIELD: &str = "quote_lot_collateral";
const DIRECT_MARK_SYMBOL_FIELD: &str = "symbol";
const DIRECT_MARK_TICKS_FIELD: &str = "target_ticks";
const REFERENCE_SPOT_TICKS_FIELD: &str = "spot_ticks";
const REFERENCE_PERP_TICKS_FIELD: &str = "perp_ticks";
const PREPARATION_SLOT: u64 = 0;
const MARK_PRICE_RANGE: core::ops::Range<usize> = 16..32;
const MARK_PRICE_TICKS_RANGE: core::ops::Range<usize> = 24..32;

#[derive(Clone, Debug, PartialEq)]
pub struct PhoenixCollateralPreparation {
    pub scenario: Scenario,
    pub trader: Pubkey,
    pub target_quote_lots: i64,
}

pub fn phoenix_market_symbols(
    perp_asset_map: Pubkey,
    account: &Account,
) -> SurfpoolResult<Vec<String>> {
    if account.owner != PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(SurfpoolError::invalid_account_owner(
            perp_asset_map,
            None::<PhoenixAccountDecodeError>,
        ));
    }

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
pub enum PhoenixTraderPatchError {
    #[error("expected Phoenix Eternal owner, got {actual}")]
    InvalidOwner { actual: Pubkey },
    #[error("invalid Phoenix Trader account: {0}")]
    InvalidTrader(#[from] PhoenixAccountDecodeError),
    #[error("collateral patch changed byte {offset} outside the collateral field")]
    UnexpectedByteChange { offset: usize },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PhoenixDirectMarkPatchError {
    #[error("expected Phoenix Eternal owner, got {actual}")]
    InvalidOwner { actual: Pubkey },
    #[error("invalid Phoenix PerpAssetMap account: {0}")]
    InvalidPerpAssetMap(#[from] PhoenixAccountDecodeError),
    #[error("Phoenix market {symbol} was not found")]
    MarketNotFound { symbol: String },
    #[error("target mark ticks {ticks} exceed the Phoenix u32 tick range")]
    InvalidTicks { ticks: u64 },
    #[error("selected Phoenix market metadata does not occur exactly once")]
    InvalidMetadataLocation,
    #[error("direct mark patch changed byte {offset} outside the mark tick field")]
    UnexpectedByteChange { offset: usize },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PhoenixReferencePricePatchError {
    #[error("expected Phoenix Eternal owner, got {actual}")]
    InvalidOwner { actual: Pubkey },
    #[error("invalid Phoenix PerpAssetMap account: {0}")]
    InvalidPerpAssetMap(#[from] PhoenixAccountDecodeError),
    #[error("Phoenix market {symbol} was not found")]
    MarketNotFound { symbol: String },
    #[error("reference ticks {ticks} exceed the Phoenix u32 tick range")]
    InvalidTicks { ticks: u64 },
    #[error("selected Phoenix market metadata does not occur exactly once")]
    InvalidMetadataLocation,
    #[error("reference-price patch changed byte {offset} outside the spot/perp fields")]
    UnexpectedByteChange { offset: usize },
}

/// The offset of the first byte a patch changed outside the ranges it was
/// allowed to touch, if there is one.
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

pub fn patch_trader_collateral(
    owner: &Pubkey,
    data: &[u8],
    target_quote_lots: i64,
) -> Result<Vec<u8>, PhoenixTraderPatchError> {
    if owner != &PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(PhoenixTraderPatchError::InvalidOwner { actual: *owner });
    }

    validate_trader(data)?;

    let mut header = TraderHeader::try_read_from_account_bytes(data)?;
    header.trader_state.quote_lot_collateral = SignedQuoteLots::new(target_quote_lots);

    let mut patched_data = data.to_vec();
    patched_data[..TRADER_HEADER_LEN].copy_from_slice(bytemuck::bytes_of(&header));

    validate_trader(&patched_data)?;

    if let Some(offset) = changed_byte_outside(data, &patched_data, &[COLLATERAL_BYTE_RANGE]) {
        return Err(PhoenixTraderPatchError::UnexpectedByteChange { offset });
    }

    Ok(patched_data)
}

pub fn patch_direct_mark(
    owner: &Pubkey,
    data: &[u8],
    symbol: &str,
    target_ticks: u64,
) -> Result<Vec<u8>, PhoenixDirectMarkPatchError> {
    patch_direct_mark_inner(owner, data, symbol, target_ticks, None)
}

fn patch_direct_mark_at_slot(
    owner: &Pubkey,
    data: &[u8],
    symbol: &str,
    target_ticks: u64,
    mark_slot: u64,
) -> Result<Vec<u8>, PhoenixDirectMarkPatchError> {
    patch_direct_mark_inner(owner, data, symbol, target_ticks, Some(mark_slot))
}

fn patch_direct_mark_inner(
    owner: &Pubkey,
    data: &[u8],
    symbol: &str,
    target_ticks: u64,
    mark_slot: Option<u64>,
) -> Result<Vec<u8>, PhoenixDirectMarkPatchError> {
    if owner != &PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(PhoenixDirectMarkPatchError::InvalidOwner { actual: *owner });
    }
    let target_ticks = Ticks::new_checked(target_ticks).map_err(|_| {
        PhoenixDirectMarkPatchError::InvalidTicks {
            ticks: target_ticks,
        }
    })?;
    let map = PerpAssetMap::try_from_account_bytes(data)?;
    let entry =
        map.find_by_symbol(symbol)?
            .ok_or_else(|| PhoenixDirectMarkPatchError::MarketNotFound {
                symbol: symbol.to_string(),
            })?;
    let metadata_bytes = entry.metadata.as_bytes();
    let metadata_offset = unique_subslice_offset(data, metadata_bytes)
        .ok_or(PhoenixDirectMarkPatchError::InvalidMetadataLocation)?;
    let price_len = size_of::<PriceComponent>();
    let mut price = bytemuck::pod_read_unaligned::<PriceComponent>(&metadata_bytes[..price_len]);
    if let Some(mark_slot) = mark_slot {
        price.mark_price.price.slot = mark_slot;
    }
    price.mark_price.price.ticks = target_ticks;

    let mut patched_data = data.to_vec();
    patched_data[metadata_offset..metadata_offset + price_len]
        .copy_from_slice(bytemuck::bytes_of(&price));
    PerpAssetMap::try_from_account_bytes(&patched_data)?;

    let mark_range = if mark_slot.is_some() {
        MARK_PRICE_RANGE
    } else {
        MARK_PRICE_TICKS_RANGE
    };
    let allowed_range = metadata_offset + mark_range.start..metadata_offset + mark_range.end;
    if let Some(offset) = changed_byte_outside(data, &patched_data, &[allowed_range]) {
        return Err(PhoenixDirectMarkPatchError::UnexpectedByteChange { offset });
    }

    Ok(patched_data)
}

pub fn patch_reference_prices(
    owner: &Pubkey,
    data: &[u8],
    symbol: &str,
    spot_ticks: u64,
    perp_ticks: u64,
) -> Result<Vec<u8>, PhoenixReferencePricePatchError> {
    patch_reference_prices_inner(owner, data, symbol, spot_ticks, perp_ticks, None)
}

fn patch_reference_prices_at_slot(
    owner: &Pubkey,
    data: &[u8],
    symbol: &str,
    spot_ticks: u64,
    perp_ticks: u64,
    reference_slot: u64,
) -> Result<Vec<u8>, PhoenixReferencePricePatchError> {
    patch_reference_prices_inner(
        owner,
        data,
        symbol,
        spot_ticks,
        perp_ticks,
        Some(reference_slot),
    )
}

fn patch_reference_prices_inner(
    owner: &Pubkey,
    data: &[u8],
    symbol: &str,
    spot_ticks: u64,
    perp_ticks: u64,
    reference_slot: Option<u64>,
) -> Result<Vec<u8>, PhoenixReferencePricePatchError> {
    if owner != &PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(PhoenixReferencePricePatchError::InvalidOwner { actual: *owner });
    }
    let spot_ticks = Ticks::new_checked(spot_ticks)
        .map_err(|_| PhoenixReferencePricePatchError::InvalidTicks { ticks: spot_ticks })?;
    let perp_ticks = Ticks::new_checked(perp_ticks)
        .map_err(|_| PhoenixReferencePricePatchError::InvalidTicks { ticks: perp_ticks })?;
    let map = PerpAssetMap::try_from_account_bytes(data)?;
    let entry = map.find_by_symbol(symbol)?.ok_or_else(|| {
        PhoenixReferencePricePatchError::MarketNotFound {
            symbol: symbol.to_string(),
        }
    })?;
    let metadata_bytes = entry.metadata.as_bytes();
    let metadata_offset = unique_subslice_offset(data, metadata_bytes)
        .ok_or(PhoenixReferencePricePatchError::InvalidMetadataLocation)?;
    let price_len = size_of::<PriceComponent>();
    let mut price = bytemuck::pod_read_unaligned::<PriceComponent>(&metadata_bytes[..price_len]);
    for value in &mut price
        .mark_price
        .spot_price_component
        .last_exchange_spot_price
    {
        if let Some(reference_slot) = reference_slot {
            value.slot = reference_slot;
        }
        value.ticks = spot_ticks;
    }
    for value in &mut price
        .mark_price
        .perp_price_component
        .last_exchange_perp_price
    {
        if let Some(reference_slot) = reference_slot {
            value.slot = reference_slot;
        }
        value.ticks = perp_ticks;
    }

    let mut patched_data = data.to_vec();
    patched_data[metadata_offset..metadata_offset + price_len]
        .copy_from_slice(bytemuck::bytes_of(&price));
    PerpAssetMap::try_from_account_bytes(&patched_data)?;

    let allowed_ranges = reference_value_ranges(metadata_offset, reference_slot.is_some());
    if let Some(offset) = changed_byte_outside(data, &patched_data, &allowed_ranges) {
        return Err(PhoenixReferencePricePatchError::UnexpectedByteChange { offset });
    }

    Ok(patched_data)
}

fn reference_value_ranges(
    metadata_offset: usize,
    include_slots: bool,
) -> Vec<core::ops::Range<usize>> {
    let mark_offset = core::mem::offset_of!(PriceComponent, mark_price);
    let ticks_offset = core::mem::offset_of!(TicksAtSlot, ticks);
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
                let value_start =
                    metadata_offset + component_offset + index * size_of::<TicksAtSlot>();
                if include_slots {
                    value_start..value_start + size_of::<TicksAtSlot>()
                } else {
                    let ticks_start = value_start + ticks_offset;
                    ticks_start..ticks_start + size_of::<Ticks>()
                }
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

/// Routes a Phoenix-owned account's override to the right typed patcher by the
/// value group it carries; the patchers themselves enforce the account type.
pub fn forge_phoenix_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &HashMap<String, serde_json::Value>,
    materialization_slot: u64,
) -> SurfpoolResult<Vec<u8>> {
    let wants_collateral = account_values.contains_key(COLLATERAL_FIELD);
    let wants_direct_mark = account_values.contains_key(DIRECT_MARK_TICKS_FIELD);
    let wants_reference = account_values.contains_key(REFERENCE_SPOT_TICKS_FIELD)
        || account_values.contains_key(REFERENCE_PERP_TICKS_FIELD);

    match (wants_collateral, wants_direct_mark, wants_reference) {
        (true, false, false) => {
            forge_trader_collateral_override(account_pubkey, account, account_values)
        }
        (false, true, false) => forge_direct_mark_override(
            account_pubkey,
            account,
            account_values,
            materialization_slot,
        ),
        (false, false, true) => forge_reference_price_override(
            account_pubkey,
            account,
            account_values,
            materialization_slot,
        ),
        _ => Err(SurfpoolError::internal(
            "Phoenix overrides accept exactly one value group: quote_lot_collateral, \
             symbol + target_ticks, or symbol + spot_ticks + perp_ticks",
        )),
    }
}

pub fn forge_trader_collateral_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &HashMap<String, serde_json::Value>,
) -> SurfpoolResult<Vec<u8>> {
    if account_values.len() != 1 || !account_values.contains_key(COLLATERAL_FIELD) {
        return Err(SurfpoolError::internal(
            "Phoenix Trader collateral overrides accept only quote_lot_collateral",
        ));
    }

    let target_quote_lots = account_values[COLLATERAL_FIELD]
        .as_str()
        .ok_or_else(invalid_quote_lot_collateral)
        .and_then(parse_quote_lot_collateral)?;

    patch_trader_collateral(&account.owner, &account.data, target_quote_lots).map_err(|error| {
        match error {
            PhoenixTraderPatchError::InvalidOwner { .. } => {
                SurfpoolError::invalid_account_owner(account_pubkey, Some(error))
            }
            _ => SurfpoolError::invalid_account_data(
                account_pubkey,
                "Expected a valid Phoenix Eternal Trader account",
                Some(error),
            ),
        }
    })
}

pub fn forge_direct_mark_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &HashMap<String, serde_json::Value>,
    materialization_slot: u64,
) -> SurfpoolResult<Vec<u8>> {
    if account_values.len() != 2
        || !account_values.contains_key(DIRECT_MARK_SYMBOL_FIELD)
        || !account_values.contains_key(DIRECT_MARK_TICKS_FIELD)
    {
        return Err(SurfpoolError::internal(
            "Phoenix direct mark overrides accept only symbol and target_ticks",
        ));
    }

    let symbol = account_values[DIRECT_MARK_SYMBOL_FIELD]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SurfpoolError::internal("symbol must be a non-empty string"))?;
    let target_ticks = account_values[DIRECT_MARK_TICKS_FIELD]
        .as_str()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            SurfpoolError::internal(
                "target_ticks must be an unsigned 64-bit integer encoded as a string",
            )
        })?;

    patch_direct_mark_at_slot(
        &account.owner,
        &account.data,
        symbol,
        target_ticks,
        materialization_slot,
    )
    .map_err(|error| match error {
        PhoenixDirectMarkPatchError::InvalidOwner { .. } => {
            SurfpoolError::invalid_account_owner(account_pubkey, Some(error))
        }
        _ => SurfpoolError::invalid_account_data(
            account_pubkey,
            "Expected a valid Phoenix Eternal PerpAssetMap account",
            Some(error),
        ),
    })
}

pub fn forge_reference_price_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &HashMap<String, serde_json::Value>,
    materialization_slot: u64,
) -> SurfpoolResult<Vec<u8>> {
    if account_values.len() != 3
        || !account_values.contains_key(DIRECT_MARK_SYMBOL_FIELD)
        || !account_values.contains_key(REFERENCE_SPOT_TICKS_FIELD)
        || !account_values.contains_key(REFERENCE_PERP_TICKS_FIELD)
    {
        return Err(SurfpoolError::internal(
            "Phoenix reference-price overrides accept only symbol, spot_ticks, and perp_ticks",
        ));
    }

    let symbol = account_values[DIRECT_MARK_SYMBOL_FIELD]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SurfpoolError::internal("symbol must be a non-empty string"))?;
    let spot_ticks = parse_unsigned_ticks(
        &account_values[REFERENCE_SPOT_TICKS_FIELD],
        REFERENCE_SPOT_TICKS_FIELD,
    )?;
    let perp_ticks = parse_unsigned_ticks(
        &account_values[REFERENCE_PERP_TICKS_FIELD],
        REFERENCE_PERP_TICKS_FIELD,
    )?;

    patch_reference_prices_at_slot(
        &account.owner,
        &account.data,
        symbol,
        spot_ticks,
        perp_ticks,
        materialization_slot,
    )
    .map_err(|error| match error {
        PhoenixReferencePricePatchError::InvalidOwner { .. } => {
            SurfpoolError::invalid_account_owner(account_pubkey, Some(error))
        }
        _ => SurfpoolError::invalid_account_data(
            account_pubkey,
            "Expected a valid Phoenix Eternal PerpAssetMap account",
            Some(error),
        ),
    })
}

pub fn build_phoenix_collateral_scenario(
    trader: Pubkey,
    trader_account: &Account,
    target_quote_lots: &str,
) -> SurfpoolResult<PhoenixCollateralPreparation> {
    let target_quote_lots = parse_quote_lot_collateral(target_quote_lots)?;
    patch_trader_collateral(
        &trader_account.owner,
        &trader_account.data,
        target_quote_lots,
    )
    .map_err(|error| match error {
        PhoenixTraderPatchError::InvalidOwner { .. } => {
            SurfpoolError::invalid_account_owner(trader, Some(error))
        }
        _ => SurfpoolError::invalid_account_data(
            trader,
            "Expected a valid Phoenix Eternal Trader account",
            Some(error),
        ),
    })?;

    // Collateral is a claim on the global vault's real tokens. An override cannot
    // mint that backing, so raising it would let withdrawals draw on balances that
    // are not there; a real DepositFunds transaction is the way up.
    let current_quote_lots = TraderHeader::try_read_from_account_bytes(&trader_account.data)
        .map(|header| header.trader_state.quote_lot_collateral.as_inner())
        .map_err(|error| {
            SurfpoolError::invalid_account_data(
                trader,
                "Expected a valid Phoenix Eternal Trader account",
                Some(error),
            )
        })?;
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
        "Set exact signed quote-lot collateral on a validated Phoenix Eternal Trader account."
            .to_string(),
    );
    scenario.tags = vec![
        "phoenix-eternal".to_string(),
        "collateral".to_string(),
        "risk".to_string(),
    ];
    scenario.add_override(collateral_override);

    Ok(PhoenixCollateralPreparation {
        scenario,
        trader,
        target_quote_lots,
    })
}

pub fn phoenix_perp_asset_map_address(global_account: &Account) -> SurfpoolResult<Pubkey> {
    if global_account.owner != PHOENIX_ETERNAL_PROGRAM_ID {
        return Err(SurfpoolError::invalid_account_owner(
            PHOENIX_GLOBAL_CONFIG,
            Some("expected Phoenix Eternal owner"),
        ));
    }
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
    Ok(Pubkey::new_from_array(global.perp_asset_map_key()))
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
    value
        .parse::<i64>()
        .map_err(|_| invalid_quote_lot_collateral())
}

fn invalid_quote_lot_collateral() -> SurfpoolError {
    SurfpoolError::internal(
        "quote_lot_collateral must be a signed 64-bit integer encoded as a string",
    )
}

fn validate_trader(data: &[u8]) -> Result<(), PhoenixAccountDecodeError> {
    let mut aligned_words = vec![0_u64; data.len().div_ceil(size_of::<u64>())];
    let aligned_bytes = bytemuck::cast_slice_mut::<u64, u8>(&mut aligned_words);
    aligned_bytes[..data.len()].copy_from_slice(data);
    Trader::try_from_account_bytes(&aligned_bytes[..data.len()])?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use base64::{Engine, prelude::BASE64_STANDARD};
    use phoenix_rise_accounts::{PhoenixAccount, trader::TraderHeader};
    use solana_account::Account;

    use super::*;

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
    fn patches_only_collateral_and_preserves_the_dynamic_tail() {
        let data = trader_fixture(6_996_825_500, 1, 2);
        let dynamic_tail = data[TRADER_HEADER_LEN..].to_vec();

        let patched = patch_trader_collateral(&PHOENIX_ETERNAL_PROGRAM_ID, &data, 371_499_999)
            .expect("valid collateral patch");

        let header = TraderHeader::try_read_from_account_bytes(&patched).expect("valid header");
        assert_eq!(
            header.trader_state.quote_lot_collateral.as_inner(),
            371_499_999
        );
        assert_eq!(patched.len(), data.len());
        assert_eq!(&patched[TRADER_HEADER_LEN..], dynamic_tail);
        assert!(
            data.iter()
                .zip(&patched)
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .all(|(offset, _)| COLLATERAL_BYTE_RANGE.contains(&offset))
        );
    }

    #[test]
    fn accepts_i64_collateral_boundaries() {
        let data = trader_fixture(0, 0, 0);

        for target in [i64::MIN, i64::MAX] {
            let patched =
                patch_trader_collateral(&PHOENIX_ETERNAL_PROGRAM_ID, &data, target).unwrap();
            let header = TraderHeader::try_read_from_account_bytes(&patched).unwrap();
            assert_eq!(header.trader_state.quote_lot_collateral.as_inner(), target);
        }
    }

    #[test]
    fn rejects_the_wrong_owner() {
        let error = patch_trader_collateral(&Pubkey::new_unique(), &trader_fixture(0, 0, 0), 1)
            .unwrap_err();

        assert!(matches!(
            error,
            PhoenixTraderPatchError::InvalidOwner { .. }
        ));
    }

    #[test]
    fn rejects_the_wrong_discriminant() {
        let mut data = trader_fixture(0, 0, 0);
        data[..8].fill(0);

        let error = patch_trader_collateral(&PHOENIX_ETERNAL_PROGRAM_ID, &data, 1).unwrap_err();

        assert!(matches!(
            error,
            PhoenixTraderPatchError::InvalidTrader(
                PhoenixAccountDecodeError::InvalidDiscriminant { .. }
            )
        ));
    }

    #[test]
    fn rejects_a_truncated_header() {
        let data = trader_fixture(0, 0, 0);

        let error = patch_trader_collateral(
            &PHOENIX_ETERNAL_PROGRAM_ID,
            &data[..TRADER_HEADER_LEN - 1],
            1,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PhoenixTraderPatchError::InvalidTrader(
                PhoenixAccountDecodeError::AccountTooSmall { .. }
            )
        ));
    }

    #[test]
    fn rejects_missing_allocated_position_capacity() {
        let data = trader_fixture(0, 1, 2);
        let truncated_len = data.len() - POSITION_ENTRY_LEN;

        let error = patch_trader_collateral(&PHOENIX_ETERNAL_PROGRAM_ID, &data[..truncated_len], 1)
            .unwrap_err();

        assert!(matches!(
            error,
            PhoenixTraderPatchError::InvalidTrader(
                PhoenixAccountDecodeError::AccountTooSmall { .. }
            )
        ));
    }

    #[test]
    fn rejects_position_length_above_capacity() {
        let data = trader_fixture(2, 2, 1);

        let error = patch_trader_collateral(&PHOENIX_ETERNAL_PROGRAM_ID, &data, 1).unwrap_err();

        assert!(matches!(
            error,
            PhoenixTraderPatchError::InvalidTrader(PhoenixAccountDecodeError::InvalidData {
                reason: "position map length exceeds capacity",
                ..
            })
        ));
    }

    #[test]
    fn override_accepts_only_the_collateral_field() {
        let values = HashMap::from([
            (COLLATERAL_FIELD.to_string(), serde_json::json!("371499999")),
            ("flags".to_string(), serde_json::json!("1")),
        ]);

        let error =
            forge_trader_collateral_override(&Pubkey::new_unique(), &trader_account(), &values)
                .unwrap_err();

        assert!(error.to_string().contains("accept only"));
    }

    #[test]
    fn override_rejects_json_numbers() {
        let values =
            HashMap::from([(COLLATERAL_FIELD.to_string(), serde_json::json!(371_499_999))]);

        let error =
            forge_trader_collateral_override(&Pubkey::new_unique(), &trader_account(), &values)
                .unwrap_err();

        assert!(error.to_string().contains("encoded as a string"));
    }

    #[test]
    fn override_rejects_out_of_range_strings() {
        let values = HashMap::from([(
            COLLATERAL_FIELD.to_string(),
            serde_json::json!("9223372036854775808"),
        )]);

        let error =
            forge_trader_collateral_override(&Pubkey::new_unique(), &trader_account(), &values)
                .unwrap_err();

        assert!(error.to_string().contains("signed 64-bit integer"));
    }

    #[test]
    fn override_parses_exact_string_values() {
        let values = HashMap::from([(
            COLLATERAL_FIELD.to_string(),
            serde_json::json!("-9007199254740993"),
        )]);

        let patched =
            forge_trader_collateral_override(&Pubkey::new_unique(), &trader_account(), &values)
                .unwrap();
        let header = TraderHeader::try_read_from_account_bytes(&patched).unwrap();

        assert_eq!(
            header.trader_state.quote_lot_collateral.as_inner(),
            -9_007_199_254_740_993
        );
    }

    #[test]
    fn builds_one_editable_collateral_override_for_the_requested_trader() {
        let trader = Pubkey::new_unique();
        let preparation =
            build_phoenix_collateral_scenario(trader, &trader_account(), "-9007199254740993")
                .unwrap();

        assert_eq!(preparation.trader, trader);
        assert_eq!(preparation.target_quote_lots, -9_007_199_254_740_993);
        assert_eq!(preparation.scenario.overrides.len(), 1);

        let collateral_override = &preparation.scenario.overrides[0];
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
    fn builder_rejects_an_invalid_target_before_creating_a_scenario() {
        let error = build_phoenix_collateral_scenario(
            Pubkey::new_unique(),
            &trader_account(),
            "9223372036854775808",
        )
        .unwrap_err();

        assert!(error.to_string().contains("signed 64-bit integer"));
    }

    #[test]
    fn builder_refuses_to_raise_collateral_above_its_vault_backing() {
        let trader = Pubkey::new_unique();
        let funded = Account {
            data: trader_fixture(500, 1, 2),
            ..trader_account()
        };

        let raised = build_phoenix_collateral_scenario(trader, &funded, "501").unwrap_err();
        assert!(raised.to_string().contains("can only lower collateral"));

        let lowered = build_phoenix_collateral_scenario(trader, &funded, "499").unwrap();
        assert_eq!(lowered.target_quote_lots, 499);
        let held = build_phoenix_collateral_scenario(trader, &funded, "500").unwrap();
        assert_eq!(held.target_quote_lots, 500);
    }

    #[test]
    fn builder_rejects_a_non_phoenix_account() {
        let trader = Pubkey::new_unique();
        let mut account = trader_account();
        account.owner = Pubkey::new_unique();

        let error = build_phoenix_collateral_scenario(trader, &account, "1").unwrap_err();

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
    fn patches_only_the_selected_mark_ticks_in_an_official_market_entry() {
        let data = perp_asset_map_fixture();
        let map = PerpAssetMap::try_from_account_bytes(&data).unwrap();
        let before = map.find_by_symbol("SOL").unwrap().unwrap();
        let metadata_offset = unique_subslice_offset(&data, before.metadata.as_bytes()).unwrap();

        let patched = patch_direct_mark(&PHOENIX_ETERNAL_PROGRAM_ID, &data, "SOL", 1).unwrap();
        let map = PerpAssetMap::try_from_account_bytes(&patched).unwrap();
        let after = map.find_by_symbol("SOL").unwrap().unwrap();

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
                    (metadata_offset + MARK_PRICE_TICKS_RANGE.start
                        ..metadata_offset + MARK_PRICE_TICKS_RANGE.end)
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
            let patched = patch_reference_prices(
                &PHOENIX_ETERNAL_PROGRAM_ID,
                &data,
                "SOL",
                spot_ticks,
                perp_ticks,
            )
            .unwrap();
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
                    .all(|value| value.ticks.as_inner() == spot_ticks)
            );
            assert!(
                price
                    .mark_price
                    .perp_price_component
                    .last_exchange_perp_price
                    .iter()
                    .all(|value| value.ticks.as_inner() == perp_ticks)
            );
            assert_eq!(patched.len(), data.len());
        }
    }

    #[test]
    fn reference_price_patch_refreshes_each_reference_slot() {
        let data = perp_asset_map_fixture();
        let patched = patch_reference_prices_at_slot(
            &PHOENIX_ETERNAL_PROGRAM_ID,
            &data,
            "SOL",
            8_000,
            7_000,
            123,
        )
        .unwrap();
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
    }

    #[test]
    fn reference_price_override_requires_exact_string_fields() {
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
        forge_reference_price_override(&Pubkey::new_unique(), &account, &values, 100).unwrap();

        let mut numeric = values;
        numeric.insert(
            REFERENCE_SPOT_TICKS_FIELD.to_string(),
            serde_json::json!(8000),
        );
        assert!(
            forge_reference_price_override(&Pubkey::new_unique(), &account, &numeric, 100)
                .unwrap_err()
                .to_string()
                .contains("encoded as a string")
        );
    }

    #[test]
    fn direct_mark_rejects_unknown_markets_and_out_of_range_ticks() {
        let data = perp_asset_map_fixture();

        assert!(matches!(
            patch_direct_mark(&PHOENIX_ETERNAL_PROGRAM_ID, &data, "BTC", 1),
            Err(PhoenixDirectMarkPatchError::MarketNotFound { .. })
        ));
        assert!(matches!(
            patch_direct_mark(
                &PHOENIX_ETERNAL_PROGRAM_ID,
                &data,
                "SOL",
                u64::from(u32::MAX) + 1,
            ),
            Err(PhoenixDirectMarkPatchError::InvalidTicks { .. })
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
            forge_direct_mark_override(&Pubkey::new_unique(), &account, &values, 123).unwrap();
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
            forge_direct_mark_override(&Pubkey::new_unique(), &account, &numeric_ticks, 123)
                .unwrap_err()
                .to_string()
                .contains("encoded as a string")
        );
    }

    #[test]
    fn forge_dispatches_collateral_values_to_the_trader_patcher() {
        let values =
            HashMap::from([(COLLATERAL_FIELD.to_string(), serde_json::json!("371499999"))]);

        let patched =
            forge_phoenix_override(&Pubkey::new_unique(), &trader_account(), &values, 100).unwrap();
        let header = TraderHeader::try_read_from_account_bytes(&patched).unwrap();

        assert_eq!(
            header.trader_state.quote_lot_collateral.as_inner(),
            371_499_999
        );
    }

    #[test]
    fn forge_dispatches_symbol_and_target_ticks_to_the_direct_mark_patcher() {
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
    }

    #[test]
    fn forge_dispatches_symbol_and_reference_ticks_to_the_reference_patcher() {
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
            (COLLATERAL_FIELD.to_string(), serde_json::json!("371499999")),
            (DIRECT_MARK_TICKS_FIELD.to_string(), serde_json::json!("1")),
        ]);

        let error = forge_phoenix_override(&Pubkey::new_unique(), &trader_account(), &values, 100)
            .unwrap_err();

        assert!(error.to_string().contains("exactly one value group"));
    }
}
