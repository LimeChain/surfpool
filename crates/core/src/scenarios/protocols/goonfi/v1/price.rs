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
use surfpool_types::{AccountAddress, OverrideInstance, RawLayout, Scenario};

use super::{
    FRESHNESS_TEMPLATE, PREPARATION_SLOT, freshness_override, invalid, market_label,
    markets::market_references, read_pubkey, template, vault_addresses,
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
    types::TokenAccount,
};

pub const GOONFI_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE");
/// The companion publisher program that owns every market's price oracle.
pub const GOONFI_ORACLE_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("dijkbkCAKfFTCxQg3u1pg82gVU1jJGHBBRcteD11mBu");
pub const GOONFI_DEFAULT_MARKET: Pubkey =
    Pubkey::from_str_const("GMCJvYGf5Ex2ARiMquaBDqU6iKM8uiEQkB8jCnoNfHpC");

/// Read, never written, so no template declares it.
pub(super) const ORACLE_POINTER_OFFSET: usize = 208;

/// The layouts a GoonFi market and its oracle must have, taken from the manifests the raw
/// templates are written against so there is one definition of them. Built once; both manifests
/// are compiled in.
pub(super) static ORACLE_LAYOUT: LazyLock<RawLayout> = LazyLock::new(|| layout_of(PRICE_TEMPLATE));
pub(super) static MARKET_LAYOUT: LazyLock<RawLayout> =
    LazyLock::new(|| layout_of(REFERENCE_TEMPLATE));

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

/// Prices are the human pair price times 10^6, independent of mint decimals.
const PRICE_SCALE_DECIMALS: u32 = 6;

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
    base_mint: Pubkey,
    quote_mint: Pubkey,
}

impl GoonfiMarket {
    /// The oracle the market prices from, read from the market's own pointer. Never trust a
    /// caller-supplied oracle address: the oracle is 32 undiscriminated bytes, so the pointer
    /// plus the owner check below are what keep a write out of a foreign account.
    pub fn oracle_address(market_account: &Account) -> SurfpoolResult<Pubkey> {
        validate_goonfi_market_layout(market_account)?;
        let oracle = read_pubkey(&market_account.data, ORACLE_POINTER_OFFSET)
            .ok_or_else(|| invalid("market oracle bytes are truncated"))?;
        if oracle == Pubkey::default() {
            return Err(invalid("market carries no oracle pointer"));
        }
        Ok(oracle)
    }

    /// Account addresses must remain paired with the data returned by the account reader.
    /// These checks validate the account graph; they do not authenticate caller-supplied bytes.
    pub fn validate(
        address: Pubkey,
        market_account: &Account,
        base_vault_account: (Pubkey, &Account),
        oracle_account: (Pubkey, &Account),
    ) -> SurfpoolResult<Self> {
        let oracle = Self::oracle_address(market_account)?;
        let [base_vault, _] = vault_addresses(market_account)?;
        if base_vault_account.0 != base_vault {
            return Err(invalid(format!(
                "base vault address {} does not match the market's base vault {base_vault}",
                base_vault_account.0
            )));
        }
        if oracle_account.0 != oracle {
            return Err(invalid(format!(
                "oracle address {} does not match the market's oracle {oracle}",
                oracle_account.0
            )));
        }
        validate_market_authority(address, base_vault_account.1)?;
        validate_goonfi_oracle_layout(oracle_account.1)?;
        let [base_mint, quote_mint, _, _] = market_references(market_account)?;
        Ok(Self {
            address,
            oracle,
            base_mint,
            quote_mint,
        })
    }

    /// Read-only: the pair is fixed at validation so a caller can inspect it but not re-point it.
    pub fn address(&self) -> Pubkey {
        self.address
    }

    pub fn oracle(&self) -> Pubkey {
        self.oracle
    }

    pub fn label(&self) -> String {
        market_label(&self.base_mint, &self.quote_mint)
    }
}

fn validate_market_authority(address: Pubkey, base_vault_account: &Account) -> SurfpoolResult<()> {
    if base_vault_account.owner != spl_token_interface::ID
        && base_vault_account.owner != spl_token_2022_interface::ID
    {
        return Err(invalid(
            "base vault is not owned by a supported token program",
        ));
    }
    let vault = TokenAccount::unpack(&base_vault_account.data)
        .map_err(|error| invalid(format!("market base vault is not a token account: {error}")))?;
    if vault.owner() != address {
        return Err(invalid(format!(
            "market base vault is held by {}, not by the targeted market {address}",
            vault.owner()
        )));
    }
    Ok(())
}

/// Rejects an account that is not a GoonFi market.
///
/// The owner and byte guards use the same manifest as the materializer, so a builder cannot
/// accept an account that the template's owner predicate would later reject.
pub fn validate_goonfi_market_layout(account: &Account) -> SurfpoolResult<()> {
    MARKET_LAYOUT
        .guard_owner(&account.owner)
        .map_err(|_| invalid("market is not owned by GoonFi"))?;
    MARKET_LAYOUT.guard(&account.data).map_err(invalid)
}

/// Rejects an account that is not a GoonFi price oracle.
///
/// The oracle is 32 bytes with no magic at all, so its guard pins only the size; the owner check
/// here is the real discriminator.
pub fn validate_goonfi_oracle_layout(account: &Account) -> SurfpoolResult<()> {
    ORACLE_LAYOUT
        .guard_owner(&account.owner)
        .map_err(|_| invalid("oracle is not owned by the GoonFi publisher"))?;
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
    let market_name = market.label();
    let oracle_target = AccountAddress::Pubkey(market.oracle.to_string());

    // No fetch_before_use anywhere: the oracle and reference values are absolute targets for the
    // account graph creation read, and a Play-time refetch would reinstall remote bytes over any
    // local edit.
    let price_override =
        OverrideInstance::new(price_template.id.clone(), PREPARATION_SLOT, oracle_target)
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
    scenario.add_override(freshness_override(freshness.id.clone(), &market.oracle));

    Ok(GoonfiPricePreparation {
        scenario,
        market: market.address,
        oracle: market.oracle,
        price_x1e6,
    })
}

fn human_price_to_x1e6(price: &str) -> SurfpoolResult<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::protocols::goonfi::v1::fixtures::{
        self, FIXTURE_BASE_VAULT, FIXTURE_ORACLE, FIXTURE_QUOTE_VAULT, oracle_account,
    };

    #[test]
    fn template_owners_match_the_discovered_programs() {
        let registry = TemplateRegistry::new();
        for (id, owner) in [
            (REFERENCE_TEMPLATE, GOONFI_PROGRAM_ID),
            (PRICE_TEMPLATE, GOONFI_ORACLE_PROGRAM_ID),
            (FRESHNESS_TEMPLATE, GOONFI_ORACLE_PROGRAM_ID),
            ("goonfi-stale-quote", GOONFI_ORACLE_PROGRAM_ID),
        ] {
            let layout = registry.get(id).unwrap().raw_layout.as_ref().unwrap();
            assert_eq!(layout.owner, Some(owner.to_string()), "{id}");
        }
    }

    fn market_account(oracle: &Pubkey) -> Account {
        fixtures::market_account(
            [&Pubkey::new_unique(), &Pubkey::new_unique()],
            [&FIXTURE_BASE_VAULT, &FIXTURE_QUOTE_VAULT],
            oracle,
        )
    }

    /// A market's base vault: a token account whose authority is the market itself.
    fn vault_account(authority: &Pubkey) -> Account {
        fixtures::token_account(&Pubkey::new_unique(), authority, 0)
    }

    fn market() -> GoonfiMarket {
        let address = Pubkey::new_unique();
        GoonfiMarket::validate(
            address,
            &market_account(&FIXTURE_ORACLE),
            (FIXTURE_BASE_VAULT, &vault_account(&address)),
            (FIXTURE_ORACLE, &oracle_account()),
        )
        .expect("valid GoonFi market")
    }

    #[test]
    fn names_the_scenario_and_overrides_by_the_pair_label() {
        const WSOL: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        const USDC: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        let address = Pubkey::new_unique();
        let market = GoonfiMarket::validate(
            address,
            &fixtures::market_account(
                [&WSOL, &USDC],
                [&FIXTURE_BASE_VAULT, &FIXTURE_QUOTE_VAULT],
                &FIXTURE_ORACLE,
            ),
            (FIXTURE_BASE_VAULT, &vault_account(&address)),
            (FIXTURE_ORACLE, &oracle_account()),
        )
        .unwrap();

        let scenario = build_goonfi_price_scenario(&market, "150")
            .unwrap()
            .scenario;
        let labels: Vec<&str> = scenario
            .overrides
            .iter()
            .map(|instance| instance.label.as_deref().unwrap_or(""))
            .collect();

        assert_eq!(scenario.name, "GoonFi SOL/USDC at 150");
        assert_eq!(
            labels,
            [
                "GoonFi SOL/USDC price",
                "GoonFi SOL/USDC reference band",
                "Keep GoonFi quote fresh"
            ]
        );
        assert!(scenario.description.contains(&address.to_string()));
    }

    #[test]
    fn rejects_unrelated_base_vault_with_target_market_authority() {
        let target_market = Pubkey::new_unique();
        let mut other_market = market_account(&FIXTURE_ORACLE);
        other_market.data[144..176].copy_from_slice(Pubkey::new_unique().as_ref());
        other_market.data[176..208].copy_from_slice(Pubkey::new_unique().as_ref());
        let error = GoonfiMarket::validate(
            target_market,
            &other_market,
            (Pubkey::new_unique(), &vault_account(&target_market)),
            (FIXTURE_ORACLE, &oracle_account()),
        )
        .expect_err("an unrelated vault must not validate another market's account graph");
        assert!(error.to_string().contains("base vault address"), "{error}");
    }

    #[test]
    fn rejects_an_unrelated_publisher_owned_oracle() {
        let address = Pubkey::new_unique();
        let error = GoonfiMarket::validate(
            address,
            &market_account(&FIXTURE_ORACLE),
            (FIXTURE_BASE_VAULT, &vault_account(&address)),
            (Pubkey::new_unique(), &oracle_account()),
        )
        .expect_err("a correctly shaped oracle at another address must be refused");
        assert!(error.to_string().contains("oracle address"), "{error}");
    }

    #[test]
    fn rejects_a_base_vault_owned_by_an_unrelated_program() {
        let address = Pubkey::new_unique();
        let foreign_vault = Account {
            owner: Pubkey::new_unique(),
            ..vault_account(&address)
        };
        let error = GoonfiMarket::validate(
            address,
            &market_account(&FIXTURE_ORACLE),
            (FIXTURE_BASE_VAULT, &foreign_vault),
            (FIXTURE_ORACLE, &oracle_account()),
        )
        .expect_err("matching token bytes must not bypass the token program owner check");
        assert!(
            error.to_string().contains("supported token program"),
            "{error}"
        );
    }

    /// The market account does not carry its own address, so a caller could hand `validate` one
    /// market's address with another market's bytes. The base vault's authority is what catches it.
    #[test]
    fn rejects_an_address_that_does_not_own_the_market_vault() {
        let other_market = Pubkey::new_unique();
        assert!(
            GoonfiMarket::validate(
                Pubkey::new_unique(),
                &market_account(&FIXTURE_ORACLE),
                (FIXTURE_BASE_VAULT, &vault_account(&other_market)),
                (FIXTURE_ORACLE, &oracle_account()),
            )
            .is_err()
        );

        // A vault that is not a token account at all is refused before the comparison.
        assert!(validate_market_authority(Pubkey::new_unique(), &oracle_account()).is_err());
    }

    /// The values are absolute targets for the creation read, so nothing refetches at Play; the
    /// freshness value must stay null because the slot encoder reads a supplied number as the
    /// lead rather than ignoring it.
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
            base_mint: Pubkey::new_unique(),
            quote_mint: Pubkey::new_unique(),
        };
        let preparation = build_goonfi_price_scenario(&uncataloged, "1").unwrap();
        assert!(preparation.scenario.name.contains(&uncataloged.label()));
        assert!(
            preparation
                .scenario
                .description
                .contains(&uncataloged.address.to_string())
        );
        assert_eq!(preparation.oracle, uncataloged.oracle);

        let oracle = Pubkey::new_unique();
        let address = Pubkey::new_unique();
        let wrong_owner = Account {
            owner: Pubkey::new_unique(),
            ..market_account(&oracle)
        };
        // The raw guard cannot see the owner, which is the whole reason this check sits on top.
        assert!(MARKET_LAYOUT.guard(&wrong_owner.data).is_ok());
        let mut bad_magic = market_account(&oracle);
        bad_magic.data[0] ^= 0xff;
        // The oracle carries no magic at all, so the owner check is its only discriminator.
        let foreign_oracle = Account {
            owner: Pubkey::new_unique(),
            ..oracle_account()
        };
        assert!(ORACLE_LAYOUT.guard(&foreign_oracle.data).is_ok());
        for (label, market, oracle_fixture) in [
            (
                "market owned by another program",
                wrong_owner,
                oracle_account(),
            ),
            (
                "market with a flipped magic byte",
                bad_magic,
                oracle_account(),
            ),
            (
                "market without an oracle pointer",
                market_account(&Pubkey::default()),
                oracle_account(),
            ),
            (
                "oracle owned by another program",
                market_account(&oracle),
                foreign_oracle,
            ),
        ] {
            assert!(
                GoonfiMarket::validate(
                    address,
                    &market,
                    (FIXTURE_BASE_VAULT, &vault_account(&address)),
                    (oracle, &oracle_fixture),
                )
                .is_err(),
                "{label}"
            );
        }

        let truncated_oracle = Account {
            data: vec![0; 16],
            ..oracle_account()
        };
        assert!(validate_goonfi_oracle_layout(&truncated_oracle).is_err());
    }
}
