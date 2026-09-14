//! HumidiFi fair-value state preparation.
//!
//! HumidiFi publishes no IDL, and its quoted price and state fields are XOR-obfuscated: each word
//! is stored as `plaintext XOR key` with a fixed per-offset key. Every write goes through the raw layout in
//! `overrides.yaml`, whose properties carry those keys. This module exists for the one thing a
//! template cannot express: turning a human price into the raw ratio the program reads, which needs
//! both mints' decimals.

use std::{collections::HashMap, sync::LazyLock};

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{
    AccountAddress, OverrideInstance, OverrideTemplate, RawLayout, Scenario,
    verified_tokens::VERIFIED_TOKENS,
};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
    types::MintAccount,
};

pub const HUMIDIFI_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp");

/// The mints are read for their decimals, never written, so no template declares them.
const BASE_MINT_OFFSET: usize = 416;
const QUOTE_MINT_OFFSET: usize = 384;
const MAX_STALENESS_OFFSET: usize = 608;
const STATE_XOR_KEY: u64 = 0x6e9d_e2b3_0b19_f1ea;
pub(super) const SCHEMA_VERSION_OFFSET: usize = 1720;
const SCHEMA_VERSION_XOR_KEY: u64 = 0;
const ACTIVE_SCHEMA_VERSION: u64 = 8;

/// The four per-word XOR keys the program uses to obfuscate a 32-byte pubkey field. Global across
/// the supported markets. Used only to READ the mints for their decimals, never to
/// write, which is why they live here rather than as template masks.
pub(super) const PUBKEY_XOR_KEYS: [u64; 4] = [
    0xfb5c_e87a_ae44_3c38,
    0x04a2_1784_51ba_c3c7,
    0x04a1_1787_51b9_c3c6,
    0x04a0_1786_51b8_c3c5,
];

/// Fair value is quote atoms per base atom, scaled by 2^48.
const FAIR_VALUE_SCALE: u128 = 1u128 << 48;

const FAIR_VALUE_TEMPLATE: &str = "humidifi-fair-value";
pub(super) const FRESHNESS_TEMPLATE: &str = "humidifi-freshness";

/// Both overrides apply on Play, before any slot advance.
pub(super) const PREPARATION_SLOT: u64 = 0;

/// The size and layout tag a HumidiFi market must have, taken from the manifest the raw templates
/// are written against so there is one definition of them. Built once; the manifest is compiled in.
pub(super) static MARKET_LAYOUT: LazyLock<RawLayout> = LazyLock::new(|| {
    template(&TemplateRegistry::new(), FAIR_VALUE_TEMPLATE)
        .and_then(|template| {
            template.raw_layout.clone().ok_or_else(|| {
                SurfpoolError::internal("the HumidiFi manifest carries no raw layout")
            })
        })
        .expect("the HumidiFi manifest is compiled in and always parses")
});

/// The parts of a HumidiFi market a price needs: which mints it quotes, and at what scale.
#[derive(Clone, Debug, PartialEq)]
pub struct HumidiFiMarket {
    pub address: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
    pub base_decimals: u8,
    pub quote_decimals: u8,
    pub max_staleness_slots: u64,
}

impl HumidiFiMarket {
    pub fn mint_addresses(market_account: &Account) -> SurfpoolResult<(Pubkey, Pubkey)> {
        validate_humidifi_market_layout(market_account)?;
        let base_mint = read_masked_pubkey(&market_account.data, BASE_MINT_OFFSET)?;
        let quote_mint = read_masked_pubkey(&market_account.data, QUOTE_MINT_OFFSET)?;
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
            base_token_program: base_mint_account.owner,
            quote_token_program: quote_mint_account.owner,
            base_decimals,
            quote_decimals,
            max_staleness_slots: read_masked_u64(
                &market_account.data,
                MAX_STALENESS_OFFSET,
                STATE_XOR_KEY,
            )?,
        })
    }

    pub fn label(&self) -> String {
        let symbol = |mint: &Pubkey| {
            let address = mint.to_string();
            VERIFIED_TOKENS
                .iter()
                .filter(|token| token.address == address)
                .map(|token| token.symbol.as_str())
                .min()
                .map(str::to_string)
                .unwrap_or(address)
        };
        format!("{}/{}", symbol(&self.base_mint), symbol(&self.quote_mint))
    }
}

/// Rejects an account that is not a HumidiFi market.
///
/// The shared raw-layout guard has no owner predicate, so a foreign account of the same size
/// carrying the same masked layout tag would pass it. Every builder-made scenario comes through
/// here, which also gates the separate schema-version word.
pub fn validate_humidifi_market_layout(account: &Account) -> SurfpoolResult<()> {
    if account.owner != HUMIDIFI_PROGRAM_ID {
        return Err(invalid("market is not owned by HumidiFi"));
    }
    MARKET_LAYOUT.guard(&account.data).map_err(invalid)?;
    let version = read_masked_u64(&account.data, SCHEMA_VERSION_OFFSET, SCHEMA_VERSION_XOR_KEY)?;
    if version != ACTIVE_SCHEMA_VERSION {
        return Err(invalid(format!(
            "HumidiFi market schema version {version} is not supported; expected version 8"
        )));
    }
    Ok(())
}

pub(super) fn schema_version_bytes() -> [u8; 8] {
    (ACTIVE_SCHEMA_VERSION ^ SCHEMA_VERSION_XOR_KEY).to_le_bytes()
}

#[derive(Clone, Debug, PartialEq)]
pub struct HumidiFiFairValuePreparation {
    pub scenario: Scenario,
    pub market: Pubkey,
    pub fair_value: u64,
}

pub fn build_humidifi_fair_value_scenario(
    market: &HumidiFiMarket,
    price: &str,
) -> SurfpoolResult<HumidiFiFairValuePreparation> {
    let fair_value = human_price_to_fair_value(price, market.base_decimals, market.quote_decimals)?;

    let registry = TemplateRegistry::new();
    let fair_value_template = template(&registry, FAIR_VALUE_TEMPLATE)?;
    let freshness = template(&registry, FRESHNESS_TEMPLATE)?;
    let market_name = market.label();
    let target = AccountAddress::Pubkey(market.address.to_string());

    let mut price_override = OverrideInstance::new(
        fair_value_template.id.clone(),
        PREPARATION_SLOT,
        target.clone(),
    )
    .with_values(HashMap::from([(
        "fair_value".to_string(),
        serde_json::json!(fair_value.to_string()),
    )]))
    .with_label(format!("HumidiFi {market_name} fair value"));
    price_override.fetch_before_use = true;

    // Null, not zero: the slot encoder reads a supplied number as the lead, so only null takes the
    // template's own lead of zero. Persisted, so the prepared price stays inside the market's
    // freshness window however long the scenario is left running.
    let freshness_override = OverrideInstance::new(freshness.id.clone(), PREPARATION_SLOT, target)
        .with_values(HashMap::from([(
            "last_update_slot".to_string(),
            serde_json::Value::Null,
        )]))
        .with_label("Keep HumidiFi quote fresh".to_string())
        .with_persist(true);

    let normalized_price = price.trim();
    let mut scenario = Scenario::new(
        format!("HumidiFi {market_name} at {normalized_price}"),
        format!(
            "Prepare HumidiFi market {} to quote one base token at {normalized_price} quote tokens; no swap is sent.",
            market.address
        ),
    );
    scenario.tags = vec![
        "humidifi".to_string(),
        "pmm".to_string(),
        "price-dislocation".to_string(),
    ];
    scenario.add_override(price_override);
    scenario.add_override(freshness_override);

    Ok(HumidiFiFairValuePreparation {
        scenario,
        market: market.address,
        fair_value,
    })
}

/// Unmasks a 32-byte pubkey stored as four XOR-obfuscated words.
pub(super) fn read_masked_pubkey(data: &[u8], offset: usize) -> SurfpoolResult<Pubkey> {
    let end = offset
        .checked_add(32)
        .ok_or_else(|| invalid("market mint offset overflow"))?;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| invalid("market mint bytes are truncated"))?;
    let mut bytes = [0u8; 32];
    for (i, key) in PUBKEY_XOR_KEYS.iter().enumerate() {
        let word = u64::from_le_bytes(slice[i * 8..i * 8 + 8].try_into().unwrap());
        bytes[i * 8..i * 8 + 8].copy_from_slice(&(word ^ key).to_le_bytes());
    }
    Ok(Pubkey::new_from_array(bytes))
}

fn read_masked_u64(data: &[u8], offset: usize, key: u64) -> SurfpoolResult<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| invalid("market word offset overflow"))?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| invalid("market word bytes are truncated"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()) ^ key)
}

fn validate_mint_owner(account: &Account, side: &str) -> SurfpoolResult<()> {
    if account.owner != spl_token_interface::id() && account.owner != spl_token_2022_interface::id()
    {
        return Err(invalid(format!(
            "{side} mint is not owned by a supported token program"
        )));
    }
    Ok(())
}

/// `raw = floor(price * 2^48 * 10^(quote_decimals - base_decimals))`, computed on integers so a
/// long price never passes through f64.
fn human_price_to_fair_value(
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
    let scaled = digits
        .checked_mul(FAIR_VALUE_SCALE)
        .ok_or_else(|| invalid("price is too large"))?;
    let exponent = i32::from(quote_decimals)
        - i32::from(base_decimals)
        - i32::try_from(fractional.len()).map_err(|_| invalid("price is too precise"))?;
    let raw = if exponent >= 0 {
        scaled
            .checked_mul(checked_power_of_ten(exponent as u32)?)
            .ok_or_else(|| invalid("price is too large"))?
    } else {
        scaled / checked_power_of_ten(exponent.unsigned_abs())?
    };
    if raw == 0 {
        return Err(invalid(
            "price is too small for this market's mint decimals",
        ));
    }
    u64::try_from(raw).map_err(|_| invalid("price is too large for HumidiFi's fair-value field"))
}

fn checked_power_of_ten(exponent: u32) -> SurfpoolResult<u128> {
    10u128
        .checked_pow(exponent)
        .ok_or_else(|| invalid("price scale exceeds supported precision"))
}

pub(super) fn template<'a>(
    registry: &'a TemplateRegistry,
    id: &str,
) -> SurfpoolResult<&'a OverrideTemplate> {
    registry
        .get(id)
        .ok_or_else(|| SurfpoolError::internal(format!("HumidiFi template {id} is unavailable")))
}

pub(super) fn invalid(message: impl Into<String>) -> SurfpoolError {
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
            owner: spl_token_interface::id(),
            ..Account::default()
        }
    }

    fn write_masked_pubkey(data: &mut [u8], offset: usize, pubkey: &Pubkey) {
        let bytes = pubkey.to_bytes();
        for (i, key) in PUBKEY_XOR_KEYS.iter().enumerate() {
            let word = u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
            data[offset + i * 8..offset + i * 8 + 8].copy_from_slice(&(word ^ key).to_le_bytes());
        }
    }

    fn market_account(base_mint: &Pubkey, quote_mint: &Pubkey) -> Account {
        let mut data = vec![0; MARKET_LAYOUT.account_size];
        let magic = MARKET_LAYOUT.magic.as_ref().unwrap();
        data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        data[SCHEMA_VERSION_OFFSET..SCHEMA_VERSION_OFFSET + 8]
            .copy_from_slice(&schema_version_bytes());
        data[MAX_STALENESS_OFFSET..MAX_STALENESS_OFFSET + 8]
            .copy_from_slice(&(6u64 ^ STATE_XOR_KEY).to_le_bytes());
        write_masked_pubkey(&mut data, BASE_MINT_OFFSET, base_mint);
        write_masked_pubkey(&mut data, QUOTE_MINT_OFFSET, quote_mint);
        Account {
            data,
            owner: HUMIDIFI_PROGRAM_ID,
            ..Account::default()
        }
    }

    fn market(base_decimals: u8, quote_decimals: u8) -> HumidiFiMarket {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        HumidiFiMarket::validate(
            Pubkey::new_unique(),
            &market_account(&base_mint, &quote_mint),
            &mint_account(base_decimals),
            &mint_account(quote_decimals),
        )
        .expect("valid HumidiFi market")
    }

    #[test]
    fn reads_metadata_for_a_market_outside_the_token_catalog() {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let account = market_account(&base_mint, &quote_mint);
        assert_eq!(
            HumidiFiMarket::mint_addresses(&account).unwrap(),
            (base_mint, quote_mint)
        );
        let address = Pubkey::new_unique();
        let market =
            HumidiFiMarket::validate(address, &account, &mint_account(9), &mint_account(6))
                .unwrap();
        assert_eq!(market.address, address);
        assert_eq!(market.base_mint, base_mint);
        assert_eq!(market.quote_mint, quote_mint);
        assert_eq!(market.base_decimals, 9);
        assert_eq!(market.quote_decimals, 6);
        assert_eq!(market.max_staleness_slots, 6);
        assert_eq!(market.label(), format!("{base_mint}/{quote_mint}"));
    }

    #[test]
    fn labels_known_mints_when_tokens_share_a_symbol() {
        let mut market = market(6, 6);
        market.quote_mint = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        for base_mint in [
            "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN",
            "HaP8r3ksG76PhQLTqR8FYBeNiQpejcFbQmiHbg787Ut1",
        ] {
            market.base_mint = Pubkey::from_str_const(base_mint);
            assert_eq!(market.label(), "TRUMP/USDC", "mint {base_mint}");
        }
    }

    #[test]
    fn builds_fair_value_for_sol_usdc_decimals() {
        let market = market(9, 6);
        let preparation = build_humidifi_fair_value_scenario(&market, "208").unwrap();
        // 208 * 2^48 * 10^(6-9), floored.
        assert_eq!(preparation.fair_value, 58_546_795_155_816);
        assert_eq!(preparation.scenario.overrides.len(), 2);

        let [price, _] = &preparation.scenario.overrides[..] else {
            panic!("expected exactly a price and a freshness override");
        };
        let stored: u64 = price
            .values
            .get("fair_value")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert_eq!(stored, 58_546_795_155_816);
    }

    #[test]
    fn price_fetches_the_market_before_use_and_freshness_preserves_the_price() {
        let preparation = build_humidifi_fair_value_scenario(&market(9, 6), "100.25").unwrap();
        let [price, freshness] = &preparation.scenario.overrides[..] else {
            panic!("expected exactly a price and a freshness override");
        };
        assert!(price.fetch_before_use);
        assert!(!price.persist);
        assert!(!freshness.fetch_before_use);
        assert!(freshness.persist);
        assert_eq!(
            freshness.values.get("last_update_slot"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn derives_fair_value_from_market_mint_decimals() {
        // A d6/d6 pair: the decimals cancel, so raw is price * 2^48 directly.
        let market = market(6, 6);
        let preparation =
            build_humidifi_fair_value_scenario(&market, "0.4433").expect("build JUP/USDC price");
        assert_eq!(
            preparation.fair_value,
            (4433u128 * FAIR_VALUE_SCALE / 10_000) as u64
        );
    }

    #[test]
    fn rejects_invalid_price_and_market_inputs() {
        let market = market(9, 6);
        for price in ["0", "-1", "1.2.3", "not-a-price", ""] {
            assert!(build_humidifi_fair_value_scenario(&market, price).is_err());
        }

        let base_mint = mint_account(9);
        let quote_mint = mint_account(6);
        let wrong_owner = Account {
            owner: Pubkey::new_unique(),
            ..market_account(&Pubkey::new_unique(), &Pubkey::new_unique())
        };
        assert!(
            HumidiFiMarket::validate(Pubkey::new_unique(), &wrong_owner, &base_mint, &quote_mint)
                .is_err()
        );
        // The raw guard cannot see the owner, which is the whole reason this check sits on top.
        assert!(MARKET_LAYOUT.guard(&wrong_owner.data).is_ok());
        assert!(validate_humidifi_market_layout(&wrong_owner).is_err());

        let same_mint = Pubkey::new_unique();
        assert!(
            HumidiFiMarket::validate(
                Pubkey::new_unique(),
                &market_account(&same_mint, &same_mint),
                &base_mint,
                &quote_mint,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_version_5_market_the_guard_admits() {
        let mut account = market_account(&Pubkey::new_unique(), &Pubkey::new_unique());
        account.data[SCHEMA_VERSION_OFFSET..SCHEMA_VERSION_OFFSET + 8]
            .copy_from_slice(&(5u64 ^ SCHEMA_VERSION_XOR_KEY).to_le_bytes());
        assert!(MARKET_LAYOUT.guard(&account.data).is_ok());
        let error = validate_humidifi_market_layout(&account).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("schema version 5 is not supported")
        );
        assert!(HumidiFiMarket::mint_addresses(&account).is_err());
    }

    #[test]
    fn templates_require_an_explicit_market() {
        let registry = TemplateRegistry::new();
        for id in [
            FAIR_VALUE_TEMPLATE,
            FRESHNESS_TEMPLATE,
            "humidifi-stale-quote",
        ] {
            assert_eq!(
                template(&registry, id).unwrap().address,
                AccountAddress::Pubkey(String::new()),
                "{id}"
            );
        }
    }

    #[test]
    fn state_key_matches_the_freshness_template_mask() {
        let registry = TemplateRegistry::new();
        let freshness = registry.get(FRESHNESS_TEMPLATE).unwrap();
        assert_eq!(STATE_XOR_KEY, freshness.properties[0].xor_mask.unwrap());
    }
}
