//! Velocity integration tests. They fetch real mainnet accounts and the deployed program, so they
//! need a network connection and `--features integration-tests`. `SURFPOOL_TEST_RPC_URL`
//! overrides the endpoint.
//!
//! The behavioral tests run the program from its ProgramData account in a local VM and send real
//! Velocity instructions, encoded through the bundled IDL. Velocity upgrades often: a redeploy
//! fails these tests until the pinned slot is moved and the bundled IDL is checked against the new
//! release.

use std::{collections::HashMap, ops::Range, sync::OnceLock};

use anchor_lang_idl::types::{IdlDefinedFields, IdlInstructionAccountItem, IdlTypeDefTy};
use serde_json::json;
use solana_account::Account;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{config::RpcAccountInfoConfig, filter::RpcFilterType};
use solana_sdk_ids::{system_program, sysvar};
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use spl_token_interface::state::{Account as TokenAccount, AccountState};
use surfpool_types::{AccountAddress, DEFAULT_MAINNET_RPC_URL, OverrideInstance, Scenario};
use txtx_addon_kit::{indexmap::IndexMap, types::types::Value};
use txtx_addon_network_svm::codec::idl::IdlRef;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    tests::helpers::diff_indices,
    types::RemoteRpcResult,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";

const PROGRAM: Pubkey = Pubkey::from_str_const("vELoC1audYbSYVRXn1vPaV8Axoa9oU6BYmNGZZBDZ1P");
const STATE: Pubkey = Pubkey::from_str_const("2etx5NvPNxeMZ7EfHE6GjJfW2imRYEUANehNS1WB4CVW");
const PROGRAMDATA: Pubkey = Pubkey::from_str_const("HkRf36jvUB32dnHjHqpMnPcKJhUP2TUKJTH7RDTifJm7");
/// Release program-velocity-v2.169.0.
const DEPLOYED_SLOT: u64 = 451_568_782;

const ORACLE_PRICE: &str = "velocity-oracle-price";
const MARKET_RISK: &str = "velocity-perp-market-risk";
const EXCHANGE_RISK: &str = "velocity-exchange-risk";
const FEES: &str = "velocity-fees";
const SPOT_RISK: &str = "velocity-spot-market-risk";

const SOL_PERP: u16 = 0;
const BTC_PERP: u16 = 1;
const USDT_SPOT_MARKET: u16 = 0;
const SOL_SPOT_MARKET: u16 = 1;
const TRADER_DEPOSIT: u64 = 20_000_000_000;
const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;

// Byte ranges from the bundled IDL, discriminator included.
const PERP_MARKET_LEN: usize = 1560;
const BASE_ASSET_AMOUNT_LONG: Range<usize> = 40..56;
const MAX_OPEN_INTEREST: Range<usize> = 152..168;
const PENDING_IF_FEE: Range<usize> = 264..280;
const MARKET_ORACLE: Range<usize> = 312..344;
const TAKER_FEE_ADDON: Range<usize> = 412..414;
const MARKET_NAME: Range<usize> = 424..456;
const ORDER_STEP_SIZE: Range<usize> = 544..552;
const ORDER_TICK_SIZE: Range<usize> = 552..560;
const MARGIN_RATIO_INITIAL: Range<usize> = 616..620;
const MARGIN_RATIO_MAINTENANCE: Range<usize> = 620..624;
const PAUSED_OPERATIONS: usize = 645;
const FEE_ADJUSTMENT: Range<usize> = 648..650;
const IMMEDIATE_FILL_DELAY_OVERRIDE: usize = 674;
const MM_ORACLE_SEQUENCE_ID: Range<usize> = 816..824;
const ORACLE_TWAP_5MIN: Range<usize> = 880..888;

const PRICE: Range<usize> = 8..16;
const PUBLISH_TIME: Range<usize> = 16..24;
const POSTED_SLOT: Range<usize> = 24..32;
const EXPONENT: Range<usize> = 32..36;

const STATE_SIGNER: Range<usize> = 488..520;
const SLOTS_BEFORE_STALE_FOR_AMM: Range<usize> = 1304..1312;
const SLOTS_BEFORE_STALE_FOR_MARGIN: Range<usize> = 1312..1320;
const LIQUIDATION_MARGIN_BUFFER: Range<usize> = 1352..1356;
const NUMBER_OF_MARKETS: Range<usize> = 1358..1360;
const NUMBER_OF_SPOT_MARKETS: Range<usize> = 1360..1362;
const SLOT_DURATION_MS: Range<usize> = 1506..1508;

const SPOT_ORACLE: Range<usize> = 40..72;
const SPOT_MINT: Range<usize> = 72..104;
const SPOT_VAULT: Range<usize> = 104..136;
const CUMULATIVE_BORROW_INTEREST: Range<usize> = 344..360;
const LAST_INTEREST_TS: Range<usize> = 584..592;
const MAINTENANCE_ASSET_WEIGHT: Range<usize> = 660..664;
const SPOT_DECIMALS: Range<usize> = 696..700;

const USER_PERP_POSITIONS: usize = 424;
const PERP_POSITION_LEN: usize = 80;
const POSITION_BASE: Range<usize> = 8..16;
const POSITION_QUOTE: Range<usize> = 16..24;
const POSITION_QUOTE_ENTRY: Range<usize> = 32..40;
const USER_ORDERS: usize = 1064;
const ORDER_LEN: usize = 104;
const TOTAL_FEE_PAID: Range<usize> = 72..80;
const TOTAL_FEE_REBATE: Range<usize> = 80..88;

/// State `slots_before_stale_for_margin` is 120 units of 400 ms, read through the program's 200 ms
/// slot clock. Stable coin prices get three times that.
const MARGIN_STALENESS_SLOTS: u64 = 240;
const STABLE_COIN_STALENESS_SLOTS: u64 = 3 * MARGIN_STALENESS_SLOTS;
/// State `slots_before_stale_for_amm`, 10 units of 400 ms.
const AMM_STALENESS_SLOTS: u64 = 20;
/// How far the tests move the clock past a price.
const FAR_PAST: u64 = 2000;
const LIQUIDATION_PAUSED: u8 = 0b0010_0000;
const EXCHANGE_LIQUIDATIONS_PAUSED: u8 = 0b0001_0000;
/// `PlaceAndTakeOrderSuccessCondition::FullFill`.
const FULL_FILL: u32 = 2;

const SUFFICIENT_COLLATERAL: u32 = 6004;
const PRICE_BANDS_BREACHED: u32 = 6023;
const EXCHANGE_PAUSED: u32 = 6024;
const INVALID_ORACLE: u32 = 6035;
const CASTING_FAILURE: u32 = 6041;
const ORDER_NOT_RISK_REDUCING: u32 = 6046;
const MAX_OPEN_INTEREST_REACHED: u32 = 6154;
const INVALID_LIQUIDATION: u32 = 6220;
const ORACLE_STALE_FOR_AMM: u32 = 6266;
const NOT_FILLED: u32 = 6289;

fn client() -> SurfnetRemoteClient {
    SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_MAINNET_RPC_URL.to_string()),
    )
}

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    client()
        .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| {
            result.map_account().unwrap_or_else(|_| {
                panic!("{address} no longer exists on mainnet; the test needs a new address")
            })
        })
        .collect()
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &PROGRAM).0
}

fn perp_market_address(index: u16) -> Pubkey {
    pda(&[b"perp_market", &index.to_le_bytes()])
}

/// The `User` of an authority's first sub-account and its `UserStats`.
fn user_keys(trader: &Keypair) -> (Pubkey, Pubkey) {
    let authority = trader.pubkey();
    let user = pda(&[b"user", authority.as_ref(), &0u16.to_le_bytes()]);
    (user, pda(&[b"user_stats", authority.as_ref()]))
}

fn read<const N: usize>(data: &[u8], range: Range<usize>) -> [u8; N] {
    data[range].try_into().unwrap()
}

fn i64_at(data: &[u8], range: Range<usize>) -> i64 {
    i64::from_le_bytes(read(data, range))
}

fn u64_at(data: &[u8], range: Range<usize>) -> u64 {
    u64::from_le_bytes(read(data, range))
}

fn u32_at(data: &[u8], range: Range<usize>) -> u32 {
    u32::from_le_bytes(read(data, range))
}

fn pubkey_at(data: &[u8], range: Range<usize>) -> Pubkey {
    Pubkey::new_from_array(read(data, range))
}

/// The bundled IDL, which also encodes every instruction the tests send.
fn idl() -> &'static IdlRef {
    static IDL: OnceLock<IdlRef> = OnceLock::new();
    IDL.get_or_init(|| {
        let registry = TemplateRegistry::new();
        IdlRef::from_idl(registry.get(ORACLE_PRICE).unwrap().idl().clone())
    })
}

fn account_discriminator(name: &str) -> &'static [u8] {
    &idl().get_account(name).unwrap().discriminator
}

fn int(value: impl Into<i128>) -> Value {
    Value::integer(value.into())
}

/// A Velocity instruction encoded through the bundled IDL. `accounts` come in the IDL's order and
/// take its writable and signer flags; `remaining` follow as given.
fn instruction(
    name: &str,
    accounts: &[Pubkey],
    args: Vec<Value>,
    remaining: Vec<AccountMeta>,
) -> Instruction {
    let bundled = idl();
    let expected = &bundled.get_instruction(name).unwrap().accounts;
    assert_eq!(accounts.len(), expected.len(), "{name} account count");
    let metas = expected.iter().zip(accounts).map(|(item, pubkey)| {
        let IdlInstructionAccountItem::Single(account) = item else {
            panic!("{name} has a composite account");
        };
        AccountMeta {
            pubkey: *pubkey,
            is_signer: account.signer,
            is_writable: account.writable,
        }
    });
    let discriminator = bundled.get_discriminator(name).unwrap();
    let data = [discriminator, bundled.get_encoded_args(name, args).unwrap()].concat();
    Instruction::new_with_bytes(PROGRAM, &data, metas.chain(remaining).collect())
}

/// Perp `OrderParams` without expiry, trigger, auction or builder settings: every field not set
/// here is an `Option` the IDL lists, left empty.
fn order(
    kind: &str,
    direction: &str,
    base: u64,
    price: u64,
    market: u16,
    post_only: &str,
) -> Value {
    let variant = |name: &str| Value::object(IndexMap::from([(name.to_string(), Value::null())]));
    let set = IndexMap::from([
        ("order_type", variant(kind)),
        ("market_type", variant("Perp")),
        ("direction", variant(direction)),
        ("user_order_id", int(0)),
        ("base_asset_amount", int(base)),
        ("price", int(price)),
        ("market_index", int(market)),
        ("reduce_only", Value::bool(false)),
        ("post_only", variant(post_only)),
        ("bit_flags", int(0)),
        ("trigger_condition", variant("Above")),
    ]);
    let ty = &idl().get_type("OrderParams").unwrap().ty;
    let IdlTypeDefTy::Struct {
        fields: Some(IdlDefinedFields::Named(fields)),
    } = ty
    else {
        panic!("OrderParams is not a struct with named fields");
    };
    let value = |name: &str| set.get(name).cloned().unwrap_or_else(Value::null);
    Value::object(
        fields
            .iter()
            .map(|field| (field.name.clone(), value(&field.name)))
            .collect(),
    )
}

/// The oracle price at the program's 10^6 price precision.
fn oracle_price_in_price_precision(oracle: &[u8]) -> i128 {
    i128::from(i64_at(oracle, PRICE)) / oracle_scale(oracle)
}

/// Raw oracle units per unit of 10^6 price precision.
fn oracle_scale(oracle: &[u8]) -> i128 {
    let exponent = i32::from_le_bytes(read(oracle, EXPONENT));
    assert!(exponent <= -6, "exponent {exponent} is coarser than 10^-6");
    10i128.pow((-exponent - 6) as u32)
}

/// A field of the user's position in `market`, 0 when it holds none.
fn position_field(user: &[u8], market: u16, field: Range<usize>) -> i64 {
    (0..8)
        .map(|slot| USER_PERP_POSITIONS + slot * PERP_POSITION_LEN)
        .find(|offset| u16::from_le_bytes(read(user, offset + 76..offset + 78)) == market)
        .map(|offset| i64_at(user, offset + field.start..offset + field.end))
        .unwrap_or(0)
}

fn oracle_values(price: i64, clock: &Clock) -> serde_json::Value {
    let publish_time = clock.unix_timestamp as u64 * 1_000_000;
    json!({"price": price, "posted_slot": clock.slot, "publish_time": publish_time})
}

/// The deployed state the behavioral tests start from: the program, the State, every market,
/// every oracle the markets read and every spot market's vault and mint, with the clock read
/// alongside the oracles.
struct Mainnet {
    elf: Vec<u8>,
    clock: Clock,
    accounts: HashMap<Pubkey, Account>,
    perp_markets: Vec<Pubkey>,
    spot_markets: Vec<Pubkey>,
    oracles: Vec<Pubkey>,
}

async fn mainnet() -> &'static Mainnet {
    static MAINNET: tokio::sync::OnceCell<Mainnet> = tokio::sync::OnceCell::const_new();
    MAINNET.get_or_init(load_mainnet).await
}

async fn load_mainnet() -> Mainnet {
    let [programdata, state] =
        <[Account; 2]>::try_from(fetch(&[PROGRAMDATA, STATE]).await).unwrap();
    let deployed_slot = u64_at(&programdata.data, 4..12);
    assert_eq!(
        deployed_slot, DEPLOYED_SLOT,
        "Velocity was redeployed at slot {deployed_slot}; check the bundled IDL against the new \
         release, then move the pin"
    );

    let perp_count = u16::from_le_bytes(read(&state.data, NUMBER_OF_MARKETS));
    let spot_count = u16::from_le_bytes(read(&state.data, NUMBER_OF_SPOT_MARKETS));
    let perp_markets: Vec<Pubkey> = (0..perp_count).map(perp_market_address).collect();
    let by_index = |seed: &[u8], index: u16| pda(&[seed, &index.to_le_bytes()]);
    let spot_markets: Vec<Pubkey> = (0..spot_count)
        .map(|i| by_index(b"spot_market", i))
        .collect();
    // A vault is read with its market, so the two agree on what the market holds.
    let vaults: Vec<Pubkey> = (0..spot_count)
        .map(|i| by_index(b"spot_market_vault", i))
        .collect();
    let markets = [perp_markets.clone(), spot_markets.clone(), vaults.clone()].concat();
    let market_accounts = fetch(&markets).await;
    let (perps, rest) = market_accounts.split_at(perp_markets.len());
    let spots = &rest[..spot_markets.len()];
    for (spot, vault) in spots.iter().zip(&vaults) {
        assert_eq!(pubkey_at(&spot.data, SPOT_VAULT), *vault);
    }

    let perp_oracles = perps
        .iter()
        .map(|market| pubkey_at(&market.data, MARKET_ORACLE));
    let spot_oracles = spots.iter().map(|spot| pubkey_at(&spot.data, SPOT_ORACLE));
    let mut oracles: Vec<Pubkey> = perp_oracles.chain(spot_oracles).collect();
    oracles.sort();
    oracles.dedup();
    let mints = spots.iter().map(|spot| pubkey_at(&spot.data, SPOT_MINT));
    let feeds: Vec<Pubkey> = oracles.iter().copied().chain(mints).collect();
    let mut fetched = fetch(&[&[sysvar::clock::ID][..], &feeds].concat()).await;
    let clock: Clock = bincode::deserialize(&fetched.remove(0).data).unwrap();

    let accounts = [(STATE, state)]
        .into_iter()
        .chain(markets.into_iter().zip(market_accounts))
        .chain(feeds.into_iter().zip(fetched))
        .collect();
    let elf = programdata.data[45..].to_vec();
    Mainnet {
        elf,
        clock,
        accounts,
        perp_markets,
        spot_markets,
        oracles,
    }
}

impl Mainnet {
    fn account(&self, address: &Pubkey) -> &Account {
        self.accounts
            .get(address)
            .unwrap_or_else(|| panic!("{address} is not in the mainnet snapshot"))
    }

    fn market(&self, index: u16) -> &[u8] {
        &self.account(&perp_market_address(index)).data
    }

    fn oracle_of(&self, index: u16) -> Pubkey {
        pubkey_at(self.market(index), MARKET_ORACLE)
    }

    fn spot_market(&self, index: u16) -> &[u8] {
        &self.account(&self.spot_markets[index as usize]).data
    }

    /// The live oracle price of a perp market at the program's 10^6 price precision.
    fn price(&self, market: u16) -> i128 {
        oracle_price_in_price_precision(&self.account(&self.oracle_of(market)).data)
    }

    /// The base amount, in whole order steps, worth about `notional` USDT at the snapshot price.
    fn base_for(&self, market: u16, notional: u64) -> u64 {
        let step = u64_at(self.market(market), ORDER_STEP_SIZE);
        let base = i128::from(notional) * 1_000_000_000 / self.price(market);
        u64::try_from(base).unwrap() / step * step
    }

    /// The remaining accounts every margin-reading instruction takes: oracles, spot markets, then
    /// perp markets.
    fn market_accounts(&self, perp: Option<u16>, spot: Option<u16>) -> Vec<AccountMeta> {
        let meta = |pubkey: &Pubkey, is_writable: bool| AccountMeta {
            pubkey: *pubkey,
            is_signer: false,
            is_writable,
        };
        let spots = (0..).zip(&self.spot_markets);
        let perps = (0..).zip(&self.perp_markets);
        self.oracles
            .iter()
            .map(|oracle| meta(oracle, false))
            .chain(spots.map(|(index, market)| meta(market, Some(index) == spot)))
            .chain(perps.map(|(index, market)| meta(market, Some(index) == perp)))
            .collect()
    }

    fn place_order(&self, trader: &Keypair, market: u16, params: Value) -> Instruction {
        let accounts = [STATE, user_keys(trader).0, trader.pubkey()];
        let remaining = self.market_accounts(Some(market), None);
        instruction("place_perp_order", &accounts, vec![params], remaining)
    }

    /// An order that must fill in full at once, against `maker`'s orders or the AMM.
    fn take_order(
        &self,
        taker: &Keypair,
        market: u16,
        params: Value,
        maker: Option<&Keypair>,
    ) -> Instruction {
        let (user, stats) = user_keys(taker);
        let mut remaining = self.market_accounts(Some(market), None);
        if let Some((maker_user, maker_stats)) = maker.map(user_keys) {
            remaining.push(AccountMeta::new(maker_user, false));
            remaining.push(AccountMeta::new(maker_stats, false));
        }
        let accounts = [STATE, user, stats, taker.pubkey()];
        let args = vec![params, int(FULL_FILL)];
        instruction("place_and_take_perp_order", &accounts, args, remaining)
    }

    /// A keeper fill of `trader`'s newest order, with the trader as its own filler.
    fn fill_order(&self, trader: &Keypair, market: u16) -> Instruction {
        let (user, stats) = user_keys(trader);
        let accounts = [STATE, trader.pubkey(), user, stats, user, stats];
        let args = vec![Value::null(), Value::null()];
        let remaining = self.market_accounts(Some(market), None);
        instruction("fill_perp_order", &accounts, args, remaining)
    }

    fn withdraw_one_usdt(&self, trader: &Keypair) -> Instruction {
        let (user, stats) = user_keys(trader);
        let usdt = self.spot_market(USDT_SPOT_MARKET);
        let signer = pubkey_at(&self.account(&STATE).data, STATE_SIGNER);
        let wallet = token_account_address(&trader.pubkey(), &pubkey_at(usdt, SPOT_MINT));
        let vault = pubkey_at(usdt, SPOT_VAULT);
        let (owner, token_program) = (trader.pubkey(), spl_token_interface::ID);
        let accounts = [
            STATE,
            user,
            stats,
            owner,
            vault,
            signer,
            wallet,
            token_program,
        ];
        let args = vec![int(USDT_SPOT_MARKET), int(1_000_000u64), Value::bool(false)];
        let remaining = self.market_accounts(None, Some(USDT_SPOT_MARKET));
        instruction("withdraw", &accounts, args, remaining)
    }

    fn settle_pnl(&self, trader: &Keypair, market: u16) -> Instruction {
        let vault = pubkey_at(self.spot_market(USDT_SPOT_MARKET), SPOT_VAULT);
        let accounts = [STATE, user_keys(trader).0, trader.pubkey(), vault];
        let remaining = self.market_accounts(Some(market), Some(USDT_SPOT_MARKET));
        instruction("settle_pnl", &accounts, vec![int(market)], remaining)
    }

    /// The keeper crank that refreshes a market's AMM; it loads every perp market it is given as
    /// writable, so it gets only this one.
    fn update_amms(&self, keeper: &Keypair, market: u16) -> Instruction {
        let oracles = self
            .oracles
            .iter()
            .map(|oracle| AccountMeta::new_readonly(*oracle, false));
        let market_meta = AccountMeta::new(perp_market_address(market), false);
        let remaining = oracles.chain([market_meta]).collect();
        let args = vec![Value::array(vec![int(market)])];
        instruction("update_amms", &[STATE, keeper.pubkey()], args, remaining)
    }

    /// The oracle price, in raw oracle units, at which an account holding `base` in `market` keeps
    /// half its maintenance requirement, from the program's reading at the snapshot price. The
    /// requirement scales with the price; collateral moves by the position's PnL.
    fn half_maintenance_price(&self, market: u16, base: i64, reading: (i128, i128)) -> i64 {
        let (collateral, requirement) = reading;
        let (price, base) = (self.price(market), i128::from(base));
        let numerator = 2 * price * (1_000_000_000 * collateral - base * price);
        let target = numerator / (1_000_000_000 * requirement - 2 * base * price);
        let scale = oracle_scale(&self.account(&self.oracle_of(market)).data);
        i64::try_from(target * scale).unwrap()
    }
}

fn token_account_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    spl_associated_token_account_interface::address::get_associated_token_address(owner, mint)
}

fn local_account(locker: &SurfnetSvmLocker, address: &Pubkey) -> Account {
    locker
        .with_svm_reader(|svm| svm.get_account(address))
        .unwrap()
        .unwrap_or_else(|| panic!("{address} is missing from the local VM"))
}

fn assert_oracle(locker: &SurfnetSvmLocker, oracle: &Pubkey, price: i64, clock: &Clock) {
    let data = local_account(locker, oracle).data;
    let written = (
        i64_at(&data, PRICE),
        u64_at(&data, POSTED_SLOT),
        u64_at(&data, PUBLISH_TIME),
    );
    let expected = (price, clock.slot, clock.unix_timestamp as u64 * 1_000_000);
    assert_eq!(written, expected, "price, posted_slot, publish_time");
}

type Sent = Result<Vec<String>, (TransactionError, Vec<String>)>;

fn expect_error(result: Sent, code: u32, what: &str) {
    match result {
        Err((TransactionError::InstructionError(_, InstructionError::Custom(got)), _))
            if got == code => {}
        other => panic!("{what}: expected error {code}, got {other:#?}"),
    }
}

fn expect_success(result: Sent, what: &str) {
    if let Err((error, logs)) = result {
        panic!("{what} failed with {error:?}; logs {logs:#?}");
    }
}

fn logged(result: &Sent, text: &str) -> bool {
    let (Ok(logs) | Err((_, logs))) = result;
    logs.iter().any(|line| line.contains(text))
}

/// The program's maintenance reading of a healthy account, (total collateral, requirement), from
/// the log of the liquidation it refuses.
fn healthy_reading(result: Sent) -> (i128, i128) {
    let logs = match result {
        Err((
            TransactionError::InstructionError(_, InstructionError::Custom(SUFFICIENT_COLLATERAL)),
            logs,
        )) => logs,
        other => panic!("the account should start healthy: {other:#?}"),
    };
    let line = logs
        .iter()
        .find(|line| line.contains("margin calculation: "));
    let line = line.expect("the program logs its margin calculation");
    let field = |key: &str| -> i128 {
        let rest = &line[line.find(key).unwrap() + key.len()..];
        let end = rest
            .find(|c: char| c != '-' && !c.is_ascii_digit())
            .unwrap();
        rest[..end].parse().unwrap()
    };
    (
        field(", total_collateral: "),
        field(", margin_requirement: "),
    )
}

/// A local VM with the deployed program and the mainnet snapshot, at the snapshot's clock, with
/// every exchange-wide pause lifted.
struct Vm {
    locker: SurfnetSvmLocker,
    mainnet: &'static Mainnet,
}

impl Vm {
    async fn new(mainnet: &'static Mainnet) -> Self {
        let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        let locker = SurfnetSvmLocker::new(svm);
        locker.with_svm_writer(|svm| {
            svm.inner.set_sysvar(&mainnet.clock);
            svm.inner.svm.add_program(PROGRAM, &mainnet.elf).unwrap();
            for (address, account) in &mainnet.accounts {
                svm.set_account(address, account.clone()).unwrap();
            }
        });
        let vm = Self { locker, mainnet };
        vm.apply(EXCHANGE_RISK, STATE, json!({"exchange_status": 0}))
            .await;
        vm
    }

    fn data(&self, address: &Pubkey) -> Vec<u8> {
        local_account(&self.locker, address).data
    }

    fn clock(&self) -> Clock {
        self.locker.with_svm_reader(|svm| svm.inner.get_sysvar())
    }

    fn set_slot(&self, slot: u64) {
        let mut clock = self.clock();
        clock.unix_timestamp += (slot.saturating_sub(clock.slot) / 5) as i64;
        clock.slot = slot;
        self.locker
            .with_svm_writer(|svm| svm.inner.set_sysvar(&clock));
    }

    /// Applies one template to `address` through the scenario registry and materializer.
    async fn apply(&self, template: &str, address: Pubkey, values: serde_json::Value) {
        let slot = self.clock().slot;
        let mut scenario = Scenario::new(template.to_string(), "Velocity test".to_string());
        let address = AccountAddress::Pubkey(address.to_string());
        let values = values.as_object().unwrap().clone().into_iter().collect();
        let instance = OverrideInstance::new(template.to_string(), 0, address).with_values(values);
        scenario.add_override(instance);
        self.locker.register_scenario(scenario, Some(slot)).unwrap();
        let materialized = self.locker.materialize_overrides_for_slot(&None, slot);
        materialized.await.unwrap();
    }

    fn send(&self, signer: &Keypair, instructions: &[Instruction]) -> Sent {
        let budget = ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);
        let instructions = [&[budget], instructions].concat();
        self.locker.with_svm_writer(|svm| {
            // Identical retries would otherwise be refused as already processed.
            svm.inner.expire_blockhash();
            let blockhash = svm.inner.svm.latest_blockhash();
            let payer = signer.pubkey();
            let transaction = Transaction::new_signed_with_payer(
                &instructions,
                Some(&payer),
                &[signer],
                blockhash,
            );
            let sent = svm.inner.send_transaction(transaction);
            sent.map(|meta| meta.logs)
                .map_err(|failed| (failed.err, failed.meta.logs))
        })
    }

    /// Writes bytes into a local account directly, outside any scenario.
    fn write(&self, address: &Pubkey, range: Range<usize>, bytes: &[u8]) {
        self.locker.with_svm_writer(|svm| {
            let mut account = svm.get_account(address).unwrap().unwrap();
            account.data[range].copy_from_slice(bytes);
            svm.set_account(address, account).unwrap();
        });
    }

    /// Posts the price at `slot` again, outside any scenario, with a publish time after every
    /// market's own MM oracle price, so fills and margin checks read this price and age it from
    /// `slot`.
    fn stamp(&self, oracle: &Pubkey, slot: u64) {
        let markets = &self.mainnet.perp_markets;
        let mm_price = |market| u64_at(&self.mainnet.account(market).data, MM_ORACLE_SEQUENCE_ID);
        let newest_mm_price = markets.iter().map(mm_price).max().unwrap();
        let published = u64_at(&self.data(oracle), PUBLISH_TIME).max(newest_mm_price) + 1;
        self.write(oracle, POSTED_SLOT, &slot.to_le_bytes());
        self.write(oracle, PUBLISH_TIME, &published.to_le_bytes());
    }

    /// A trader holding a real deposit of 20,000 USDT.
    fn funded_trader(&self) -> Keypair {
        self.trader(USDT_SPOT_MARKET, TRADER_DEPOSIT)
    }

    /// A trader created by the program's own instructions, holding a real deposit of `amount` base
    /// units into `spot_market`.
    fn trader(&self, spot_market: u16, amount: u64) -> Keypair {
        let trader = Keypair::new();
        let authority = trader.pubkey();
        let market = self.mainnet.spot_market(spot_market);
        let mint = pubkey_at(market, SPOT_MINT);
        let wallet = token_account_address(&authority, &mint);
        // Wrapped SOL is held as the token account's lamports.
        let native = mint == spl_token_interface::native_mint::ID;
        let mut data = vec![0u8; TokenAccount::LEN];
        TokenAccount {
            mint,
            owner: authority,
            amount,
            state: AccountState::Initialized,
            is_native: native.then_some(TOKEN_ACCOUNT_RENT).into(),
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        let lamports = TOKEN_ACCOUNT_RENT + if native { amount } else { 0 };
        let token_program = spl_token_interface::ID;
        let token = Account {
            lamports,
            data,
            owner: token_program,
            ..Account::default()
        };
        self.locker.with_svm_writer(|svm| {
            svm.inner.airdrop(&authority, 10_000_000_000).unwrap();
            svm.set_account(&wallet, token).unwrap();
        });

        let (user, stats) = user_keys(&trader);
        let (rent, system) = (sysvar::rent::ID, system_program::ID);
        let stats_accounts = [stats, STATE, authority, authority, rent, system];
        let user_accounts = [user, stats, STATE, authority, authority, rent, system];
        let user_args = vec![int(0), Value::array(vec![int(0); 32])];
        let create = [
            instruction("initialize_user_stats", &stats_accounts, vec![], vec![]),
            instruction("initialize_user", &user_accounts, user_args, vec![]),
        ];
        expect_success(self.send(&trader, &create), "creating a trader");
        let vault = pubkey_at(market, SPOT_VAULT);
        let accounts = [STATE, user, stats, authority, vault, wallet, token_program];
        let args = vec![int(spot_market), int(amount), Value::bool(false)];
        let remaining = self.mainnet.market_accounts(None, Some(spot_market));
        let deposit = instruction("deposit", &accounts, args, remaining);
        let what = format!("depositing into spot market {spot_market} (MaxDeposit: it is full)");
        expect_success(self.send(&trader, &[deposit]), &what);
        trader
    }

    /// `holder` buys (`long`) or sells about `notional` USDT of `market` to a funded counterparty.
    fn open(&self, holder: &Keypair, market: u16, notional: u64, long: bool) {
        let counterparty = self.funded_trader();
        let (buyer, seller) = if long {
            (holder, &counterparty)
        } else {
            (&counterparty, holder)
        };
        let base = self.mainnet.base_for(market, notional);
        expect_success(
            self.match_fill(buyer, seller, market, base),
            "opening a position",
        );
    }

    /// `liquidate_perp` on `user`, taking over as much of its position as the program allows.
    fn liquidate(&self, liquidator: &Keypair, user: &Keypair, market: u16) -> Sent {
        let (liquidator_user, liquidator_stats) = user_keys(liquidator);
        let (user, user_stats) = user_keys(user);
        let accounts = [
            STATE,
            liquidator.pubkey(),
            liquidator_user,
            liquidator_stats,
            user,
            user_stats,
        ];
        let args = vec![int(market), int(u64::MAX), Value::null()];
        let remaining = self.mainnet.market_accounts(Some(market), None);
        let liquidation = instruction("liquidate_perp", &accounts, args, remaining);
        self.send(liquidator, &[liquidation])
    }

    /// The base amount `trader` holds in `market`.
    fn position(&self, trader: &Keypair, market: u16) -> i64 {
        position_field(&self.data(&user_keys(trader).0), market, POSITION_BASE)
    }

    /// A post-only bid 1% over the local oracle price, slid by the program to just below the
    /// AMM's ask.
    fn place_slid_bid(&self, bidder: &Keypair, market: u16, base: u64) -> Instruction {
        let oracle = self.data(&self.mainnet.oracle_of(market));
        let price = oracle_price_in_price_precision(&oracle) as u64 * 101 / 100;
        let params = order("Limit", "Long", base, price, market, "Slide");
        self.mainnet.place_order(bidder, market, params)
    }

    /// `seller` sells `base` into `bidder`'s newest resting bid, at that bid's price.
    fn sell_into(&self, seller: &Keypair, bidder: &Keypair, market: u16, base: u64) -> Sent {
        let bidder_account = self.data(&user_keys(bidder).0);
        let (_, bid) = (0..32)
            .map(|index| &bidder_account[USER_ORDERS + index * ORDER_LEN..][..ORDER_LEN])
            .filter(|order| order[86] == 1) // OrderStatus::Open
            .map(|order| (u32_at(order, 80..84), u64_at(order, 8..16)))
            .max()
            .expect("the bidder has a resting bid");
        // A slid bid can sit between ticks; the sell is priced on a tick, at or under it.
        let tick = u64_at(self.mainnet.market(market), ORDER_TICK_SIZE);
        let params = order("Limit", "Short", base, bid / tick * tick, market, "None");
        let sell = self
            .mainnet
            .take_order(seller, market, params, Some(bidder));
        self.send(seller, &[sell])
    }

    /// A matched fill of `base` between two local traders: `buyer` rests a bid, `seller` sells into
    /// it.
    fn match_fill(&self, buyer: &Keypair, seller: &Keypair, market: u16, base: u64) -> Sent {
        let bid = self.place_slid_bid(buyer, market, base);
        expect_success(self.send(buyer, &[bid]), "placing the buyer's bid");
        self.sell_into(seller, buyer, market, base)
    }
}

/// Traders in one market with its price and the USDT price posted at the snapshot slot: a long and
/// a short of about 25,000 USDT, each holding 20,000 USDT, and a resting bid with the trader who
/// will sell into it.
struct Book {
    vm: Vm,
    market: u16,
    posted: u64,
    long: Keypair,
    short: Keypair,
    bidder: Keypair,
    seller: Keypair,
    bid: u64,
}

impl Book {
    async fn open(mainnet: &'static Mainnet, market: u16) -> Self {
        let vm = Vm::new(mainnet).await;
        let posted = mainnet.clock.slot;
        let usdt_oracle = pubkey_at(mainnet.spot_market(USDT_SPOT_MARKET), SPOT_ORACLE);
        for oracle in [mainnet.oracle_of(market), usdt_oracle] {
            vm.stamp(&oracle, posted);
        }
        let [long, short, bidder, seller] = std::array::from_fn(|_| vm.funded_trader());
        let base = mainnet.base_for(market, 25_000_000_000);
        expect_success(
            vm.match_fill(&long, &short, market, base),
            "opening the book",
        );
        let bid = u64_at(mainnet.market(market), ORDER_STEP_SIZE) * 10;
        let resting = vm.place_slid_bid(&bidder, market, bid);
        expect_success(vm.send(&bidder, &[resting]), "placing the resting bid");
        Self {
            vm,
            market,
            posted,
            long,
            short,
            bidder,
            seller,
            bid,
        }
    }

    fn matched_fill(&self) -> Sent {
        self.vm
            .sell_into(&self.seller, &self.bidder, self.market, self.bid)
    }
}

/// The program is the pinned release, and the bundled IDL reads and writes its live accounts as
/// they are: a surfnet's lookup recipe in the templates' `llm_context` finds and decodes them.
#[tokio::test]
async fn the_bundled_idl_matches_the_pinned_release_on_mainnet() {
    let program = fetch(&[PROGRAM]).await.remove(0);
    let programdata = pubkey_at(&program.data, 4..36);
    assert_eq!(programdata, PROGRAMDATA, "the program moved");
    let mainnet = mainnet().await;
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let sol_perp = perp_market_address(SOL_PERP);
    let sol_oracle = mainnet.oracle_of(SOL_PERP);
    let usdt = mainnet.spot_markets[USDT_SPOT_MARKET as usize];
    for (name, address) in [
        ("State", STATE),
        ("PerpMarket", sol_perp),
        ("PythLazerOracle", sol_oracle),
        ("SpotMarket", usdt),
    ] {
        let account = mainnet.account(&address);
        assert_eq!(account.owner, PROGRAM, "{name} owner");
        assert_eq!(&account.data[..8], account_discriminator(name), "{name}");
        let unchanged = HashMap::new();
        let forged = svm.get_forged_account_data(&address, &account.data, &idl().idl, &unchanged);
        let forged = forged.unwrap_or_else(|e| panic!("{name} failed to round-trip: {e}"));
        assert_eq!(forged.len(), account.data.len(), "{name} changed size");
        let first = diff_indices(&forged, &account.data).first().copied();
        assert_eq!(first, None, "a no-op round-trip changed {name}");
    }
    // publish_time is the Lazer timestamp in microseconds.
    let published = u64_at(&mainnet.account(&sol_oracle).data, PUBLISH_TIME) / 1_000_000;
    let lag = published as i64 - mainnet.clock.unix_timestamp;
    assert!(lag.abs() < 600, "publish_time is {lag} s off the clock");

    // Every account of the perp market size is a perp market.
    let config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        data_slice: Some(UiDataSliceConfig {
            offset: 0,
            length: 8,
        }),
        ..Default::default()
    };
    let filters = Some(vec![RpcFilterType::DataSize(PERP_MARKET_LEN as u64)]);
    let rpc = client();
    let found = rpc.get_program_accounts(&PROGRAM, config, filters).await;
    let found = found.unwrap_or_else(|e| panic!("getProgramAccounts on Velocity failed: {e}"));
    let RemoteRpcResult::Ok(found) = found else {
        panic!("the RPC endpoint does not serve getProgramAccounts; set {RPC_URL_ENV}")
    };
    let mut addresses: Vec<Pubkey> = found.iter().map(|(address, _)| *address).collect();
    addresses.sort();
    let mut expected = mainnet.perp_markets.clone();
    expected.sort();
    assert_eq!(addresses, expected, "accounts of the perp market size");
    for (address, account) in found {
        let data = account.to_account().unwrap().data;
        assert_eq!(data, account_discriminator("PerpMarket"), "{address}");
    }

    // jsonParsed decodes each market's name, index and oracle, and each oracle's price.
    let encoding = Some(UiAccountEncoding::JsonParsed);
    let config = RpcAccountInfoConfig {
        encoding,
        ..Default::default()
    };
    let parsed = |address: &Pubkey| -> serde_json::Value {
        let account = mainnet.account(address);
        let ui = svm.account_to_rpc_keyed_account(address, account, &config, None);
        let json = serde_json::to_value(&ui.account.data).unwrap();
        assert!(json["parsed"].is_object(), "{address}: {json}");
        json["parsed"].clone()
    };
    for (index, address) in (0..).zip(&mainnet.perp_markets) {
        let market = parsed(address);
        let name: Vec<u8> = serde_json::from_value(market["name"].clone()).unwrap();
        assert_eq!(name, mainnet.market(index)[MARKET_NAME]);
        assert_eq!(market["market_index"], index);
        assert_eq!(market["oracle"], mainnet.oracle_of(index).to_string());
        let oracle = parsed(&mainnet.oracle_of(index));
        let numbers = oracle["price"].is_i64() && oracle["posted_slot"].is_u64();
        assert!(numbers, "{oracle}");
        assert_eq!(oracle["exponent"], -8, "prices are x 10^8");
    }
    let sol_spot_oracle = pubkey_at(mainnet.spot_market(SOL_SPOT_MARKET), SPOT_ORACLE);
    assert_eq!(sol_spot_oracle, sol_oracle, "SOL spot oracle");
    assert_eq!(STATE, pda(&[b"velocity_state"]));
    let registry = TemplateRegistry::new();
    let template = registry.get(EXCHANGE_RISK).unwrap();
    assert_eq!(template.address, AccountAddress::Pubkey(STATE.to_string()));
}

/// A price move makes a position liquidatable and a real liquidation takes part of it over. A move
/// past the 5-minute TWAP band or the too-volatile ratio, and an exchange-wide pause, refuse
/// liquidation until the exchange template widens or lifts them.
#[tokio::test(flavor = "multi_thread")]
async fn price_moves_liquidate_positions_within_the_guard_rails() {
    const TWAP_DIVERGENCE: &str =
        "oracle_guard_rails.price_divergence.oracle_twap_5min_percent_divergence";
    let mainnet = mainnet().await;
    // A long of 4,000 USDT on 1,000 USDT of collateral, and a liquidator.
    let long = async |market: u16| {
        let vm = Vm::new(mainnet).await;
        let holder = vm.trader(USDT_SPOT_MARKET, 1_000_000_000);
        vm.open(&holder, market, 4_000_000_000, true);
        let liquidator = vm.funded_trader();
        (vm, holder, liquidator)
    };

    for market in [SOL_PERP, BTC_PERP] {
        let (vm, holder, liquidator) = long(market).await;
        let liquidate = || vm.liquidate(&liquidator, &holder, market);
        let opened = vm.position(&holder, market);
        let shocked = mainnet.half_maintenance_price(market, opened, healthy_reading(liquidate()));
        let oracle = mainnet.oracle_of(market);
        let values = oracle_values(shocked, &mainnet.clock);
        vm.apply(ORACLE_PRICE, oracle, values).await;
        expect_success(liquidate(), &format!("liquidating at {shocked}"));
        let left = vm.position(&holder, market);
        assert!(left.abs() < opened.abs(), "{left} of {opened} left");
        let taken = vm.position(&liquidator, market);
        assert_ne!(taken, 0, "the liquidator took nothing over");
    }

    let market = SOL_PERP;
    let oracle = mainnet.oracle_of(market);
    let scale = oracle_scale(&mainnet.account(&oracle).data);
    let twap = i128::from(i64_at(mainnet.market(market), ORACLE_TWAP_5MIN)) * scale;
    let down = |percent: i128| {
        let price = twap * (100 - percent) / 100;
        oracle_values(price as i64, &mainnet.clock)
    };

    let (vm, holder, liquidator) = long(market).await;
    let liquidate = || vm.liquidate(&liquidator, &holder, market);
    vm.apply(ORACLE_PRICE, oracle, down(60)).await;
    expect_error(liquidate(), PRICE_BANDS_BREACHED, "a 60% drop");
    let paused = EXCHANGE_LIQUIDATIONS_PAUSED;
    let widened = json!({TWAP_DIVERGENCE: 1_000_000, "exchange_status": paused});
    vm.apply(EXCHANGE_RISK, STATE, widened).await;
    expect_error(liquidate(), EXCHANGE_PAUSED, "paused exchange-wide");
    let unpaused = json!({"exchange_status": 0});
    vm.apply(EXCHANGE_RISK, STATE, unpaused).await;
    expect_success(liquidate(), "a 60% drop with the band widened");

    let (vm, holder, liquidator) = long(market).await;
    let liquidate = || vm.liquidate(&liquidator, &holder, market);
    vm.apply(ORACLE_PRICE, oracle, down(85)).await;
    let widest = json!({TWAP_DIVERGENCE: 10_000_000});
    vm.apply(EXCHANGE_RISK, STATE, widest).await;
    expect_error(liquidate(), INVALID_ORACLE, "an 85% drop, too volatile");
    let ratio = json!({"oracle_guard_rails.validity.too_volatile_ratio": 10});
    vm.apply(EXCHANGE_RISK, STATE, ratio).await;
    expect_success(liquidate(), "an 85% drop with too_volatile_ratio 10");
}

/// Raising the maintenance margin liquidates without a price move unless the market pauses
/// liquidation. The liquidator earns `liquidator_fee` of the value it takes over, the account pays
/// `if_liquidation_fee` of it to the insurance fund, and a larger `liquidation_margin_buffer_ratio`
/// closes more of the position.
#[tokio::test(flavor = "multi_thread")]
async fn margin_stress_liquidates_and_the_fees_and_buffer_decide_what_it_moves() {
    const LIQUIDATOR_FEE: u32 = 20_000;
    const IF_FEE: u32 = 15_000;
    let mainnet = mainnet().await;
    let market = SOL_PERP;
    let address = perp_market_address(market);
    let paused = mainnet.market(market)[PAUSED_OPERATIONS];
    let live_buffer = u32_at(&mainnet.account(&STATE).data, LIQUIDATION_MARGIN_BUFFER);
    let mut closed = Vec::new();
    for buffer in [live_buffer, 2_000] {
        let vm = Vm::new(mainnet).await;
        let holder = vm.trader(USDT_SPOT_MARKET, 2_000_000_000);
        vm.open(&holder, market, 5_000_000_000, false);
        let stress = json!({
            "margin_ratio_maintenance": 5_000, "margin_ratio_initial": 5_500,
            "liquidator_fee": LIQUIDATOR_FEE, "if_liquidation_fee": IF_FEE,
            "paused_operations": paused | LIQUIDATION_PAUSED,
        });
        vm.apply(MARKET_RISK, address, stress).await;
        let buffer_ratio = json!({"liquidation_margin_buffer_ratio": buffer});
        vm.apply(EXCHANGE_RISK, STATE, buffer_ratio).await;
        let liquidator = vm.funded_trader();
        let liquidate = || vm.liquidate(&liquidator, &holder, market);
        expect_error(liquidate(), INVALID_LIQUIDATION, "paused in the market");
        let unpaused = json!({"paused_operations": paused});
        vm.apply(MARKET_RISK, address, unpaused).await;
        let pending_if = || u128::from_le_bytes(read(&vm.data(&address), PENDING_IF_FEE));
        let pending_before = pending_if();
        expect_success(liquidate(), &format!("liquidating at buffer {buffer}"));

        let taken = vm.data(&user_keys(&liquidator).0);
        let base = i128::from(position_field(&taken, market, POSITION_BASE));
        let value = base.abs() * mainnet.price(market) / 1_000_000_000;
        let fee = |rate: u32| value * i128::from(rate) / 1_000_000;
        // The position comes over at the oracle price; the fee is credited on top.
        let quote = i128::from(position_field(&taken, market, POSITION_QUOTE));
        let earned = quote + base.signum() * value;
        assert_eq!(earned, fee(LIQUIDATOR_FEE), "the liquidator's take");
        let insurance = pending_if() - pending_before;
        let expected = u128::try_from(fee(IF_FEE)).unwrap();
        assert_eq!(insurance, expected, "the insurance fund's take");
        closed.push(base.abs());
    }
    assert!(closed[1] > closed[0], "closed {closed:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn open_interest_cap_and_reduce_only_refuse_new_risk() {
    let mainnet = mainnet().await;
    let market = SOL_PERP;
    let (address, live) = (perp_market_address(market), mainnet.market(market));
    let vm = Vm::new(mainnet).await;
    let trader = vm.funded_trader();
    // One order step at 90% of the oracle price: it adds risk and rests.
    let price = mainnet.price(market) as u64 * 9 / 10;
    let step = u64_at(live, ORDER_STEP_SIZE);
    let params = order("Limit", "Long", step, price, market, "MustPostOnly");
    let bid = || {
        let place = mainnet.place_order(&trader, market, params.clone());
        vm.send(&trader, &[place])
    };
    expect_success(bid(), "placing a bid");

    let long_side = i128::from_le_bytes(read(live, BASE_ASSET_AMOUNT_LONG)).unsigned_abs();
    let capped = json!({"max_open_interest": long_side.to_string()});
    vm.apply(MARKET_RISK, address, capped).await;
    expect_error(bid(), MAX_OPEN_INTEREST_REACHED, "a bid past the cap");

    let live_cap = u128::from_le_bytes(read(live, MAX_OPEN_INTEREST)).to_string();
    let reduce_only = json!({"max_open_interest": live_cap, "status": {"ReduceOnly": null}});
    vm.apply(MARKET_RISK, address, reduce_only).await;
    expect_error(bid(), ORDER_NOT_RISK_REDUCING, "a ReduceOnly bid");
}

/// A new account has no 30-day volume, so it pays the fees of the promo tier. Writing tier 3 with
/// the promo tier at 3 sets what every fill costs.
#[tokio::test(flavor = "multi_thread")]
async fn fee_tier_three_sets_what_every_fill_costs() {
    let mainnet = mainnet().await;
    let market = SOL_PERP;
    let data = mainnet.market(market);
    let addon = u16::from_le_bytes(read(data, TAKER_FEE_ADDON));
    let adjustment = i16::from_le_bytes(read(data, FEE_ADJUSTMENT));
    assert_eq!((addon, adjustment), (0, 0), "a market add-on to leave out");
    let vm = Vm::new(mainnet).await;
    let [maker, taker] = std::array::from_fn(|_| vm.funded_trader());
    let fees = json!({
        "promo_fee_tier": 3,
        "perp_fee_structure.fee_tiers.3.fee_numerator": 100,
        "perp_fee_structure.fee_tiers.3.fee_denominator": 100_000,
        "perp_fee_structure.fee_tiers.3.maker_rebate_numerator": 1,
        "perp_fee_structure.fee_tiers.3.maker_rebate_denominator": 100_000,
    });
    vm.apply(FEES, STATE, fees).await;
    let base = mainnet.base_for(market, 10_000_000_000);
    expect_success(vm.match_fill(&maker, &taker, market, base), "a fill");

    let taker_user = vm.data(&user_keys(&taker).0);
    let quote = position_field(&taker_user, market, POSITION_QUOTE_ENTRY).unsigned_abs();
    let fee = u64_at(&vm.data(&user_keys(&taker).1), TOTAL_FEE_PAID);
    assert_eq!(fee, (quote * 100).div_ceil(100_000), "taker fee on {quote}");
    let rebate = u64_at(&vm.data(&user_keys(&maker).1), TOTAL_FEE_REBATE);
    assert_eq!(rebate, quote / 100_000, "maker rebate on {quote}");
}

/// Lowering a collateral asset's maintenance weight makes an account that holds it liquidatable
/// without a price move, and a flat borrow curve at 100% a year makes a real interest update accrue
/// exactly that rate.
#[tokio::test(flavor = "multi_thread")]
async fn spot_market_risk_sets_collateral_value_and_borrow_interest() {
    const FULL_RATE: u128 = 1_000_000;
    let mainnet = mainnet().await;
    let market = SOL_PERP;
    let address = mainnet.spot_markets[SOL_SPOT_MARKET as usize];
    let spot = mainnet.spot_market(SOL_SPOT_MARKET);
    let oracle = mainnet.account(&pubkey_at(spot, SPOT_ORACLE));
    let price = oracle_price_in_price_precision(&oracle.data);
    // About 5,000 USDT of the asset, and a short of 20,000 USDT.
    let collateral = 5_000_000_000 * 10i128.pow(u32_at(spot, SPOT_DECIMALS)) / price;
    let vm = Vm::new(mainnet).await;
    let holder = vm.trader(SOL_SPOT_MARKET, u64::try_from(collateral).unwrap());
    vm.open(&holder, market, 20_000_000_000, false);
    let opened = vm.position(&holder, market);
    let liquidator = vm.funded_trader();
    let liquidate = || vm.liquidate(&liquidator, &holder, market);
    let (total_collateral, requirement) = healthy_reading(liquidate());

    // Weighted collateral down to about half the requirement.
    let live_weight = i128::from(u32_at(spot, MAINTENANCE_ASSET_WEIGHT));
    let weight = u32::try_from(live_weight * requirement / (2 * total_collateral)).unwrap();
    let values = json!({
        "maintenance_asset_weight": weight, "initial_asset_weight": weight,
        "optimal_borrow_rate": FULL_RATE, "max_borrow_rate": FULL_RATE, "min_borrow_rate": 200,
    });
    vm.apply(SPOT_RISK, address, values).await;
    expect_success(liquidate(), &format!("liquidating at weight {weight}"));
    let left = vm.position(&holder, market);
    assert!(left.abs() < opened.abs(), "{left} of {opened} left");

    let before = vm.data(&address);
    vm.set_slot(mainnet.clock.slot + 30 * 86_400 * 5);
    let oracle = pubkey_at(&before, SPOT_ORACLE);
    let vault = pubkey_at(&before, SPOT_VAULT);
    let name = "update_spot_market_cumulative_interest";
    let accrue = instruction(name, &[STATE, address, oracle, vault], vec![], vec![]);
    expect_success(vm.send(&liquidator, &[accrue]), "accruing interest");
    let now = u64::try_from(vm.clock().unix_timestamp).unwrap();
    let seconds = u128::from(now - u64_at(&before, LAST_INTEREST_TS));
    let index = |data: &[u8]| u128::from_le_bytes(read(data, CUMULATIVE_BORROW_INTEREST));
    let growth = index(&vm.data(&address)) - index(&before);
    let expected = index(&before) * (FULL_RATE * seconds) / 31_536_000 / FULL_RATE + 1;
    assert_eq!(growth, expected, "the borrow index over {seconds} s");
}

/// Margin checks and matched fills refuse a price older than the State's margin window, three times
/// that for the USDT price, and liquidations ignore its age. The exchange template raises the
/// window.
#[tokio::test(flavor = "multi_thread")]
async fn the_margin_window_ages_prices_until_the_exchange_template_raises_it() {
    const MARGIN_WINDOW: &str = "oracle_guard_rails.validity.slots_before_stale_for_margin";
    let mainnet = mainnet().await;
    let state = &mainnet.account(&STATE).data;
    let units = i64_at(state, SLOTS_BEFORE_STALE_FOR_MARGIN);
    let window = (units, u16::from_le_bytes(read(state, SLOT_DURATION_MS)));
    assert_eq!(window, (120, 200), "recompute MARGIN_STALENESS_SLOTS");
    let market = SOL_PERP;
    let oracle = mainnet.oracle_of(market);
    let book = Book::open(mainnet, market).await;
    let vm = &book.vm;
    let at_age = |age: u64| vm.set_slot(book.posted + age);
    let withdraw = || vm.send(&book.short, &[mainnet.withdraw_one_usdt(&book.short)]);

    at_age(MARGIN_STALENESS_SLOTS);
    expect_success(withdraw(), "a withdrawal at the end of the window");
    at_age(MARGIN_STALENESS_SLOTS + 1);
    expect_error(withdraw(), INVALID_ORACLE, "a withdrawal past the window");
    expect_error(book.matched_fill(), NOT_FILLED, "a fill past the window");

    // From here on only the USDT price ages.
    let usdt_age = |age: u64| {
        at_age(age);
        vm.stamp(&oracle, book.posted + age);
    };
    usdt_age(STABLE_COIN_STALENESS_SLOTS);
    expect_success(withdraw(), "a withdrawal at the end of the USDT window");
    usdt_age(STABLE_COIN_STALENESS_SLOTS + 1);
    expect_error(withdraw(), INVALID_ORACLE, "past the USDT window");

    at_age(FAR_PAST);
    let address = perp_market_address(market);
    let stress = json!({"margin_ratio_maintenance": 9_000, "margin_ratio_initial": 9_500});
    vm.apply(MARKET_RISK, address, stress).await;
    let liquidator = vm.funded_trader();
    let liquidated = vm.liquidate(&liquidator, &book.long, market);
    expect_success(liquidated, "a liquidation far past the window");
    let live = mainnet.market(market);
    let live_ratios = json!({
        "margin_ratio_maintenance": u32_at(live, MARGIN_RATIO_MAINTENANCE),
        "margin_ratio_initial": u32_at(live, MARGIN_RATIO_INITIAL),
    });
    vm.apply(MARKET_RISK, address, live_ratios).await;

    // Both prices aged FAR_PAST, in a window raised to FAR_PAST slots, two per 400 ms unit.
    vm.stamp(&oracle, book.posted);
    let raised = json!({MARGIN_WINDOW: FAR_PAST / 2});
    vm.apply(EXCHANGE_RISK, STATE, raised).await;
    expect_success(withdraw(), "a withdrawal at the end of the raised window");
    expect_success(book.matched_fill(), "a fill in the raised window");
    at_age(FAR_PAST + 1);
    expect_error(withdraw(), INVALID_ORACLE, "past the raised window");
}

/// AMM fills and a settle right after a price move need a recent price. An order placed after the
/// price fills at once only within the market's immediate-fill window. After a scenario moves a
/// price at the clock, an order placed in that slot fills from the AMM within the State's AMM
/// window and not after, and a settle needs the same window. Every fill reads that window as an
/// `i8`, so a larger one fails them all.
#[tokio::test(flavor = "multi_thread")]
async fn amm_fills_and_settling_a_moved_price_need_a_price_within_the_amm_window() {
    let mainnet = mainnet().await;
    let amm_window = i64_at(&mainnet.account(&STATE).data, SLOTS_BEFORE_STALE_FOR_AMM);
    assert_eq!(amm_window, 10, "recompute AMM_STALENESS_SLOTS");
    let posted = mainnet.clock.slot;
    let moved_price =
        |market: u16| i64_at(&mainnet.account(&mainnet.oracle_of(market)).data, PRICE) / 100 * 98;

    for market in [SOL_PERP, BTC_PERP] {
        let (oracle, live) = (mainnet.oracle_of(market), mainnet.market(market));
        let base = u64_at(live, ORDER_STEP_SIZE) * 10;
        // A positive override is a window in 400 ms units; unset means the price's own slot.
        let override_units = live[IMMEDIATE_FILL_DELAY_OVERRIDE] as i8;
        let window = u64::try_from(override_units).unwrap_or(0) * 2;
        for age in [window, window + 1] {
            let vm = Vm::new(mainnet).await;
            vm.stamp(&oracle, posted);
            let trader = vm.funded_trader();
            vm.set_slot(posted + age);
            let buy = order("Market", "Long", base, 0, market, "None");
            let taken = vm.send(&trader, &[mainnet.take_order(&trader, market, buy, None)]);
            let refused = logged(&taken, "oracle not valid for immediate fills");
            assert_eq!(refused, age > window, "{market} at {age}: {taken:#?}");
        }

        // A market order's auction can outlast the AMM window, so the buy is priced through the
        // AMM's quote.
        for age in [AMM_STALENESS_SLOTS, AMM_STALENESS_SLOTS + 1] {
            let vm = Vm::new(mainnet).await;
            let trader = vm.funded_trader();
            let price = moved_price(market);
            vm.apply(ORACLE_PRICE, oracle, oracle_values(price, &vm.clock()))
                .await;
            assert_oracle(&vm.locker, &oracle, price, &mainnet.clock);
            let tick = u64_at(live, ORDER_TICK_SIZE);
            let limit = oracle_price_in_price_precision(&vm.data(&oracle)) as u64 * 105 / 100;
            let buy = order("Limit", "Long", base, limit / tick * tick, market, "None");
            let place = mainnet.place_order(&trader, market, buy);
            expect_success(vm.send(&trader, &[place]), "placing a buy");
            vm.set_slot(posted + age);
            let filled = vm.send(&trader, &[mainnet.fill_order(&trader, market)]);
            let refused = logged(&filled, "oracle not valid for low risk fills");
            let in_window = age <= AMM_STALENESS_SLOTS;
            assert_eq!(refused, !in_window, "{market} at {age}: {filled:#?}");
            let bought = vm.position(&trader, market) != 0;
            assert_eq!(bought, in_window, "{market} at {age}: the AMM fill");
        }
    }

    // A price 2% from the 5-minute TWAP settles only behind a fresh AMM crank.
    let market = SOL_PERP;
    let oracle = mainnet.oracle_of(market);
    let book = Book::open(mainnet, market).await;
    let (vm, short) = (&book.vm, &book.short);
    vm.write(&oracle, PRICE, &moved_price(market).to_le_bytes());
    vm.set_slot(book.posted + AMM_STALENESS_SLOTS + 1);
    let crank = mainnet.update_amms(short, market);
    let settle = [crank, mainnet.settle_pnl(short, market)];
    let settled = vm.send(short, &settle);
    expect_error(settled, ORACLE_STALE_FOR_AMM, "21 slots after a move");
    vm.stamp(&oracle, book.posted + 1);
    expect_success(vm.send(short, &settle), "settling 20 slots after a move");

    let past_i8 = (i64::from(i8::MAX) + 1).to_le_bytes();
    vm.write(&STATE, SLOTS_BEFORE_STALE_FOR_AMM, &past_i8);
    let filled = book.matched_fill();
    expect_error(filled, CASTING_FAILURE, "an AMM window over 127");
}
