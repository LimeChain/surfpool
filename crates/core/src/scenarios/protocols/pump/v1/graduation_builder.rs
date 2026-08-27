use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{OverrideInstance, Scenario};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
    types::TokenAccount,
};

const PUMP_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

const VIRTUAL_TOKEN_RESERVES_OFFSET: usize = 8;
const VIRTUAL_QUOTE_RESERVES_OFFSET: usize = 16;
const REAL_TOKEN_RESERVES_OFFSET: usize = 24;
const REAL_QUOTE_RESERVES_OFFSET: usize = 32;
const COMPLETE_OFFSET: usize = 48;
const QUOTE_MINT_OFFSET: usize = 83;
const POOL_MIGRATION_FEE_OFFSET: usize = 146;
const GRADUATION_PREPARATION_SLOT: u64 = 1;
const MIGRATION_FEE_BUFFER: u64 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpGraduationAddresses {
    pub bonding_curve: Pubkey,
    pub curve_vault: Pubkey,
    pub canonical_pool: Pubkey,
    pub global: Pubkey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PumpGraduationPreparation {
    pub scenario: Scenario,
    pub token_mint: Pubkey,
    pub addresses: PumpGraduationAddresses,
    pub completing_buy_amount: u64,
    pub migration_reserve: u64,
}

/// Resolved from the templates' PDA specs, so the builder reads the addresses
/// the materializer writes to.
pub fn pump_graduation_addresses(token_mint: &Pubkey) -> SurfpoolResult<PumpGraduationAddresses> {
    let registry = TemplateRegistry::new();
    let resolve = |template_id: &str, mint_property: Option<&str>| {
        let template = registry.get(template_id).ok_or_else(|| {
            SurfpoolError::internal(format!("{template_id} template is unavailable"))
        })?;
        let values = mint_property.map(|property| {
            HashMap::from([(
                property.to_string(),
                serde_json::json!(token_mint.to_string()),
            )])
        });
        template.address.resolve(values.as_ref()).ok_or_else(|| {
            SurfpoolError::internal(format!("{template_id} address does not resolve"))
        })
    };

    Ok(PumpGraduationAddresses {
        bonding_curve: resolve("pump-bonding-curve-custom", Some("token_mint"))?,
        curve_vault: resolve("pump-token-2022-curve-balance", Some("token_mint"))?,
        canonical_pool: resolve("pump-amm-canonical-pool", Some("base_mint"))?,
        global: resolve("pump-global", None)?,
    })
}

pub fn build_pump_graduation_scenario(
    token_mint: Pubkey,
    mint_account: &Account,
    curve_account: &Account,
    curve_vault_account: &Account,
    canonical_pool_account: Option<&Account>,
    global_account: &Account,
) -> SurfpoolResult<PumpGraduationPreparation> {
    validate_accounts(
        token_mint,
        mint_account,
        curve_account,
        curve_vault_account,
        canonical_pool_account,
        global_account,
    )?;

    let virtual_token_reserves = read_u64(
        &curve_account.data,
        VIRTUAL_TOKEN_RESERVES_OFFSET,
        "virtual_token_reserves",
    )?;
    let virtual_quote_reserves = read_u64(
        &curve_account.data,
        VIRTUAL_QUOTE_RESERVES_OFFSET,
        "virtual_quote_reserves",
    )?;
    let real_token_reserves = read_u64(
        &curve_account.data,
        REAL_TOKEN_RESERVES_OFFSET,
        "real_token_reserves",
    )?;
    let real_quote_reserves = read_u64(
        &curve_account.data,
        REAL_QUOTE_RESERVES_OFFSET,
        "real_quote_reserves",
    )?;
    let token_offset = virtual_token_reserves
        .checked_sub(real_token_reserves)
        .ok_or_else(|| invalid_curve("virtual token reserves are below real token reserves"))?;
    virtual_quote_reserves
        .checked_sub(real_quote_reserves)
        .ok_or_else(|| invalid_curve("virtual quote reserves are below real quote reserves"))?;

    let token_account = TokenAccount::unpack(&curve_vault_account.data)?;
    let migration_reserve = token_account
        .amount()
        .checked_sub(real_token_reserves)
        .ok_or_else(|| invalid_curve("curve vault balance is below real token reserves"))?;
    if migration_reserve == 0 {
        return Err(invalid_curve("curve vault has no migration reserve"));
    }

    let pool_migration_fee = read_u64(
        &global_account.data,
        POOL_MIGRATION_FEE_OFFSET,
        "pool_migration_fee",
    )?;
    let target_final_quote = pool_migration_fee
        .checked_mul(MIGRATION_FEE_BUFFER)
        .ok_or_else(|| invalid_curve("pool migration fee overflows"))?;
    let required_quote_in = target_final_quote
        .saturating_sub(real_quote_reserves)
        .max(1);
    let prepared_real_quote_reserves = real_quote_reserves;
    let prepared_virtual_quote_reserves = virtual_quote_reserves;
    let completing_buy_amount = div_ceil(
        u128::from(required_quote_in) * u128::from(token_offset),
        u128::from(prepared_virtual_quote_reserves),
    )?;
    let completing_buy_amount = u64::try_from(completing_buy_amount)
        .map_err(|_| invalid_curve("completing buy amount does not fit in u64"))?;
    if completing_buy_amount == 0 || completing_buy_amount > real_token_reserves {
        return Err(invalid_curve(
            "curve does not have enough real token reserves for a migration-safe finishing buy",
        ));
    }

    let prepared_virtual_token_reserves = token_offset
        .checked_add(completing_buy_amount)
        .ok_or_else(|| invalid_curve("virtual token reserves overflow"))?;
    let prepared_vault_amount = migration_reserve
        .checked_add(completing_buy_amount)
        .ok_or_else(|| invalid_curve("curve vault amount overflow"))?;
    let registry = TemplateRegistry::new();
    let curve_template = registry
        .get("pump-bonding-curve-custom")
        .ok_or_else(|| SurfpoolError::internal("pump bonding curve template is unavailable"))?;
    let vault_template = registry
        .get("pump-token-2022-curve-balance")
        .ok_or_else(|| SurfpoolError::internal("pump Token-2022 vault template is unavailable"))?;
    let global_template = registry
        .get("pump-global")
        .ok_or_else(|| SurfpoolError::internal("pump Global template is unavailable"))?;
    let mint = token_mint.to_string();

    let curve_values = HashMap::from([
        ("token_mint".to_string(), serde_json::json!(mint)),
        (
            "virtual_token_reserves".to_string(),
            serde_json::json!(prepared_virtual_token_reserves),
        ),
        (
            "virtual_quote_reserves".to_string(),
            serde_json::json!(prepared_virtual_quote_reserves),
        ),
        (
            "real_token_reserves".to_string(),
            serde_json::json!(completing_buy_amount),
        ),
        (
            "real_quote_reserves".to_string(),
            serde_json::json!(prepared_real_quote_reserves),
        ),
        ("complete".to_string(), serde_json::json!(false)),
    ]);
    let vault_values = HashMap::from([
        ("token_mint".to_string(), serde_json::json!(mint)),
        (
            "amount".to_string(),
            serde_json::json!(prepared_vault_amount),
        ),
    ]);
    let global_values = HashMap::from([("enable_migrate".to_string(), serde_json::json!(true))]);
    let curve_override = OverrideInstance::new(
        curve_template.id.clone(),
        GRADUATION_PREPARATION_SLOT,
        curve_template.address.clone(),
    )
    .with_values(curve_values)
    .with_label("Near-complete bonding curve".to_string());
    let vault_override = OverrideInstance::new(
        vault_template.id.clone(),
        GRADUATION_PREPARATION_SLOT,
        vault_template.address.clone(),
    )
    .with_values(vault_values)
    .with_label("Migration-safe curve vault".to_string());
    let global_override = OverrideInstance::new(
        global_template.id.clone(),
        GRADUATION_PREPARATION_SLOT,
        global_template.address.clone(),
    )
    .with_values(global_values)
    .with_label("Migration enabled".to_string());

    let mut scenario = Scenario::new(
        "Pump Graduation".to_string(),
        "Prepare a SOL-quoted Token-2022 pump.fun curve for one finishing buy and migration to PumpSwap."
            .to_string(),
    );
    scenario.tags = vec!["pump".to_string(), "graduation".to_string()];
    scenario.add_override(curve_override);
    scenario.add_override(vault_override);
    scenario.add_override(global_override);

    Ok(PumpGraduationPreparation {
        scenario,
        token_mint,
        addresses: pump_graduation_addresses(&token_mint)?,
        completing_buy_amount,
        migration_reserve,
    })
}

fn validate_accounts(
    token_mint: Pubkey,
    mint_account: &Account,
    curve_account: &Account,
    curve_vault_account: &Account,
    canonical_pool_account: Option<&Account>,
    global_account: &Account,
) -> SurfpoolResult<()> {
    if mint_account.owner != TOKEN_2022_PROGRAM_ID {
        return Err(invalid_curve("mint is not owned by Token-2022"));
    }
    if curve_account.owner != PUMP_PROGRAM_ID {
        return Err(invalid_curve("bonding curve is not owned by pump"));
    }
    if curve_account.data.get(COMPLETE_OFFSET) != Some(&0) {
        return Err(invalid_curve("bonding curve is already complete"));
    }
    if read_pubkey(&curve_account.data, QUOTE_MINT_OFFSET, "quote_mint")? != Pubkey::default() {
        return Err(invalid_curve(
            "Pump graduation preset supports SOL-quoted bonding curves only",
        ));
    }
    if canonical_pool_account.is_some() {
        return Err(invalid_curve("canonical PumpSwap pool already exists"));
    }
    if curve_vault_account.owner != TOKEN_2022_PROGRAM_ID {
        return Err(invalid_curve("curve vault is not owned by Token-2022"));
    }
    let token_account = TokenAccount::unpack(&curve_vault_account.data)?;
    if token_account.mint() != token_mint {
        return Err(invalid_curve("curve vault contains a different mint"));
    }
    if global_account.owner != PUMP_PROGRAM_ID {
        return Err(invalid_curve("pump Global account has the wrong owner"));
    }
    Ok(())
}

fn read_u64(data: &[u8], offset: usize, field: &str) -> SurfpoolResult<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| invalid_curve(format!("missing {field}")))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_pubkey(data: &[u8], offset: usize, field: &str) -> SurfpoolResult<Pubkey> {
    let bytes = data
        .get(offset..offset + 32)
        .ok_or_else(|| invalid_curve(format!("missing {field}")))?;
    Pubkey::try_from(bytes).map_err(|_| invalid_curve(format!("invalid {field}")))
}

fn div_ceil(numerator: u128, denominator: u128) -> SurfpoolResult<u128> {
    if denominator == 0 {
        return Err(invalid_curve("division by zero"));
    }
    Ok(numerator.div_ceil(denominator))
}

fn invalid_curve(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::internal(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_u64(data: &mut [u8], offset: usize, value: u64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn graduation_preserves_quote_reserves_and_sizes_only_the_shortfall() {
        let token_mint = Pubkey::new_unique();
        let token_offset = 1_000;
        let real_token_reserves = 500;
        let virtual_token_reserves = token_offset + real_token_reserves;
        let real_quote_reserves = 200;
        let virtual_quote_reserves = 500;
        let pool_migration_fee = 100;
        let migration_reserve = 50;

        let mint_account = Account {
            owner: TOKEN_2022_PROGRAM_ID,
            ..Account::default()
        };
        let mut curve_data = vec![0; QUOTE_MINT_OFFSET + 32];
        write_u64(
            &mut curve_data,
            VIRTUAL_TOKEN_RESERVES_OFFSET,
            virtual_token_reserves,
        );
        write_u64(
            &mut curve_data,
            VIRTUAL_QUOTE_RESERVES_OFFSET,
            virtual_quote_reserves,
        );
        write_u64(
            &mut curve_data,
            REAL_TOKEN_RESERVES_OFFSET,
            real_token_reserves,
        );
        write_u64(
            &mut curve_data,
            REAL_QUOTE_RESERVES_OFFSET,
            real_quote_reserves,
        );
        let curve_account = Account {
            owner: PUMP_PROGRAM_ID,
            data: curve_data,
            ..Account::default()
        };

        let mut vault = TokenAccount::new(
            &TOKEN_2022_PROGRAM_ID,
            Pubkey::new_unique(),
            token_mint,
            None,
        );
        vault.set_amount(real_token_reserves + migration_reserve);
        let curve_vault_account = Account {
            owner: TOKEN_2022_PROGRAM_ID,
            data: vault.pack_into_vec(),
            ..Account::default()
        };

        let mut global_data = vec![0; POOL_MIGRATION_FEE_OFFSET + 8];
        write_u64(
            &mut global_data,
            POOL_MIGRATION_FEE_OFFSET,
            pool_migration_fee,
        );
        let global_account = Account {
            owner: PUMP_PROGRAM_ID,
            data: global_data,
            ..Account::default()
        };

        let preparation = build_pump_graduation_scenario(
            token_mint,
            &mint_account,
            &curve_account,
            &curve_vault_account,
            None,
            &global_account,
        )
        .expect("valid graduation state");

        assert_eq!(preparation.completing_buy_amount, 200);
        let curve_override = preparation
            .scenario
            .overrides
            .iter()
            .find(|instance| instance.template_id == "pump-bonding-curve-custom")
            .expect("curve override");
        assert_eq!(
            curve_override.values["real_quote_reserves"],
            serde_json::json!(real_quote_reserves)
        );
        assert_eq!(
            curve_override.values["virtual_quote_reserves"],
            serde_json::json!(virtual_quote_reserves)
        );
    }
}
