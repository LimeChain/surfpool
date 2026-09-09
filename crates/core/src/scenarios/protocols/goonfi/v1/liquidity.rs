//! GoonFi liquidity state preparation.
//!
//! A market draws liquidity from two SPL token vaults whose addresses live in the market account
//! at fixed offsets. Unlike price or depth, the balances are not in the protocol account itself but
//! in those separate token accounts, so this scales each vault through the generic
//! `spl-token-account-balance` template. Draining a vault to zero makes the deployed program reject
//! a swap with custom error 0x1; a fresh re-stamp keeps that rejection about liquidity and not a
//! stale quote.

use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, OverrideTemplate, Scenario};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

use super::{
    GoonfiMarket, market_label, validate_goonfi_market_layout, validate_goonfi_oracle_layout,
};

/// Read, never written, so no template declares them.
const BASE_MINT_OFFSET: usize = 80;
const QUOTE_MINT_OFFSET: usize = 112;
const BASE_VAULT_OFFSET: usize = 144;
const QUOTE_VAULT_OFFSET: usize = 176;
/// The SPL token account amount field.
const AMOUNT_OFFSET: usize = 64;

const LIQUIDITY_TEMPLATE: &str = "spl-token-account-balance";
const FRESHNESS_TEMPLATE: &str = "goonfi-freshness";

/// Both overrides apply on Play, before any slot advance.
const PREPARATION_SLOT: u64 = 0;

/// 10000 basis points leaves a vault untouched; 0 drains it.
const FULL_BPS: u16 = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct GoonfiLiquidityPreparation {
    pub scenario: Scenario,
    pub market: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_amount: u64,
    pub quote_amount: u64,
}

/// The two SPL token vaults a market draws liquidity from, read from the market's own pointers.
///
/// Validates the market first: the shared raw-layout guard has no owner predicate, so the owner
/// check in `validate_goonfi_market_layout` is what keeps these offsets pointed at a real market.
pub fn vault_addresses(market_account: &Account) -> SurfpoolResult<[Pubkey; 2]> {
    validate_goonfi_market_layout(market_account)?;
    let base = read_pubkey(&market_account.data, BASE_VAULT_OFFSET)?;
    let quote = read_pubkey(&market_account.data, QUOTE_VAULT_OFFSET)?;
    if base == Pubkey::default() || quote == Pubkey::default() || base == quote {
        return Err(invalid("market carries invalid vault pointers"));
    }
    Ok([base, quote])
}

/// Scales each vault balance to the requested basis points and keeps the quote fresh.
///
/// `market_account` is the source of truth for the vault and oracle addresses; the three passed
/// accounts are the base vault, quote vault and oracle the caller fetched by those addresses, in
/// that order. A side left at 10000 bps is untouched and gets no override.
pub fn build_goonfi_liquidity_scenario(
    market: Pubkey,
    market_account: &Account,
    base_vault_account: &Account,
    quote_vault_account: &Account,
    oracle_account: &Account,
    base_remaining_bps: u16,
    quote_remaining_bps: u16,
) -> SurfpoolResult<GoonfiLiquidityPreparation> {
    if [base_remaining_bps, quote_remaining_bps]
        .iter()
        .any(|bps| *bps > FULL_BPS)
    {
        return Err(invalid(
            "remaining liquidity must be 0..=10000 basis points; 0 drains a vault, 10000 leaves it unchanged",
        ));
    }
    if base_remaining_bps == FULL_BPS && quote_remaining_bps == FULL_BPS {
        return Err(invalid(
            "both vaults left unchanged; set a lower basis point value to drain at least one side",
        ));
    }

    let [base_vault, quote_vault] = vault_addresses(market_account)?;
    let oracle = GoonfiMarket::oracle_address(market_account)?;
    validate_goonfi_oracle_layout(oracle_account)?;

    let base_amount = vault_amount(base_vault_account)?;
    let quote_amount = vault_amount(quote_vault_account)?;

    let base_mint = read_pubkey(&market_account.data, BASE_MINT_OFFSET)?;
    let quote_mint = read_pubkey(&market_account.data, QUOTE_MINT_OFFSET)?;
    let label = market_label(&base_mint, &quote_mint);

    let registry = TemplateRegistry::new();
    let liquidity = template(&registry, LIQUIDITY_TEMPLATE)?;

    let mut scenario = Scenario::new(
        format!("GoonFi {label} liquidity drain"),
        format!(
            "Prepare GoonFi {label} market ({market}) vaults to {} of base and {} of quote liquidity; no swap is sent.",
            remaining_label(base_remaining_bps),
            remaining_label(quote_remaining_bps)
        ),
    );
    scenario.tags = vec![
        "goonfi".to_string(),
        "pmm".to_string(),
        "liquidity-drain".to_string(),
    ];

    for (side, vault, current, bps) in [
        ("base", base_vault, base_amount, base_remaining_bps),
        ("quote", quote_vault, quote_amount, quote_remaining_bps),
    ] {
        if bps == FULL_BPS {
            continue;
        }
        let scaled = (u128::from(current) * u128::from(bps) / u128::from(FULL_BPS)) as u64;
        scenario.add_override(
            OverrideInstance::new(
                liquidity.id.clone(),
                PREPARATION_SLOT,
                AccountAddress::Pubkey(vault.to_string()),
            )
            .with_values(HashMap::from([(
                "amount".to_string(),
                serde_json::json!(scaled.to_string()),
            )]))
            .with_label(format!("Drain GoonFi {side} vault")),
        );
    }

    // Null, not zero: the slot encoder reads a supplied number AS the lead, so only null keeps the
    // template's own lead of zero. Persisted so the quote stays inside the staleness window and the
    // swap the drained state is proven against is rejected for liquidity (0x1), not a stale quote.
    scenario.add_override(
        OverrideInstance::new(
            FRESHNESS_TEMPLATE.to_string(),
            PREPARATION_SLOT,
            AccountAddress::Pubkey(oracle.to_string()),
        )
        .with_values(HashMap::from([(
            "last_update_slot".to_string(),
            serde_json::Value::Null,
        )]))
        .with_label("Keep GoonFi quote fresh".to_string())
        .with_persist(true),
    );

    Ok(GoonfiLiquidityPreparation {
        scenario,
        market,
        base_vault,
        quote_vault,
        base_amount,
        quote_amount,
    })
}

/// The SPL token vaults are 32 undiscriminated-looking bytes at the front; the owner check is the
/// real discriminator that keeps a balance write out of a foreign account.
fn vault_amount(account: &Account) -> SurfpoolResult<u64> {
    if account.owner != spl_token_interface::ID && account.owner != spl_token_2022_interface::ID {
        return Err(invalid("vault is not owned by a supported token program"));
    }
    let bytes: [u8; 8] = account
        .data
        .get(AMOUNT_OFFSET..AMOUNT_OFFSET + 8)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| invalid("vault is too small to be an SPL token account"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn remaining_label(bps: u16) -> String {
    format!("{}.{:02}%", bps / 100, bps % 100)
}

fn read_pubkey(data: &[u8], offset: usize) -> SurfpoolResult<Pubkey> {
    let bytes: [u8; 32] = data
        .get(offset..offset + 32)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| invalid("market vault bytes are truncated"))?;
    Ok(Pubkey::new_from_array(bytes))
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
    use crate::scenarios::protocols::goonfi::v1::{GOONFI_ORACLE_PROGRAM_ID, GOONFI_PROGRAM_ID};

    const FIXTURE_ORACLE: Pubkey =
        Pubkey::from_str_const("7yecFG22heommABQ5svcbQLK1Ua4ZrJsHPiktZ17jfm3");
    const WSOL: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
    const USDC: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

    fn market_account(base_vault: &Pubkey, quote_vault: &Pubkey) -> Account {
        let mut data = vec![0u8; 2048];
        // Magic tag every live market shares.
        data[0..8].copy_from_slice(&[48, 188, 47, 53, 52, 88, 50, 154]);
        data[BASE_MINT_OFFSET..BASE_MINT_OFFSET + 32].copy_from_slice(WSOL.as_ref());
        data[QUOTE_MINT_OFFSET..QUOTE_MINT_OFFSET + 32].copy_from_slice(USDC.as_ref());
        data[BASE_VAULT_OFFSET..BASE_VAULT_OFFSET + 32].copy_from_slice(base_vault.as_ref());
        data[QUOTE_VAULT_OFFSET..QUOTE_VAULT_OFFSET + 32].copy_from_slice(quote_vault.as_ref());
        data[208..240].copy_from_slice(FIXTURE_ORACLE.as_ref());
        Account {
            data,
            owner: GOONFI_PROGRAM_ID,
            ..Account::default()
        }
    }

    fn vault(amount: u64) -> Account {
        let mut data = vec![0u8; 165];
        data[AMOUNT_OFFSET..AMOUNT_OFFSET + 8].copy_from_slice(&amount.to_le_bytes());
        Account {
            data,
            owner: spl_token_interface::ID,
            ..Account::default()
        }
    }

    fn oracle() -> Account {
        Account {
            data: vec![0u8; 32],
            owner: GOONFI_ORACLE_PROGRAM_ID,
            ..Account::default()
        }
    }

    #[test]
    fn drains_both_vaults_and_keeps_the_quote_fresh() {
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let market = Pubkey::new_unique();
        let preparation = build_goonfi_liquidity_scenario(
            market,
            &market_account(&base_vault, &quote_vault),
            &vault(2_441_078_070_812),
            &vault(216_136_231_615),
            &oracle(),
            0,
            0,
        )
        .unwrap();

        assert_eq!(preparation.base_vault, base_vault);
        assert_eq!(preparation.quote_vault, quote_vault);
        // A friendly pair label, not the raw market pubkey.
        assert_eq!(
            preparation.scenario.name,
            "GoonFi SOL/USDC liquidity drain"
        );
        let [base, quote, freshness] = &preparation.scenario.overrides[..] else {
            panic!("expected base drain, quote drain and freshness overrides");
        };
        assert_eq!(base.account, AccountAddress::Pubkey(base_vault.to_string()));
        assert_eq!(quote.account, AccountAddress::Pubkey(quote_vault.to_string()));
        assert_eq!(base.values.get("amount"), Some(&serde_json::json!("0")));
        assert_eq!(quote.values.get("amount"), Some(&serde_json::json!("0")));
        assert!(!base.fetch_before_use);
        assert!(!base.persist);
        assert_eq!(
            freshness.account,
            AccountAddress::Pubkey(FIXTURE_ORACLE.to_string())
        );
        assert!(freshness.persist);
        assert_eq!(
            freshness.values.get("last_update_slot"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn scales_partially_and_skips_an_unchanged_side() {
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let preparation = build_goonfi_liquidity_scenario(
            Pubkey::new_unique(),
            &market_account(&base_vault, &quote_vault),
            &vault(1_000),
            &vault(999),
            &oracle(),
            2_500,
            FULL_BPS,
        )
        .unwrap();

        let [base, freshness] = &preparation.scenario.overrides[..] else {
            panic!("the unchanged quote side must not get an override");
        };
        assert_eq!(base.account, AccountAddress::Pubkey(base_vault.to_string()));
        // 1000 * 2500 / 10000, exact integer arithmetic.
        assert_eq!(base.values.get("amount"), Some(&serde_json::json!("250")));
        assert_eq!(freshness.values.len(), 1);
    }

    #[test]
    fn rejects_bad_basis_points_and_accounts() {
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let good_market = market_account(&base_vault, &quote_vault);

        // Out of range and a no-op leave nothing to prepare.
        assert!(
            build_goonfi_liquidity_scenario(
                Pubkey::new_unique(),
                &good_market,
                &vault(1),
                &vault(1),
                &oracle(),
                10_001,
                0
            )
            .is_err()
        );
        assert!(
            build_goonfi_liquidity_scenario(
                Pubkey::new_unique(),
                &good_market,
                &vault(1),
                &vault(1),
                &oracle(),
                FULL_BPS,
                FULL_BPS
            )
            .is_err()
        );

        // A foreign account of the same size passes the raw guard, so the owner check must reject.
        let foreign_market = Account {
            owner: Pubkey::new_unique(),
            ..market_account(&base_vault, &quote_vault)
        };
        assert!(
            build_goonfi_liquidity_scenario(
                Pubkey::new_unique(),
                &foreign_market,
                &vault(1),
                &vault(1),
                &oracle(),
                0,
                0
            )
            .is_err()
        );

        // A vault not owned by a token program is not a real vault.
        let foreign_vault = Account {
            owner: Pubkey::new_unique(),
            ..vault(1)
        };
        assert!(
            build_goonfi_liquidity_scenario(
                Pubkey::new_unique(),
                &good_market,
                &foreign_vault,
                &vault(1),
                &oracle(),
                0,
                0
            )
            .is_err()
        );

        // A foreign oracle carries no magic, so its owner is the only discriminator.
        let foreign_oracle = Account {
            owner: Pubkey::new_unique(),
            ..oracle()
        };
        assert!(
            build_goonfi_liquidity_scenario(
                Pubkey::new_unique(),
                &good_market,
                &vault(1),
                &vault(1),
                &foreign_oracle,
                0,
                0
            )
            .is_err()
        );
    }

    #[test]
    fn resolves_vault_addresses_from_the_market() {
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let [base, quote] = vault_addresses(&market_account(&base_vault, &quote_vault)).unwrap();
        assert_eq!(base, base_vault);
        assert_eq!(quote, quote_vault);

        let mut zero_pointer = market_account(&base_vault, &quote_vault);
        zero_pointer.data[BASE_VAULT_OFFSET..BASE_VAULT_OFFSET + 32].fill(0);
        assert!(vault_addresses(&zero_pointer).is_err());
    }
}
