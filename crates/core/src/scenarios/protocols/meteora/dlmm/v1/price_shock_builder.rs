use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};
use txtx_addon_kit::types::types::Value;
use txtx_addon_network_svm_types::{
    SvmValue, idl::parse_bytes_to_value_with_expected_idl_type_def_ty,
};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

pub const METEORA_DLMM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");

const POOL_STATE_TEMPLATE_ID: &str = "meteora-dlmm-pool-state";
const POOL_STATE_ACCOUNT_NAME: &str = "LbPair";
const LB_PAIR_LEN: usize = 904;
const PRICE_SHOCK_SLOT: u64 = 1;

const BIN_ARRAY_SEED: &[u8] = b"bin_array";
const BINS_PER_ARRAY: i32 = 70;
const BIN_ARRAY_LEN: usize = 10136;
const BIN_ARRAY_DISCRIMINATOR: [u8; 8] = [92, 142, 92, 220, 5, 148, 70, 181];

/// What the shock will do, worked out from the pool account alone. Holds the bin array the
/// shocked price lands on so the caller can read it before committing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DlmmPriceShockPlan {
    pub pool: Pubkey,
    pub price_factor: f64,
    pub bin_step: u16,
    pub old_active_id: i32,
    pub new_active_id: i32,
    pub bin_array: Pubkey,
    pub bin_array_index: i64,
}

/// Index of the bin array a bin sits in. The division floors, so bin -2222 lives in array -32,
/// not -31.
pub fn bin_array_index(active_id: i32) -> i64 {
    active_id.div_euclid(BINS_PER_ARRAY) as i64
}

/// `["bin_array", lb_pair, index as i64 LE]`.
pub fn bin_array_address(pool: &Pubkey, index: i64) -> Pubkey {
    Pubkey::find_program_address(
        &[BIN_ARRAY_SEED, pool.as_ref(), &index.to_le_bytes()],
        &METEORA_DLMM_PROGRAM_ID,
    )
    .0
}

/// `round(ln(price_factor) / ln(1 + bin_step / 10000))`.
pub fn active_id_delta(price_factor: f64, bin_step: u16) -> SurfpoolResult<i32> {
    let factor_per_bin = 1.0 + f64::from(bin_step) / 10000.0;
    let delta = (price_factor.ln() / factor_per_bin.ln()).round();
    if !delta.is_finite() || delta < i32::MIN as f64 || delta > i32::MAX as f64 {
        return Err(invalid(format!(
            "price factor moves the active bin by {delta}, outside what an i32 bin id can hold"
        )));
    }
    Ok(delta as i32)
}

fn factor_to_reach(active_id: i32, target_active_id: i32, bin_step: u16) -> f64 {
    let factor_per_bin = 1.0 + f64::from(bin_step) / 10000.0;
    factor_per_bin.powi(target_active_id - active_id)
}

/// Checks the one input that needs no account, so a caller can reject it before spending a read.
pub fn validate_price_factor(price_factor: f64) -> SurfpoolResult<()> {
    if !price_factor.is_finite() {
        return Err(invalid("price factor must be a finite number"));
    }
    if price_factor <= 0.0 {
        return Err(invalid("price factor must be greater than zero"));
    }
    if price_factor == 1.0 {
        return Err(invalid("price factor of 1 would leave the pool unchanged"));
    }
    Ok(())
}

/// Works out the shock from the pool account, without deciding whether it is safe yet.
pub fn plan_price_shock(
    pool: Pubkey,
    pool_account: &Account,
    price_factor: f64,
) -> SurfpoolResult<DlmmPriceShockPlan> {
    validate_price_factor(price_factor)?;
    if pool_account.owner != METEORA_DLMM_PROGRAM_ID {
        return Err(invalid(format!(
            "{pool} is owned by {}, not the Meteora DLMM program {METEORA_DLMM_PROGRAM_ID}",
            pool_account.owner
        )));
    }
    if pool_account.data.len() != LB_PAIR_LEN {
        return Err(invalid(format!(
            "{pool} is {} bytes, not the {LB_PAIR_LEN} bytes of an LbPair",
            pool_account.data.len()
        )));
    }

    let lb_pair = decode_lb_pair(&pool_account.data)?;
    let bin_step: u16 = number(&lb_pair, "bin_step")?;
    if bin_step == 0 {
        return Err(invalid("pool declares a bin step of zero"));
    }
    let old_active_id: i32 = number(&lb_pair, "active_id")?;

    let delta = active_id_delta(price_factor, bin_step)?;
    let new_active_id = old_active_id
        .checked_add(delta)
        .ok_or_else(|| invalid("shocked active bin id overflows an i32"))?;
    let index = bin_array_index(new_active_id);

    Ok(DlmmPriceShockPlan {
        pool,
        price_factor,
        bin_step,
        old_active_id,
        new_active_id,
        bin_array: bin_array_address(&pool, index),
        bin_array_index: index,
    })
}

/// Turns a plan into a scenario once the bin array covering the new active id exists. A missing
/// array is rejected here rather than left to fail later at swap time.
pub fn build_price_shock_scenario(
    plan: DlmmPriceShockPlan,
    bin_array_account: Option<&Account>,
) -> SurfpoolResult<Scenario> {
    match bin_array_account {
        None => return Err(invalid(missing_bin_array_message(&plan))),
        Some(account) => {
            if account.owner != METEORA_DLMM_PROGRAM_ID {
                return Err(invalid(format!(
                    "bin array {} is owned by {}, not the Meteora DLMM program",
                    plan.bin_array, account.owner
                )));
            }
            if account.data.len() != BIN_ARRAY_LEN {
                return Err(invalid(format!(
                    "bin array {} is {} bytes, not the {BIN_ARRAY_LEN} bytes of a BinArray",
                    plan.bin_array,
                    account.data.len()
                )));
            }
            if account.data.get(..8) != Some(BIN_ARRAY_DISCRIMINATOR.as_slice()) {
                return Err(invalid(format!(
                    "bin array {} does not carry the BinArray discriminator",
                    plan.bin_array
                )));
            }
        }
    }

    let registry = TemplateRegistry::new();
    let template = registry.get(POOL_STATE_TEMPLATE_ID).ok_or_else(|| {
        SurfpoolError::internal(format!("{POOL_STATE_TEMPLATE_ID} template is unavailable"))
    })?;

    let values = HashMap::from([(
        "active_id".to_string(),
        serde_json::json!(plan.new_active_id),
    )]);
    let mut pool_override = OverrideInstance::new(
        template.id.clone(),
        PRICE_SHOCK_SLOT,
        AccountAddress::Pubkey(plan.pool.to_string()),
    )
    .with_values(values)
    .with_label(format!("DLMM price x{}", plan.price_factor));
    // Unset fields (liquidity, fees, reserves) must come from the live pool, so fetch before override.
    pool_override.fetch_before_use = true;

    let mut scenario = Scenario::new(
        "Meteora DLMM Price Shock".to_string(),
        format!(
            "Move Meteora DLMM pool {} to {}x its price, onto bin {} of the bin array at index {}.",
            plan.pool, plan.price_factor, plan.new_active_id, plan.bin_array_index
        ),
    );
    scenario.tags = vec![
        "meteora".to_string(),
        "dlmm".to_string(),
        "price-shock".to_string(),
    ];
    scenario.add_override(pool_override);

    Ok(scenario)
}

fn missing_bin_array_message(plan: &DlmmPriceShockPlan) -> String {
    let current_index = bin_array_index(plan.old_active_id);
    let array_start = (current_index as i32) * BINS_PER_ARRAY;
    let array_end = array_start + BINS_PER_ARRAY - 1;
    let (bound_active_id, adjective) = if plan.price_factor > 1.0 {
        (array_end, "largest")
    } else {
        (array_start, "smallest")
    };
    let safe_factor = factor_to_reach(plan.old_active_id, bound_active_id, plan.bin_step);
    format!(
        "bin array {} (index {}) does not exist, so a swap could not resume from bin {}. The \
         {adjective} factor that stays on the pool's current bin array [{array_start}, \
         {array_end}] is {safe_factor:.6}.",
        plan.bin_array, plan.bin_array_index, plan.new_active_id
    )
}

/// Decodes with the same IDL codec the materializer writes with, so a drifted IDL fails here.
fn decode_lb_pair(data: &[u8]) -> SurfpoolResult<Value> {
    let registry = TemplateRegistry::new();
    let template = registry.get(POOL_STATE_TEMPLATE_ID).ok_or_else(|| {
        SurfpoolError::internal(format!("{POOL_STATE_TEMPLATE_ID} template is unavailable"))
    })?;
    let account_def = template
        .idl
        .accounts
        .iter()
        .find(|account| account.name == POOL_STATE_ACCOUNT_NAME)
        .ok_or_else(|| {
            SurfpoolError::internal(format!(
                "{POOL_STATE_ACCOUNT_NAME} is not in the bundled IDL"
            ))
        })?;
    let discriminator = account_def.discriminator.as_slice();
    if data.len() < discriminator.len() || &data[..discriminator.len()] != discriminator {
        return Err(invalid(
            "account does not carry the LbPair discriminator declared by the bundled IDL",
        ));
    }
    let type_def = template
        .idl
        .types
        .iter()
        .find(|ty| ty.name == POOL_STATE_ACCOUNT_NAME)
        .ok_or_else(|| {
            SurfpoolError::internal(format!(
                "{POOL_STATE_ACCOUNT_NAME} has no type definition in the bundled IDL"
            ))
        })?;

    parse_bytes_to_value_with_expected_idl_type_def_ty(
        &data[discriminator.len()..],
        &type_def.ty,
        &template.idl.types,
        &vec![],
        &type_def.generics,
    )
    .map_err(|error| invalid(format!("LbPair failed to decode against the IDL: {error}")))
}

fn number<T: txtx_addon_network_svm_types::ValueNumber>(
    lb_pair: &Value,
    field: &str,
) -> SurfpoolResult<T> {
    let value = lb_pair
        .as_object()
        .and_then(|fields| fields.get(field))
        .ok_or_else(|| invalid(format!("LbPair has no {field} field")))?;
    SvmValue::to_number::<T>(value)
        .map_err(|error| invalid(format!("LbPair {field} is not readable: {error}")))
}

fn invalid(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::invalid_params(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE_ID_OFFSET: usize = 76;
    const BIN_STEP_OFFSET: usize = 80;

    fn pool_account(active_id: i32, bin_step: u16) -> Account {
        let registry = TemplateRegistry::new();
        let template = registry.get(POOL_STATE_TEMPLATE_ID).unwrap();
        let discriminator = template
            .idl
            .accounts
            .iter()
            .find(|account| account.name == POOL_STATE_ACCOUNT_NAME)
            .unwrap()
            .discriminator
            .clone();

        let mut data = vec![0u8; LB_PAIR_LEN];
        data[..discriminator.len()].copy_from_slice(&discriminator);
        data[ACTIVE_ID_OFFSET..ACTIVE_ID_OFFSET + 4].copy_from_slice(&active_id.to_le_bytes());
        data[BIN_STEP_OFFSET..BIN_STEP_OFFSET + 2].copy_from_slice(&bin_step.to_le_bytes());
        Account {
            lamports: 1,
            data,
            owner: METEORA_DLMM_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn bin_array_account() -> Account {
        let mut data = vec![0u8; BIN_ARRAY_LEN];
        data[..8].copy_from_slice(&BIN_ARRAY_DISCRIMINATOR);
        Account {
            lamports: 1,
            data,
            owner: METEORA_DLMM_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn ten_percent_up_at_bin_step_ten_moves_ninety_five_bins() {
        assert_eq!(active_id_delta(1.1, 10).unwrap(), 95);
    }

    #[test]
    fn halving_the_price_at_bin_step_ten_moves_down_six_hundred_ninety_three_bins() {
        assert_eq!(active_id_delta(0.5, 10).unwrap(), -693);
    }

    #[test]
    fn quadrupling_the_price_at_bin_step_ten_moves_up_thirteen_hundred_eighty_seven_bins() {
        assert_eq!(active_id_delta(4.0, 10).unwrap(), 1387);
    }

    #[test]
    fn delta_and_price_stay_coupled_over_a_range_of_factors_and_bin_steps() {
        for bin_step in [1u16, 10, 25, 100] {
            for factor in [0.01, 0.25, 0.5, 0.9, 1.1, 2.0, 4.0, 100.0] {
                let delta = active_id_delta(factor, bin_step).unwrap();
                let factor_per_bin = 1.0 + f64::from(bin_step) / 10000.0;
                let achieved = factor_per_bin.powi(delta);
                let lower = factor_per_bin.powf(delta as f64 - 0.5);
                let upper = factor_per_bin.powf(delta as f64 + 0.5);
                assert!(
                    achieved >= lower * (1.0 - 1e-9) && achieved <= upper * (1.0 + 1e-9),
                    "delta {delta} does not round ln({factor}) / ln({factor_per_bin}) at bin step {bin_step}"
                );
            }
        }
    }

    #[test]
    fn bin_array_index_floors_for_negative_bins() {
        assert_eq!(bin_array_index(-2222), -32);
        assert_eq!(bin_array_index(-2210), -32);
        assert_eq!(bin_array_index(-2170), -31);
        assert_eq!(bin_array_index(69), 0);
        assert_eq!(bin_array_index(70), 1);
    }

    #[test]
    fn plan_reads_the_pool_through_the_idl() {
        let account = pool_account(-2222, 10);
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(pool, &account, 1.1).unwrap();

        assert_eq!(plan.old_active_id, -2222);
        assert_eq!(plan.bin_step, 10);
        assert_eq!(plan.new_active_id, -2222 + 95);
        assert_eq!(plan.bin_array_index, bin_array_index(plan.new_active_id));
        assert_eq!(
            plan.bin_array,
            bin_array_address(&pool, plan.bin_array_index)
        );
    }

    #[test]
    fn plan_rejects_bad_factors_and_foreign_accounts() {
        let pool = Pubkey::new_unique();
        let account = pool_account(0, 10);
        for factor in [0.0, -1.0, 1.0, f64::NAN, f64::INFINITY] {
            assert!(plan_price_shock(pool, &account, factor).is_err());
        }

        let mut foreign = account.clone();
        foreign.owner = Pubkey::new_unique();
        assert!(plan_price_shock(pool, &foreign, 2.0).is_err());

        let mut truncated = account.clone();
        truncated.data.truncate(100);
        assert!(plan_price_shock(pool, &truncated, 2.0).is_err());

        let mut wrong_discriminator = account;
        wrong_discriminator.data[0] ^= 0xff;
        assert!(plan_price_shock(pool, &wrong_discriminator, 2.0).is_err());
    }

    #[test]
    fn plan_rejects_a_zero_bin_step() {
        let pool = Pubkey::new_unique();
        let account = pool_account(0, 0);
        let error = plan_price_shock(pool, &account, 2.0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bin step of zero"), "{error}");
    }

    #[test]
    fn scenario_overrides_active_id_at_slot_one() {
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(pool, &pool_account(-2222, 10), 1.1).unwrap();
        let scenario = build_price_shock_scenario(plan, Some(&bin_array_account())).unwrap();

        assert_eq!(scenario.overrides.len(), 1);
        let instance = &scenario.overrides[0];
        assert_eq!(instance.template_id, POOL_STATE_TEMPLATE_ID);
        assert_eq!(instance.scenario_relative_slot, 1);
        assert!(instance.fetch_before_use);
        assert_eq!(
            instance.account,
            AccountAddress::Pubkey(pool.to_string()),
            "the shock must land on the pool the caller named"
        );
        assert_eq!(
            instance.values.get("active_id"),
            Some(&serde_json::json!(plan.new_active_id))
        );
    }

    #[test]
    fn missing_bin_array_is_rejected_with_a_safe_factor() {
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(pool, &pool_account(-2222, 10), 1.1).unwrap();
        let error = build_price_shock_scenario(plan, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains(&plan.bin_array.to_string()), "{error}");
        assert!(error.contains("largest factor"), "{error}");

        let wrong_size = Account {
            data: vec![0u8; 10],
            ..bin_array_account()
        };
        assert!(build_price_shock_scenario(plan, Some(&wrong_size)).is_err());

        let wrong_discriminator = {
            let mut account = bin_array_account();
            account.data[0] ^= 0xff;
            account
        };
        assert!(build_price_shock_scenario(plan, Some(&wrong_discriminator)).is_err());
    }
}
