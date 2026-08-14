use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{OverrideInstance, Scenario};

use super::{TemplateRegistry, pump_graduation::pump_graduation_addresses};
use crate::error::{SurfpoolError, SurfpoolResult};

const PUMP_AMM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const POOL_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
const BASE_MINT_OFFSET: usize = 43;
const QUOTE_MINT_OFFSET: usize = 75;
const PREPARATION_SLOT: u64 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct PumpSwapPriceShockPreparation {
    pub scenario: Scenario,
    pub token_mint: Pubkey,
    pub canonical_pool: Pubkey,
    pub virtual_quote_reserves: u64,
}

pub fn build_pump_swap_price_shock_scenario(
    token_mint: Pubkey,
    canonical_pool_account: &Account,
    virtual_quote_reserves: u64,
) -> SurfpoolResult<PumpSwapPriceShockPreparation> {
    validate_canonical_pool(token_mint, canonical_pool_account, virtual_quote_reserves)?;

    let template_registry = TemplateRegistry::new();
    let template = template_registry
        .get("pump-amm-canonical-pool")
        .ok_or_else(|| {
            SurfpoolError::internal("PumpSwap canonical pool template is unavailable")
        })?;
    let values = HashMap::from([
        (
            "base_mint".to_string(),
            serde_json::json!(token_mint.to_string()),
        ),
        (
            "virtual_quote_reserves".to_string(),
            serde_json::json!(virtual_quote_reserves),
        ),
    ]);
    let mut pool_override = OverrideInstance::new(
        template.id.clone(),
        PREPARATION_SLOT,
        template.address.clone(),
    )
    .with_values(values)
    .with_label("PumpSwap virtual quote reserve shock".to_string());
    pool_override.fetch_before_use = true;

    let mut scenario = Scenario::new(
        "PumpSwap Price Shock".to_string(),
        "Shift a canonical PumpSwap pool price through its virtual quote reserves.".to_string(),
    );
    scenario.tags = vec!["pumpswap".to_string(), "price-shock".to_string()];
    scenario.add_override(pool_override);

    Ok(PumpSwapPriceShockPreparation {
        scenario,
        token_mint,
        canonical_pool: pump_graduation_addresses(&token_mint).canonical_pool,
        virtual_quote_reserves,
    })
}

fn validate_canonical_pool(
    token_mint: Pubkey,
    account: &Account,
    virtual_quote_reserves: u64,
) -> SurfpoolResult<()> {
    if virtual_quote_reserves == 0 {
        return Err(invalid_pool(
            "virtual quote reserves must be greater than zero",
        ));
    }
    if account.owner != PUMP_AMM_PROGRAM_ID {
        return Err(invalid_pool("canonical pool is not owned by PumpSwap"));
    }
    if account.data.get(..8) != Some(POOL_DISCRIMINATOR.as_slice()) {
        return Err(invalid_pool("canonical pool has the wrong discriminator"));
    }
    if read_pubkey(&account.data, BASE_MINT_OFFSET, "base_mint")? != token_mint {
        return Err(invalid_pool(
            "canonical pool contains a different base mint",
        ));
    }
    if read_pubkey(&account.data, QUOTE_MINT_OFFSET, "quote_mint")? != WSOL_MINT {
        return Err(invalid_pool("canonical pool is not quoted in WSOL"));
    }
    Ok(())
}

fn read_pubkey(data: &[u8], offset: usize, field: &str) -> SurfpoolResult<Pubkey> {
    let bytes = data
        .get(offset..offset + 32)
        .ok_or_else(|| invalid_pool(format!("canonical pool is missing {field}")))?;
    Pubkey::try_from(bytes)
        .map_err(|_| invalid_pool(format!("canonical pool has an invalid {field}")))
}

fn invalid_pool(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::internal(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_account(base_mint: Pubkey) -> Account {
        let mut data = vec![0; 261];
        data[..8].copy_from_slice(&POOL_DISCRIMINATOR);
        data[BASE_MINT_OFFSET..BASE_MINT_OFFSET + 32].copy_from_slice(base_mint.as_ref());
        data[QUOTE_MINT_OFFSET..QUOTE_MINT_OFFSET + 32].copy_from_slice(WSOL_MINT.as_ref());
        Account {
            data,
            owner: PUMP_AMM_PROGRAM_ID,
            ..Account::default()
        }
    }

    #[test]
    fn builds_a_canonical_pool_price_shock() {
        let mint = Pubkey::from_str_const("7LSsEoJGhLeZzGvDofTdNg7M3JttxQqGWNLo6vWMpump");
        let preparation =
            build_pump_swap_price_shock_scenario(mint, &pool_account(mint), 15_000_000_000_000)
                .unwrap();
        let pool_override = &preparation.scenario.overrides[0];

        assert_eq!(pool_override.template_id, "pump-amm-canonical-pool");
        assert_eq!(pool_override.scenario_relative_slot, PREPARATION_SLOT);
        assert!(pool_override.fetch_before_use);
        assert_eq!(
            pool_override.values["virtual_quote_reserves"],
            serde_json::json!(15_000_000_000_000u64)
        );
        assert_eq!(
            pool_override.account.resolve(Some(&pool_override.values)),
            Some(preparation.canonical_pool)
        );
    }

    #[test]
    fn rejects_a_pool_for_a_different_mint() {
        let requested_mint = Pubkey::new_unique();
        let stored_mint = Pubkey::new_unique();

        assert!(
            build_pump_swap_price_shock_scenario(requested_mint, &pool_account(stored_mint), 1,)
                .is_err()
        );
    }
}
