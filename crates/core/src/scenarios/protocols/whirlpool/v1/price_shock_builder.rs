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

pub const WHIRLPOOL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");

const POOL_STATE_TEMPLATE_ID: &str = "whirlpool-pool-state";
const POOL_STATE_ACCOUNT_NAME: &str = "Whirlpool";
const POOL_STATE_LEN: usize = 653;
const PRICE_SHOCK_SLOT: u64 = 1;

const TICK_ARRAY_SEED: &[u8] = b"tick_array";
const TICK_ARRAY_SIZE: i32 = 88;
const TICK_ARRAY_LEN: usize = 9988;

const MIN_TICK: i32 = -443636;
const MAX_TICK: i32 = 443636;
const MIN_SQRT_PRICE_X64: u128 = 4295048016;
const MAX_SQRT_PRICE_X64: u128 = 79226673515401279992447579055;

const Q64: f64 = 18446744073709551616.0;
const TICK_BASE: f64 = 1.0001;

/// What the shock will do, worked out from the pool account alone. Holds the tick array the
/// shocked price lands on so the caller can read it before committing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhirlpoolPriceShockPlan {
    pub pool: Pubkey,
    pub price_factor: f64,
    pub tick_spacing: u16,
    pub old_sqrt_price: u128,
    pub new_sqrt_price: u128,
    pub old_tick_current_index: i32,
    pub new_tick_current_index: i32,
    pub tick_array: Pubkey,
    pub tick_array_start_index: i32,
}

/// Start index of the tick array a tick sits on: floor division towards negative infinity, times
/// the array's tick span (`div_euclid` gives exactly that for a positive divisor).
pub fn tick_array_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let ticks_in_array = TICK_ARRAY_SIZE * i32::from(tick_spacing);
    tick.div_euclid(ticks_in_array) * ticks_in_array
}

/// `["tick_array", whirlpool, start_index.to_string()]`.
pub fn tick_array_address(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TICK_ARRAY_SEED,
            pool.as_ref(),
            start_tick_index.to_string().as_bytes(),
        ],
        &WHIRLPOOL_PROGRAM_ID,
    )
    .0
}

/// `sqrt_price * sqrt(price_factor)`, kept in Q64.64. f64 is exact to ~1e-16 here, far below
/// the 1e-4 relative tick step.
pub fn shocked_sqrt_price(sqrt_price: u128, price_factor: f64) -> SurfpoolResult<u128> {
    let shocked = (sqrt_price as f64) * price_factor.sqrt();
    if !shocked.is_finite() {
        return Err(invalid("shocked price is not a finite number"));
    }
    let rounded = shocked.round();
    if rounded < MIN_SQRT_PRICE_X64 as f64 || rounded >= MAX_SQRT_PRICE_X64 as f64 {
        return Err(invalid(format!(
            "price factor moves sqrt_price to {rounded:.0}, outside the range the program accepts [{MIN_SQRT_PRICE_X64}, {MAX_SQRT_PRICE_X64})"
        )));
    }
    Ok(rounded as u128)
}

/// `floor(log_1.0001((sqrt_price / 2^64)^2))`.
pub fn tick_at_sqrt_price(sqrt_price: u128) -> SurfpoolResult<i32> {
    let ratio = (sqrt_price as f64) / Q64;
    let tick = (2.0 * ratio.ln() / TICK_BASE.ln()).floor();
    if !tick.is_finite() || tick < MIN_TICK as f64 || tick > MAX_TICK as f64 {
        return Err(invalid(format!(
            "shocked price lands on tick {tick:.0}, outside [{MIN_TICK}, {MAX_TICK}]"
        )));
    }
    Ok(tick as i32)
}

fn factor_to_reach(tick_current_index: i32, target_tick: i32) -> f64 {
    TICK_BASE.powi(target_tick - tick_current_index)
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
) -> SurfpoolResult<WhirlpoolPriceShockPlan> {
    validate_price_factor(price_factor)?;
    if pool_account.owner != WHIRLPOOL_PROGRAM_ID {
        return Err(invalid(format!(
            "{pool} is owned by {}, not the Whirlpool program {WHIRLPOOL_PROGRAM_ID}",
            pool_account.owner
        )));
    }
    if pool_account.data.len() != POOL_STATE_LEN {
        return Err(invalid(format!(
            "{pool} is {} bytes, not the {POOL_STATE_LEN} bytes of a Whirlpool account",
            pool_account.data.len()
        )));
    }

    let pool_state = decode_pool_state(&pool_account.data)?;
    let tick_spacing: u16 = number(&pool_state, "tick_spacing")?;
    if tick_spacing == 0 {
        return Err(invalid("pool declares a tick spacing of zero"));
    }
    let old_sqrt_price: u128 = number(&pool_state, "sqrt_price")?;
    let old_tick_current_index: i32 = number(&pool_state, "tick_current_index")?;

    let new_sqrt_price = shocked_sqrt_price(old_sqrt_price, price_factor)?;
    let new_tick_current_index = tick_at_sqrt_price(new_sqrt_price)?;
    let start_index = tick_array_start_index(new_tick_current_index, tick_spacing);

    Ok(WhirlpoolPriceShockPlan {
        pool,
        price_factor,
        tick_spacing,
        old_sqrt_price,
        new_sqrt_price,
        old_tick_current_index,
        new_tick_current_index,
        tick_array: tick_array_address(&pool, start_index),
        tick_array_start_index: start_index,
    })
}

/// Turns a plan into a scenario once the tick array covering the new tick exists. A missing
/// array is rejected here rather than left to fail later at swap time.
pub fn build_price_shock_scenario(
    plan: WhirlpoolPriceShockPlan,
    tick_array_account: Option<&Account>,
) -> SurfpoolResult<Scenario> {
    match tick_array_account {
        None => return Err(invalid(missing_tick_array_message(&plan))),
        Some(account) => {
            if account.owner != WHIRLPOOL_PROGRAM_ID {
                return Err(invalid(format!(
                    "tick array {} is owned by {}, not the Whirlpool program",
                    plan.tick_array, account.owner
                )));
            }
            if account.data.len() != TICK_ARRAY_LEN {
                return Err(invalid(format!(
                    "tick array {} is {} bytes, not the {TICK_ARRAY_LEN} bytes of a TickArray",
                    plan.tick_array,
                    account.data.len()
                )));
            }
        }
    }

    let registry = TemplateRegistry::new();
    let template = registry.get(POOL_STATE_TEMPLATE_ID).ok_or_else(|| {
        SurfpoolError::internal(format!("{POOL_STATE_TEMPLATE_ID} template is unavailable"))
    })?;

    let values = HashMap::from([
        (
            "sqrt_price".to_string(),
            serde_json::json!(plan.new_sqrt_price.to_string()),
        ),
        (
            "tick_current_index".to_string(),
            serde_json::json!(plan.new_tick_current_index),
        ),
    ]);
    let mut pool_override = OverrideInstance::new(
        template.id.clone(),
        PRICE_SHOCK_SLOT,
        AccountAddress::Pubkey(plan.pool.to_string()),
    )
    .with_values(values)
    .with_label(format!("Whirlpool price x{}", plan.price_factor));
    // Unset fields (liquidity, vaults, fee growth) must come from the live pool, so fetch before override.
    pool_override.fetch_before_use = true;

    let mut scenario = Scenario::new(
        "Whirlpool Price Shock".to_string(),
        format!(
            "Move Whirlpool pool {} to {}x its price, onto tick {} of the tick array starting at {}.",
            plan.pool, plan.price_factor, plan.new_tick_current_index, plan.tick_array_start_index
        ),
    );
    scenario.tags = vec![
        "whirlpool".to_string(),
        "orca".to_string(),
        "price-shock".to_string(),
    ];
    scenario.add_override(pool_override);

    Ok(scenario)
}

fn missing_tick_array_message(plan: &WhirlpoolPriceShockPlan) -> String {
    let ticks_in_array = TICK_ARRAY_SIZE * i32::from(plan.tick_spacing);
    let current_start = tick_array_start_index(plan.old_tick_current_index, plan.tick_spacing);
    let (bound_tick, adjective) = if plan.price_factor > 1.0 {
        (current_start + ticks_in_array - 1, "largest")
    } else {
        (current_start, "smallest")
    };
    let safe_factor = factor_to_reach(plan.old_tick_current_index, bound_tick);
    format!(
        "tick array {} (start index {}) does not exist, so a swap could not resume from tick {}. \
         The {adjective} factor that stays on the pool's current tick array [{}, {}] is {:.6}.",
        plan.tick_array,
        plan.tick_array_start_index,
        plan.new_tick_current_index,
        current_start,
        current_start + ticks_in_array - 1,
        safe_factor
    )
}

/// Decodes with the same IDL codec the materializer writes with, so a drifted IDL fails here.
fn decode_pool_state(data: &[u8]) -> SurfpoolResult<Value> {
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
            "account does not carry the Whirlpool discriminator declared by the bundled IDL",
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
            "Whirlpool failed to decode against the IDL: {error}"
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
        .ok_or_else(|| invalid(format!("Whirlpool has no {field} field")))?;
    SvmValue::to_number::<T>(value)
        .map_err(|error| invalid(format!("Whirlpool {field} is not readable: {error}")))
}

fn invalid(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::invalid_params(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQRT_PRICE_ONE: u128 = 1 << 64;

    fn pool_account(sqrt_price: u128, tick_current_index: i32, tick_spacing: u16) -> Account {
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

        let mut data = vec![0u8; POOL_STATE_LEN];
        data[..discriminator.len()].copy_from_slice(&discriminator);
        // Whirlpool layout (discriminator included): tick_spacing 41..43, sqrt_price 65..81, tick_current_index 81..85.
        data[41..43].copy_from_slice(&tick_spacing.to_le_bytes());
        data[65..81].copy_from_slice(&sqrt_price.to_le_bytes());
        data[81..85].copy_from_slice(&tick_current_index.to_le_bytes());
        Account {
            lamports: 1,
            data,
            owner: WHIRLPOOL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn tick_array_account() -> Account {
        Account {
            lamports: 1,
            data: vec![0u8; TICK_ARRAY_LEN],
            owner: WHIRLPOOL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn halving_the_price_halves_it_to_within_a_billionth() {
        let shocked = shocked_sqrt_price(SQRT_PRICE_ONE, 0.5).unwrap();
        let ratio = (shocked as f64) / (SQRT_PRICE_ONE as f64);
        let expected = std::f64::consts::FRAC_1_SQRT_2;
        assert!(
            ((ratio - expected).abs() / expected) < 1e-9,
            "sqrt factor {ratio} is not sqrt(0.5) to within 1e-9"
        );
        assert_eq!(tick_at_sqrt_price(shocked).unwrap(), -6932);
    }

    #[test]
    fn quadrupling_the_price_doubles_the_sqrt_price() {
        let shocked = shocked_sqrt_price(SQRT_PRICE_ONE, 4.0).unwrap();
        let ratio = (shocked as f64) / (SQRT_PRICE_ONE as f64);
        assert!(
            ((ratio - 2.0).abs() / 2.0) < 1e-9,
            "sqrt factor {ratio} is not 2.0 to within 1e-9"
        );
        assert_eq!(tick_at_sqrt_price(shocked).unwrap(), 13863);
    }

    #[test]
    fn shocked_price_and_tick_stay_coupled_over_a_range_of_factors() {
        for factor in [0.01, 0.25, 0.5, 0.9, 1.1, 2.0, 4.0, 100.0] {
            let shocked = shocked_sqrt_price(SQRT_PRICE_ONE, factor).unwrap();
            let tick = tick_at_sqrt_price(shocked).unwrap();
            let price = ((shocked as f64) / Q64).powi(2);
            assert!(
                TICK_BASE.powi(tick) <= price * (1.0 + 1e-9)
                    && price < TICK_BASE.powi(tick + 1) * (1.0 + 1e-9),
                "tick {tick} does not bracket price {price} for factor {factor}"
            );
        }
    }

    #[test]
    fn tick_array_start_index_floors_towards_negative_infinity() {
        assert_eq!(tick_array_start_index(-1, 1), -88);
        assert_eq!(tick_array_start_index(-88, 1), -88);
        assert_eq!(tick_array_start_index(-89, 1), -176);
        assert_eq!(tick_array_start_index(0, 1), 0);
        assert_eq!(tick_array_start_index(87, 1), 0);
        assert_eq!(tick_array_start_index(88, 1), 88);
        assert_eq!(tick_array_start_index(-1, 4), -352);
        assert_eq!(tick_array_start_index(351, 4), 0);
        assert_eq!(tick_array_start_index(352, 4), 352);
    }

    #[test]
    fn plan_reads_the_pool_through_the_idl() {
        let account = pool_account(SQRT_PRICE_ONE, 0, 1);
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(pool, &account, 4.0).unwrap();

        assert_eq!(plan.old_sqrt_price, SQRT_PRICE_ONE);
        assert_eq!(plan.old_tick_current_index, 0);
        assert_eq!(plan.tick_spacing, 1);
        assert_eq!(plan.new_tick_current_index, 13863);
        assert_eq!(plan.tick_array_start_index, 13816);
        assert_eq!(
            plan.tick_array,
            tick_array_address(&pool, plan.tick_array_start_index)
        );
    }

    #[test]
    fn plan_rejects_bad_factors_and_foreign_accounts() {
        let pool = Pubkey::new_unique();
        let account = pool_account(SQRT_PRICE_ONE, 0, 1);
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
    fn scenario_overrides_both_coupled_fields_at_slot_one() {
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(pool, &pool_account(SQRT_PRICE_ONE, 0, 1), 4.0).unwrap();
        let scenario = build_price_shock_scenario(plan, Some(&tick_array_account())).unwrap();

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
            instance.values.get("tick_current_index"),
            Some(&serde_json::json!(13863))
        );
        assert_eq!(
            instance.values.get("sqrt_price"),
            Some(&serde_json::json!(plan.new_sqrt_price.to_string()))
        );
    }

    #[test]
    fn missing_tick_array_is_rejected_with_a_safe_factor() {
        let pool = Pubkey::new_unique();
        let plan = plan_price_shock(pool, &pool_account(SQRT_PRICE_ONE, 0, 1), 4.0).unwrap();
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
