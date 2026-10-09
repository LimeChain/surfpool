use core::mem::size_of;
use std::{collections::HashMap, str::FromStr, sync::OnceLock};

use anchor_lang_idl::types::{
    IdlDefinedFields, IdlInstruction, IdlInstructionAccountItem, IdlType, IdlTypeDef, IdlTypeDefTy,
};
use log::{info, warn};
use phoenix_rise_accounts::{
    global_config::GlobalConfig,
    perp_asset_map::{FundingAccumulator, PerpAssetMetadata, PriceComponent},
};
use solana_account::Account;
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use surfpool_types::Idl;
use txtx_addon_network_svm::codec::idl::borsh_encode_value_to_idl_type;

use super::{
    market::{Market, invalid_perp_asset_map, map_entries, move_market},
    trader::{
        Side, cancel_orders, deposit, place_market_order, prepare_cascade, prepare_cross_margin,
        withdraw,
    },
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::registry::PHOENIX_ETERNAL_IDL_CONTENT,
    surfnet::{
        remote::SurfnetRemoteClient,
        svm::{AccountUpdatePolicy, SurfnetSvm, json_to_txtx_value_for_idl_type},
    },
};

pub const PHOENIX_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih");
pub const PHOENIX_GLOBAL_CONFIG: Pubkey =
    Pubkey::from_str_const("2zskx2iyCvb6Stg7RBZkt1f6MrF4dpYtMG3yMvKwqtUZ");
/// GlobalConfig names this map too; the mainnet tests check the two agree.
pub const PHOENIX_PERP_ASSET_MAP: Pubkey =
    Pubkey::from_str_const("2nHGAaEw3D5dd4hVueaUNoygkQFmoeKqRQWnSPqSMFUC");

pub fn log_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"log"], &PHOENIX_PROGRAM_ID).0
}

/// The exchange-wide accounts, as GlobalConfig names them.
pub struct Exchange {
    pub config: GlobalConfig,
    pub perp_asset_map: Pubkey,
    pub global_trader_index: Pubkey,
    pub active_trader_buffer: Pubkey,
}

impl Exchange {
    /// Reads GlobalConfig from the local VM, after putting it and the program there.
    pub async fn load(
        svm: &mut SurfnetSvm,
        remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    ) -> SurfpoolResult<Self> {
        hydrate(
            svm,
            remote_ctx,
            &[PHOENIX_PROGRAM_ID, PHOENIX_GLOBAL_CONFIG],
        )
        .await?;
        Self::from_config(&local_account(svm, &PHOENIX_GLOBAL_CONFIG)?)
    }

    /// The exchange the GlobalConfig `account` describes.
    pub fn from_config(account: &Account) -> SurfpoolResult<Self> {
        let config = GlobalConfig::try_from_account_bytes(&account.data).map_err(|e| {
            SurfpoolError::invalid_account_data(
                PHOENIX_GLOBAL_CONFIG,
                "Expected the Phoenix Eternal GlobalConfig",
                Some(e),
            )
        })?;
        Ok(Self {
            perp_asset_map: Pubkey::new_from_array(config.perp_asset_map_key()),
            global_trader_index: Pubkey::new_from_array(config.global_trader_index_header_key()),
            active_trader_buffer: Pubkey::new_from_array(config.active_trader_buffer_header_key()),
            config,
        })
    }
}

pub fn phoenix_idl() -> &'static Idl {
    static IDL: OnceLock<Idl> = OnceLock::new();
    IDL.get_or_init(|| {
        serde_json::from_str(PHOENIX_ETERNAL_IDL_CONTENT).expect("the bundled Phoenix IDL parses")
    })
}

/// An Eternal instruction encoded from its IDL definition. Accounts are named as in the IDL, and
/// `args` holds one JSON value per IDL argument, shaped like its type.
pub fn phoenix_instruction(
    name: &str,
    accounts: &[(&str, Pubkey)],
    args: &serde_json::Value,
) -> SurfpoolResult<Instruction> {
    encode_instruction(name, accounts, args, true)
}

/// Like [`phoenix_instruction`], picking the accounts the instruction takes out of `known` and
/// ignoring the rest.
pub fn phoenix_instruction_from(
    name: &str,
    known: &[(&str, Pubkey)],
    args: &serde_json::Value,
) -> SurfpoolResult<Instruction> {
    encode_instruction(name, known, args, false)
}

fn instruction_definition(name: &str) -> SurfpoolResult<&'static IdlInstruction> {
    phoenix_idl()
        .instructions
        .iter()
        .find(|instruction| instruction.name == name)
        .ok_or_else(|| SurfpoolError::internal(format!("the Phoenix IDL has no {name}")))
}

fn encode_instruction(
    name: &str,
    accounts: &[(&str, Pubkey)],
    args: &serde_json::Value,
    strict: bool,
) -> SurfpoolResult<Instruction> {
    let idl = phoenix_idl();
    let program_id = Pubkey::from_str(&idl.address)
        .map_err(|e| SurfpoolError::internal(format!("invalid Phoenix IDL address: {e}")))?;
    let definition = instruction_definition(name)?;

    let mut data = definition.discriminator.clone();
    for arg in &definition.args {
        let value = args.get(&arg.name).ok_or_else(|| {
            SurfpoolError::internal(format!("{name} needs the argument {}", arg.name))
        })?;
        let value = json_to_txtx_value_for_idl_type(value, &arg.ty, &idl.types)?;
        let encoded = borsh_encode_value_to_idl_type(&value, &arg.ty, &idl.types, None)
            .map_err(|e| SurfpoolError::internal(format!("{name} {}: {e}", arg.name)))?;
        data.extend(encoded);
    }

    let mut metas = Vec::with_capacity(definition.accounts.len());
    for item in &definition.accounts {
        let IdlInstructionAccountItem::Single(account) = item else {
            return Err(SurfpoolError::internal(format!(
                "{name} groups its accounts, which the Phoenix IDL does not do"
            )));
        };
        let Some((_, pubkey)) = accounts.iter().find(|(given, _)| *given == account.name) else {
            return Err(SurfpoolError::internal(format!(
                "{name} needs the account {}",
                account.name
            )));
        };
        metas.push(AccountMeta {
            pubkey: *pubkey,
            is_signer: account.signer,
            is_writable: account.writable,
        });
    }
    if let Some((unknown, _)) = accounts.iter().filter(|_| strict).find(|(given, _)| {
        !definition
            .accounts
            .iter()
            .any(|item| matches!(item, IdlInstructionAccountItem::Single(a) if a.name == *given))
    }) {
        return Err(SurfpoolError::internal(format!(
            "{name} takes no account named {unknown}"
        )));
    }

    Ok(Instruction {
        program_id,
        accounts: metas,
        data,
    })
}

/// A market symbol as Phoenix stores it: its bytes, zero-padded to 16.
pub fn symbol_bytes(symbol: &str) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    let len = symbol.len().min(bytes.len());
    bytes[..len].copy_from_slice(&symbol.as_bytes()[..len]);
    bytes
}

/// The type `ty` names, when it names one in `types`.
fn defined<'t>(ty: &IdlType, types: &'t [IdlTypeDef]) -> Option<&'t IdlTypeDefTy> {
    let IdlType::Defined { name, .. } = ty else {
        return None;
    };
    types.iter().find(|t| &t.name == name).map(|t| &t.ty)
}

/// The arguments of `name` built from a template's flat values: every IDL field takes the value
/// named after it, a market symbol fills `perpAssetSymbol`, and an optional field left out is kept.
pub fn instruction_args(
    name: &str,
    values: &HashMap<String, serde_json::Value>,
    symbol: Option<&str>,
) -> SurfpoolResult<serde_json::Value> {
    let idl = phoenix_idl();
    let mut args = serde_json::Map::new();
    for arg in &instruction_definition(name)?.args {
        let Some(IdlTypeDefTy::Struct {
            fields: Some(IdlDefinedFields::Named(fields)),
        }) = defined(&arg.ty, &idl.types)
        else {
            return Err(SurfpoolError::internal(format!(
                "{name} {} is not a struct of named fields",
                arg.name
            )));
        };
        let mut object = serde_json::Map::new();
        for field in fields {
            let value = if field.name == "perpAssetSymbol" {
                let symbol = symbol.ok_or_else(|| {
                    SurfpoolError::internal(format!("{name} needs a market symbol"))
                })?;
                serde_json::json!({ "symbolBytes": symbol_bytes(symbol) })
            } else if let Some(value) = given(values, &field.name) {
                shape(value, &field.ty, &idl.types)?
            } else if matches!(field.ty, IdlType::Option(_)) {
                serde_json::Value::Null
            } else {
                return Err(SurfpoolError::internal(format!(
                    "{name} needs {}",
                    field.name
                )));
            };
            object.insert(field.name.clone(), value);
        }
        args.insert(arg.name.clone(), object.into());
    }
    Ok(args.into())
}

/// A template value in the JSON shape the IDL encoder expects for `ty`: decimal strings become
/// numbers, a bare value fills a one-field wrapper such as `BaseLots`, and a name picks a variant.
fn shape(
    value: &serde_json::Value,
    ty: &IdlType,
    types: &[IdlTypeDef],
) -> SurfpoolResult<serde_json::Value> {
    use serde_json::Value;
    let invalid = || SurfpoolError::internal(format!("{value} does not fit {ty:?}"));
    Ok(match (ty, value) {
        (IdlType::Option(_), Value::Null) => Value::Null,
        (IdlType::Option(inner), _) => shape(value, inner, types)?,
        (IdlType::Bool, Value::String(text)) => Value::Bool(text.parse().map_err(|_| invalid())?),
        (IdlType::U8 | IdlType::U16 | IdlType::U32 | IdlType::U64, Value::String(text)) => {
            Value::from(text.trim().parse::<u64>().map_err(|_| invalid())?)
        }
        (IdlType::I8 | IdlType::I16 | IdlType::I32 | IdlType::I64, Value::String(text)) => {
            Value::from(text.trim().parse::<i64>().map_err(|_| invalid())?)
        }
        (IdlType::Defined { .. }, _) => match (defined(ty, types), value) {
            (
                Some(IdlTypeDefTy::Struct {
                    fields: Some(IdlDefinedFields::Named(fields)),
                }),
                _,
            ) => match fields.as_slice() {
                [field] => serde_json::json!({ &field.name: shape(value, &field.ty, types)? }),
                _ => return Err(invalid()),
            },
            (Some(IdlTypeDefTy::Enum { .. }), Value::String(variant)) => {
                serde_json::json!({ variant: null })
            }
            _ => value.clone(),
        },
        _ => value.clone(),
    })
}

const FEE_PAYER_LAMPORTS: u64 = 1_000_000_000;
const COMPUTE_UNIT_LIMIT: u32 = 1_400_000;

/// Puts every listed account into the local VM, reading only the missing ones from the
/// upstream datasource, the way any account read does.
pub async fn hydrate(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    addresses: &[Pubkey],
) -> SurfpoolResult<()> {
    let mut missing = Vec::new();
    for address in addresses {
        if svm.inner.get_account(address)?.is_none() && !missing.contains(address) {
            missing.push(*address);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let phoenix_offline = svm
        .offline_accounts
        .get(&PHOENIX_PROGRAM_ID.to_string())?
        .is_some_and(|config| config.include_owned_accounts);
    for address in &missing {
        if phoenix_offline || svm.offline_accounts.contains_key(&address.to_string())? {
            return Err(SurfpoolError::internal(format!(
                "{address} is offline and missing locally"
            )));
        }
    }
    let (client, commitment) = remote_ctx.as_ref().ok_or_else(|| {
        SurfpoolError::internal(format!(
            "{} accounts are missing locally and there is no datasource",
            missing.len()
        ))
    })?;
    for fetched in client.get_multiple_accounts(&missing, *commitment).await? {
        svm.apply_account_update(fetched, AccountUpdatePolicy::HydrateIfAbsent)?;
    }
    Ok(())
}

pub(crate) fn local_account(svm: &SurfnetSvm, address: &Pubkey) -> SurfpoolResult<Account> {
    svm.inner
        .get_account(address)?
        .ok_or_else(|| SurfpoolError::internal(format!("{address} is missing locally")))
}

/// A copy of the local VM without signature checks, so instructions run as the signers they name.
/// Nothing reaches the local VM until the caller writes back what [`Sandbox::writes`] returns.
pub struct Sandbox<'a> {
    live: &'a SurfnetSvm,
    svm: SurfnetSvm,
    written: Vec<Pubkey>,
}

impl<'a> Sandbox<'a> {
    pub fn new(live: &'a SurfnetSvm) -> Self {
        let mut svm = live.clone_for_profiling();
        svm.inner.set_sigverify(false);
        Self {
            live,
            svm,
            written: Vec::new(),
        }
    }

    /// Runs the instructions in order; the first failing one fails the run.
    pub fn run(&mut self, instructions: &[Instruction]) -> SurfpoolResult<()> {
        for (index, instruction) in instructions.iter().enumerate() {
            self.send(instruction)
                .map_err(|e| SurfpoolError::internal(format!("instruction {index} failed: {e}")))?;
            for meta in &instruction.accounts {
                if meta.is_writable && !self.written.contains(&meta.pubkey) {
                    self.written.push(meta.pubkey);
                }
            }
        }
        Ok(())
    }

    /// The return data of a read-only instruction, such as a Hawkeye view.
    pub fn view(&mut self, instruction: &Instruction) -> SurfpoolResult<Vec<u8>> {
        self.send(instruction).map_err(SurfpoolError::internal)
    }

    /// Moves the copy's Clock one slot on. Phoenix fixes the mark it uses for risk for the rest of
    /// a slot once a risk action, such as a cancel, has read it.
    pub fn next_slot(&mut self) {
        let mut clock = self.svm.inner.get_sysvar::<Clock>();
        clock.slot += 1;
        self.svm.inner.set_sysvar(&clock);
    }

    /// A further copy, for a trial that must leave this one as it is.
    pub fn trial(&self) -> Sandbox<'a> {
        let mut svm = self.svm.clone_for_profiling();
        svm.inner.set_sigverify(false);
        Sandbox {
            live: self.live,
            svm,
            written: self.written.clone(),
        }
    }

    /// Every account the instructions run so far changed, as it now is.
    pub fn writes(self) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
        let mut changed = Vec::new();
        for pubkey in self.written {
            let Some(after) = self.svm.inner.get_account(&pubkey)? else {
                continue;
            };
            if self.live.inner.get_account(&pubkey)?.as_ref() != Some(&after) {
                changed.push((pubkey, after));
            }
        }
        Ok(changed)
    }

    fn send(&mut self, instruction: &Instruction) -> Result<Vec<u8>, String> {
        // A fresh payer gives every transaction its own signature.
        let payer = Keypair::new();
        self.svm
            .inner
            .airdrop(&payer.pubkey(), FEE_PAYER_LAMPORTS)
            .map_err(|failed| format!("funding the fee payer failed: {:?}", failed.err))?;
        let mut transaction = Transaction::new_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNIT_LIMIT),
                instruction.clone(),
            ],
            Some(&payer.pubkey()),
        );
        transaction.partial_sign(&[&payer], self.svm.inner.svm.latest_blockhash());
        self.svm
            .inner
            .send_transaction(transaction)
            .map(|meta| meta.return_data.data)
            .map_err(|failed| format!("{:?}; logs: {:?}", failed.err, failed.meta.logs))
    }
}

/// Runs the instructions in a [`Sandbox`] and returns what they changed.
pub fn run_instructions(
    svm: &SurfnetSvm,
    instructions: &[Instruction],
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let mut sandbox = Sandbox::new(svm);
    sandbox.run(instructions)?;
    sandbox.writes()
}

/// Phoenix refuses a market once its readings are older than its stale threshold times its `u8`
/// `oracle_hard_stale_multiplier`, or than the threshold alone when that is 0. `u32::MAX` puts the
/// limit beyond any session and keeps within a `u64`.
const RAISED_STALE_THRESHOLD_SLOTS: u64 = u32::MAX as u64;

pub const LIQUIDATION_READY_TEMPLATE_ID: &str = "phoenix-liquidation-ready";
const TRADER_CAPABILITIES_TEMPLATE_ID: &str = "phoenix-trader-capabilities";
const MARKET_SYMBOL_FIELD: &str = "symbol";
const TARGET_TICKS_FIELD: &str = "target_ticks";

/// The GlobalConfig role that signs a configuration instruction on mainnet.
#[derive(Clone, Copy, Debug)]
enum Role {
    Root,
    Risk,
    Market,
}

/// Templates that run one of Phoenix's own configuration instructions, as the role that signs it
/// on mainnet. Each template's fields are named after the instruction's IDL fields.
const CONFIG_TEMPLATES: [(&str, &str, Role); 12] = [
    (
        "phoenix-market-risk-factors",
        "UpdatePerpRiskFactors",
        Role::Risk,
    ),
    (
        "phoenix-market-cancel-risk-factor",
        "UpdatePerpCancelRiskFactor",
        Role::Risk,
    ),
    (
        "phoenix-market-max-liquidation-size",
        "UpdatePerpMaxLiquidationSize",
        Role::Risk,
    ),
    (
        "phoenix-market-open-interest-cap",
        "UpdatePerpOpenInterestCap",
        Role::Risk,
    ),
    (
        "phoenix-market-funding",
        "UpdateFundingParameters",
        Role::Market,
    ),
    ("phoenix-market-fees", "UpdateMarketFees", Role::Market),
    ("phoenix-market-status", "ChangeMarketStatus", Role::Market),
    (
        "phoenix-exchange-status",
        "ChangeExchangeStatus",
        Role::Root,
    ),
    (
        "phoenix-withdraw-limits",
        "UpdateWithdrawRateLimits",
        Role::Root,
    ),
    (
        "phoenix-withdraw-parameters",
        "UpdateWithdrawParameters",
        Role::Risk,
    ),
    ("phoenix-trader-fees", "UpdateTraderFees", Role::Market),
    (
        TRADER_CAPABILITIES_TEMPLATE_ID,
        "SetTraderCapability",
        Role::Risk,
    ),
];

/// The writes of a configuration template's instruction, signed by its GlobalConfig role, which
/// passes its own key as the permission account, as mainnet's configuration transactions do.
async fn run_config_template(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    template_id: &str,
    instruction: &str,
    role: Role,
    target: &Pubkey,
    values: &HashMap<String, serde_json::Value>,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let exchange = Exchange::load(svm, remote_ctx).await?;
    let authority = Pubkey::new_from_array(match role {
        Role::Root => exchange.config.root_authority(),
        Role::Risk => exchange.config.risk_authority(),
        Role::Market => exchange.config.market_authority(),
    });
    let withdraw_queue = Pubkey::new_from_array(exchange.config.withdraw_queue_key());
    hydrate(
        svm,
        remote_ctx,
        &[
            exchange.perp_asset_map,
            exchange.global_trader_index,
            exchange.active_trader_buffer,
            withdraw_queue,
            *target,
        ],
    )
    .await?;
    let mut known = vec![
        ("phoenixProgram", PHOENIX_PROGRAM_ID),
        ("phoenixLogAuthority", log_authority()),
        ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
        ("authority", authority),
        ("maybePermissionAccount", authority),
        ("perpAssetMap", exchange.perp_asset_map),
        ("globalTraderIndex", exchange.global_trader_index),
        ("activeTraderBuffer", exchange.active_trader_buffer),
        ("withdrawQueueAccount", withdraw_queue),
        ("traderAccount", *target),
    ];
    let symbol = values
        .get(MARKET_SYMBOL_FIELD)
        .and_then(serde_json::Value::as_str);
    let mut values = values.clone();
    if let Some(symbol) = symbol {
        let map = local_account(svm, &exchange.perp_asset_map)?;
        let market = Market::find(&exchange.perp_asset_map, &map.data, symbol)?;
        hydrate(svm, remote_ctx, &[market.orderbook]).await?;
        known.push(("orderbook", market.orderbook));
        known.push(("orderbookAccount", market.orderbook));
        // UpdatePerpRiskFactors takes all three factors; one left out keeps its current value.
        if instruction == "UpdatePerpRiskFactors" {
            let fields = [
                "maintenanceRiskFactor",
                "backstopRiskFactor",
                "highRiskRiskFactor",
            ];
            for (field, current) in fields.into_iter().zip(market.risk_factors) {
                if given(&values, field).is_none() {
                    values.insert(field.to_string(), current.to_string().into());
                }
            }
        }
    }
    let args = if template_id == TRADER_CAPABILITIES_TEMPLATE_ID {
        capability_toggles(&values)?
    } else {
        instruction_args(instruction, &values, symbol)?
    };
    run_instructions(
        svm,
        &[phoenix_instruction_from(instruction, &known, &args)?],
    )
}

/// A template input, unless it was left out, left empty or null.
fn given<'v>(
    values: &'v HashMap<String, serde_json::Value>,
    field: &str,
) -> Option<&'v serde_json::Value> {
    values
        .get(field)
        .filter(|value| !value.is_null() && value.as_str() != Some(""))
}

/// `SetTraderCapability` takes a list of toggles; its template takes one boolean per Phoenix
/// capability, named after it, and toggles only those given.
fn capability_toggles(
    values: &HashMap<String, serde_json::Value>,
) -> SurfpoolResult<serde_json::Value> {
    const TARGETS: [&str; 6] = [
        "PlaceLimitOrder",
        "PlaceMarketOrder",
        "RiskIncreasingTrade",
        "RiskReducingTrade",
        "DepositCollateral",
        "WithdrawCollateral",
    ];
    let mut toggles = Vec::new();
    for target in TARGETS {
        let enable = match given(values, target) {
            None => continue,
            Some(serde_json::Value::Bool(enable)) => *enable,
            Some(serde_json::Value::String(text)) => text
                .parse()
                .map_err(|_| SurfpoolError::internal(format!("{target} must be true or false")))?,
            Some(other) => {
                return Err(SurfpoolError::internal(format!(
                    "{target} must be true or false, not {other}"
                )));
            }
        };
        toggles.push(serde_json::json!({ "target": { target: null }, "enable": enable }));
    }
    if toggles.is_empty() {
        return Err(SurfpoolError::internal(format!(
            "set at least one of {}",
            TARGETS.join(", ")
        )));
    }
    Ok(serde_json::json!({ "params": { "toggles": toggles } }))
}

/// The writes a Phoenix override needs, or `None` when the account takes the generic IDL path.
/// Each first keeps the markets usable ([`keep_markets_usable`]); `target_slot` is the slot played.
pub async fn prepare_phoenix_override(
    svm: &mut SurfnetSvm,
    template_id: &str,
    account_pubkey: &Pubkey,
    account: &Account,
    values: &HashMap<String, serde_json::Value>,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    target_slot: u64,
) -> SurfpoolResult<Option<Vec<(Pubkey, Account)>>> {
    if !template_id.starts_with("phoenix-") && account.owner != PHOENIX_PROGRAM_ID {
        return Ok(None);
    }
    keep_markets_usable(svm, remote_ctx, target_slot).await;
    if let Some((_, instruction, role)) = CONFIG_TEMPLATES
        .iter()
        .find(|(config_template, ..)| *config_template == template_id)
    {
        return run_config_template(
            svm,
            remote_ctx,
            template_id,
            instruction,
            *role,
            account_pubkey,
            values,
        )
        .await
        .map(Some);
    }
    match template_id {
        "phoenix-market-move" => {
            let symbol = text_input(values, MARKET_SYMBOL_FIELD)?;
            let target_ticks = whole_number_input(values, TARGET_TICKS_FIELD)?;
            move_market(svm, remote_ctx, symbol, target_ticks)
                .await
                .map(Some)
        }
        "phoenix-liquidation-cascade" => {
            let symbol = text_input(values, MARKET_SYMBOL_FIELD)?;
            let long = match text_input(values, "side")? {
                "long" => true,
                "short" => false,
                other => {
                    return Err(SurfpoolError::internal(format!(
                        "side must be long or short, not {other}"
                    )));
                }
            };
            prepare_cascade(svm, remote_ctx, symbol, long)
                .await
                .map(|ready| {
                    let traders: Vec<String> = ready
                        .liquidations
                        .iter()
                        .map(|(trader, _)| trader.to_string())
                        .collect();
                    info!(
                        "Phoenix {symbol} moved to {} ticks: liquidatable in turn: {}",
                        ready.target_ticks,
                        traders.join(", ")
                    );
                    Some(ready.writes)
                })
        }
        LIQUIDATION_READY_TEMPLATE_ID => {
            let symbols: Vec<&str> = text_input(values, "symbols")?
                .split(',')
                .map(str::trim)
                .filter(|symbol| !symbol.is_empty())
                .collect();
            prepare_cross_margin(svm, remote_ctx, *account_pubkey, &symbols)
                .await
                .map(|ready| {
                    let (mut order, mut moves) = (Vec::new(), Vec::new());
                    for (index, (symbol, ticks)) in ready.moves.iter().enumerate() {
                        let verb = if index == 0 { " moved" } else { "" };
                        order.push(symbol.as_str());
                        moves.push(format!("{symbol}{verb} to {ticks} ticks"));
                    }
                    info!(
                        "Phoenix liquidation-ready: {account_pubkey} liquidatable in turn: {} ({})",
                        order.join(", "),
                        moves.join(", ")
                    );
                    Some(ready.writes)
                })
        }
        "phoenix-open-position" => {
            let symbol = text_input(values, MARKET_SYMBOL_FIELD)?;
            let side = match text_input(values, "side")? {
                "Bid" => Side::Bid,
                "Ask" => Side::Ask,
                other => {
                    return Err(SurfpoolError::internal(format!(
                        "side must be Bid or Ask, not {other}"
                    )));
                }
            };
            let base_lots = whole_number_input(values, "base_lots")?;
            place_market_order(svm, remote_ctx, *account_pubkey, symbol, side, base_lots)
                .await
                .map(Some)
        }
        "phoenix-cancel-orders" => {
            let symbol = text_input(values, MARKET_SYMBOL_FIELD)?;
            cancel_orders(svm, remote_ctx, *account_pubkey, symbol)
                .await
                .map(Some)
        }
        "phoenix-withdraw" => {
            let quote_lots = whole_number_input(values, "quote_lots")?;
            withdraw(svm, remote_ctx, *account_pubkey, quote_lots)
                .await
                .map(Some)
        }
        "phoenix-deposit" => {
            let quote_lots = whole_number_input(values, "quote_lots")?;
            deposit(svm, remote_ctx, *account_pubkey, quote_lots)
                .await
                .map(Some)
        }
        // Phoenix's other accounts take the generic IDL path.
        _ => Ok(None),
    }
}

fn text_input<'a>(
    values: &'a HashMap<String, serde_json::Value>,
    field: &str,
) -> SurfpoolResult<&'a str> {
    values
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SurfpoolError::internal(format!("{field} must be a non-empty string")))
}

fn whole_number_input(
    values: &HashMap<String, serde_json::Value>,
    field: &str,
) -> SurfpoolResult<u64> {
    let parsed = match values.get(field) {
        Some(serde_json::Value::String(text)) => text.trim().parse().ok(),
        Some(serde_json::Value::Number(number)) => number.as_u64(),
        _ => None,
    };
    parsed.ok_or_else(|| {
        SurfpoolError::internal(format!(
            "{field} must be a whole number, as a decimal string or a JSON number"
        ))
    })
}

/// Nothing refreshes oracle readings locally, so this raises the map's stale thresholds, restamps
/// old readings and rewinds funding ahead of the Clock. A failure only warns and keeps the map.
pub(crate) async fn keep_markets_usable(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    target_slot: u64,
) {
    let kept = async {
        hydrate(svm, remote_ctx, &[PHOENIX_PERP_ASSET_MAP]).await?;
        let map = local_account(svm, &PHOENIX_PERP_ASSET_MAP)?;
        let clock = svm.inner.get_sysvar::<Clock>();
        let raised = raise_stale_thresholds(&PHOENIX_PERP_ASSET_MAP, &map.data)?;
        let now = u64::try_from(clock.unix_timestamp).unwrap_or(0);
        let rewound = rewind_funding_timestamps(
            &PHOENIX_PERP_ASSET_MAP,
            raised.as_deref().unwrap_or(&map.data),
            now,
        )?;
        let patched = rewound.or(raised);
        let restamped = restamp_readings(
            &PHOENIX_PERP_ASSET_MAP,
            patched.as_deref().unwrap_or(&map.data),
            clock.slot,
        )?;
        let Some(data) = restamped.or(patched) else {
            return Ok(());
        };
        svm.set_scenario_override_account(
            &PHOENIX_PERP_ASSET_MAP,
            Account { data, ..map },
            target_slot,
        )
    }
    .await;
    if let Err(e) = kept {
        warn!("Could not keep the Phoenix markets usable in the local VM: {e}");
    }
}

/// Every market's metadata in the map, with the offset it starts at in `data`.
fn markets_in_map(
    account_pubkey: &Pubkey,
    data: &[u8],
) -> SurfpoolResult<Vec<(usize, PerpAssetMetadata)>> {
    let mut markets = Vec::new();
    // The decoder walks the entries in storage order, and each market's metadata holds its own
    // market account, so it occurs once in the map
    let mut cursor = 0;
    for entry in map_entries(account_pubkey, data)? {
        let metadata = entry.metadata;
        let metadata_bytes = metadata.as_bytes();
        let offset = data[cursor..]
            .windows(metadata_bytes.len())
            .position(|window| window == metadata_bytes)
            .map(|position| cursor + position)
            .ok_or_else(|| {
                invalid_perp_asset_map(account_pubkey, "Phoenix market metadata was not found")
            })?;
        cursor = offset + metadata_bytes.len();
        markets.push((offset, metadata));
    }
    Ok(markets)
}

/// The map with every market's spot and perp oracle stale threshold raised to
/// [`RAISED_STALE_THRESHOLD_SLOTS`], or `None` when they already are. A higher threshold is kept.
fn raise_stale_thresholds(account_pubkey: &Pubkey, data: &[u8]) -> SurfpoolResult<Option<Vec<u8>>> {
    let price_len = size_of::<PriceComponent>();
    let mut patched: Option<Vec<u8>> = None;
    for (offset, metadata) in markets_in_map(account_pubkey, data)? {
        // The PriceComponent is the metadata's first field.
        let mut price = *metadata.oracle_price();
        let mark = &mut price.mark_price;
        let mut raised = false;
        for threshold in [
            &mut mark.spot_price_component.stale_threshold,
            &mut mark.perp_price_component.stale_threshold,
        ] {
            if *threshold < RAISED_STALE_THRESHOLD_SLOTS {
                *threshold = RAISED_STALE_THRESHOLD_SLOTS;
                raised = true;
            }
        }
        if raised {
            patched.get_or_insert_with(|| data.to_vec())[offset..offset + price_len]
                .copy_from_slice(bytemuck::bytes_of(&price));
        }
    }
    Ok(patched)
}

/// The map with reading slots before `slot` moved up to it, or `None`; prices stay. On an old map a
/// report's EMA leaves a deep search move no positive price, and Phoenix refuses the margin.
fn restamp_readings(
    account_pubkey: &Pubkey,
    data: &[u8],
    slot: u64,
) -> SurfpoolResult<Option<Vec<u8>>> {
    let price_len = size_of::<PriceComponent>();
    let mut patched: Option<Vec<u8>> = None;
    for (offset, metadata) in markets_in_map(account_pubkey, data)? {
        // The PriceComponent is the metadata's first field.
        let mut price = *metadata.oracle_price();
        let mark = &mut price.mark_price;
        let samples = mark
            .spot_price_component
            .last_exchange_spot_price
            .iter_mut()
            .chain(
                mark.perp_price_component
                    .last_exchange_perp_price
                    .iter_mut(),
            )
            .map(|sample| &mut sample.slot);
        let mut stamped = false;
        for reading in [&mut mark.price.slot, &mut mark.spot_price_component.slot]
            .into_iter()
            .chain(samples)
        {
            // An unused oracle sample stays empty rather than becoming a fresh zero price.
            if *reading != 0 && *reading < slot {
                *reading = slot;
                stamped = true;
            }
        }
        if stamped {
            patched.get_or_insert_with(|| data.to_vec())[offset..offset + price_len]
                .copy_from_slice(bytemuck::bytes_of(&price));
        }
    }
    Ok(patched)
}

/// The map with funding timestamps after `now` (Unix seconds) moved back to it, or `None`. Phoenix
/// fails to update funding, and every price, while the Clock is before the last update.
fn rewind_funding_timestamps(
    account_pubkey: &Pubkey,
    data: &[u8],
    now: u64,
) -> SurfpoolResult<Option<Vec<u8>>> {
    let mut patched: Option<Vec<u8>> = None;
    for (offset, metadata) in markets_in_map(account_pubkey, data)? {
        let mut funding = *metadata.funding_accumulator();
        let mut rewound = false;
        for timestamp in [
            &mut funding.start_interval_timestamp,
            &mut funding.last_funding_update_timestamp,
        ] {
            if bytemuck::cast::<_, u64>(*timestamp) > now {
                *timestamp = bytemuck::cast(now);
                rewound = true;
            }
        }
        if rewound {
            // The metadata layout is private to the crate, so the field's offset is taken from
            // the decoded copy.
            let start = offset
                + metadata.funding_accumulator() as *const FundingAccumulator as usize
                - metadata.as_bytes().as_ptr() as usize;
            patched.get_or_insert_with(|| data.to_vec())
                [start..start + size_of::<FundingAccumulator>()]
                .copy_from_slice(bytemuck::bytes_of(&funding));
        }
    }
    Ok(patched)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashSet;

    use bytemuck::Zeroable;
    use phoenix_rise_accounts::{
        PhoenixAccount,
        perp_asset_map::{PerpAssetMap, PerpAssetMetadata},
        trader::TraderHeader,
    };
    use solana_system_interface::instruction as system_instruction;
    use surfpool_types::AccountAddress;

    use super::*;
    use crate::scenarios::{
        protocols::phoenix_eternal::v1::market::tests::perp_asset_map_account,
        registry::template_registry,
    };

    /// Every Phoenix template that carries a fixed address: its id, account type and address.
    pub(crate) fn template_addresses() -> Vec<(&'static str, &'static str, Pubkey)> {
        template_registry()
            .by_protocol("Phoenix Eternal")
            .into_iter()
            .filter_map(|template| match &template.address {
                AccountAddress::Pubkey(address) if !address.is_empty() => Some((
                    template.id.as_str(),
                    template.account_type.as_str(),
                    Pubkey::from_str(address).unwrap_or_else(|e| panic!("{}: {e}", template.id)),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn templates_carry_the_addresses_the_writers_use() {
        let addresses = template_addresses();
        assert!(!addresses.is_empty());
        let mut withdraw_queues = HashSet::new();
        for (id, account_type, address) in addresses {
            match account_type {
                "PerpAssetMap" => assert_eq!(address, PHOENIX_PERP_ASSET_MAP, "{id}"),
                "GlobalConfiguration" => assert_eq!(address, PHOENIX_GLOBAL_CONFIG, "{id}"),
                // The writers read this one from GlobalConfig; the mainnet tests check it there.
                "WithdrawQueueHeader" => {
                    withdraw_queues.insert(address);
                }
                other => panic!("{id} carries an address for {other}, which nothing checks"),
            }
        }
        assert_eq!(withdraw_queues.len(), 1, "{withdraw_queues:?}");
    }

    /// Where the fixture markets' PriceComponents start: after the 48-byte map header, each
    /// 1584-byte entry holds a 16-byte symbol and then the metadata, which starts with them.
    const PRICE_COMPONENT_STARTS: [usize; 2] = [48 + 16, 48 + 1_584 + 16];

    /// The SOL fixture followed by a second market, a copy of SOL with its own symbol, mark and
    /// thresholds, so a walk over the entries has a later market to find.
    fn two_market_map_account() -> Account {
        let mut account = perp_asset_map_account();
        let [first, second] = PRICE_COMPONENT_STARTS.map(|start| start - 16);
        account.data.copy_within(first..second, second);
        let mut symbol = [0_u8; 16];
        symbol[..3].copy_from_slice(b"BTC");
        account.data[second..second + 16].copy_from_slice(&symbol);
        let price_range =
            PRICE_COMPONENT_STARTS[1]..PRICE_COMPONENT_STARTS[1] + size_of::<PriceComponent>();
        let mut price: PriceComponent =
            bytemuck::pod_read_unaligned(&account.data[price_range.clone()]);
        price.mark_price.price.ticks = bytemuck::cast(12_345_u64);
        price.mark_price.spot_price_component.stale_threshold = 25;
        price.mark_price.perp_price_component.stale_threshold = 25;
        account.data[price_range].copy_from_slice(bytemuck::bytes_of(&price));
        // Two assets in two used slots.
        account.data[24..26].copy_from_slice(&2_u16.to_le_bytes());
        account.data[32..36].copy_from_slice(&2_u32.to_le_bytes());
        account
    }

    fn market(data: &[u8], symbol: &str) -> PerpAssetMetadata {
        PerpAssetMap::try_from_account_bytes(data)
            .unwrap()
            .find_by_symbol(symbol)
            .unwrap()
            .unwrap()
            .metadata
    }

    fn oracle_thresholds(data: &[u8], symbol: &str) -> (u64, u64) {
        let mark = market(data, symbol).oracle_price().mark_price;
        (
            mark.spot_price_component.stale_threshold,
            mark.perp_price_component.stale_threshold,
        )
    }

    const RAISED_THRESHOLDS: (u64, u64) =
        (RAISED_STALE_THRESHOLD_SLOTS, RAISED_STALE_THRESHOLD_SLOTS);

    #[test]
    fn raising_the_stale_thresholds_keeps_the_readings_of_every_market() {
        let account = two_market_map_account();
        assert_ne!(
            market(&account.data, "SOL").oracle_price(),
            market(&account.data, "BTC").oracle_price(),
            "the markets differ, so a write to the wrong one shows"
        );

        let raised = raise_stale_thresholds(&PHOENIX_PERP_ASSET_MAP, &account.data)
            .unwrap()
            .expect("upstream's thresholds are raised");

        for symbol in ["SOL", "BTC"] {
            let before = market(&account.data, symbol);
            assert!(
                oracle_thresholds(&account.data, symbol).0 < RAISED_STALE_THRESHOLD_SLOTS,
                "{symbol}: the fixture carries upstream's thresholds"
            );
            let after = market(&raised, symbol);
            let mut expected = before.oracle_price().mark_price;
            expected.spot_price_component.stale_threshold = RAISED_STALE_THRESHOLD_SLOTS;
            expected.perp_price_component.stale_threshold = RAISED_STALE_THRESHOLD_SLOTS;
            assert_eq!(
                after.oracle_price().mark_price,
                expected,
                "{symbol}: prices, reading slots and the book component stay as they were"
            );
            assert_eq!(after.risk_params(), before.risk_params(), "{symbol}");
        }
        let in_price_component = |index: usize| {
            PRICE_COMPONENT_STARTS
                .iter()
                .any(|start| (*start..*start + size_of::<PriceComponent>()).contains(&index))
        };
        assert_eq!(raised.len(), account.data.len());
        assert_eq!(
            raised
                .iter()
                .zip(&account.data)
                .enumerate()
                .position(|(index, (after, before))| after != before && !in_price_component(index)),
            None,
            "nothing outside the markets' PriceComponents is written"
        );
        assert_eq!(
            raise_stale_thresholds(&PHOENIX_PERP_ASSET_MAP, &raised).unwrap(),
            None,
            "a raised map is written once"
        );
    }

    #[test]
    fn rewinding_funding_timestamps_moves_only_those_ahead_of_the_clock() {
        let account = two_market_map_account();
        let funding = |data: &[u8], symbol: &str| *market(data, symbol).funding_accumulator();
        let seconds = |timestamp| bytemuck::cast::<_, u64>(timestamp);
        let now = seconds(funding(&account.data, "SOL").last_funding_update_timestamp) - 60;

        let rewound = rewind_funding_timestamps(&PHOENIX_PERP_ASSET_MAP, &account.data, now)
            .unwrap()
            .expect("the fixture's last funding update is ahead of the clock");

        for symbol in ["SOL", "BTC"] {
            let before = market(&account.data, symbol);
            let mut expected = *before.funding_accumulator();
            expected.last_funding_update_timestamp = bytemuck::cast(now);
            if seconds(expected.start_interval_timestamp) > now {
                expected.start_interval_timestamp = bytemuck::cast(now);
            }
            let after = market(&rewound, symbol);
            assert_eq!(*after.funding_accumulator(), expected, "{symbol}");
            assert_eq!(after.oracle_price(), before.oracle_price(), "{symbol}");
            assert_eq!(after.risk_params(), before.risk_params(), "{symbol}");
        }
        assert_eq!(
            rewind_funding_timestamps(&PHOENIX_PERP_ASSET_MAP, &rewound, now).unwrap(),
            None,
            "a rewound map is written once"
        );
        assert_eq!(
            rewind_funding_timestamps(&PHOENIX_PERP_ASSET_MAP, &account.data, u64::MAX).unwrap(),
            None,
            "timestamps before the clock stay"
        );
    }

    #[test]
    fn restamping_moves_old_reading_slots_to_the_clock_and_keeps_prices() {
        let account = two_market_map_account();
        let readings = |data: &[u8], symbol: &str| {
            let mark = market(data, symbol).oracle_price().mark_price;
            let samples: Vec<_> = mark
                .spot_price_component
                .last_exchange_spot_price
                .into_iter()
                .chain(mark.perp_price_component.last_exchange_perp_price)
                .collect();
            (mark, samples)
        };
        let (sol, _) = readings(&account.data, "SOL");
        let slot = sol.price.slot + 300;

        let restamped = restamp_readings(&PHOENIX_PERP_ASSET_MAP, &account.data, slot)
            .unwrap()
            .expect("the fixture's readings are before the slot");

        for symbol in ["SOL", "BTC"] {
            let (before, before_samples) = readings(&account.data, symbol);
            let (after, after_samples) = readings(&restamped, symbol);
            assert_eq!(after.price.ticks, before.price.ticks, "{symbol}");
            assert_eq!(after.price.slot, slot, "{symbol}");
            assert_eq!(after.spot_price_component.slot, slot, "{symbol}");
            for (after, before) in after_samples.iter().zip(&before_samples) {
                assert_eq!(after.ticks, before.ticks, "{symbol}");
                assert_eq!(
                    after.slot,
                    if before.slot == 0 { 0 } else { slot },
                    "{symbol}"
                );
            }
            assert_eq!(
                after.book_price_component, before.book_price_component,
                "{symbol}"
            );
            assert_eq!(
                market(&restamped, symbol).risk_params(),
                market(&account.data, symbol).risk_params(),
                "{symbol}"
            );
        }
        assert_eq!(
            restamp_readings(&PHOENIX_PERP_ASSET_MAP, &restamped, slot).unwrap(),
            None,
            "a restamped map is written once"
        );
        assert_eq!(
            restamp_readings(&PHOENIX_PERP_ASSET_MAP, &account.data, 1).unwrap(),
            None,
            "readings ahead of the clock stay"
        );
    }

    #[tokio::test]
    async fn keeps_every_local_market_usable_and_never_fails_an_override() {
        let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        // Nothing to raise and nowhere to fetch the map from: the override goes on.
        keep_markets_usable(&mut svm, &None, 0).await;
        assert!(
            svm.inner
                .get_account(&PHOENIX_PERP_ASSET_MAP)
                .unwrap()
                .is_none()
        );

        svm.inner
            .set_account(PHOENIX_PERP_ASSET_MAP, two_market_map_account())
            .unwrap();
        keep_markets_usable(&mut svm, &None, 0).await;
        let map = svm
            .inner
            .get_account(&PHOENIX_PERP_ASSET_MAP)
            .unwrap()
            .unwrap();
        for symbol in ["SOL", "BTC"] {
            assert_eq!(
                oracle_thresholds(&map.data, symbol),
                RAISED_THRESHOLDS,
                "{symbol}"
            );
        }
    }

    #[test]
    fn reads_template_inputs_as_strings_or_numbers() {
        let values = HashMap::from([
            ("symbol".to_string(), serde_json::json!("SOL")),
            ("target_ticks".to_string(), serde_json::json!("10750")),
            ("as_number".to_string(), serde_json::json!(10750)),
        ]);
        assert_eq!(text_input(&values, "symbol").unwrap(), "SOL");
        assert_eq!(whole_number_input(&values, "target_ticks").unwrap(), 10750);
        assert_eq!(whole_number_input(&values, "as_number").unwrap(), 10750);

        let bad = HashMap::from([
            ("symbol".to_string(), serde_json::json!("")),
            ("target_ticks".to_string(), serde_json::json!("-1")),
            ("as_number".to_string(), serde_json::json!(1.5)),
        ]);
        assert!(text_input(&bad, "symbol").is_err());
        assert!(text_input(&bad, "missing").is_err());
        assert!(whole_number_input(&bad, "target_ticks").is_err());
        assert!(whole_number_input(&bad, "as_number").is_err());
    }

    #[tokio::test]
    async fn a_liquidation_ready_run_takes_one_or_more_different_symbols() {
        let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        let trader = Pubkey::new_unique();
        for (symbols, refusal) in [
            (" , ", "list at least one market symbol"),
            ("SOL, ETH, SOL", "SOL is listed twice"),
        ] {
            let error = prepare_phoenix_override(
                &mut svm,
                LIQUIDATION_READY_TEMPLATE_ID,
                &trader,
                &Account::default(),
                &HashMap::from([("symbols".to_string(), serde_json::json!(symbols))]),
                &None,
                0,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains(refusal), "{symbols:?}: {error}");
        }
    }

    #[tokio::test]
    async fn a_spline_market_maker_is_refused_before_anything_is_fetched() {
        let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        let trader = Pubkey::new_unique();
        let mut header = TraderHeader::zeroed();
        header.discriminant = u64::from_le_bytes(PhoenixAccount::Trader.discriminant());
        header.num_markets_with_splines = 2;
        let account = Account {
            lamports: 1,
            data: bytemuck::bytes_of(&header).to_vec(),
            owner: PHOENIX_PROGRAM_ID,
            ..Account::default()
        };
        svm.inner.set_account(trader, account.clone()).unwrap();
        let error = prepare_phoenix_override(
            &mut svm,
            LIQUIDATION_READY_TEMPLATE_ID,
            &trader,
            &account,
            &HashMap::from([("symbols".to_string(), serde_json::json!("SOL"))]),
            &None,
            0,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(&format!(
                "{trader} quotes splines on 2 markets; liquidating a spline market maker is not \
                 supported"
            )),
            "{error}"
        );
    }

    const LIQUIDATOR: Pubkey =
        Pubkey::from_str_const("BP7sV1VFnbPMPyJX1tZNbXHbZkyLNFEaBWJhyMvkbxKz");
    const QUOTE_MINT: &str = "PhUsd11YkbjSaWjFncfAAmatntsjx3MgDR9B6g1ks3A";

    fn liquidation_accounts() -> Vec<(&'static str, Pubkey)> {
        let trader = Pubkey::new_unique();
        vec![
            (
                "phoenixProgram",
                Pubkey::from_str(&phoenix_idl().address).unwrap(),
            ),
            ("phoenixLogAuthority", Pubkey::new_unique()),
            ("globalConfiguration", Pubkey::new_unique()),
            ("liquidatorWallet", LIQUIDATOR),
            ("liquidatedTrader", trader),
            ("traderAccount", trader),
            ("perpAssetMap", Pubkey::new_unique()),
            ("globalTraderIndex", Pubkey::new_unique()),
            ("activeTraderBuffer", Pubkey::new_unique()),
            ("orderbook", Pubkey::new_unique()),
            ("splines", Pubkey::new_unique()),
        ]
    }

    #[test]
    fn encodes_a_mainnet_liquidation_byte_for_byte() {
        // liquidate_via_market_order data of a mainnet transaction at slot 429053028.
        let mainnet = "fbf1b86c46467fc605d08ca0437ee7562b6648e76d5094af350b57c3820b6716fc96e777d449a9dd\
                       951c0000000000004d4000000000000000";
        let instruction = phoenix_instruction(
            "LiquidateViaMarketOrder",
            &liquidation_accounts(),
            &serde_json::json!({
                "params": {
                    "assetMint": QUOTE_MINT,
                    "liquidationSize": { "inner": 7317 },
                    "liquidationPrice": { "inner": 16461 },
                    "fillOrKill": false,
                }
            }),
        )
        .unwrap();
        let expected: Vec<u8> = (0..mainnet.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&mainnet[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(instruction.data, expected);
        assert_eq!(instruction.accounts.len(), 11);
        let liquidator = &instruction.accounts[3];
        assert_eq!(
            (
                liquidator.pubkey,
                liquidator.is_signer,
                liquidator.is_writable
            ),
            (LIQUIDATOR, true, false)
        );
        assert!(instruction.accounts[4].is_writable);
    }

    #[test]
    fn capability_toggles_keep_a_capability_left_empty() {
        let values = HashMap::from([
            ("PlaceLimitOrder".to_string(), serde_json::json!("")),
            ("WithdrawCollateral".to_string(), serde_json::json!("false")),
            ("DepositCollateral".to_string(), serde_json::json!(true)),
        ]);
        assert_eq!(
            capability_toggles(&values).unwrap(),
            serde_json::json!({ "params": { "toggles": [
                { "target": { "DepositCollateral": null }, "enable": true },
                { "target": { "WithdrawCollateral": null }, "enable": false },
            ] } })
        );
        let empty = HashMap::from([("PlaceLimitOrder".to_string(), serde_json::json!(""))]);
        let error = capability_toggles(&empty).unwrap_err().to_string();
        assert!(error.contains("set at least one"), "{error}");
        let bad = HashMap::from([("PlaceLimitOrder".to_string(), serde_json::json!("yes"))]);
        assert!(capability_toggles(&bad).is_err());
    }

    #[test]
    fn refuses_missing_and_unknown_accounts_and_arguments() {
        let args = serde_json::json!({
            "params": {
                "assetMint": QUOTE_MINT,
                "liquidationSize": { "inner": 1 },
                "liquidationPrice": { "inner": 1 },
                "fillOrKill": false,
            }
        });
        let mut missing = liquidation_accounts();
        missing.retain(|(name, _)| *name != "orderbook");
        assert!(phoenix_instruction("LiquidateViaMarketOrder", &missing, &args).is_err());

        let mut unknown = liquidation_accounts();
        unknown.push(("orderBook", Pubkey::new_unique()));
        assert!(phoenix_instruction("LiquidateViaMarketOrder", &unknown, &args).is_err());

        assert!(
            phoenix_instruction(
                "LiquidateViaMarketOrder",
                &liquidation_accounts(),
                &serde_json::json!({})
            )
            .is_err()
        );
        assert!(phoenix_instruction("NoSuchInstruction", &[], &args).is_err());
    }

    fn funded_svm(owner: &Pubkey) -> SurfnetSvm {
        let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        svm.inner.airdrop(owner, 5_000_000_000).unwrap();
        svm
    }

    #[test]
    fn returns_what_the_instructions_wrote_and_leaves_the_local_vm_alone() {
        // Neither key is ever available here, yet the transfer runs as its owner.
        let owner = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let svm = funded_svm(&owner);
        let before = svm.inner.get_account(&owner).unwrap().unwrap();

        let written = run_instructions(
            &svm,
            &[system_instruction::transfer(&owner, &recipient, 1_000_000)],
        )
        .unwrap();

        let lamports: Vec<(Pubkey, u64)> = written
            .iter()
            .map(|(pubkey, account)| (*pubkey, account.lamports))
            .collect();
        assert_eq!(
            lamports,
            vec![(owner, before.lamports - 1_000_000), (recipient, 1_000_000)]
        );
        assert_eq!(svm.inner.get_account(&owner).unwrap().unwrap(), before);
        assert!(svm.inner.get_account(&recipient).unwrap().is_none());
    }

    #[test]
    fn a_failing_instruction_fails_the_run_with_its_logs() {
        let owner = Pubkey::new_unique();
        let svm = funded_svm(&owner);
        let error = run_instructions(
            &svm,
            &[
                system_instruction::transfer(&owner, &Pubkey::new_unique(), 1_000_000),
                system_instruction::transfer(&owner, &Pubkey::new_unique(), u64::MAX),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("instruction 1 failed"), "{error}");
        assert!(error.contains("logs"), "{error}");
    }
}
