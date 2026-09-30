use core::mem::size_of;
use std::collections::HashMap;

use phoenix_rise_accounts::{
    PhoenixAccount, PhoenixAccountDecodeError,
    perp_asset_map::{PerpAssetMap, PerpAssetMetadata, PriceComponent},
};
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use super::collateral::{
    effective_collateral, index_trader_state_range, parse_quote_lot_collateral, trader_header,
    validate_hot_trader_fields,
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
    surfnet::{
        remote::SurfnetRemoteClient,
        svm::{AccountUpdatePolicy, SurfnetSvm},
    },
};

pub const PHOENIX_ETERNAL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih");
// Singletons that GlobalConfig points at; the live suite checks them against GlobalConfig.
pub const PHOENIX_PERP_ASSET_MAP: Pubkey =
    Pubkey::from_str_const("2nHGAaEw3D5dd4hVueaUNoygkQFmoeKqRQWnSPqSMFUC");
pub const PHOENIX_GLOBAL_TRADER_INDEX: Pubkey =
    Pubkey::from_str_const("HCrPXLByGqRh2szQi3gj7oRdRVBNi1gccAyn4CQCT3HK");

fn phoenix_account_kind(data: &[u8]) -> Option<PhoenixAccount> {
    PhoenixAccount::from_discriminant(data.get(..8)?.try_into().unwrap())
}

const COLLATERAL_FIELD: &str = "traderState.quoteLotCollateral";
const MARKET_SYMBOL_FIELD: &str = "symbol";
const DIRECT_MARK_TICKS_FIELD: &str = "target_ticks";
const MAINTENANCE_FACTOR_FIELD: &str = "maintenance_risk_factor_bps";
const PREPARATION_SLOT: u64 = 0;

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
    let invalid = |error: PhoenixAccountDecodeError| {
        SurfpoolError::invalid_account_data(
            perp_asset_map,
            "Expected a valid Phoenix Eternal PerpAssetMap account",
            Some(error),
        )
    };
    let map = PerpAssetMap::try_from_account_bytes(&account.data).map_err(invalid)?;
    let mut symbols = map
        .iter()
        .map(|entry| entry.map(|entry| entry.symbol.as_str().to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    symbols.sort_unstable();

    Ok(symbols)
}

fn price_patch_error(account_pubkey: &Pubkey, message: impl core::fmt::Display) -> SurfpoolError {
    SurfpoolError::invalid_account_data(
        account_pubkey,
        "Expected a valid Phoenix Eternal PerpAssetMap account",
        Some(message),
    )
}

fn checked_ticks(account_pubkey: &Pubkey, ticks: u64) -> SurfpoolResult<u64> {
    if ticks > u64::from(u32::MAX) {
        return Err(price_patch_error(
            account_pubkey,
            format!("price ticks {ticks} exceed the Phoenix u32 tick range"),
        ));
    }
    Ok(ticks)
}

fn patch_direct_mark(
    account_pubkey: &Pubkey,
    data: &[u8],
    symbol: &str,
    target_ticks: u64,
    mark_slot: u64,
) -> SurfpoolResult<Vec<u8>> {
    let target_ticks = checked_ticks(account_pubkey, target_ticks)?;
    patch_market_metadata(account_pubkey, data, symbol, |_, bytes| {
        let price_len = size_of::<PriceComponent>();
        let mut price = bytemuck::pod_read_unaligned::<PriceComponent>(&bytes[..price_len]);
        price.mark_price.price.slot = mark_slot;
        price.mark_price.price.ticks = bytemuck::cast(target_ticks);
        bytes[..price_len].copy_from_slice(bytemuck::bytes_of(&price));
    })
}

fn patch_maintenance_factor(
    account_pubkey: &Pubkey,
    data: &[u8],
    symbol: &str,
    factor: u16,
) -> SurfpoolResult<Vec<u8>> {
    patch_market_metadata(account_pubkey, data, symbol, |metadata, bytes| {
        // The metadata layout type is private to the crate, so the field offset comes from the view.
        let offset = metadata.risk_params().risk_factors.as_ptr() as usize
            - metadata.as_bytes().as_ptr() as usize;
        bytes[offset..offset + 2].copy_from_slice(&factor.to_le_bytes());
    })
}

fn patch_market_metadata(
    account_pubkey: &Pubkey,
    data: &[u8],
    symbol: &str,
    update: impl FnOnce(&PerpAssetMetadata, &mut [u8]),
) -> SurfpoolResult<Vec<u8>> {
    let decode_error = |error: PhoenixAccountDecodeError| {
        price_patch_error(
            account_pubkey,
            format!("invalid Phoenix PerpAssetMap account: {error}"),
        )
    };
    let map = PerpAssetMap::try_from_account_bytes(data).map_err(decode_error)?;
    let entry = map
        .find_by_symbol(symbol)
        .map_err(decode_error)?
        .ok_or_else(|| {
            price_patch_error(
                account_pubkey,
                format!("Phoenix market {symbol} was not found"),
            )
        })?;
    let metadata_bytes = entry.metadata.as_bytes();
    let metadata_offset = unique_subslice_offset(data, metadata_bytes).ok_or_else(|| {
        price_patch_error(
            account_pubkey,
            "selected Phoenix market metadata does not occur exactly once",
        )
    })?;
    let mut patched = data.to_vec();
    update(
        &entry.metadata,
        &mut patched[metadata_offset..metadata_offset + metadata_bytes.len()],
    );
    Ok(patched)
}

fn unique_subslice_offset(data: &[u8], needle: &[u8]) -> Option<usize> {
    let mut matches = data
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(offset, _)| offset);
    let offset = matches.next()?;
    matches.next().is_none().then_some(offset)
}

fn forge_phoenix_override(
    account_pubkey: &Pubkey,
    account: &Account,
    account_values: &HashMap<String, serde_json::Value>,
    materialization_slot: u64,
) -> SurfpoolResult<Vec<u8>> {
    // Only the codec's inputs are read: other keys, such as PerpAssetMap fields a client copied
    // from the decoded account, cannot be written through this codec.
    let symbol = || {
        account_values
            .get(MARKET_SYMBOL_FIELD)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SurfpoolError::internal("symbol must be a non-empty string"))
    };
    match (
        account_values.get(DIRECT_MARK_TICKS_FIELD),
        account_values.get(MAINTENANCE_FACTOR_FIELD),
    ) {
        (Some(ticks), None) => patch_direct_mark(
            account_pubkey,
            &account.data,
            symbol()?,
            parse_decimal(ticks, DIRECT_MARK_TICKS_FIELD, "an unsigned 64-bit integer")?,
            materialization_slot,
        ),
        (None, Some(factor)) => patch_maintenance_factor(
            account_pubkey,
            &account.data,
            symbol()?,
            parse_decimal::<core::num::NonZeroU16>(
                factor,
                MAINTENANCE_FACTOR_FIELD,
                "basis points from 1 to 65535",
            )?
            .get(),
        ),
        _ => Err(SurfpoolError::internal(
            "Phoenix map overrides take symbol plus exactly one of target_ticks or \
             maintenance_risk_factor_bps",
        )),
    }
}

/// The writes a Phoenix override needs, or `None` when the account takes the generic IDL path.
pub async fn prepare_phoenix_override(
    svm: &mut SurfnetSvm,
    account_pubkey: &Pubkey,
    account: &Account,
    values: &HashMap<String, serde_json::Value>,
    materialization_slot: u64,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
) -> SurfpoolResult<Option<Vec<(Pubkey, Account)>>> {
    if account.owner != PHOENIX_ETERNAL_PROGRAM_ID {
        return Ok(None);
    }
    match phoenix_account_kind(&account.data) {
        Some(PhoenixAccount::PerpAssetMap) => {
            let data =
                forge_phoenix_override(account_pubkey, account, values, materialization_slot)?;
            Ok(Some(vec![(
                *account_pubkey,
                Account {
                    data,
                    ..account.clone()
                },
            )]))
        }
        Some(PhoenixAccount::Trader) => {
            prepare_trader_override(svm, account_pubkey, account, values, remote_ctx)
                .await
                .map(Some)
        }
        _ => Ok(None),
    }
}

async fn prepare_trader_override(
    svm: &mut SurfnetSvm,
    trader: &Pubkey,
    account: &Account,
    values: &HashMap<String, serde_json::Value>,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let header = trader_header(trader, account)?;
    let hot = header.trader_state.is_hot();
    if hot {
        validate_hot_trader_fields(values)?;
    }
    let mut values = values.clone();
    if let Some(value) = values.get_mut(COLLATERAL_FIELD) {
        *value = serde_json::Value::from(parse_quote_lot_collateral(value)?);
    }
    let idl_versions = svm
        .registered_idls
        .get(&PHOENIX_ETERNAL_PROGRAM_ID.to_string())?
        .unwrap_or_default();
    let idl = &idl_versions
        .first()
        .ok_or_else(|| SurfpoolError::internal("No IDL registered for Phoenix Eternal"))?
        .1;
    let data = svm.get_forged_account_data(trader, &account.data, idl, &values)?;

    let mut writes = Vec::new();
    if hot && let Some(collateral) = values.get(COLLATERAL_FIELD) {
        let mut index = phoenix_dependency(svm, &PHOENIX_GLOBAL_TRADER_INDEX, remote_ctx).await?;
        let range = index_trader_state_range(&index, &header.key)?;
        let encoded = SurfnetSvm::get_forged_idl_type_data(
            &index.data[range.clone()],
            idl,
            "TraderState",
            &HashMap::from([("quoteLotCollateral".to_string(), collateral.clone())]),
        )?;
        index.data[range].copy_from_slice(&encoded);
        writes.push((PHOENIX_GLOBAL_TRADER_INDEX, index));
    }
    writes.push((
        *trader,
        Account {
            data,
            ..account.clone()
        },
    ));
    Ok(writes)
}

async fn phoenix_dependency(
    svm: &mut SurfnetSvm,
    address: &Pubkey,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
) -> SurfpoolResult<Account> {
    if let Some(account) = svm.inner.get_account(address)? {
        return Ok(account);
    }
    if svm.offline_accounts.contains_key(&address.to_string())?
        || svm
            .offline_accounts
            .get(&PHOENIX_ETERNAL_PROGRAM_ID.to_string())?
            .is_some_and(|config| config.include_owned_accounts)
    {
        return Err(SurfpoolError::internal(format!(
            "Phoenix dependency {address} is offline and missing locally"
        )));
    }
    let (client, commitment) = remote_ctx.as_ref().ok_or_else(|| {
        SurfpoolError::internal(format!("Phoenix dependency {address} is missing locally"))
    })?;
    let fetched = client.get_account(address, *commitment).await?;
    let account = fetched.clone().map_account()?;
    // Fill the fork gap once instead of refetching the same dependency per override, the way a
    // fork read does: the account is also indexed by owner, so getProgramAccounts serves the
    // local copy the override then patches.
    svm.apply_account_update(fetched, AccountUpdatePolicy::HydrateIfAbsent)?;
    Ok(account)
}

pub fn build_phoenix_collateral_scenario(
    trader: Pubkey,
    trader_account: &Account,
    target_quote_lots: &str,
    global_trader_index: Option<&Account>,
) -> SurfpoolResult<Scenario> {
    let target_quote_lots = parse_quote_lot_collateral(&serde_json::json!(target_quote_lots))?;
    let header = trader_header(&trader, trader_account)?;

    // Raising collateral needs a real deposit into the global vault.
    let current_quote_lots = effective_collateral(&header, global_trader_index)?;
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

fn parse_decimal<T: core::str::FromStr>(
    value: &serde_json::Value,
    field: &str,
    expected: &str,
) -> SurfpoolResult<T> {
    value
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            SurfpoolError::internal(format!("{field} must be {expected} encoded as a string"))
        })
}

#[cfg(test)]
mod tests {
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
        account.data[24..56].copy_from_slice(trader.as_ref());
        account
    }

    fn perp_asset_map_fixture() -> Vec<u8> {
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

    fn perp_asset_map_account() -> Account {
        Account {
            lamports: 1,
            data: perp_asset_map_fixture(),
            owner: PHOENIX_ETERNAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn builds_one_collateral_override_bounded_by_its_vault_backing() {
        let trader = Pubkey::new_unique();
        let funded = trader_account_for(trader, 500);
        let raised = build_phoenix_collateral_scenario(trader, &funded, "501", None).unwrap_err();
        assert!(raised.to_string().contains("can only lower collateral"));

        for target in ["500", "-9007199254740993"] {
            let preparation =
                build_phoenix_collateral_scenario(trader, &funded, target, None).unwrap();
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
                serde_json::json!(target)
            );
            assert_eq!(collateral_override.scenario_relative_slot, PREPARATION_SLOT);
            assert!(!collateral_override.fetch_before_use);
        }
    }

    #[test]
    fn direct_mark_patches_only_the_selected_mark_ticks_and_slot() {
        let account = perp_asset_map_account();
        let values = HashMap::from([
            (MARKET_SYMBOL_FIELD.to_string(), serde_json::json!("SOL")),
            (DIRECT_MARK_TICKS_FIELD.to_string(), serde_json::json!("1")),
        ]);

        let patched =
            forge_phoenix_override(&Pubkey::new_unique(), &account, &values, 123).unwrap();
        let after = PerpAssetMap::try_from_account_bytes(&patched)
            .unwrap()
            .find_by_symbol("SOL")
            .unwrap()
            .unwrap();
        let price = after.metadata.oracle_price().mark_price.price;
        assert_eq!((price.ticks.as_inner(), price.slot), (1, 123));
        assert_eq!(patched.len(), account.data.len());
    }

    #[test]
    fn maintenance_factor_patches_only_the_selected_market_factor() {
        let account = perp_asset_map_account();
        let factors = |data: &[u8]| {
            PerpAssetMap::try_from_account_bytes(data)
                .unwrap()
                .find_by_symbol("SOL")
                .unwrap()
                .unwrap()
                .metadata
                .risk_params()
                .risk_factors
        };
        let before = factors(&account.data);
        let values = HashMap::from([
            (MARKET_SYMBOL_FIELD.to_string(), serde_json::json!("SOL")),
            (
                MAINTENANCE_FACTOR_FIELD.to_string(),
                serde_json::json!("10000"),
            ),
        ]);

        let patched =
            forge_phoenix_override(&Pubkey::new_unique(), &account, &values, 123).unwrap();
        assert_eq!(factors(&patched), [10_000, before[1], before[2]]);
        let changed = patched
            .iter()
            .zip(&account.data)
            .filter(|(after, before)| after != before)
            .count();
        assert!(changed <= 2, "only the factor's two bytes may change");

        for rejected in ["0", "65536", "1.5"] {
            let values = HashMap::from([
                (MARKET_SYMBOL_FIELD.to_string(), serde_json::json!("SOL")),
                (
                    MAINTENANCE_FACTOR_FIELD.to_string(),
                    serde_json::json!(rejected),
                ),
            ]);
            assert!(forge_phoenix_override(&Pubkey::new_unique(), &account, &values, 123).is_err());
        }
    }

    // These templates' fields are codec inputs, not IDL paths, so the registry's IDL check
    // cannot cover them; this ties the YAML field names to what the codec accepts.
    #[test]
    fn the_codec_accepts_every_market_template_field_set() {
        let registry = TemplateRegistry::new();
        let market_templates: Vec<_> = registry
            .by_protocol("Phoenix Eternal")
            .into_iter()
            .filter(|template| template.account_type == "PerpAssetMap")
            .collect();
        assert_eq!(
            market_templates.len(),
            2,
            "direct mark and maintenance margin"
        );

        for template in market_templates {
            let values = template
                .properties
                .iter()
                .map(|property| {
                    let value = if property.path == MARKET_SYMBOL_FIELD {
                        "SOL"
                    } else {
                        "1"
                    };
                    (property.path.clone(), serde_json::json!(value))
                })
                .collect();
            forge_phoenix_override(&Pubkey::new_unique(), &perp_asset_map_account(), &values, 1)
                .unwrap_or_else(|error| panic!("{}: {error}", template.id));
        }
    }

    #[test]
    fn market_overrides_ignore_keys_outside_the_codec_inputs() {
        // An editor that starts from the decoded map sends its top-level scalars and arrays too.
        let account = perp_asset_map_account();
        for (field, value) in [
            (DIRECT_MARK_TICKS_FIELD, "1"),
            (MAINTENANCE_FACTOR_FIELD, "10000"),
        ] {
            let clean = HashMap::from([
                (MARKET_SYMBOL_FIELD.to_string(), serde_json::json!("SOL")),
                (field.to_string(), serde_json::json!(value)),
            ]);
            let mut editor = clean.clone();
            editor.insert("numAssets".to_string(), serde_json::json!(1));
            editor.insert(
                "padding0".to_string(),
                serde_json::json!([0, 0, 0, 0, 0, 0]),
            );
            assert_eq!(
                forge_phoenix_override(&Pubkey::new_unique(), &account, &editor, 123).unwrap(),
                forge_phoenix_override(&Pubkey::new_unique(), &account, &clean, 123).unwrap(),
            );
        }
    }

    #[test]
    fn market_overrides_need_a_symbol_and_exactly_one_codec_input() {
        let account = perp_asset_map_account();
        let symbol = (MARKET_SYMBOL_FIELD.to_string(), serde_json::json!("SOL"));
        let ticks = (DIRECT_MARK_TICKS_FIELD.to_string(), serde_json::json!("1"));
        let factor = (
            MAINTENANCE_FACTOR_FIELD.to_string(),
            serde_json::json!("10000"),
        );
        for values in [
            vec![symbol.clone()],
            vec![symbol.clone(), ticks.clone(), factor.clone()],
            vec![ticks.clone()],
            vec![factor.clone()],
            vec![
                (MARKET_SYMBOL_FIELD.to_string(), serde_json::json!(1)),
                ticks.clone(),
            ],
        ] {
            let values = HashMap::from_iter(values);
            assert!(
                forge_phoenix_override(&Pubkey::new_unique(), &account, &values, 123).is_err(),
                "{values:?}"
            );
        }
    }
}
