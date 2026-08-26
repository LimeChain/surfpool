use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{OverrideInstance, Scenario};

use super::TemplateRegistry;
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    types::TokenAccount,
};

const PUMP_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const PUMP_AMM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const GLOBAL_ACCOUNT: Pubkey =
    Pubkey::from_str_const("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf");

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

pub fn pump_graduation_addresses(token_mint: &Pubkey) -> PumpGraduationAddresses {
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", token_mint.as_ref()], &PUMP_PROGRAM_ID).0;
    let curve_vault = Pubkey::find_program_address(
        &[
            bonding_curve.as_ref(),
            TOKEN_2022_PROGRAM_ID.as_ref(),
            token_mint.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0;
    let pool_authority =
        Pubkey::find_program_address(&[b"pool-authority", token_mint.as_ref()], &PUMP_PROGRAM_ID).0;
    let canonical_pool = Pubkey::find_program_address(
        &[
            b"pool",
            &0u16.to_le_bytes(),
            pool_authority.as_ref(),
            token_mint.as_ref(),
            WSOL_MINT.as_ref(),
        ],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0;

    PumpGraduationAddresses {
        bonding_curve,
        curve_vault,
        canonical_pool,
        global: GLOBAL_ACCOUNT,
    }
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
    let quote_offset = virtual_quote_reserves
        .checked_sub(real_quote_reserves)
        .ok_or_else(|| invalid_curve("virtual quote reserves are below real quote reserves"))?;

    let token_account =
        TokenAccount::unpack_for_program(&curve_vault_account.data, &curve_vault_account.owner)?;
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
    let target_quote = pool_migration_fee
        .checked_mul(MIGRATION_FEE_BUFFER)
        .ok_or_else(|| invalid_curve("pool migration fee overflows"))?;
    let prepared_real_quote_reserves = 1u64;
    let prepared_virtual_quote_reserves = quote_offset
        .checked_add(prepared_real_quote_reserves)
        .ok_or_else(|| invalid_curve("virtual quote reserves overflow"))?;
    let completing_buy_amount = div_ceil(
        u128::from(target_quote) * u128::from(token_offset),
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
    let mut curve_override = OverrideInstance::new(
        curve_template.id.clone(),
        GRADUATION_PREPARATION_SLOT,
        curve_template.address.clone(),
    )
    .with_values(curve_values)
    .with_label("Near-complete bonding curve".to_string());
    curve_override.fetch_before_use = true;
    let mut vault_override = OverrideInstance::new(
        vault_template.id.clone(),
        GRADUATION_PREPARATION_SLOT,
        vault_template.address.clone(),
    )
    .with_values(vault_values)
    .with_label("Migration-safe curve vault".to_string());
    vault_override.fetch_before_use = true;
    let mut global_override = OverrideInstance::new(
        global_template.id.clone(),
        GRADUATION_PREPARATION_SLOT,
        global_template.address.clone(),
    )
    .with_values(global_values)
    .with_label("Migration enabled".to_string());
    global_override.fetch_before_use = true;

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
        addresses: pump_graduation_addresses(&token_mint),
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
    let token_account =
        TokenAccount::unpack_for_program(&curve_vault_account.data, &curve_vault_account.owner)?;
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
    use std::collections::BTreeMap;

    use surfpool_types::AccountSnapshot;

    use super::*;

    fn fixture_account(
        snapshot: &BTreeMap<String, Option<AccountSnapshot>>,
        address: &Pubkey,
    ) -> Account {
        snapshot[&address.to_string()]
            .as_ref()
            .unwrap()
            .to_account()
            .unwrap()
    }

    #[test]
    fn builds_the_verified_hrt_z_graduation_scenario() {
        let snapshot: BTreeMap<String, Option<AccountSnapshot>> = serde_json::from_str(
            include_str!("../tests/assets/pump_token2022_graduation.snapshot.json"),
        )
        .unwrap();
        let mint = Pubkey::from_str_const("HRTzNRJNnY78xe8e4a9DuMotw6qA97GwSQLzpVw9pump");
        let addresses = pump_graduation_addresses(&mint);
        let preparation = build_pump_graduation_scenario(
            mint,
            &fixture_account(&snapshot, &mint),
            &fixture_account(&snapshot, &addresses.bonding_curve),
            &fixture_account(&snapshot, &addresses.curve_vault),
            None,
            &fixture_account(&snapshot, &addresses.global),
        )
        .unwrap();

        assert_eq!(
            preparation.addresses.bonding_curve.to_string(),
            "GBpTHrtF8dGwxC7thRD7T6VfGtbVYEabKkQ7k6g3u7QF"
        );
        assert_eq!(
            preparation.addresses.curve_vault.to_string(),
            "9sXf9hAtryY1mncMxKGZnLMJzQbnTsUoSu8GJTX3FpFh"
        );
        assert_eq!(
            preparation.addresses.canonical_pool.to_string(),
            "FFgT2bSo5xrGs5uHyRY7xztL8hntvwuswGM8iYLrdBgx"
        );
        assert_eq!(preparation.completing_buy_amount, 216_645_197_009);
        assert_eq!(preparation.migration_reserve, 206_900_000_000_000);
        assert_eq!(preparation.token_mint, mint);
        assert_eq!(
            preparation
                .scenario
                .overrides
                .iter()
                .map(|item| item.template_id.as_str())
                .collect::<Vec<_>>(),
            [
                "pump-bonding-curve-custom",
                "pump-token-2022-curve-balance",
                "pump-global",
            ]
        );
        assert!(
            preparation
                .scenario
                .overrides
                .iter()
                .all(|item| item.scenario_relative_slot == GRADUATION_PREPARATION_SLOT)
        );
    }

    #[test]
    fn rejects_a_mint_with_an_existing_pool() {
        let snapshot: BTreeMap<String, Option<AccountSnapshot>> = serde_json::from_str(
            include_str!("../tests/assets/pump_token2022_graduation.snapshot.json"),
        )
        .unwrap();
        let mint = Pubkey::from_str_const("HRTzNRJNnY78xe8e4a9DuMotw6qA97GwSQLzpVw9pump");
        let addresses = pump_graduation_addresses(&mint);
        let pool = Account::default();

        assert!(
            build_pump_graduation_scenario(
                mint,
                &fixture_account(&snapshot, &mint),
                &fixture_account(&snapshot, &addresses.bonding_curve),
                &fixture_account(&snapshot, &addresses.curve_vault),
                Some(&pool),
                &fixture_account(&snapshot, &addresses.global),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_non_sol_quoted_bonding_curve() {
        let snapshot: BTreeMap<String, Option<AccountSnapshot>> = serde_json::from_str(
            include_str!("../tests/assets/pump_token2022_graduation.snapshot.json"),
        )
        .unwrap();
        let mint = Pubkey::from_str_const("HRTzNRJNnY78xe8e4a9DuMotw6qA97GwSQLzpVw9pump");
        let addresses = pump_graduation_addresses(&mint);
        let mut curve_account = fixture_account(&snapshot, &addresses.bonding_curve);
        let usdc_mint = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        curve_account.data[QUOTE_MINT_OFFSET..QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(usdc_mint.as_ref());

        let error = build_pump_graduation_scenario(
            mint,
            &fixture_account(&snapshot, &mint),
            &curve_account,
            &fixture_account(&snapshot, &addresses.curve_vault),
            None,
            &fixture_account(&snapshot, &addresses.global),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Pump graduation preset supports SOL-quoted bonding curves only")
        );
    }

    #[test]
    fn graduation_uses_the_supplied_curve_state() {
        let snapshot: BTreeMap<String, Option<AccountSnapshot>> = serde_json::from_str(
            include_str!("../tests/assets/pump_token2022_graduation.snapshot.json"),
        )
        .unwrap();
        let mint = Pubkey::from_str_const("HRTzNRJNnY78xe8e4a9DuMotw6qA97GwSQLzpVw9pump");
        let addresses = pump_graduation_addresses(&mint);
        let remote_curve = fixture_account(&snapshot, &addresses.bonding_curve);
        let global_account = fixture_account(&snapshot, &addresses.global);
        let mut modified_curve = remote_curve.clone();
        let virtual_quote_reserves = read_u64(
            &modified_curve.data,
            VIRTUAL_QUOTE_RESERVES_OFFSET,
            "virtual_quote_reserves",
        )
        .unwrap();
        modified_curve.data[VIRTUAL_QUOTE_RESERVES_OFFSET..VIRTUAL_QUOTE_RESERVES_OFFSET + 8]
            .copy_from_slice(&(virtual_quote_reserves * 2).to_le_bytes());
        let original = build_pump_graduation_scenario(
            mint,
            &fixture_account(&snapshot, &mint),
            &remote_curve,
            &fixture_account(&snapshot, &addresses.curve_vault),
            None,
            &global_account,
        )
        .unwrap();
        let modified = build_pump_graduation_scenario(
            mint,
            &fixture_account(&snapshot, &mint),
            &modified_curve,
            &fixture_account(&snapshot, &addresses.curve_vault),
            None,
            &global_account,
        )
        .unwrap();
        assert_ne!(
            modified.completing_buy_amount,
            original.completing_buy_amount
        );
    }
}
