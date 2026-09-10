//! GoonFi price state preparation.
//!
//! GoonFi publishes no IDL. Every write goes through the raw layouts in `oracle_overrides.yaml`
//! and `market_overrides.yaml`; this module exists for what those templates cannot express: the
//! price lives in a per-market oracle account that must be resolved from the market's own pointer
//! and validated by owner, and a price move is one invariant across two accounts - oracle bid and
//! ask, the market's reference band, and a freshness re-stamp.

use std::{collections::HashMap, sync::LazyLock};

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, OverrideTemplate, RawLayout, Scenario};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

pub const GOONFI_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE");
/// The companion publisher program that owns every market's price oracle.
pub const GOONFI_ORACLE_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("dijkbkCAKfFTCxQg3u1pg82gVU1jJGHBBRcteD11mBu");
pub const GOONFI_DEFAULT_MARKET: Pubkey =
    Pubkey::from_str_const("GMCJvYGf5Ex2ARiMquaBDqU6iKM8uiEQkB8jCnoNfHpC");

/// Read, never written, so no template declares it.
const ORACLE_POINTER_OFFSET: usize = 208;

/// The layouts a GoonFi market and its oracle must have, taken from the manifests the raw
/// templates are written against so there is one definition of them. Built once; both manifests
/// are compiled in.
static ORACLE_LAYOUT: LazyLock<RawLayout> = LazyLock::new(|| layout_of(PRICE_TEMPLATE));
static MARKET_LAYOUT: LazyLock<RawLayout> = LazyLock::new(|| layout_of(REFERENCE_TEMPLATE));

fn layout_of(template_id: &str) -> RawLayout {
    template(&TemplateRegistry::new(), template_id)
        .and_then(|template| {
            template
                .raw_layout
                .clone()
                .ok_or_else(|| SurfpoolError::internal("the GoonFi manifests carry no raw layout"))
        })
        .expect("the GoonFi manifests are compiled in and always parse")
}

const PRICE_TEMPLATE: &str = "goonfi-price";
const REFERENCE_TEMPLATE: &str = "goonfi-reference-band";
const FRESHNESS_TEMPLATE: &str = "goonfi-freshness";

/// Prices are the human pair price times 10^6, independent of mint decimals.
const PRICE_SCALE_DECIMALS: u32 = 6;

/// All three overrides apply on Play, before any slot advance.
const PREPARATION_SLOT: u64 = 0;

/// The parts of a GoonFi market a price move needs: the market account itself and the oracle it
/// points at.
///
/// The two are private so the pair can only be built through `validate`, which reads the oracle
/// from the market's own pointer. Public fields would let a caller assemble the pair from scratch
/// or re-point a validated one, aiming a price move at one market's reference band and an
/// unrelated market's oracle - a combination the deployed program rejects with 0x24 at best, and
/// silently misprices at worst.
#[derive(Clone, Debug, PartialEq)]
pub struct GoonfiMarket {
    address: Pubkey,
    oracle: Pubkey,
}

impl GoonfiMarket {
    /// The oracle the market prices from, read from the market's own pointer. Never trust a
    /// caller-supplied oracle address: the oracle is 32 undiscriminated bytes, so the pointer
    /// plus the owner check below are what keep a write out of a foreign account.
    pub fn oracle_address(market_account: &Account) -> SurfpoolResult<Pubkey> {
        validate_goonfi_market_layout(market_account)?;
        let oracle = read_pubkey(&market_account.data, ORACLE_POINTER_OFFSET)?;
        if oracle == Pubkey::default() {
            return Err(invalid("market carries no oracle pointer"));
        }
        Ok(oracle)
    }

    pub fn validate(
        address: Pubkey,
        market_account: &Account,
        oracle_account: &Account,
    ) -> SurfpoolResult<Self> {
        let oracle = Self::oracle_address(market_account)?;
        validate_goonfi_oracle_layout(oracle_account)?;
        Ok(Self { address, oracle })
    }

    /// Read-only: the pair is fixed at validation so a caller can inspect it but not re-point it.
    pub fn address(&self) -> Pubkey {
        self.address
    }

    pub fn oracle(&self) -> Pubkey {
        self.oracle
    }
}

/// Rejects an account that is not a GoonFi market.
///
/// The shared raw-layout guard has no owner predicate, so a foreign account of the same size
/// carrying the same magic would pass it. Every builder-made scenario comes through here, which
/// adds the ownership check the schema cannot express.
pub fn validate_goonfi_market_layout(account: &Account) -> SurfpoolResult<()> {
    if account.owner != GOONFI_PROGRAM_ID {
        return Err(invalid("market is not owned by GoonFi"));
    }
    MARKET_LAYOUT.guard(&account.data).map_err(invalid)
}

/// Rejects an account that is not a GoonFi price oracle.
///
/// The oracle is 32 bytes with no magic at all, so its guard pins only the size; the owner check
/// here is the real discriminator.
pub fn validate_goonfi_oracle_layout(account: &Account) -> SurfpoolResult<()> {
    if account.owner != GOONFI_ORACLE_PROGRAM_ID {
        return Err(invalid("oracle is not owned by the GoonFi publisher"));
    }
    ORACLE_LAYOUT.guard(&account.data).map_err(invalid)
}

#[derive(Clone, Debug, PartialEq)]
pub struct GoonfiPricePreparation {
    pub scenario: Scenario,
    pub market: Pubkey,
    pub oracle: Pubkey,
    pub price_x1e6: u64,
}

pub fn build_goonfi_price_scenario(
    market: &GoonfiMarket,
    price: &str,
) -> SurfpoolResult<GoonfiPricePreparation> {
    let price_x1e6 = human_price_to_x1e6(price)?;
    let scaled = price_x1e6.to_string();

    let registry = TemplateRegistry::new();
    let price_template = template(&registry, PRICE_TEMPLATE)?;
    let reference = template(&registry, REFERENCE_TEMPLATE)?;
    let freshness = template(&registry, FRESHNESS_TEMPLATE)?;
    let market_name = market.address.to_string();
    let oracle_target = AccountAddress::Pubkey(market.oracle.to_string());

    // No fetch_before_use anywhere: the oracle and reference values are absolute targets for the
    // account graph creation read, and a Play-time refetch would reinstall remote bytes over any
    // local edit.
    let price_override = OverrideInstance::new(
        price_template.id.clone(),
        PREPARATION_SLOT,
        oracle_target.clone(),
    )
    .with_values(HashMap::from([
        (
            "bid_price_x1e6".to_string(),
            serde_json::json!(scaled.clone()),
        ),
        (
            "ask_price_x1e6".to_string(),
            serde_json::json!(scaled.clone()),
        ),
    ]))
    .with_label(format!("GoonFi {market_name} price"));

    // The deployed program rejects an oracle price outside the market's reference band with
    // custom error 0x24, so the band moves to the same target as one invariant.
    let reference_override = OverrideInstance::new(
        reference.id.clone(),
        PREPARATION_SLOT,
        AccountAddress::Pubkey(market.address.to_string()),
    )
    .with_values(HashMap::from([
        (
            "reference_price_a_x1e6".to_string(),
            serde_json::json!(scaled.clone()),
        ),
        (
            "reference_price_b_x1e6".to_string(),
            serde_json::json!(scaled),
        ),
    ]))
    .with_label(format!("GoonFi {market_name} reference band"));

    // Null, not zero: the slot encoder reads a supplied number AS the lead, so only null takes
    // the template's own lead of zero. Persisted, so the prepared price stays inside the oracle's
    // staleness window however long the scenario is left running.
    let freshness_override =
        OverrideInstance::new(freshness.id.clone(), PREPARATION_SLOT, oracle_target)
            .with_values(HashMap::from([(
                "last_update_slot".to_string(),
                serde_json::Value::Null,
            )]))
            .with_label("Keep GoonFi quote fresh".to_string())
            .with_persist(true);

    let normalized_price = price.trim();
    let mut scenario = Scenario::new(
        format!("GoonFi {market_name} at {normalized_price}"),
        format!(
            "Prepare GoonFi market {} to quote one base token at {normalized_price} quote tokens; no swap is sent.",
            market.address
        ),
    );
    scenario.tags = vec![
        "goonfi".to_string(),
        "pmm".to_string(),
        "price-dislocation".to_string(),
    ];
    scenario.add_override(price_override);
    scenario.add_override(reference_override);
    scenario.add_override(freshness_override);

    Ok(GoonfiPricePreparation {
        scenario,
        market: market.address,
        oracle: market.oracle,
        price_x1e6,
    })
}

fn read_pubkey(data: &[u8], offset: usize) -> SurfpoolResult<Pubkey> {
    let bytes: [u8; 32] = data[offset..offset + 32]
        .try_into()
        .map_err(|_| invalid("market oracle bytes are truncated"))?;
    Ok(Pubkey::new_from_array(bytes))
}

pub(super) fn human_price_to_x1e6(price: &str) -> SurfpoolResult<u64> {
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

    // Reject rather than truncate: a seventh decimal place cannot be represented, and silently
    // dropping it would prepare a different price than the caller asked for.
    if fractional.len() > PRICE_SCALE_DECIMALS as usize {
        return Err(invalid(format!(
            "price carries more than {PRICE_SCALE_DECIMALS} decimal places, past GoonFi's 10^-6 resolution"
        )));
    }
    let digits = format!("{whole}{fractional}")
        .parse::<u128>()
        .map_err(|_| invalid("price is too large"))?;
    let exponent = PRICE_SCALE_DECIMALS - fractional.len() as u32;
    let scaled = 10u128
        .checked_pow(exponent)
        .and_then(|power| digits.checked_mul(power))
        .ok_or_else(|| invalid("price is too large"))?;
    if scaled == 0 {
        return Err(invalid("price must be greater than zero"));
    }
    u64::try_from(scaled).map_err(|_| {
        let max_price = u64::MAX / 10u64.pow(PRICE_SCALE_DECIMALS);
        invalid(format!(
            "price is too large for GoonFi's u64 field; a market accepts at most about {max_price} quote per base"
        ))
    })
}

fn template<'a>(registry: &'a TemplateRegistry, id: &str) -> SurfpoolResult<&'a OverrideTemplate> {
    registry
        .get(id)
        .ok_or_else(|| SurfpoolError::internal(format!("GoonFi template {id} is unavailable")))
}

fn invalid(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::internal(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market_account(oracle: &Pubkey) -> Account {
        let mut data = vec![0; MARKET_LAYOUT.account_size];
        let magic = MARKET_LAYOUT.magic.as_ref().expect("manifest layout tag");
        data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        data[ORACLE_POINTER_OFFSET..ORACLE_POINTER_OFFSET + 32].copy_from_slice(oracle.as_ref());
        Account {
            data,
            owner: GOONFI_PROGRAM_ID,
            ..Account::default()
        }
    }

    fn oracle_account() -> Account {
        Account {
            data: vec![0; ORACLE_LAYOUT.account_size],
            owner: GOONFI_ORACLE_PROGRAM_ID,
            ..Account::default()
        }
    }

    const FIXTURE_ORACLE: Pubkey =
        Pubkey::from_str_const("7yecFG22heommABQ5svcbQLK1Ua4ZrJsHPiktZ17jfm3");

    fn market() -> GoonfiMarket {
        GoonfiMarket::validate(
            Pubkey::new_unique(),
            &market_account(&FIXTURE_ORACLE),
            &oracle_account(),
        )
        .expect("valid GoonFi market")
    }

    #[test]
    fn builds_price_scenario_across_both_accounts() {
        let market = market();
        let preparation = build_goonfi_price_scenario(&market, "99.74").unwrap();
        assert_eq!(preparation.price_x1e6, 99_740_000);

        let [price, reference, freshness] = &preparation.scenario.overrides[..] else {
            panic!("expected price, reference-band and freshness overrides");
        };
        assert_eq!(
            price.account,
            AccountAddress::Pubkey(market.oracle.to_string())
        );
        assert_eq!(
            reference.account,
            AccountAddress::Pubkey(market.address.to_string())
        );
        assert_eq!(
            freshness.account,
            AccountAddress::Pubkey(market.oracle.to_string())
        );
        assert_eq!(
            price.values.get("bid_price_x1e6"),
            Some(&serde_json::json!("99740000"))
        );
        assert_eq!(
            reference.values.get("reference_price_b_x1e6"),
            Some(&serde_json::json!("99740000"))
        );
    }

    /// The values are absolute targets for the creation read, so nothing refetches at Play; the
    /// freshness value must stay null because the slot encoder reads a supplied number as the
    /// lead rather than ignoring it.
    #[test]
    fn price_stays_on_the_creation_read_and_freshness_keeps_the_template_lead() {
        let preparation = build_goonfi_price_scenario(&market(), "1").unwrap();
        let [price, reference, freshness] = &preparation.scenario.overrides[..] else {
            panic!("expected exactly three overrides");
        };
        assert!(!price.fetch_before_use);
        assert!(!price.persist);
        assert!(!reference.fetch_before_use);
        assert!(!reference.persist);
        assert!(!freshness.fetch_before_use);
        assert!(freshness.persist);
        assert_eq!(
            freshness.values.get("last_update_slot"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn scales_prices_by_ten_to_the_sixth_regardless_of_decimals() {
        for (price, expected) in [
            ("77526.523154", 77_526_523_154u64),
            ("0.00841", 8_410),
            ("1558.9384", 1_558_938_400),
        ] {
            let preparation = build_goonfi_price_scenario(&market(), price).unwrap();
            assert_eq!(preparation.price_x1e6, expected, "price {price}");
        }
    }

    #[test]
    fn rejects_invalid_price_and_account_inputs() {
        let market = market();
        for price in [
            "0",
            "-1",
            "1.2.3",
            "not-a-price",
            "",
            "0.0000001",
            "1.0000009",
        ] {
            assert!(
                build_goonfi_price_scenario(&market, price).is_err(),
                "price {price} must be refused"
            );
        }

        // A pathological fraction must come back as an error, never a panic or a wrapped value.
        let poison = format!("0.{}1", "0".repeat(133));
        assert!(build_goonfi_price_scenario(&market, &poison).is_err());
        let long_whole = "9".repeat(60);
        assert!(build_goonfi_price_scenario(&market, &long_whole).is_err());

        let uncataloged = GoonfiMarket {
            address: Pubkey::new_unique(),
            oracle: Pubkey::new_unique(),
        };
        let preparation = build_goonfi_price_scenario(&uncataloged, "1").unwrap();
        assert!(
            preparation
                .scenario
                .name
                .contains(&uncataloged.address.to_string())
        );
        assert_eq!(preparation.oracle, uncataloged.oracle);

        let oracle = Pubkey::new_unique();
        let wrong_owner = Account {
            owner: Pubkey::new_unique(),
            ..market_account(&oracle)
        };
        assert!(
            GoonfiMarket::validate(Pubkey::new_unique(), &wrong_owner, &oracle_account()).is_err()
        );
        // The raw guard cannot see the owner, which is the whole reason this check sits on top.
        assert!(MARKET_LAYOUT.guard(&wrong_owner.data).is_ok());

        let mut bad_magic = market_account(&oracle);
        bad_magic.data[0] ^= 0xff;
        assert!(
            GoonfiMarket::validate(Pubkey::new_unique(), &bad_magic, &oracle_account()).is_err()
        );

        let no_pointer = market_account(&Pubkey::default());
        assert!(
            GoonfiMarket::validate(Pubkey::new_unique(), &no_pointer, &oracle_account()).is_err()
        );

        // The oracle carries no magic at all, so the owner check is its only discriminator.
        let foreign_oracle = Account {
            owner: Pubkey::new_unique(),
            ..oracle_account()
        };
        assert!(
            GoonfiMarket::validate(
                Pubkey::new_unique(),
                &market_account(&oracle),
                &foreign_oracle
            )
            .is_err()
        );
        assert!(ORACLE_LAYOUT.guard(&foreign_oracle.data).is_ok());

        let truncated_oracle = Account {
            data: vec![0; 16],
            ..oracle_account()
        };
        assert!(validate_goonfi_oracle_layout(&truncated_oracle).is_err());
    }
}
