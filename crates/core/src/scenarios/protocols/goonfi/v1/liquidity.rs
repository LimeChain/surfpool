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
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use super::{
    FRESHNESS_TEMPLATE, GoonfiMarket, PREPARATION_SLOT, freshness_override, invalid, market_label,
    read_pubkey, template, validate_goonfi_market_layout, validate_goonfi_oracle_layout,
};
use crate::{error::SurfpoolResult, scenarios::TemplateRegistry, types::TokenAccount};

/// Read, never written, so no template declares them.
pub(super) const BASE_MINT_OFFSET: usize = 80;
pub(super) const QUOTE_MINT_OFFSET: usize = 112;
pub(super) const BASE_VAULT_OFFSET: usize = 144;
pub(super) const QUOTE_VAULT_OFFSET: usize = 176;

const LIQUIDITY_TEMPLATE: &str = "spl-token-account-balance";

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
/// Validates the market's owner and byte layout before reading its pointers.
pub fn vault_addresses(market_account: &Account) -> SurfpoolResult<[Pubkey; 2]> {
    validate_goonfi_market_layout(market_account)?;
    let base = market_pointer(&market_account.data, BASE_VAULT_OFFSET)?;
    let quote = market_pointer(&market_account.data, QUOTE_VAULT_OFFSET)?;
    if base == Pubkey::default() || quote == Pubkey::default() || base == quote {
        return Err(invalid("market carries invalid vault pointers"));
    }
    Ok([base, quote])
}

/// Scales each vault balance to the requested basis points and keeps the quote fresh.
///
/// `market_account` is the source of truth for the vault and oracle addresses. Callers must keep
/// each account paired with the address used to read it. A side left at 10000 bps is untouched.
pub fn build_goonfi_liquidity_scenario(
    market: Pubkey,
    market_account: &Account,
    base_vault_account: (Pubkey, &Account),
    quote_vault_account: (Pubkey, &Account),
    oracle_account: (Pubkey, &Account),
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
    if oracle_account.0 != oracle {
        return Err(invalid(format!(
            "oracle address {} does not match the market's oracle {oracle}",
            oracle_account.0
        )));
    }
    validate_goonfi_oracle_layout(oracle_account.1)?;

    let base_mint = market_pointer(&market_account.data, BASE_MINT_OFFSET)?;
    let quote_mint = market_pointer(&market_account.data, QUOTE_MINT_OFFSET)?;
    let base_amount = vault_amount(base_vault_account, base_vault, "base", &base_mint, market)?;
    let quote_amount = vault_amount(
        quote_vault_account,
        quote_vault,
        "quote",
        &quote_mint,
        market,
    )?;
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

    // Persisted so the swap the drained state is proven against is rejected for liquidity (0x1),
    // not a stale quote.
    scenario.add_override(freshness_override(FRESHNESS_TEMPLATE.to_string(), &oracle));

    Ok(GoonfiLiquidityPreparation {
        scenario,
        market,
        base_vault,
        quote_vault,
        base_amount,
        quote_amount,
    })
}

fn vault_amount(
    (address, account): (Pubkey, &Account),
    expected_address: Pubkey,
    side: &str,
    expected_mint: &Pubkey,
    market: Pubkey,
) -> SurfpoolResult<u64> {
    if address != expected_address {
        return Err(invalid(format!(
            "{side} vault address {address} does not match the market's {side} vault {expected_address}"
        )));
    }
    if account.owner != spl_token_interface::ID && account.owner != spl_token_2022_interface::ID {
        return Err(invalid(format!(
            "{side} vault is not owned by a supported token program"
        )));
    }
    let vault = TokenAccount::unpack(&account.data)
        .map_err(|error| invalid(format!("{side} vault is not a token account: {error}")))?;
    if vault.owner() != market {
        return Err(invalid(format!(
            "{side} vault is held by {}, not by the market {market} this scenario targets",
            vault.owner()
        )));
    }
    if vault.mint() != *expected_mint {
        return Err(invalid(format!(
            "{side} vault holds mint {} but the market's {side} mint is {expected_mint}",
            vault.mint()
        )));
    }
    Ok(vault.amount())
}

fn remaining_label(bps: u16) -> String {
    format!("{}.{:02}%", bps / 100, bps % 100)
}

fn market_pointer(data: &[u8], offset: usize) -> SurfpoolResult<Pubkey> {
    read_pubkey(data, offset).ok_or_else(|| invalid("market vault bytes are truncated"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::protocols::goonfi::v1::fixtures::{
        self, FIXTURE_BASE_VAULT, FIXTURE_ORACLE, FIXTURE_QUOTE_VAULT, oracle_account,
    };

    const WSOL: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
    const USDC: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

    /// The market every fixture below belongs to; its vaults name it as their authority.
    const MARKET: Pubkey = Pubkey::from_str_const("GMCJvYGf5Ex2ARiMquaBDqU6iKM8uiEQkB8jCnoNfHpC");

    fn market_account() -> Account {
        fixtures::market_account(
            [&WSOL, &USDC],
            [&FIXTURE_BASE_VAULT, &FIXTURE_QUOTE_VAULT],
            &FIXTURE_ORACLE,
        )
    }

    fn vault(mint: &Pubkey, amount: u64) -> Account {
        fixtures::token_account(mint, &MARKET, amount)
    }

    #[test]
    fn rejects_unrelated_vault_with_matching_mint_and_authority() {
        let market = market_account();
        for (base_address, quote_address, side) in [
            (Pubkey::new_unique(), FIXTURE_QUOTE_VAULT, "base"),
            (FIXTURE_BASE_VAULT, Pubkey::new_unique(), "quote"),
        ] {
            let error = build_goonfi_liquidity_scenario(
                MARKET,
                &market,
                (base_address, &vault(&WSOL, 999_999_999)),
                (quote_address, &vault(&USDC, 999_999_999)),
                (FIXTURE_ORACLE, &oracle_account()),
                5_000,
                5_000,
            )
            .expect_err("an unrelated account's balance must not be scaled into the real vault");
            assert!(
                error.to_string().contains(&format!("{side} vault address")),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_an_unrelated_publisher_owned_oracle() {
        let error = build_goonfi_liquidity_scenario(
            MARKET,
            &market_account(),
            (FIXTURE_BASE_VAULT, &vault(&WSOL, 1_000)),
            (FIXTURE_QUOTE_VAULT, &vault(&USDC, 1_000)),
            (Pubkey::new_unique(), &oracle_account()),
            5_000,
            FULL_BPS,
        )
        .expect_err("freshness must target the oracle whose account was validated");
        assert!(error.to_string().contains("oracle address"), "{error}");
    }

    #[test]
    fn drains_both_vaults_and_keeps_the_quote_fresh() {
        let preparation = build_goonfi_liquidity_scenario(
            MARKET,
            &market_account(),
            (FIXTURE_BASE_VAULT, &vault(&WSOL, 2_441_078_070_812)),
            (FIXTURE_QUOTE_VAULT, &vault(&USDC, 216_136_231_615)),
            (FIXTURE_ORACLE, &oracle_account()),
            0,
            0,
        )
        .unwrap();

        assert_eq!(preparation.base_vault, FIXTURE_BASE_VAULT);
        assert_eq!(preparation.quote_vault, FIXTURE_QUOTE_VAULT);
        // A friendly pair label, not the raw market pubkey.
        assert_eq!(preparation.scenario.name, "GoonFi SOL/USDC liquidity drain");
        let [base, quote, freshness] = &preparation.scenario.overrides[..] else {
            panic!("expected base drain, quote drain and freshness overrides");
        };
        assert_eq!(
            base.account,
            AccountAddress::Pubkey(FIXTURE_BASE_VAULT.to_string())
        );
        assert_eq!(
            quote.account,
            AccountAddress::Pubkey(FIXTURE_QUOTE_VAULT.to_string())
        );
        assert_eq!(base.values.get("amount"), Some(&serde_json::json!("0")));
        assert_eq!(quote.values.get("amount"), Some(&serde_json::json!("0")));
        assert!(!base.fetch_before_use);
        assert_eq!(
            freshness.account,
            AccountAddress::Pubkey(FIXTURE_ORACLE.to_string())
        );
        assert_eq!(
            freshness.values.get("last_update_slot"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn scales_partially_and_skips_an_unchanged_side() {
        let preparation = build_goonfi_liquidity_scenario(
            MARKET,
            &market_account(),
            (FIXTURE_BASE_VAULT, &vault(&WSOL, 1_000)),
            (FIXTURE_QUOTE_VAULT, &vault(&USDC, 999)),
            (FIXTURE_ORACLE, &oracle_account()),
            2_500,
            FULL_BPS,
        )
        .unwrap();

        let [base, freshness] = &preparation.scenario.overrides[..] else {
            panic!("the unchanged quote side must not get an override");
        };
        assert_eq!(
            base.account,
            AccountAddress::Pubkey(FIXTURE_BASE_VAULT.to_string())
        );
        // 1000 * 2500 / 10000, exact integer arithmetic.
        assert_eq!(base.values.get("amount"), Some(&serde_json::json!("250")));
        assert_eq!(freshness.values.len(), 1);
    }

    #[test]
    fn rejects_bad_basis_points_and_accounts() {
        let good_market = market_account();
        let foreign_market = Account {
            owner: Pubkey::new_unique(),
            ..market_account()
        };
        let base = vault(&WSOL, 1);
        let foreign_vault = Account {
            owner: Pubkey::new_unique(),
            ..vault(&WSOL, 1)
        };
        let quote_mint_vault = vault(&USDC, 1);
        let mint_account = Account {
            data: vec![0u8; 82],
            owner: spl_token_interface::ID,
            ..Account::default()
        };
        let oracle = oracle_account();
        let foreign_oracle = Account {
            owner: Pubkey::new_unique(),
            ..oracle_account()
        };
        let rows = [
            // Out of range and a no-op leave nothing to prepare.
            (
                "basis points above 10000",
                MARKET,
                &good_market,
                &base,
                &oracle,
                10_001,
                0,
            ),
            (
                "both sides left unchanged",
                MARKET,
                &good_market,
                &base,
                &oracle,
                FULL_BPS,
                FULL_BPS,
            ),
            // A foreign account of the same size passes the raw guard, so the owner check must reject.
            (
                "market owned by another program",
                MARKET,
                &foreign_market,
                &base,
                &oracle,
                0,
                0,
            ),
            // A vault not owned by a token program is not a real vault.
            (
                "vault owned by another program",
                MARKET,
                &good_market,
                &foreign_vault,
                &oracle,
                0,
                0,
            ),
            // A foreign oracle carries no magic, so its owner is the only discriminator.
            (
                "oracle owned by another program",
                MARKET,
                &good_market,
                &base,
                &foreign_oracle,
                0,
                0,
            ),
            // An owner-and-length check would pass a mint: it is token-program-owned and long
            // enough to misread an amount out of. Unpacking plus the mint comparison is what
            // rejects it.
            (
                "mint account in place of the base vault",
                MARKET,
                &good_market,
                &mint_account,
                &oracle,
                0,
                0,
            ),
            // A real token account holding the other side's mint is refused as well.
            (
                "base vault holding the quote mint",
                MARKET,
                &good_market,
                &quote_mint_vault,
                &oracle,
                0,
                0,
            ),
            // The market account carries no self-address, so the vault's authority is what ties
            // the requested market to these bytes.
            (
                "market address that does not hold the vault",
                Pubkey::new_unique(),
                &good_market,
                &base,
                &oracle,
                0,
                0,
            ),
        ];
        for (label, market, market_data, base, oracle, base_bps, quote_bps) in rows {
            assert!(
                build_goonfi_liquidity_scenario(
                    market,
                    market_data,
                    (FIXTURE_BASE_VAULT, base),
                    (FIXTURE_QUOTE_VAULT, &vault(&USDC, 1)),
                    (FIXTURE_ORACLE, oracle),
                    base_bps,
                    quote_bps,
                )
                .is_err(),
                "{label}"
            );
        }
    }

    /// The balance is read from the passed account but written to the vault the market names, so a
    /// vault of the right mint belonging to another market would scale the wrong balance into this
    /// one - a drain that silently tops the vault up instead.
    #[test]
    fn rejects_a_vault_belonging_to_another_market() {
        let other_market = Pubkey::new_unique();
        let foreign_quote = {
            let mut account = vault(&USDC, 999_999_999);
            account.data[32..64].copy_from_slice(other_market.as_ref());
            account
        };
        let error = build_goonfi_liquidity_scenario(
            MARKET,
            &market_account(),
            (FIXTURE_BASE_VAULT, &vault(&WSOL, 1_000)),
            (FIXTURE_QUOTE_VAULT, &foreign_quote),
            (FIXTURE_ORACLE, &oracle_account()),
            0,
            0,
        )
        .expect_err("a vault held by another market must be refused");
        assert!(
            error.to_string().contains("quote vault is held by"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolves_vault_addresses_from_the_market() {
        let [base, quote] = vault_addresses(&market_account()).unwrap();
        assert_eq!(base, FIXTURE_BASE_VAULT);
        assert_eq!(quote, FIXTURE_QUOTE_VAULT);

        let mut zero_pointer = market_account();
        zero_pointer.data[BASE_VAULT_OFFSET..BASE_VAULT_OFFSET + 32].fill(0);
        assert!(vault_addresses(&zero_pointer).is_err());
    }
}
