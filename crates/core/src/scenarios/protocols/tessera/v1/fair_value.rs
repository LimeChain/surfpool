//! Tessera fair-value state preparation.
//!
//! Tessera publishes no IDL. Every write goes through the raw layout in `overrides.yaml`; this
//! module exists only for the one thing a template cannot express: turning a human price into the
//! pair of reciprocal atomic ratios the program reads, which needs both mints' decimals.

use std::{collections::HashMap, sync::LazyLock};

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, OverrideTemplate, RawLayout, Scenario};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
    types::MintAccount,
};

pub const TESSERA_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH");
pub const TESSERA_DEFAULT_MARKET: Pubkey =
    Pubkey::from_str_const("FLckHLGMJy5gEoXWwcE68Nprde1D4araK4TGLw4pQq2n");

/// The two mint offsets are read, never written, so no template declares them.
const BASE_MINT_OFFSET: usize = 24;
const QUOTE_MINT_OFFSET: usize = 56;

/// The size and layout tag a Tessera market must have, taken from the manifest the raw templates
/// are written against so there is one definition of them. Built once; the manifest is compiled in.
static MARKET_LAYOUT: LazyLock<RawLayout> = LazyLock::new(|| {
    template(&TemplateRegistry::new(), FAIR_VALUE_TEMPLATE)
        .and_then(|template| {
            template.raw_layout.clone().ok_or_else(|| {
                SurfpoolError::internal("the Tessera manifest carries no raw layout")
            })
        })
        .expect("the Tessera manifest is compiled in and always parses")
});

const FAIR_VALUE_TEMPLATE: &str = "tessera-fair-value";
const FRESHNESS_TEMPLATE: &str = "tessera-freshness";

/// Both ratio fields are integers scaled by 10^15, so their product is 10^30.
const ATOMIC_RATIO_SCALE: u128 = 1_000_000_000_000_000;
const RECIPROCAL_PRODUCT: u128 = ATOMIC_RATIO_SCALE * ATOMIC_RATIO_SCALE;

/// Both overrides apply on Play, before any slot advance.
const PREPARATION_SLOT: u64 = 0;

/// The parts of a Tessera market a price needs: which mints it quotes, and at what scale.
#[derive(Clone, Debug, PartialEq)]
pub struct TesseraMarket {
    pub address: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_decimals: u8,
    pub quote_decimals: u8,
}

impl TesseraMarket {
    pub fn mint_addresses(market_account: &Account) -> SurfpoolResult<(Pubkey, Pubkey)> {
        validate_tessera_market_layout(market_account)?;
        let base_mint = read_pubkey(&market_account.data, BASE_MINT_OFFSET)?;
        let quote_mint = read_pubkey(&market_account.data, QUOTE_MINT_OFFSET)?;
        if base_mint == Pubkey::default()
            || quote_mint == Pubkey::default()
            || base_mint == quote_mint
        {
            return Err(invalid("market has invalid mint identities"));
        }
        Ok((base_mint, quote_mint))
    }

    pub fn validate(
        address: Pubkey,
        market_account: &Account,
        base_mint_account: &Account,
        quote_mint_account: &Account,
    ) -> SurfpoolResult<Self> {
        let (base_mint, quote_mint) = Self::mint_addresses(market_account)?;

        validate_mint_owner(base_mint_account, "base")?;
        validate_mint_owner(quote_mint_account, "quote")?;
        let base_decimals = MintAccount::unpack(&base_mint_account.data)
            .map_err(|_| invalid("base mint account is invalid"))?
            .decimals();
        let quote_decimals = MintAccount::unpack(&quote_mint_account.data)
            .map_err(|_| invalid("quote mint account is invalid"))?
            .decimals();

        Ok(Self {
            address,
            base_mint,
            quote_mint,
            base_decimals,
            quote_decimals,
        })
    }
}

/// Rejects an account that is not a Tessera market.
///
/// The shared raw-layout guard has no owner predicate, so a foreign account of the same size
/// carrying the same layout tag would pass it. Every builder-made scenario comes through here,
/// which adds the ownership check the schema cannot express.
pub fn validate_tessera_market_layout(account: &Account) -> SurfpoolResult<()> {
    if account.owner != TESSERA_PROGRAM_ID {
        return Err(invalid("market is not owned by Tessera"));
    }
    MARKET_LAYOUT.guard(&account.data).map_err(invalid)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TesseraFairValuePreparation {
    pub scenario: Scenario,
    pub market: Pubkey,
    pub quote_atoms_per_base_atom_x1e15: u64,
    pub base_atoms_per_quote_atom_x1e15: u64,
}

pub fn build_tessera_fair_value_scenario(
    market: &TesseraMarket,
    price: &str,
) -> SurfpoolResult<TesseraFairValuePreparation> {
    let quote_atoms_per_base_atom_x1e15 =
        human_price_to_atomic_ratio(price, market.base_decimals, market.quote_decimals)?;
    let reciprocal = RECIPROCAL_PRODUCT / u128::from(quote_atoms_per_base_atom_x1e15);
    let base_atoms_per_quote_atom_x1e15 = u64::try_from(reciprocal)
        .map_err(|_| invalid("price is too small for Tessera's reciprocal u64 field"))?;

    let registry = TemplateRegistry::new();
    let fair_value = template(&registry, FAIR_VALUE_TEMPLATE)?;
    let freshness = template(&registry, FRESHNESS_TEMPLATE)?;
    let market_name = market_display_name(fair_value, &market.address);
    let target = AccountAddress::Pubkey(market.address.to_string());

    // No fetch_before_use: these values were derived from the market account this scenario was
    // built against, which creation already hydrated into local state. A Play-time refetch would
    // reinstall remote bytes over any local edit and apply numbers derived from a different read.
    let price_override =
        OverrideInstance::new(fair_value.id.clone(), PREPARATION_SLOT, target.clone())
            .with_values(HashMap::from([
                (
                    "quote_atoms_per_base_atom_x1e15".to_string(),
                    serde_json::json!(quote_atoms_per_base_atom_x1e15.to_string()),
                ),
                (
                    "base_atoms_per_quote_atom_x1e15".to_string(),
                    serde_json::json!(base_atoms_per_quote_atom_x1e15.to_string()),
                ),
            ]))
            .with_label(format!("Tessera {market_name} fair value"));

    // Null, not zero: the slot encoder reads a supplied number AS the lead, so only null takes the
    // template's own lead of zero. Persisted, so the prepared price stays inside the market's
    // freshness window however long the scenario is left running.
    let freshness_override = OverrideInstance::new(freshness.id.clone(), PREPARATION_SLOT, target)
        .with_values(HashMap::from([(
            "last_update_slot".to_string(),
            serde_json::Value::Null,
        )]))
        .with_label("Keep Tessera quote fresh".to_string())
        .with_persist(true);

    let normalized_price = price.trim();
    let mut scenario = Scenario::new(
        format!("Tessera {market_name} at {normalized_price}"),
        format!(
            "Prepare Tessera market {} to quote one base token at {normalized_price} quote tokens; no swap is sent.",
            market.address
        ),
    );
    scenario.tags = vec![
        "tessera".to_string(),
        "pmm".to_string(),
        "price-dislocation".to_string(),
    ];
    scenario.add_override(price_override);
    scenario.add_override(freshness_override);

    Ok(TesseraFairValuePreparation {
        scenario,
        market: market.address,
        quote_atoms_per_base_atom_x1e15,
        base_atoms_per_quote_atom_x1e15,
    })
}

fn read_pubkey(data: &[u8], offset: usize) -> SurfpoolResult<Pubkey> {
    let bytes: [u8; 32] = data[offset..offset + 32]
        .try_into()
        .map_err(|_| invalid("market mint bytes are truncated"))?;
    Ok(Pubkey::new_from_array(bytes))
}

fn validate_mint_owner(account: &Account, side: &str) -> SurfpoolResult<()> {
    if account.owner != spl_token_interface::ID && account.owner != spl_token_2022_interface::ID {
        return Err(invalid(format!(
            "{side} mint is not owned by a supported token program"
        )));
    }
    Ok(())
}

fn human_price_to_atomic_ratio(
    price: &str,
    base_decimals: u8,
    quote_decimals: u8,
) -> SurfpoolResult<u64> {
    let value = price.trim();
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fractional = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid("price must be a positive decimal string"));
    }

    let digits = format!("{whole}{fractional}")
        .parse::<u128>()
        .map_err(|_| invalid("price is too large"))?;
    let exponent = i32::from(quote_decimals) + 15
        - i32::from(base_decimals)
        - i32::try_from(fractional.len()).map_err(|_| invalid("price is too precise"))?;
    let scaled = if exponent >= 0 {
        digits
            .checked_mul(checked_power_of_ten(exponent as u32)?)
            .ok_or_else(|| invalid("price is too large"))?
    } else {
        digits / checked_power_of_ten(exponent.unsigned_abs())?
    };
    if scaled == 0 {
        return Err(invalid(
            "price is too small for this market's mint decimals",
        ));
    }
    u64::try_from(scaled).map_err(|_| {
        let scale = i32::from(quote_decimals) + 15 - i32::from(base_decimals);
        let max_price = checked_power_of_ten(scale.unsigned_abs())
            .map(|power| u128::from(u64::MAX) / power)
            .unwrap_or_default();
        invalid(format!(
            "price is too large for Tessera's u64 field; this market accepts at most about {max_price} quote per base"
        ))
    })
}

fn checked_power_of_ten(exponent: u32) -> SurfpoolResult<u128> {
    10u128
        .checked_pow(exponent)
        .ok_or_else(|| invalid("price scale exceeds supported precision"))
}

/// The catalog pair for a listed market ("SOL/USDC"), a shortened address for one it does not list.
fn market_display_name(template: &OverrideTemplate, market: &Pubkey) -> String {
    let address = market.to_string();
    template
        .constants
        .get("market")
        .and_then(|constant| {
            constant
                .options
                .iter()
                .find(|option| option.value == address)
        })
        .map(|option| option.label.clone())
        .unwrap_or_else(|| format!("{}…{}", &address[..4], &address[address.len() - 4..]))
}

fn template<'a>(registry: &'a TemplateRegistry, id: &str) -> SurfpoolResult<&'a OverrideTemplate> {
    registry
        .get(id)
        .ok_or_else(|| SurfpoolError::internal(format!("Tessera template {id} is unavailable")))
}

fn invalid(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::internal(message.into())
}

#[cfg(test)]
mod tests {
    use solana_program_pack::Pack;

    use super::*;

    fn mint_account(decimals: u8) -> Account {
        let mut data = vec![0; spl_token_interface::state::Mint::LEN];
        spl_token_interface::state::Mint {
            decimals,
            is_initialized: true,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        Account {
            data,
            owner: spl_token_interface::ID,
            ..Account::default()
        }
    }

    fn market_account(base_mint: &Pubkey, quote_mint: &Pubkey) -> Account {
        let mut data = vec![0; MARKET_LAYOUT.account_size];
        data[BASE_MINT_OFFSET..BASE_MINT_OFFSET + 32].copy_from_slice(base_mint.as_ref());
        data[QUOTE_MINT_OFFSET..QUOTE_MINT_OFFSET + 32].copy_from_slice(quote_mint.as_ref());
        let magic = MARKET_LAYOUT.magic.as_ref().expect("manifest layout tag");
        data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        Account {
            data,
            owner: TESSERA_PROGRAM_ID,
            ..Account::default()
        }
    }

    fn market(base_decimals: u8, quote_decimals: u8) -> TesseraMarket {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        TesseraMarket::validate(
            Pubkey::new_unique(),
            &market_account(&base_mint, &quote_mint),
            &mint_account(base_decimals),
            &mint_account(quote_decimals),
        )
        .expect("valid Tessera market")
    }

    #[test]
    fn builds_atomic_fair_value_for_wsol_usdc_decimals() {
        let market = market(9, 6);
        let preparation = build_tessera_fair_value_scenario(&market, "100.25").unwrap();
        assert_eq!(
            preparation.quote_atoms_per_base_atom_x1e15,
            100_250_000_000_000
        );
        assert_eq!(
            preparation.base_atoms_per_quote_atom_x1e15,
            (RECIPROCAL_PRODUCT / 100_250_000_000_000u128) as u64
        );
        assert_eq!(preparation.scenario.overrides.len(), 2);
        assert_eq!(
            preparation.scenario.overrides[0].account,
            AccountAddress::Pubkey(market.address.to_string())
        );
    }

    #[test]
    fn derives_price_scale_from_market_mint_decimals() {
        let market = market(8, 6);
        let preparation = build_tessera_fair_value_scenario(&market, "78.8477010015472512")
            .expect("build CBB/USDC price");
        assert_eq!(
            preparation.quote_atoms_per_base_atom_x1e15,
            788_477_010_015_472
        );
        assert_eq!(
            preparation.base_atoms_per_quote_atom_x1e15,
            (RECIPROCAL_PRODUCT / 788_477_010_015_472u128) as u64
        );
    }

    /// The price is computed from one read of the market; a Play-time refetch would apply it to a
    /// different one and overwrite local edits. The freshness value must stay null, because the
    /// slot encoder reads a supplied number as the lead rather than ignoring it.
    #[test]
    fn price_applies_to_the_read_it_came_from_and_freshness_keeps_the_template_lead() {
        let preparation = build_tessera_fair_value_scenario(&market(9, 6), "100.25").unwrap();
        let [price, freshness] = &preparation.scenario.overrides[..] else {
            panic!("expected exactly a price and a freshness override");
        };
        assert!(!price.fetch_before_use);
        assert!(!price.persist);
        assert!(!freshness.fetch_before_use);
        assert!(freshness.persist);
        assert_eq!(
            freshness.values.get("last_update_slot"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn rejects_invalid_price_and_market_inputs() {
        let market = market(9, 6);
        for price in ["0", "-1", "1.2.3", "not-a-price", ""] {
            assert!(build_tessera_fair_value_scenario(&market, price).is_err());
        }

        let base_mint = mint_account(9);
        let quote_mint = mint_account(6);
        let wrong_owner = Account {
            owner: Pubkey::new_unique(),
            ..market_account(&Pubkey::new_unique(), &Pubkey::new_unique())
        };
        assert!(
            TesseraMarket::validate(Pubkey::new_unique(), &wrong_owner, &base_mint, &quote_mint)
                .is_err()
        );
        // The raw guard cannot see the owner, which is the whole reason this check sits on top.
        assert!(MARKET_LAYOUT.guard(&wrong_owner.data).is_ok());

        let same_mint = Pubkey::new_unique();
        assert!(
            TesseraMarket::validate(
                Pubkey::new_unique(),
                &market_account(&same_mint, &same_mint),
                &base_mint,
                &quote_mint,
            )
            .is_err()
        );
    }
}
