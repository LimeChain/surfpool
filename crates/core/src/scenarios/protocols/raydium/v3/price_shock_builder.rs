use std::collections::HashMap;

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, OverrideTemplate, Scenario};
use txtx_addon_kit::types::types::Value;
use txtx_addon_network_svm_types::{
    SvmValue, idl::parse_bytes_to_value_with_expected_idl_type_def_ty,
};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

/// A deployment of the Raydium CLMM program: the layout, seeds and bounds are shared, only these differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClmmProgram {
    pub program_id: Pubkey,
    pub pool_state_template: &'static str,
}

pub const RAYDIUM: ClmmProgram = ClmmProgram {
    program_id: Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK"),
    pool_state_template: "raydium-clmm-pool-state",
};

const POOL_STATE_ACCOUNT_NAME: &str = "PoolState";
const POOL_STATE_LEN: usize = 1544;
const PRICE_SHOCK_SLOT: u64 = 1;

const TICK_ARRAY_SEED: &[u8] = b"tick_array";
const TICK_ARRAY_SIZE: i32 = 60;
const TICK_ARRAY_LEN: usize = 10240;

const MIN_TICK: i32 = -443636;
const MAX_TICK: i32 = 443636;
const MIN_SQRT_PRICE_X64: u128 = 4295048016;
const MAX_SQRT_PRICE_X64: u128 = 79226673521066979257578248091;

const Q64: f64 = 18446744073709551616.0;
const TICK_BASE: f64 = 1.0001;

/// What the shock will do, worked out from the pool account alone. Holds the tick array the
/// shocked price lands on so the caller can read it before committing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClmmPriceShockPlan {
    pub program: ClmmProgram,
    pub pool: Pubkey,
    pub price_factor: f64,
    pub tick_spacing: u16,
    pub old_sqrt_price_x64: u128,
    pub new_sqrt_price_x64: u128,
    pub old_tick_current: i32,
    pub new_tick_current: i32,
    pub tick_array: Pubkey,
    pub tick_array_start_index: i32,
}

/// Start index of the tick array a tick sits on: integer division rounded towards negative
/// infinity, times the array's tick span.
pub fn tick_array_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let ticks_in_array = TICK_ARRAY_SIZE * i32::from(tick_spacing);
    let mut start = tick / ticks_in_array;
    if tick < 0 && tick % ticks_in_array != 0 {
        start -= 1;
    }
    start * ticks_in_array
}

/// `TickArrayState::key`: `["tick_array", pool, start_tick_index as i32 big-endian]`.
pub fn tick_array_address(program_id: &Pubkey, pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TICK_ARRAY_SEED,
            pool.as_ref(),
            &start_tick_index.to_be_bytes(),
        ],
        program_id,
    )
    .0
}

/// `sqrt_price_x64 * sqrt(price_factor)`, kept in Q64.64. f64 is exact to ~1e-16 here, far
/// below the 1e-4 relative tick step.
pub fn shocked_sqrt_price_x64(sqrt_price_x64: u128, price_factor: f64) -> SurfpoolResult<u128> {
    let shocked = (sqrt_price_x64 as f64) * price_factor.sqrt();
    if !shocked.is_finite() {
        return Err(invalid("shocked price is not a finite number"));
    }
    let rounded = shocked.round();
    if rounded < MIN_SQRT_PRICE_X64 as f64 || rounded >= MAX_SQRT_PRICE_X64 as f64 {
        return Err(invalid(format!(
            "price factor moves sqrt_price_x64 to {rounded:.0}, outside the range the program accepts [{MIN_SQRT_PRICE_X64}, {MAX_SQRT_PRICE_X64})"
        )));
    }
    Ok(rounded as u128)
}

/// `floor(log_1.0001((sqrt_price_x64 / 2^64)^2))`.
pub fn tick_at_sqrt_price_x64(sqrt_price_x64: u128) -> SurfpoolResult<i32> {
    let ratio = (sqrt_price_x64 as f64) / Q64;
    let tick = (2.0 * ratio.ln() / TICK_BASE.ln()).floor();
    if !tick.is_finite() || tick < MIN_TICK as f64 || tick > MAX_TICK as f64 {
        return Err(invalid(format!(
            "shocked price lands on tick {tick:.0}, outside [{MIN_TICK}, {MAX_TICK}]"
        )));
    }
    Ok(tick as i32)
}

fn factor_to_reach(tick_current: i32, target_tick: i32) -> f64 {
    TICK_BASE.powi(target_tick - tick_current)
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
    program: ClmmProgram,
    pool: Pubkey,
    pool_account: &Account,
    price_factor: f64,
) -> SurfpoolResult<ClmmPriceShockPlan> {
    validate_price_factor(price_factor)?;
    if pool_account.owner != program.program_id {
        return Err(invalid(format!(
            "{pool} is owned by {}, not the CLMM program {}",
            pool_account.owner, program.program_id
        )));
    }
    if pool_account.data.len() != POOL_STATE_LEN {
        return Err(invalid(format!(
            "{pool} is {} bytes, not the {POOL_STATE_LEN} bytes of a PoolState",
            pool_account.data.len()
        )));
    }

    let pool_state = decode_pool_state(program, &pool_account.data)?;
    let tick_spacing: u16 = number(&pool_state, "tick_spacing")?;
    if tick_spacing == 0 {
        return Err(invalid("pool declares a tick spacing of zero"));
    }
    let old_sqrt_price_x64: u128 = number(&pool_state, "sqrt_price_x64")?;
    let old_tick_current: i32 = number(&pool_state, "tick_current")?;

    let new_sqrt_price_x64 = shocked_sqrt_price_x64(old_sqrt_price_x64, price_factor)?;
    let new_tick_current = tick_at_sqrt_price_x64(new_sqrt_price_x64)?;
    let start_index = tick_array_start_index(new_tick_current, tick_spacing);

    Ok(ClmmPriceShockPlan {
        program,
        pool,
        price_factor,
        tick_spacing,
        old_sqrt_price_x64,
        new_sqrt_price_x64,
        old_tick_current,
        new_tick_current,
        tick_array: tick_array_address(&program.program_id, &pool, start_index),
        tick_array_start_index: start_index,
    })
}

/// Turns a plan into a scenario once the tick array covering the new tick exists. A missing
/// array is rejected here rather than left to fail later at swap time.
pub fn build_price_shock_scenario(
    plan: ClmmPriceShockPlan,
    tick_array_account: Option<&Account>,
) -> SurfpoolResult<Scenario> {
    match tick_array_account {
        None => return Err(invalid(missing_tick_array_message(&plan))),
        Some(account) => {
            if account.owner != plan.program.program_id {
                return Err(invalid(format!(
                    "tick array {} is owned by {}, not the CLMM program {}",
                    plan.tick_array, account.owner, plan.program.program_id
                )));
            }
            if account.data.len() != TICK_ARRAY_LEN {
                return Err(invalid(format!(
                    "tick array {} is {} bytes, not the {TICK_ARRAY_LEN} bytes of a TickArrayState",
                    plan.tick_array,
                    account.data.len()
                )));
            }
        }
    }

    let registry = TemplateRegistry::new();
    let template = pool_state_template(&registry, plan.program)?;

    let values = HashMap::from([
        (
            "sqrt_price_x64".to_string(),
            serde_json::json!(plan.new_sqrt_price_x64.to_string()),
        ),
        (
            "tick_current".to_string(),
            serde_json::json!(plan.new_tick_current),
        ),
    ]);
    let mut pool_override = OverrideInstance::new(
        template.id.clone(),
        PRICE_SHOCK_SLOT,
        AccountAddress::Pubkey(plan.pool.to_string()),
    )
    .with_values(values)
    .with_label(format!("CLMM price x{}", plan.price_factor));
    // Unset fields (liquidity, vaults, fee growth) must come from the live pool, so fetch before override.
    pool_override.fetch_before_use = true;

    let mut scenario = Scenario::new(
        format!("{} CLMM Price Shock", template.protocol),
        format!(
            "Move {} CLMM pool {} to {}x its price, onto tick {} of the tick array starting at {}.",
            template.protocol,
            plan.pool,
            plan.price_factor,
            plan.new_tick_current,
            plan.tick_array_start_index
        ),
    );
    scenario.tags = vec![
        template.protocol.to_lowercase(),
        "clmm".to_string(),
        "price-shock".to_string(),
    ];
    scenario.add_override(pool_override);

    Ok(scenario)
}

fn pool_state_template(
    registry: &TemplateRegistry,
    program: ClmmProgram,
) -> SurfpoolResult<&OverrideTemplate> {
    registry.get(program.pool_state_template).ok_or_else(|| {
        SurfpoolError::internal(format!(
            "{} template is unavailable",
            program.pool_state_template
        ))
    })
}

fn missing_tick_array_message(plan: &ClmmPriceShockPlan) -> String {
    let ticks_in_array = TICK_ARRAY_SIZE * i32::from(plan.tick_spacing);
    let current_start = tick_array_start_index(plan.old_tick_current, plan.tick_spacing);
    let (bound_tick, adjective) = if plan.price_factor > 1.0 {
        (current_start + ticks_in_array - 1, "largest")
    } else {
        (current_start, "smallest")
    };
    let safe_factor = factor_to_reach(plan.old_tick_current, bound_tick);
    format!(
        "tick array {} (start index {}) does not exist, so a swap could not resume from tick {}. \
         The {adjective} factor that stays on the pool's current tick array [{}, {}] is {:.6}.",
        plan.tick_array,
        plan.tick_array_start_index,
        plan.new_tick_current,
        current_start,
        current_start + ticks_in_array - 1,
        safe_factor
    )
}

/// Decodes with the same IDL codec the materializer writes with, so a drifted IDL fails here.
fn decode_pool_state(program: ClmmProgram, data: &[u8]) -> SurfpoolResult<Value> {
    let registry = TemplateRegistry::new();
    let template = pool_state_template(&registry, program)?;
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
            "account does not carry the PoolState discriminator declared by the bundled IDL",
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
    .map_err(|error| {
        invalid(format!(
            "PoolState failed to decode against the IDL: {error}"
        ))
    })
}

fn number<T: txtx_addon_network_svm_types::ValueNumber>(
    pool_state: &Value,
    field: &str,
) -> SurfpoolResult<T> {
    let value = pool_state
        .as_object()
        .and_then(|fields| fields.get(field))
        .ok_or_else(|| invalid(format!("PoolState has no {field} field")))?;
    SvmValue::to_number::<T>(value)
        .map_err(|error| invalid(format!("PoolState {field} is not readable: {error}")))
}

fn invalid(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::invalid_params(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQRT_PRICE_ONE: u128 = 1 << 64;

    fn pool_account(sqrt_price_x64: u128, tick_current: i32, tick_spacing: u16) -> Account {
        let registry = TemplateRegistry::new();
        let template = registry.get(RAYDIUM.pool_state_template).unwrap();
        let discriminator = template
            .idl
            .accounts
            .iter()
            .find(|account| account.name == POOL_STATE_ACCOUNT_NAME)
            .unwrap()
            .discriminator
            .clone();

        let mut data = vec![0u8; POOL_STATE_LEN];
        data[..discriminator.len()].copy_from_slice(&discriminator);
        data[235..237].copy_from_slice(&tick_spacing.to_le_bytes());
        data[253..269].copy_from_slice(&sqrt_price_x64.to_le_bytes());
        data[269..273].copy_from_slice(&tick_current.to_le_bytes());
        Account {
            lamports: 1,
            data,
            owner: RAYDIUM.program_id,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn tick_array_account() -> Account {
        Account {
            lamports: 1,
            data: vec![0u8; TICK_ARRAY_LEN],
            owner: RAYDIUM.program_id,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn halving_the_price_halves_it_to_within_a_billionth() {
        let shocked = shocked_sqrt_price_x64(SQRT_PRICE_ONE, 0.5).unwrap();
        let ratio = (shocked as f64) / (SQRT_PRICE_ONE as f64);
        let expected = std::f64::consts::FRAC_1_SQRT_2;
        assert!(
            ((ratio - expected).abs() / expected) < 1e-9,
            "sqrt factor {ratio} is not sqrt(0.5) to within 1e-9"
        );
        assert_eq!(tick_at_sqrt_price_x64(shocked).unwrap(), -6932);
    }

    #[test]
    fn quadrupling_the_price_doubles_the_sqrt_price() {
        let shocked = shocked_sqrt_price_x64(SQRT_PRICE_ONE, 4.0).unwrap();
        let ratio = (shocked as f64) / (SQRT_PRICE_ONE as f64);
        assert!(
            ((ratio - 2.0).abs() / 2.0) < 1e-9,
            "sqrt factor {ratio} is not 2.0 to within 1e-9"
        );
        assert_eq!(tick_at_sqrt_price_x64(shocked).unwrap(), 13863);
    }

    #[test]
    fn shocked_price_and_tick_stay_coupled_over_a_range_of_factors() {
        for factor in [0.01, 0.25, 0.5, 0.9, 1.1, 2.0, 4.0, 100.0] {
            let shocked = shocked_sqrt_price_x64(SQRT_PRICE_ONE, factor).unwrap();
            let tick = tick_at_sqrt_price_x64(shocked).unwrap();
            let price = ((shocked as f64) / Q64).powi(2);
            assert!(
                TICK_BASE.powi(tick) <= price * (1.0 + 1e-9)
                    && price < TICK_BASE.powi(tick + 1) * (1.0 + 1e-9),
                "tick {tick} does not bracket price {price} for factor {factor}"
            );
        }
    }

    #[test]
    fn tick_array_start_index_matches_the_program() {
        assert_eq!(tick_array_start_index(-600, 15), -900);
        assert_eq!(tick_array_start_index(-600, 10), -600);
        assert_eq!(tick_array_start_index(-600, 60), -3600);
        assert_eq!(tick_array_start_index(600, 15), 0);
        assert_eq!(tick_array_start_index(600, 10), 600);
        assert_eq!(tick_array_start_index(600, 60), 0);
        assert_eq!(tick_array_start_index(0, 60), 0);
        assert_eq!(tick_array_start_index(-1, 60), -3600);
    }

    #[test]
    fn plan_reads_the_pool_through_the_idl() {
        let account = pool_account(SQRT_PRICE_ONE, 0, 1);
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(RAYDIUM, pool, &account, 4.0).unwrap();

        assert_eq!(plan.old_sqrt_price_x64, SQRT_PRICE_ONE);
        assert_eq!(plan.old_tick_current, 0);
        assert_eq!(plan.tick_spacing, 1);
        assert_eq!(plan.new_tick_current, 13863);
        assert_eq!(plan.tick_array_start_index, 13860);
        assert_eq!(
            plan.tick_array,
            tick_array_address(&RAYDIUM.program_id, &pool, plan.tick_array_start_index)
        );
    }

    #[test]
    fn plan_rejects_bad_factors_and_foreign_accounts() {
        let pool = Pubkey::new_unique();
        let account = pool_account(SQRT_PRICE_ONE, 0, 1);
        for factor in [0.0, -1.0, 1.0, f64::NAN, f64::INFINITY] {
            assert!(plan_price_shock(RAYDIUM, pool, &account, factor).is_err());
        }

        let mut foreign = account.clone();
        foreign.owner = Pubkey::new_unique();
        assert!(plan_price_shock(RAYDIUM, pool, &foreign, 2.0).is_err());

        let mut truncated = account.clone();
        truncated.data.truncate(100);
        assert!(plan_price_shock(RAYDIUM, pool, &truncated, 2.0).is_err());

        let mut wrong_discriminator = account;
        wrong_discriminator.data[0] ^= 0xff;
        assert!(plan_price_shock(RAYDIUM, pool, &wrong_discriminator, 2.0).is_err());
    }

    #[test]
    fn scenario_overrides_both_coupled_fields_at_slot_one() {
        let pool = Pubkey::new_unique();
        let plan =
            plan_price_shock(RAYDIUM, pool, &pool_account(SQRT_PRICE_ONE, 0, 1), 4.0).unwrap();
        let scenario = build_price_shock_scenario(plan, Some(&tick_array_account())).unwrap();

        assert_eq!(scenario.overrides.len(), 1);
        let instance = &scenario.overrides[0];
        assert_eq!(instance.template_id, RAYDIUM.pool_state_template);
        assert_eq!(instance.scenario_relative_slot, 1);
        assert!(instance.fetch_before_use);
        assert_eq!(
            instance.account,
            AccountAddress::Pubkey(pool.to_string()),
            "the shock must land on the pool the caller named"
        );
        assert_eq!(
            instance.values.get("tick_current"),
            Some(&serde_json::json!(13863))
        );
        assert_eq!(
            instance.values.get("sqrt_price_x64"),
            Some(&serde_json::json!(plan.new_sqrt_price_x64.to_string()))
        );
    }

    #[test]
    fn missing_tick_array_is_rejected_with_a_safe_factor() {
        let pool = Pubkey::new_unique();
        let plan =
            plan_price_shock(RAYDIUM, pool, &pool_account(SQRT_PRICE_ONE, 0, 1), 4.0).unwrap();
        let error = build_price_shock_scenario(plan, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains(&plan.tick_array.to_string()), "{error}");
        assert!(error.contains("largest factor"), "{error}");

        let wrong_size = Account {
            data: vec![0u8; 10],
            ..tick_array_account()
        };
        assert!(build_price_shock_scenario(plan, Some(&wrong_size)).is_err());
    }
}
