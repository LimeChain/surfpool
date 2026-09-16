//! HumidiFi liquidity stress.
//!
//! Scales the market's current vault balances through the generic SPL token balance template while
//! keeping the market fresh. Every override derives an exact integer balance from current state.

use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use super::fair_value::{
    HumidiFiMarket, PREPARATION_SLOT, freshness_override, invalid, read_masked_pubkey_pair,
    template,
};
use crate::{error::SurfpoolResult, scenarios::TemplateRegistry, types::TokenAccount};

/// The vault addresses are read, never written: their balances ride the generic token template.
const QUOTE_VAULT_OFFSET: usize = 448;
const BASE_VAULT_OFFSET: usize = 480;

const TOKEN_BALANCE_TEMPLATE: &str = "spl-token-account-balance";
const BPS: u128 = 10_000;

/// The market's `[base, quote]` vault addresses, unmasked.
pub fn humidifi_vault_addresses(market_account: &Account) -> SurfpoolResult<[Pubkey; 2]> {
    read_masked_pubkey_pair(
        market_account,
        [BASE_VAULT_OFFSET, QUOTE_VAULT_OFFSET],
        "vault",
    )
}

pub fn build_humidifi_liquidity_scenario(
    market: &HumidiFiMarket,
    market_account: &Account,
    base_vault: &Account,
    quote_vault: &Account,
    base_remaining_bps: u16,
    quote_remaining_bps: u16,
) -> SurfpoolResult<Scenario> {
    if u128::from(base_remaining_bps) > BPS || u128::from(quote_remaining_bps) > BPS {
        return Err(invalid(
            "remaining liquidity must be between 0 and 10000 basis points",
        ));
    }
    if u128::from(base_remaining_bps) == BPS && u128::from(quote_remaining_bps) == BPS {
        return Err(invalid(
            "liquidity stress changes nothing; lower at least one side",
        ));
    }

    let [base_vault_address, quote_vault_address] = humidifi_vault_addresses(market_account)?;
    let (base_mint, quote_mint) = HumidiFiMarket::mint_addresses(market_account)?;
    if base_mint != market.base_mint || quote_mint != market.quote_mint {
        return Err(invalid(
            "market account mint identities do not match validated market metadata",
        ));
    }
    let base_balance = vault_balance(
        base_vault,
        market,
        market.base_mint,
        market.base_token_program,
        "base",
    )?;
    let quote_balance = vault_balance(
        quote_vault,
        market,
        market.quote_mint,
        market.quote_token_program,
        "quote",
    )?;

    let registry = TemplateRegistry::new();
    let balance_template = template(&registry, TOKEN_BALANCE_TEMPLATE)?;
    let label = market.label();

    let mut scenario = Scenario::new(
        format!("HumidiFi {label} liquidity stress"),
        format!(
            "Keep {} of the base and {} of the quote vault balance on HumidiFi market {}, preserving the price and keeping the quote fresh; no swap is sent.",
            percent(base_remaining_bps),
            percent(quote_remaining_bps),
            market.address
        ),
    );
    scenario.tags = vec![
        "humidifi".to_string(),
        "pmm".to_string(),
        "liquidity-stress".to_string(),
    ];

    for (side, address, balance, bps) in [
        ("base", base_vault_address, base_balance, base_remaining_bps),
        (
            "quote",
            quote_vault_address,
            quote_balance,
            quote_remaining_bps,
        ),
    ] {
        if u128::from(bps) == BPS {
            continue;
        }
        let remaining = u64::try_from(u128::from(balance) * u128::from(bps) / BPS)
            .map_err(|_| invalid(format!("{side} vault balance overflow")))?;
        scenario.add_override(
            OverrideInstance::new(
                balance_template.id.clone(),
                PREPARATION_SLOT,
                AccountAddress::Pubkey(address.to_string()),
            )
            .with_values(HashMap::from([(
                "amount".to_string(),
                serde_json::json!(remaining.to_string()),
            )]))
            .with_label(format!("Reduce HumidiFi {side} liquidity")),
        );
    }

    scenario.add_override(freshness_override(&registry, &market.address)?);
    Ok(scenario)
}

fn vault_balance(
    vault: &Account,
    market: &HumidiFiMarket,
    mint: Pubkey,
    token_program: Pubkey,
    side: &str,
) -> SurfpoolResult<u64> {
    if vault.owner != token_program {
        return Err(invalid(format!(
            "{side} vault token program does not match the market's {side} mint"
        )));
    }
    let token = TokenAccount::unpack(&vault.data)
        .map_err(|_| invalid(format!("{side} vault is not an initialized token account")))?;
    if !token_account_is_initialized(&token) {
        return Err(invalid(format!("{side} vault is not initialized")));
    }
    if token.mint() != mint {
        return Err(invalid(format!(
            "{side} vault does not hold the market's {side} mint"
        )));
    }
    if token.owner() != market.address {
        return Err(invalid(format!(
            "{side} vault is not controlled by the market"
        )));
    }
    Ok(token.amount())
}

fn token_account_is_initialized(token: &TokenAccount) -> bool {
    match token {
        TokenAccount::SplToken2022(account) => {
            account.state == spl_token_2022_interface::state::AccountState::Initialized
        }
        TokenAccount::SplToken(account) => {
            account.state == spl_token_interface::state::AccountState::Initialized
        }
    }
}

fn percent(bps: u16) -> String {
    format!("{}.{:02}%", bps / 100, bps % 100)
}

#[cfg(test)]
mod tests {
    use super::{
        super::fair_value::{
            FRESHNESS_TEMPLATE, SCHEMA_VERSION_OFFSET, market_account, mint_account,
            write_masked_pubkey,
        },
        *,
    };

    struct Fixture {
        market: HumidiFiMarket,
        market_account: Account,
        base_vault_address: Pubkey,
        quote_vault_address: Pubkey,
        base_vault: Account,
        quote_vault: Account,
    }

    impl Fixture {
        fn build(
            &self,
            base_vault: &Account,
            quote_vault: &Account,
            base_remaining_bps: u16,
            quote_remaining_bps: u16,
        ) -> SurfpoolResult<Scenario> {
            build_humidifi_liquidity_scenario(
                &self.market,
                &self.market_account,
                base_vault,
                quote_vault,
                base_remaining_bps,
                quote_remaining_bps,
            )
        }
    }

    fn token_account(
        mint: Pubkey,
        authority: Pubkey,
        amount: u64,
        token_program: Pubkey,
        state: &str,
    ) -> Account {
        let mut token = TokenAccount::new(&token_program, authority, mint, None);
        token.set_amount(amount);
        token.set_state_from_str(state).unwrap();
        Account {
            lamports: 2_039_280,
            data: token.pack_into_vec(),
            owner: token_program,
            ..Account::default()
        }
    }

    fn fixture(base_amount: u64, quote_amount: u64) -> Fixture {
        fixture_with_token_programs(
            base_amount,
            quote_amount,
            spl_token_interface::id(),
            spl_token_interface::id(),
        )
    }

    fn fixture_with_token_programs(
        base_amount: u64,
        quote_amount: u64,
        base_token_program: Pubkey,
        quote_token_program: Pubkey,
    ) -> Fixture {
        let address = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let base_vault_address = Pubkey::new_unique();
        let quote_vault_address = Pubkey::new_unique();

        let mut market_account = market_account(&base_mint, &quote_mint);
        write_masked_pubkey(
            &mut market_account.data,
            BASE_VAULT_OFFSET,
            &base_vault_address,
        );
        write_masked_pubkey(
            &mut market_account.data,
            QUOTE_VAULT_OFFSET,
            &quote_vault_address,
        );
        let market = HumidiFiMarket::validate(
            address,
            &market_account,
            &mint_account(9, base_token_program),
            &mint_account(6, quote_token_program),
        )
        .unwrap();
        Fixture {
            base_vault: token_account(
                base_mint,
                address,
                base_amount,
                base_token_program,
                "initialized",
            ),
            quote_vault: token_account(
                quote_mint,
                address,
                quote_amount,
                quote_token_program,
                "initialized",
            ),
            market,
            market_account,
            base_vault_address,
            quote_vault_address,
        }
    }

    fn amount_of(instance: &OverrideInstance) -> u64 {
        instance
            .values
            .get("amount")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse().ok())
            .unwrap()
    }

    #[test]
    fn reads_the_vault_addresses_the_market_stores() {
        let fixture = fixture(1, 1);
        assert_eq!(
            humidifi_vault_addresses(&fixture.market_account).unwrap(),
            [fixture.base_vault_address, fixture.quote_vault_address]
        );
    }

    #[test]
    fn scales_only_the_selected_side_and_keeps_the_quote_fresh() {
        let fixture = fixture(1_000_000, 2_000_000);
        let scenario = fixture
            .build(&fixture.base_vault, &fixture.quote_vault, 50, 10_000)
            .unwrap();

        let [base, freshness] = &scenario.overrides[..] else {
            panic!("expected one vault override and the freshness override");
        };
        assert_eq!(base.template_id, TOKEN_BALANCE_TEMPLATE);
        assert_eq!(
            base.account,
            AccountAddress::Pubkey(fixture.base_vault_address.to_string())
        );
        assert_eq!(amount_of(base), 5_000);
        assert!(!base.fetch_before_use);
        assert!(!base.persist);
        assert_eq!(freshness.template_id, FRESHNESS_TEMPLATE);
        assert_eq!(
            freshness.account,
            AccountAddress::Pubkey(fixture.market.address.to_string())
        );
        assert!(freshness.persist);
        assert_eq!(
            freshness.values.get("last_update_slot"),
            Some(&serde_json::Value::Null)
        );
        assert!(scenario.name.contains(&fixture.market.label()));
        assert!(scenario.description.contains("0.50% of the base"));
        assert!(scenario.tags.contains(&"liquidity-stress".to_string()));
    }

    #[test]
    fn drains_a_side_floors_the_remainder_and_rejects_noops() {
        let fixture = fixture(1_000_001, 2_000_000);

        let drained = fixture
            .build(&fixture.base_vault, &fixture.quote_vault, 0, 10_000)
            .unwrap();
        assert_eq!(amount_of(&drained.overrides[0]), 0);

        let both = fixture
            .build(&fixture.base_vault, &fixture.quote_vault, 3_333, 2_500)
            .unwrap();
        assert_eq!(both.overrides.len(), 3);
        assert_eq!(amount_of(&both.overrides[0]), 333_300);
        assert_eq!(
            both.overrides[1].account,
            AccountAddress::Pubkey(fixture.quote_vault_address.to_string())
        );
        assert_eq!(amount_of(&both.overrides[1]), 500_000);

        assert!(
            fixture
                .build(&fixture.base_vault, &fixture.quote_vault, 10_000, 10_000)
                .is_err()
        );
        assert!(
            fixture
                .build(&fixture.base_vault, &fixture.quote_vault, 10_001, 10_000)
                .is_err()
        );
    }

    #[test]
    fn rejects_vaults_that_do_not_belong_to_the_market() {
        let fixture = fixture(1_000_000, 2_000_000);
        let market = &fixture.market;
        let token = spl_token_interface::id();
        let token_2022 = spl_token_2022_interface::id();
        for (side, vault, expected) in [
            (
                "base",
                token_account(
                    Pubkey::new_unique(),
                    market.address,
                    1,
                    token,
                    "initialized",
                ),
                "base vault does not hold the market's base mint",
            ),
            (
                "base",
                token_account(
                    market.base_mint,
                    Pubkey::new_unique(),
                    1,
                    token,
                    "initialized",
                ),
                "base vault is not controlled by the market",
            ),
            (
                "base",
                Account {
                    owner: Pubkey::new_unique(),
                    ..fixture.base_vault.clone()
                },
                "base vault token program",
            ),
            (
                "base",
                Account {
                    data: vec![0; 10],
                    ..fixture.base_vault.clone()
                },
                "base vault is not an initialized token account",
            ),
            (
                "base",
                Account {
                    owner: token_2022,
                    ..fixture.base_vault.clone()
                },
                "base vault token program",
            ),
            (
                "quote",
                Account {
                    owner: token_2022,
                    ..fixture.quote_vault.clone()
                },
                "quote vault token program",
            ),
            (
                "base",
                token_account(market.base_mint, market.address, 1_000_000, token, "frozen"),
                "base vault is not initialized",
            ),
            (
                "quote",
                token_account(
                    market.quote_mint,
                    market.address,
                    2_000_000,
                    token,
                    "uninitialized",
                ),
                "quote vault is not an initialized token account",
            ),
        ] {
            let (base_vault, quote_vault) = if side == "base" {
                (&vault, &fixture.quote_vault)
            } else {
                (&fixture.base_vault, &vault)
            };
            let error = fixture
                .build(base_vault, quote_vault, 500, 10_000)
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{side}: {error}");
        }

        let mut version_5 = fixture.market_account.clone();
        version_5.data[SCHEMA_VERSION_OFFSET..SCHEMA_VERSION_OFFSET + 8]
            .copy_from_slice(&5u64.to_le_bytes());
        assert!(humidifi_vault_addresses(&version_5).is_err());
    }

    #[test]
    fn accepts_initialized_token_2022_vaults() {
        let fixture = fixture_with_token_programs(
            1_000_000,
            2_000_000,
            spl_token_2022_interface::id(),
            spl_token_2022_interface::id(),
        );
        let scenario = fixture
            .build(&fixture.base_vault, &fixture.quote_vault, 500, 10_000)
            .unwrap();
        assert_eq!(amount_of(&scenario.overrides[0]), 50_000);
    }

    #[test]
    fn rejects_market_metadata_from_a_different_account_graph() {
        let mut fixture = fixture(1_000_000, 2_000_000);
        fixture.market.base_mint = Pubkey::new_unique();
        let error = fixture
            .build(&fixture.base_vault, &fixture.quote_vault, 500, 10_000)
            .unwrap_err();
        assert!(error.to_string().contains("mint identities do not match"));
    }
}
