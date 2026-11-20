//! Jupiter Perps integration tests. They read live mainnet accounts and run the deployed Perps
//! and Doves programs, so they need a network connection and `--features integration-tests`:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests tests::protocols::jupiter:: -- --test-threads=1
//! ```
//!
//! `SURFPOOL_TEST_RPC_URL` overrides the endpoint. Instructions are built from the bundled IDL. The
//! local VM does not check signatures, so transactions run as the keeper with no key.

use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use anchor_lang_idl::types::IdlInstructionAccountItem;
use litesvm::types::TransactionMetadata;
use serde_json::{Value, json};
use solana_account::Account;
use solana_account_decoder::{UiAccountData, UiAccountEncoding, UiDataSliceConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
use solana_message::{Message, VersionedMessage};
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_sdk_ids::system_program;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use spl_associated_token_account_interface::address::get_associated_token_address;
use spl_token_interface::state::{Account as TokenAccount, AccountState};
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};
use txtx_addon_kit::types::types::Value as TxtxValue;
use txtx_addon_network_svm::codec::idl::IdlRef;
use txtx_addon_network_svm_types::idl::parse_bytes_to_value_with_expected_idl_type_def_ty;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{
        locker::SurfnetSvmLocker,
        remote::SurfnetRemoteClient,
        svm::{SurfnetSvm, get_txtx_value_json_converters},
    },
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const PERPS: Pubkey = Pubkey::from_str_const("PERPHjGBqRHArX4DySjwM6UJHiR3sWAatqfdBS2qQJu");
const PERPS_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("C258LjtN8Q928yz2nJe5kwQycckGfWBSrhZBP2mTpLQt");
const DOVES: Pubkey = Pubkey::from_str_const("DoVEsk76QybCEHQGzkvYPWLQu9gzNoZZZt3TPiL597e");
const DOVES_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("4QRRLV8zvwvWDgPNWYXffcMLTS4M6UFCuj2pPMA1tg5D");

const POOL: Pubkey = Pubkey::from_str_const("5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq");
/// Set in the deployed bytecode, not in any account, so a rotation shows up only as
/// `InvalidKeeper`.
const KEEPER: Pubkey = Pubkey::from_str_const("Ag28fGtwtpnqassHURUBsQ1WfiyaWWzDDNs4Q28qHRjv");
const CLOCK: Pubkey = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");
const TOKEN_PROGRAM: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

const SOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const BTC_MINT: Pubkey = Pubkey::from_str_const("3NZ9JMVBmGAqocybic2c7LQCJScmgsAZ6vQqTDzcqmJh");
const USDC_MINT: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const JUPUSD_MINT: Pubkey = Pubkey::from_str_const("JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD");
const SOL_USD_FEED_ID: &str = "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

const PRICE: &str = "jupiter-perps-doves-price";
const RISK: &str = "jupiter-perps-custody-risk";
const BORROW_RATE: &str = "jupiter-perps-custody-borrow-rate";
const TRADING_FEES: &str = "jupiter-perps-custody-fees";
const POOL_LIMITS: &str = "jupiter-perps-pool-limits";
const POOL_FEES: &str = "jupiter-perps-pool-fees";
const BORROW_FEE: &str = "jupiter-perps-custody-borrow-fee";
const POSITION: &str = "jupiter-perps-position";
const POSITION_TOTALS: &str = "jupiter-perps-position-totals";
const DEPOSIT_QUOTE: &str = "get_add_liquidity_amount_and_fee2";

/// Anchor's prefix on the self-invocation that carries an event.
const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

const STALE_ORACLE_PRICE: u32 = 6003;
const INVALID_POSITION_STATE: u32 = 6009;
const MAX_LEVERAGE: u32 = 6022;
const POOL_AMOUNT_LIMIT: u32 = 6024;
const INSTRUCTION_NOT_ALLOWED: u32 = 6027;
const INVALID_KEEPER: u32 = 6034;
const PULL_ORACLE_PUBLISH_TIME_TOO_EARLY: u32 = 6045;
const LP_TOKEN_PRICE_CHANGE_LIMIT_EXCEEDED: u32 = 6066;

const BPS: u128 = 10_000;
const RATE_POWER: u128 = 1_000_000_000;
const DAY: i64 = 24 * 3600;
const ONE_SOL: u64 = 1_000_000_000;

/// Byte offsets, discriminator included, of the fields these tests read.
mod custody {
    pub const MINT: usize = 40;
    pub const TOKEN_ACCOUNT: usize = 72;
    pub const DECIMALS: usize = 104;
    pub const ORACLE_ACCOUNT: usize = 106;
    pub const TRADE_IMPACT_FEE_SCALAR: usize = 151;
    pub const MAX_LEVERAGE: usize = 175;
    pub const LOCKED: usize = 230;
    pub const GUARANTEED_USD: usize = 238;
    pub const GLOBAL_SHORT_SIZES: usize = 246;
    pub const GLOBAL_SHORT_AVERAGE_PRICES: usize = 254;
    pub const CUMULATIVE_INTEREST_RATE: usize = 262;
    pub const INCREASE_POSITION_BPS: usize = 296;
    pub const DECREASE_POSITION_BPS: usize = 304;
    pub const DOVES_AG_ORACLE: usize = 384;
    pub const EXTERNAL_SWAP_FEE_MULTIPLIER_BPS: usize = 1076;
}

mod feed {
    pub const PRICE: usize = 168;
    pub const EXPO: usize = 176;
    pub const TIMESTAMP: usize = 177;
}

mod position {
    pub const OWNER: usize = 8;
    pub const POOL: usize = 40;
    pub const CUSTODY: usize = 72;
    pub const COLLATERAL_CUSTODY: usize = 104;
    pub const UPDATE_TIME: usize = 144;
    pub const SIDE: usize = 152;
    pub const PRICE: usize = 153;
    pub const SIZE_USD: usize = 161;
    pub const COLLATERAL_USD: usize = 169;
    pub const CUMULATIVE_INTEREST_SNAPSHOT: usize = 185;
    pub const LOCKED_AMOUNT: usize = 201;
    pub const LEN: usize = 216;
    pub const LONG: u8 = 1;
    pub const SHORT: u8 = 2;
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

fn u128_at(data: &[u8], at: usize) -> u128 {
    u128::from_le_bytes(data[at..at + 16].try_into().unwrap())
}

fn pubkey_at(data: &[u8], at: usize) -> Pubkey {
    Pubkey::try_from(&data[at..at + 32]).unwrap()
}

fn pubkey(json: &Value) -> Pubkey {
    let address = json.as_str().and_then(|address| address.parse().ok());
    address.unwrap_or_else(|| panic!("{json} is not a pubkey"))
}

fn pubkeys(json: &Value) -> Vec<Pubkey> {
    json.as_array().unwrap().iter().map(pubkey).collect()
}

fn amount(json: &Value, field: &str) -> u64 {
    let amount = json[field].as_u64();
    amount.unwrap_or_else(|| panic!("{field} is not an amount in {json}"))
}

fn client() -> SurfnetRemoteClient {
    SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
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

/// The deployed bytecode, read from the upgradeable loader's ProgramData account once per test
/// process. The ELF starts 45 bytes in, past the loader header.
async fn deployed_program(programdata: Pubkey) -> Vec<u8> {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<Pubkey, Vec<u8>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().await;
    if let Some(bytes) = cache.get(&programdata) {
        return bytes.clone();
    }
    let data = fetch(&[programdata]).await.remove(0).data;
    let bytes = data[45..].to_vec();
    cache.insert(programdata, bytes.clone());
    bytes
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &PERPS).0
}

fn perpetuals_address() -> Pubkey {
    pda(&[b"perpetuals"])
}

fn custody_address(mint: Pubkey) -> Pubkey {
    pda(&[b"custody", POOL.as_ref(), mint.as_ref()])
}

fn jlp_mint() -> Pubkey {
    pda(&[b"lp_token_mint", POOL.as_ref()])
}

/// The bundled Perps IDL, as the registry loads it.
fn perps_idl() -> &'static IdlRef {
    static IDL: OnceLock<IdlRef> = OnceLock::new();
    IDL.get_or_init(|| {
        let registry = TemplateRegistry::new();
        IdlRef::from_idl(registry.get(RISK).unwrap().idl().clone())
    })
}

/// `bytes` decoded as the Perps IDL type `name`, rendered as `jsonParsed` renders it.
fn decode(name: &str, bytes: &[u8]) -> Value {
    let idl = perps_idl();
    let ty = &idl.get_type(name).unwrap().ty;
    parse_bytes_to_value_with_expected_idl_type_def_ty(bytes, ty, &idl.idl.types, &vec![], &vec![])
        .unwrap_or_else(|e| panic!("{name} does not decode: {e}"))
        .to_json(Some(&get_txtx_value_json_converters()))
}

/// Instruction params as the txtx value the IDL encoder takes.
fn txtx(json: &Value) -> TxtxValue {
    match json {
        Value::Null => TxtxValue::null(),
        Value::Bool(flag) => TxtxValue::bool(*flag),
        Value::Number(number) => TxtxValue::integer(number.as_u64().unwrap().into()),
        Value::Object(fields) => TxtxValue::object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), txtx(value)))
                .collect(),
        ),
        _ => panic!("{json} is not a Perps instruction param"),
    }
}

/// The Perps instruction `name` from the bundled IDL: its discriminator, `params` encoded through
/// the IDL, and its accounts in IDL order with the IDL's signer and writable flags. An account
/// missing from `named` is a fixed address, or the feed or vault of the custody its name starts
/// with; the feed fills both oracle slots, as live keepers send it.
fn perps_ix(
    graph: &PerpsGraph,
    name: &str,
    named: &[(&str, Pubkey)],
    params: Value,
) -> Instruction {
    let idl = perps_idl();
    let named: HashMap<&str, Pubkey> = named.iter().copied().collect();
    let custody_of = |role: &str| named[format!("{role}custody").as_str()];
    let address = |account: &str| match account {
        _ if named.contains_key(account) => named[account],
        "perpetuals" => perpetuals_address(),
        "pool" => POOL,
        "transfer_authority" => pda(&[b"transfer_authority"]),
        "lp_token_mint" => jlp_mint(),
        "event_authority" => pda(&[b"__event_authority"]),
        "program" | "referral" => PERPS,
        "token_program" => TOKEN_PROGRAM,
        "associated_token_program" => spl_associated_token_account_interface::program::ID,
        "system_program" => system_program::ID,
        _ => match account.split_once("custody_") {
            Some((role, "doves_price_account" | "pythnet_price_account")) => {
                graph.feed_of(custody_of(role))
            }
            Some((role, "token_account")) => {
                pubkey_at(graph.data(&custody_of(role)), custody::TOKEN_ACCOUNT)
            }
            _ => panic!("{name}: no address for {account}"),
        },
    };
    let instruction = idl.get_instruction(name).unwrap();
    let accounts = instruction.accounts.iter().map(|item| match item {
        IdlInstructionAccountItem::Single(account) => AccountMeta {
            pubkey: address(&account.name),
            is_signer: account.signer,
            is_writable: account.writable,
        },
        IdlInstructionAccountItem::Composite(_) => panic!("{name} has nested accounts"),
    });
    let mut data = instruction.discriminator.clone();
    data.extend(idl.get_encoded_args(name, vec![txtx(&params)]).unwrap());
    Instruction {
        program_id: PERPS,
        accounts: accounts.collect(),
        data,
    }
}

/// The live accounts a Perps instruction reads, all from one slot, with that slot's clock.
#[derive(Clone)]
struct PerpsGraph {
    clock: Clock,
    accounts: HashMap<Pubkey, Account>,
    /// The Pool's custodies, in the order the Pool lists them.
    custodies: Vec<Pubkey>,
    /// Each custody's `doves_ag_oracle`, in the same order.
    feeds: Vec<Pubkey>,
}

impl PerpsGraph {
    fn data(&self, address: &Pubkey) -> &[u8] {
        let missing = || panic!("{address} is not in the graph");
        &self.accounts.get(address).unwrap_or_else(missing).data
    }

    fn feed_of(&self, custody: Pubkey) -> Pubkey {
        pubkey_at(self.data(&custody), custody::DOVES_AG_ORACLE)
    }

    /// The feed's price in USD with 6 decimals, the unit positions use.
    fn feed_usd(&self, feed: Pubkey) -> u128 {
        let data = self.data(&feed);
        usd(u64_at(data, feed::PRICE), data[feed::EXPO] as i8)
    }

    /// `usd` (6 decimals) in the smallest unit of the token, at its feed's price.
    fn tokens_worth(&self, mint: Pubkey, usd: u128) -> u64 {
        let custody = custody_address(mint);
        let decimals = self.data(&custody)[custody::DECIMALS] as u32;
        u64::try_from(usd * 10u128.pow(decimals) / self.feed_usd(self.feed_of(custody))).unwrap()
    }

    /// Every custody, then every feed: the accounts Perps values the whole pool from.
    fn pool_remaining_accounts(&self) -> Vec<AccountMeta> {
        let all = self.custodies.iter().chain(&self.feeds);
        all.map(|address| AccountMeta::new_readonly(*address, false))
            .collect()
    }
}

/// Reads the Perpetuals and Pool accounts, every custody with its feed, vault, mint and Pyth
/// account, the JLP mint, `extra` and the clock in one request.
async fn perps_graph(extra: &[Pubkey]) -> PerpsGraph {
    let custodies = pubkeys(&decode("Pool", &fetch(&[POOL]).await[0].data[8..])["custodies"]);
    let custody_accounts = fetch(&custodies).await;
    let field = |at| -> Vec<Pubkey> {
        let accounts = custody_accounts.iter();
        accounts
            .map(|custody| pubkey_at(&custody.data, at))
            .collect()
    };
    let feeds = field(custody::DOVES_AG_ORACLE);
    let mut addresses = vec![perpetuals_address(), POOL, jlp_mint()];
    addresses.extend(custodies.iter().chain(&feeds).chain(extra));
    for at in [
        custody::TOKEN_ACCOUNT,
        custody::MINT,
        custody::ORACLE_ACCOUNT,
    ] {
        addresses.extend(field(at));
    }
    addresses.sort_unstable();
    addresses.dedup();
    addresses.push(CLOCK);
    let mut fetched = fetch(&addresses).await;
    let clock: Clock = bincode::deserialize(&fetched.pop().unwrap().data).unwrap();
    addresses.pop();
    PerpsGraph {
        clock,
        accounts: addresses.into_iter().zip(fetched).collect(),
        custodies,
        feeds,
    }
}

/// A local VM holding every graph account at the graph's clock, with no programs loaded.
fn local_vm(graph: &PerpsGraph) -> SurfnetSvmLocker {
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm| {
        // A time travel, not a Clock write: keep-alive stamps the time the surfnet keeps.
        svm.apply_time_travel(graph.clock.clone()).unwrap();
        for (address, account) in &graph.accounts {
            svm.set_account(address, account.clone()).unwrap();
        }
    });
    locker
}

/// The local VM with the deployed Perps and Doves programs and a funded keeper.
async fn perps_vm(graph: &PerpsGraph) -> SurfnetSvmLocker {
    let perps = deployed_program(PERPS_PROGRAMDATA).await;
    let doves = deployed_program(DOVES_PROGRAMDATA).await;
    let locker = local_vm(graph);
    locker.with_svm_writer(|svm| {
        svm.inner.svm.add_program(PERPS, &perps).unwrap();
        svm.inner.svm.add_program(DOVES, &doves).unwrap();
    });
    fund(&locker, KEEPER);
    locker
}

fn set_account(locker: &SurfnetSvmLocker, address: Pubkey, account: Account) {
    locker.with_svm_writer(|svm| svm.set_account(&address, account).unwrap());
}

fn fund(locker: &SurfnetSvmLocker, address: Pubkey) {
    let account = Account::new(100_000_000_000, 0, &system_program::ID);
    set_account(locker, address, account);
}

/// A time travel to `unix_timestamp` in the open slot, which keep-alive republishes at.
async fn travel(locker: &SurfnetSvmLocker, unix_timestamp: i64) {
    let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
    let clock = Clock {
        unix_timestamp,
        ..clock
    };
    locker.apply_time_travel(clock, &None).await.unwrap();
}

fn local(locker: &SurfnetSvmLocker, address: &Pubkey) -> Option<Account> {
    locker.with_svm_reader(|svm| svm.get_account(address).unwrap())
}

fn account(locker: &SurfnetSvmLocker, address: &Pubkey) -> Account {
    let missing = || panic!("{address} is missing from the local VM");
    local(locker, address).unwrap_or_else(missing)
}

/// Every Doves feed as if copied when it published at `timestamp`.
fn copy_feeds_at(locker: &SurfnetSvmLocker, graph: &PerpsGraph, timestamp: i64) {
    for feed in &graph.feeds {
        let mut copied = account(locker, feed);
        copied.data[feed::TIMESTAMP..feed::TIMESTAMP + 8].copy_from_slice(&timestamp.to_le_bytes());
        set_account(locker, *feed, copied);
    }
}

/// The account as a `jsonParsed` read returns it, decoded through the bundled IDLs.
fn parsed(locker: &SurfnetSvmLocker, address: Pubkey) -> Value {
    let encoding = UiAccountEncoding::JsonParsed;
    let ui = locker.encode_ui_account(&address, &account(locker, &address), encoding, None, None);
    let UiAccountData::Json(parsed) = ui.data else {
        panic!("{address} did not decode as JSON");
    };
    parsed.parsed
}

/// Plays the overrides through the scenario materializer at the next slot, then reads each
/// pubkey-addressed target back as `jsonParsed` and checks the last value given for each field.
async fn play(locker: &SurfnetSvmLocker, overrides: Vec<OverrideInstance>) {
    let mut scenario = Scenario::new("Jupiter Perps".into(), "State a test prepares".into());
    for instance in &overrides {
        scenario.add_override(instance.clone());
    }
    let slot = locker.get_latest_absolute_slot() + 1;
    locker.register_scenario(scenario, Some(slot)).unwrap();
    let materialized = locker.materialize_overrides_for_slot(&None, slot);
    materialized.await.unwrap();
    let mut checked = HashSet::new();
    for instance in overrides.iter().rev() {
        let AccountAddress::Pubkey(address) = &instance.account else {
            continue;
        };
        let json = parsed(locker, address.parse().unwrap());
        for (path, value) in &instance.values {
            if checked.insert((address, path)) {
                let landed = path.split('.').fold(&json, |json, key| &json[key]);
                let what = format!("{} on {address}: {path}", instance.template_id);
                assert_eq!(landed, value, "{what}");
            }
        }
    }
}

/// An override of `template_id` on `address`; `values` is a JSON object of field paths.
fn template_override(template_id: &str, address: Pubkey, values: Value) -> OverrideInstance {
    let account = AccountAddress::Pubkey(address.to_string());
    let instance = OverrideInstance::new(template_id.to_string(), 0, account);
    instance.with_values(serde_json::from_value(values).unwrap())
}

fn price_override(feed: Pubkey, price: u64) -> OverrideInstance {
    template_override(PRICE, feed, json!({ "price": price }))
}

/// The SOL feed at its live price: an override that moves nothing.
fn live_price(graph: &PerpsGraph) -> OverrideInstance {
    let feed = graph.feed_of(custody_address(SOL_MINT));
    price_override(feed, u64_at(graph.data(&feed), feed::PRICE))
}

fn pyth_override() -> OverrideInstance {
    let registry = TemplateRegistry::new();
    let template = registry.get("pyth-price-feed-v2").unwrap().clone();
    let values = json!({ "feed_id": SOL_USD_FEED_ID, "price_message.price": 1_000_000 });
    OverrideInstance::new(template.id, 0, template.address)
        .with_values(serde_json::from_value(values).unwrap())
}

/// A feed mantissa at a negative `expo` as USD with 6 decimals, the unit positions use.
fn usd(price: u64, expo: i8) -> u128 {
    price as u128 * 1_000_000 / 10u128.pow(-expo as u32)
}

/// USD with 6 decimals as a feed mantissa at a negative `expo`.
fn mantissa(usd: u128, expo: i8) -> u64 {
    u64::try_from(usd * 10u128.pow(-expo as u32) / 1_000_000).unwrap()
}

type Failed = (TransactionError, Vec<String>);
type Sent = Result<TransactionMetadata, Failed>;

/// Sends the instructions with `signer` as fee payer and every other signer unchecked: the local
/// VM runs with signature verification off, as `--skip-signature-verification` does.
fn send(locker: &SurfnetSvmLocker, signer: Pubkey, instructions: &[Instruction]) -> Sent {
    locker.with_svm_writer(|svm| {
        let mut all = vec![ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)];
        all.extend_from_slice(instructions);
        let message = Message::new_with_blockhash(&all, Some(&signer), &svm.latest_blockhash());
        let signers = message.header.num_required_signatures;
        let transaction = VersionedTransaction {
            signatures: (0..signers).map(|_| Signature::new_unique()).collect(),
            message: VersionedMessage::Legacy(message),
        };
        svm.send_transaction(transaction, false, false)
            .map_err(|failed| (failed.err, failed.meta.logs))
    })
}

fn custom_error<T>(sent: &Result<T, Failed>) -> Option<u32> {
    match sent {
        Err((TransactionError::InstructionError(_, InstructionError::Custom(code)), _)) => {
            Some(*code)
        }
        _ => None,
    }
}

fn expect_error<T: std::fmt::Debug>(sent: Result<T, Failed>, code: u32, what: &str) {
    let got = custom_error(&sent);
    assert_eq!(got, Some(code), "{what}: got {sent:?}");
}

/// The Perps event of that name the transaction emitted, decoded through the bundled IDL.
fn event(meta: &TransactionMetadata, name: &str) -> Value {
    let event = perps_idl().idl.events.iter().find(|e| e.name == name);
    let discriminator = &event.unwrap().discriminator;
    let data = meta
        .inner_instructions
        .iter()
        .flatten()
        .map(|inner| &inner.instruction.data)
        .find(|data| data.starts_with(&EVENT_IX_TAG) && data[8..16] == discriminator[..])
        .unwrap_or_else(|| panic!("the transaction emitted no {name}"));
    decode(name, &data[16..])
}

/// A JLP deposit or withdrawal quote: the `AmountAndFee` it returns.
fn quote(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    name: &str,
    named: &[(&str, Pubkey)],
    params: Value,
) -> Result<Value, Failed> {
    let mut instruction = perps_ix(graph, name, named, params);
    instruction.accounts.extend(graph.pool_remaining_accounts());
    let returned = send(locker, KEEPER, &[instruction])?.return_data.data;
    Ok(decode("AmountAndFee", &returned))
}

fn deposit_quote(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    mint: Pubkey,
    amount: u64,
) -> Result<Value, Failed> {
    let named = [("custody", custody_address(mint))];
    let params = json!({ "token_amount_in": amount });
    quote(locker, graph, DEPOSIT_QUOTE, &named, params)
}

fn liquidate(locker: &SurfnetSvmLocker, graph: &PerpsGraph, signer: Pubkey, at: Pubkey) -> Sent {
    let data = account(locker, &at).data;
    let custody = pubkey_at(&data, position::CUSTODY);
    let collateral = pubkey_at(&data, position::COLLATERAL_CUSTODY);
    let named = [
        ("signer", signer),
        ("position", at),
        ("custody", custody),
        ("collateral_custody", collateral),
    ];
    let instruction = perps_ix(graph, "liquidate_full_position4", &named, json!({}));
    send(locker, signer, &[instruction])
}

/// The keeper's liquidation, which must close the position.
fn liquidated(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    at: Pubkey,
    what: &str,
) -> TransactionMetadata {
    let meta = liquidate(locker, graph, KEEPER, at)
        .unwrap_or_else(|e| panic!("{what} was not liquidated: {e:?}"));
    let size = u64_at(&account(locker, &at).data, position::SIZE_USD);
    assert_eq!(size, 0, "{what}: the liquidated position still has a size");
    meta
}

/// One kind of position: the market it trades, the custody holding its collateral and its side.
struct Market {
    label: &'static str,
    mint: Pubkey,
    collateral_mint: Pubkey,
    side: u8,
}

const SOL_LONG: Market = Market {
    label: "SOL long",
    mint: SOL_MINT,
    collateral_mint: SOL_MINT,
    side: position::LONG,
};

const BTC_SHORT: Market = Market {
    label: "BTC short with USDC collateral",
    mint: BTC_MINT,
    collateral_mint: USDC_MINT,
    side: position::SHORT,
};

/// Open positions of the market, newest updates first. A position account outlives its trade with
/// a zero size, so the scan walks back through `update_time` in 2^16-second windows and keeps the
/// open ones.
async fn open_positions(market: &Market, wanted: usize) -> Vec<Pubkey> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let position_account = perps_idl().get_account("Position").unwrap();
    let mut pool_custody_collateral = POOL.to_bytes().to_vec();
    pool_custody_collateral.extend(custody_address(market.mint).to_bytes());
    pool_custody_collateral.extend(custody_address(market.collateral_mint).to_bytes());
    let mut found = Vec::new();
    for window in (0..16).map(|back| (now.as_secs() >> 16) - back) {
        let mut window_and_side = window.to_le_bytes()[..6].to_vec();
        window_and_side.push(market.side);
        let memcmp = [
            (0, position_account.discriminator.clone()),
            (position::POOL, pool_custody_collateral.clone()),
            (position::UPDATE_TIME + 2, window_and_side),
        ];
        let memcmp = memcmp.map(|(at, bytes)| Memcmp::new_raw_bytes(at, bytes));
        let mut filters: Vec<_> = memcmp.into_iter().map(RpcFilterType::Memcmp).collect();
        filters.push(RpcFilterType::DataSize(position::LEN as u64));
        let size_only = UiDataSliceConfig {
            offset: position::SIZE_USD,
            length: 8,
        };
        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                data_slice: Some(size_only),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            ..Default::default()
        };
        let accounts = client()
            .client
            .get_program_ui_accounts_with_config(&PERPS, config)
            .await
            .unwrap_or_else(|e| panic!("{} scan: {e}", market.label));
        let is_open = |size: Vec<u8>| u64_at(&size, 0) > 0;
        let mut open: Vec<Pubkey> = accounts
            .into_iter()
            .filter(|(_, account)| account.data.decode().is_some_and(is_open))
            .map(|(address, _)| address)
            .collect();
        open.sort_unstable();
        found.extend(open);
        if found.len() >= wanted {
            break;
        }
    }
    found.truncate(wanted);
    found
}

/// What a position keeps once its loss at `price` and its close and borrow fees are paid, and the
/// `size / max_leverage` it must keep, both USD with 6 decimals, by the liquidation price formula
/// of Jupiter's published examples.
fn margin(graph: &PerpsGraph, position_data: &[u8], price: u128) -> (i128, i128) {
    let size = u64_at(position_data, position::SIZE_USD) as u128;
    let collateral = u64_at(position_data, position::COLLATERAL_USD) as i128;
    let entry = u64_at(position_data, position::PRICE) as u128;
    let market = graph.data(&pubkey_at(position_data, position::CUSTODY));
    let collateral_custody = graph.data(&pubkey_at(position_data, position::COLLATERAL_CUSTODY));
    let impact_fee_bps =
        (size * BPS).div_ceil(u64_at(market, custody::TRADE_IMPACT_FEE_SCALAR) as u128);
    let close_fee =
        size * (u64_at(market, custody::DECREASE_POSITION_BPS) as u128 + impact_fee_bps) / BPS;
    let borrow_fee = (u128_at(collateral_custody, custody::CUMULATIVE_INTEREST_RATE)
        - u128_at(position_data, position::CUMULATIVE_INTEREST_SNAPSHOT))
        * size
        / RATE_POWER;
    let move_usd = (size * price.abs_diff(entry) / entry) as i128;
    let gain = match (position_data[position::SIDE], price >= entry) {
        (position::LONG, true) | (position::SHORT, false) => move_usd,
        _ => -move_usd,
    };
    let left = collateral + gain - (close_fee + borrow_fee) as i128;
    let required = (size * BPS / u64_at(market, custody::MAX_LEVERAGE) as u128) as i128;
    (left, required)
}

/// The feed price, USD with 6 decimals, at which `margin` runs out.
fn liquidation_price(graph: &PerpsGraph, position_data: &[u8]) -> Option<u128> {
    let size = u64_at(position_data, position::SIZE_USD) as i128;
    let entry = u64_at(position_data, position::PRICE) as i128;
    let (left_at_entry, required) = margin(graph, position_data, entry as u128);
    let room = left_at_entry - required;
    let price = match position_data[position::SIDE] {
        position::LONG => entry - room * entry / size,
        _ => entry + room * entry / size,
    };
    u128::try_from(price).ok().filter(|price| *price > 0)
}

/// The first open position of the market that the deployed program calls healthy and whose
/// liquidation price, from the fields it reads, sits at least 5% from the live price.
async fn liquidation_candidate(market: &Market) -> (PerpsGraph, Pubkey, u128) {
    let positions = open_positions(market, 30).await;
    let graph = perps_graph(&positions).await;
    let locker = perps_vm(&graph).await;
    let price = graph.feed_usd(graph.feed_of(custody_address(market.mint)));
    let mut rejected = Vec::new();
    for position_address in &positions {
        let data = graph.data(position_address);
        let Some(liquidation) = liquidation_price(&graph, data) else {
            rejected.push(format!("{position_address}: no liquidation price"));
            continue;
        };
        let room = match market.side {
            position::LONG => price * 95 >= liquidation * 100,
            _ => price * 105 <= liquidation * 100,
        };
        if !room {
            rejected.push(format!(
                "{position_address}: liquidation at {liquidation} is within 5% of {price}"
            ));
            continue;
        }
        let baseline = liquidate(&locker, &graph, KEEPER, *position_address);
        if custom_error(&baseline) == Some(INVALID_POSITION_STATE) {
            return (graph, *position_address, liquidation);
        }
        rejected.push(format!(
            "{position_address}: baseline liquidation returned {:?}",
            baseline.map(|_| ()).map_err(|(error, _)| error)
        ));
    }
    panic!(
        "no eligible live candidate: no open {} out of {} is healthy with room to move: {rejected:?}",
        market.label,
        positions.len()
    )
}

/// A local VM on which the keeper cannot liquidate the position with `healthy` played, returned
/// with `stress` played on top.
async fn stressed(
    graph: &PerpsGraph,
    at: Pubkey,
    healthy: Vec<OverrideInstance>,
    stress: Vec<OverrideInstance>,
    what: &str,
) -> SurfnetSvmLocker {
    let locker = perps_vm(graph).await;
    play(&locker, healthy).await;
    let refused = liquidate(&locker, graph, KEEPER, at);
    expect_error(refused, INVALID_POSITION_STATE, what);
    play(&locker, stress).await;
    locker
}

/// The position's feed 1% short of its liquidation price, then 3% past it.
async fn price_shocked(
    graph: &PerpsGraph,
    market: &Market,
    at: Pubkey,
    liquidation: u128,
) -> SurfnetSvmLocker {
    let feed = graph.feed_of(custody_address(market.mint));
    let expo = graph.data(&feed)[feed::EXPO] as i8;
    let (short_of, past) = match market.side {
        position::LONG => (liquidation * 101 / 100, liquidation * 97 / 100),
        _ => (liquidation * 99 / 100, liquidation * 103 / 100),
    };
    let shock = |price: u128| vec![price_override(feed, mantissa(price, expo))];
    let what = format!("{} 1% short of its liquidation price", market.label);
    stressed(graph, at, shock(short_of), shock(past), &what).await
}

/// An account of `owner` holding `amount` of `mint`. Wrapped SOL also holds the lamports, as a
/// native account does.
fn token_account(locker: &SurfnetSvmLocker, owner: Pubkey, mint: Pubkey, amount: u64) -> Pubkey {
    const RENT: u64 = 2_039_280;
    let native = mint == SOL_MINT;
    let is_native = COption::from(native.then_some(RENT));
    let state = AccountState::Initialized;
    let token = TokenAccount {
        mint,
        owner,
        amount,
        state,
        is_native,
        ..Default::default()
    };
    let lamports = if native { RENT + amount } else { RENT };
    let mut account = Account::new(lamports, TokenAccount::LEN, &TOKEN_PROGRAM);
    token.pack_into_slice(&mut account.data);
    let address = Pubkey::new_unique();
    set_account(locker, address, account);
    address
}

fn token_amount_in(data: &[u8]) -> u64 {
    TokenAccount::unpack(data).unwrap().amount
}

fn token_amount(locker: &SurfnetSvmLocker, address: Pubkey) -> u64 {
    token_amount_in(&account(locker, &address).data)
}

/// A funded owner with `amount` of `mint` and an empty JLP account.
fn depositor(locker: &SurfnetSvmLocker, mint: Pubkey, amount: u64) -> (Pubkey, Pubkey, Pubkey) {
    let owner = Pubkey::new_unique();
    fund(locker, owner);
    let funding = token_account(locker, owner, mint, amount);
    (owner, funding, token_account(locker, owner, jlp_mint(), 0))
}

/// `addLiquidity2` of `amount` of `mint` from the depositor's account.
fn add_liquidity(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    (owner, funding, jlp_account): (Pubkey, Pubkey, Pubkey),
    mint: Pubkey,
    amount: u64,
) -> Sent {
    let named = [
        ("owner", owner),
        ("funding_account", funding),
        ("lp_token_account", jlp_account),
        ("custody", custody_address(mint)),
    ];
    let params = json!({
        "token_amount_in": amount, "min_lp_amount_out": 1, "token_amount_pre_swap": null,
    });
    let mut instruction = perps_ix(graph, "add_liquidity2", &named, params);
    instruction.accounts.extend(graph.pool_remaining_accounts());
    send(locker, owner, &[instruction])
}

/// `swap2` of `usd` (6 decimals) worth of `from` into `to` by a new trader: the fee in BPS it paid.
fn swap_fee(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    from: Pubkey,
    to: Pubkey,
    usd: u128,
) -> u64 {
    let owner = Pubkey::new_unique();
    fund(locker, owner);
    let amount_in = graph.tokens_worth(from, usd);
    let funding = token_account(locker, owner, from, amount_in);
    let receiving = token_account(locker, owner, to, 0);
    let named = [
        ("owner", owner),
        ("funding_account", funding),
        ("receiving_account", receiving),
        ("receiving_custody", custody_address(from)),
        ("dispensing_custody", custody_address(to)),
    ];
    let params = json!({ "amount_in": amount_in, "min_amount_out": 1 });
    let mut instruction = perps_ix(graph, "swap2", &named, params);
    instruction.accounts.extend(graph.pool_remaining_accounts());
    let sent = send(locker, owner, &[instruction]);
    let sent = sent.unwrap_or_else(|e| panic!("swap {from} to {to} failed: {e:?}"));
    amount(&event(&sent, "PoolSwapEvent"), "fee_bps")
}

fn position_of(trader: Pubkey, market: &Market) -> Pubkey {
    let custody = custody_address(market.mint);
    let collateral = custody_address(market.collateral_mint);
    let side = [market.side];
    pda(&[
        b"position",
        trader.as_ref(),
        POOL.as_ref(),
        custody.as_ref(),
        collateral.as_ref(),
        &side,
    ])
}

/// The trader's market request for a `size_usd` position on $100 of collateral, as the app sends
/// it, the keeper's `increasePosition4` that fills it, and the position.
fn request_position(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    market: &Market,
    trader: Pubkey,
    size_usd: u64,
) -> (Sent, Instruction, Pubkey) {
    let at = position_of(trader, market);
    let mint = market.collateral_mint;
    let custody = custody_address(market.mint);
    let request = pda(&[b"position_request", at.as_ref(), &[0; 8], &[1]]);
    let shared = [
        ("position", at),
        ("position_request", request),
        (
            "position_request_ata",
            get_associated_token_address(&request, &mint),
        ),
        ("custody", custody),
        ("collateral_custody", custody_address(mint)),
    ];
    let collateral = graph.tokens_worth(mint, 100_000_000);
    fund(locker, trader);
    let funding = token_account(locker, trader, mint, collateral);
    let price = graph.feed_usd(graph.feed_of(custody)) as u64;
    let (side, worst_price) = match market.side {
        position::LONG => ("Long", price * 2),
        _ => ("Short", price / 2),
    };
    let params = json!({
        "size_usd_delta": size_usd, "collateral_token_delta": collateral, "side": { side: null },
        "price_slippage": worst_price, "jupiter_minimum_out": null, "counter": 0,
    });
    let owner = [
        ("owner", trader),
        ("funding_account", funding),
        ("input_mint", mint),
    ];
    let named = [&owner[..], &shared].concat();
    let name = "create_increase_position_market_request";
    let requested = send(locker, trader, &[perps_ix(graph, name, &named, params)]);
    let named = [&[("keeper", KEEPER)][..], &shared].concat();
    let fill = perps_ix(graph, "increase_position4", &named, json!({}));
    (requested, fill, at)
}

/// Opens a position as the app does: the request, then the keeper's fill one second later, since
/// a request cannot run in the second it was made, on a price published after the request. The
/// scenario names the market's feed at its current price, so keep-alive republishes it. Returns
/// the first failure or the keeper's transaction, and the position.
async fn open_position(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    market: &Market,
    trader: Pubkey,
    size_usd: u64,
) -> (Sent, Pubkey) {
    let feed = graph.feed_of(custody_address(market.mint));
    let price = u64_at(&account(locker, &feed).data, feed::PRICE);
    play(locker, vec![price_override(feed, price)]).await;
    let (requested, fill, at) = request_position(locker, graph, market, trader, size_usd);
    if requested.is_err() {
        return (requested, at);
    }
    let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
    travel(locker, clock.unix_timestamp + 1).await;
    (send(locker, KEEPER, &[fill]), at)
}

/// Closes the whole position as the app does: the owner's market request, then the keeper's
/// `decreasePosition4` one second later. The payout goes to the owner's associated token account.
async fn close_position(
    locker: &SurfnetSvmLocker,
    graph: &PerpsGraph,
    market: &Market,
    at: Pubkey,
) -> Sent {
    let owner = pubkey_at(&account(locker, &at).data, position::OWNER);
    let mint = market.collateral_mint;
    let receiving = get_associated_token_address(&owner, &mint);
    let made = token_account(locker, owner, mint, 0);
    set_account(locker, receiving, account(locker, &made));
    let request = pda(&[b"position_request", at.as_ref(), &[0; 8], &[2]]);
    let custody = custody_address(market.mint);
    let shared = [
        ("owner", owner),
        ("position", at),
        ("position_request", request),
        (
            "position_request_ata",
            get_associated_token_address(&request, &mint),
        ),
        ("custody", custody),
        ("collateral_custody", custody_address(mint)),
    ];
    let price = graph.feed_usd(graph.feed_of(custody)) as u64;
    let worst_price = match market.side {
        position::LONG => price / 2,
        _ => price * 2,
    };
    let params = json!({
        "collateral_usd_delta": 0, "size_usd_delta": 0, "price_slippage": worst_price,
        "jupiter_minimum_out": null, "entire_position": true, "counter": 0,
    });
    let to = [("receiving_account", receiving), ("desired_mint", mint)];
    let named = [&shared[..], &to].concat();
    let name = "create_decrease_position_market_request";
    send(locker, owner, &[perps_ix(graph, name, &named, params)])?;
    let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
    travel(locker, clock.unix_timestamp + 1).await;
    let named = [&[("keeper", KEEPER)][..], &shared].concat();
    let fill = perps_ix(graph, "decrease_position4", &named, json!({}));
    send(locker, KEEPER, &[fill])
}

/// The pool's value in USD with 6 decimals, as `getAssetsUnderManagement2` computes it from every
/// custody and feed.
fn aum(locker: &SurfnetSvmLocker, graph: &PerpsGraph) -> u128 {
    let name = "get_assets_under_management2";
    let mut instruction = perps_ix(graph, name, &[], json!({ "mode": null }));
    instruction.accounts.extend(graph.pool_remaining_accounts());
    let sent = send(locker, KEEPER, &[instruction]);
    let returned = sent
        .unwrap_or_else(|e| panic!("{name} failed: {e:?}"))
        .return_data
        .data;
    u128::from_le_bytes(returned[..16].try_into().unwrap())
}

/// A failure here means a bundled IDL disagrees with the live layout.
#[tokio::test(flavor = "multi_thread")]
async fn bundled_idls_round_trip_live_accounts() {
    let first_open = |positions: Vec<Pubkey>, market: &Market| {
        let none = || panic!("no eligible live candidate: no open {}", market.label);
        *positions.first().unwrap_or_else(none)
    };
    let long = first_open(open_positions(&SOL_LONG, 1).await, &SOL_LONG);
    let short = first_open(open_positions(&BTC_SHORT, 1).await, &BTC_SHORT);
    let graph = perps_graph(&[long, short]).await;
    let (sol, btc) = (custody_address(SOL_MINT), custody_address(BTC_MINT));

    let registry = TemplateRegistry::new();
    let perps_idl = &perps_idl().idl;
    let doves_idl = registry.get(PRICE).unwrap().idl();
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    for (name, address, idl) in [
        ("Perpetuals", perpetuals_address(), perps_idl),
        ("Pool", POOL, perps_idl),
        ("Custody", sol, perps_idl),
        ("Custody", btc, perps_idl),
        ("Position", long, perps_idl),
        ("Position", short, perps_idl),
        ("AgPriceFeed", graph.feed_of(sol), doves_idl),
        ("AgPriceFeed", graph.feed_of(btc), doves_idl),
    ] {
        let data = graph.data(&address);
        let definition = idl.accounts.iter().find(|account| account.name == name);
        let discriminator = &definition.unwrap().discriminator[..];
        assert_eq!(&data[..8], discriminator, "{address} is not a {name}");
        let forged = svm.get_forged_account_data(&address, data, idl, &HashMap::new());
        let forged = forged.unwrap_or_else(|e| panic!("{name} {address} does not decode: {e}"));
        assert_eq!(forged, data, "{name} {address} changed on a round trip");
    }
}

/// The lookup the templates' `llm_context` teaches, through `jsonParsed` reads: the one Pool, its
/// `custodies`, then each custody's `mint` and `doves_ag_oracle`. An account with bytes past its
/// IDL layout does not decode, so every custody and feed is read.
#[tokio::test(flavor = "multi_thread")]
async fn the_lookup_recipe_reads_every_market_as_json() {
    let graph = perps_graph(&[]).await;
    let locker = local_vm(&graph);
    assert_eq!(pda(&[b"pool", b"Pool"]), POOL);
    let pools = pubkeys(&parsed(&locker, perpetuals_address())["pools"]);
    assert_eq!(pools, [POOL], "Perps now runs more than one pool");
    let registry = TemplateRegistry::new();
    for protocol in ["jupiter-perps", "jupiter-perps-doves"] {
        for template in registry.by_protocol(protocol) {
            let recipe = template.llm_context.as_deref().unwrap_or_default();
            match &template.address {
                AccountAddress::Pubkey(literal) if !literal.is_empty() => {
                    assert_eq!(literal, &POOL.to_string(), "{}", template.id)
                }
                _ => assert!(
                    recipe.contains(&POOL.to_string()) || recipe.contains(&PERPS.to_string()),
                    "{} lookup",
                    template.id
                ),
            }
        }
    }
    let custodies = pubkeys(&parsed(&locker, POOL)["custodies"]);
    assert!(custodies.len() >= 3);
    for custody in custodies {
        let custody_json = parsed(&locker, custody);
        let mint = pubkey(&custody_json["mint"]);
        assert_eq!(custody, custody_address(mint));
        let feed = pubkey(&custody_json["doves_ag_oracle"]);
        let ag_feed = Pubkey::find_program_address(&[b"ag_feed", mint.as_ref()], &DOVES).0;
        assert_eq!(feed, ag_feed);
        assert_eq!(account(&locker, &feed).owner, DOVES);
        assert_eq!(pubkey(&parsed(&locker, feed)["mint"]), mint);
    }
}

/// Perps prices JLP from the Doves feed and never from the custody's Pyth account, even when the
/// Pyth account sits in the instruction's Pythnet slot.
#[tokio::test(flavor = "multi_thread")]
async fn doves_price_moves_the_jlp_quote_and_pyth_does_not() {
    let graph = perps_graph(&[]).await;
    let sol = custody_address(SOL_MINT);
    let feed = graph.feed_of(sol);
    let pyth = pubkey_at(graph.data(&sol), custody::ORACLE_ACCOUNT);
    let locker = perps_vm(&graph).await;
    play(&locker, vec![live_price(&graph)]).await;
    let quote_with = |pythnet: Pubkey| {
        let named = [("custody", sol), ("custody_pythnet_price_account", pythnet)];
        let params = json!({ "token_amount_in": ONE_SOL });
        let quoted = quote(&locker, &graph, DEPOSIT_QUOTE, &named, params);
        quoted.unwrap_or_else(|e| panic!("quote failed: {e:?}"))
    };
    let baseline = quote_with(feed);

    play(&locker, vec![pyth_override()]).await;
    let pyth_price = &parsed(&locker, pyth)["price_message"]["price"];
    assert_eq!(pyth_price, 1_000_000, "the Pyth override missed");
    for pythnet in [feed, pyth] {
        assert_eq!(quote_with(pythnet), baseline, "a Pyth price moved JLP");
    }

    let shocked = u64_at(graph.data(&feed), feed::PRICE) * 8 / 10;
    play(&locker, vec![price_override(feed, shocked)]).await;
    let quoted = quote_with(feed);
    assert!(
        amount(&quoted, "amount") < amount(&baseline, "amount"),
        "a 20% lower SOL price should mint less JLP per SOL: {quoted} vs {baseline}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_price_shock_liquidates_a_usdc_backed_btc_short() {
    let (graph, at, liquidation) = liquidation_candidate(&BTC_SHORT).await;
    let locker = price_shocked(&graph, &BTC_SHORT, at, liquidation).await;
    liquidated(&locker, &graph, at, "a BTC short 3% past");
}

/// A keeper liquidates once `collateral + PnL - close and borrow fees` falls below
/// `size x 10^4 / pricing.max_leverage`, and no one else may. Each lever moves a healthy SOL long
/// short of that line, then past it: the price, `max_leverage` (half, then twice its margin), the
/// close fee (past its spare margin, not its margin) and the borrow rate (twice its spare margin
/// over a day, accrued by the liquidation itself).
#[tokio::test(flavor = "multi_thread")]
async fn each_lever_liquidates_a_sol_long() {
    let (graph, at, liquidation) = liquidation_candidate(&SOL_LONG).await;
    let sol = custody_address(SOL_MINT);
    let size = u64_at(graph.data(&at), position::SIZE_USD) as i128;
    let (left, required) = margin(&graph, graph.data(&at), graph.feed_usd(graph.feed_of(sol)));
    let lever = |template: &str, values: Value| vec![template_override(template, sol, values)];

    let locker = price_shocked(&graph, &SOL_LONG, at, liquidation).await;
    let stranger = Pubkey::new_unique();
    fund(&locker, stranger);
    let refused = liquidate(&locker, &graph, stranger, at);
    expect_error(refused, INVALID_KEEPER, "another signer");
    liquidated(&locker, &graph, at, "3% past");

    let risk = |requirement: i128| {
        let max_leverage = u64::try_from(size * BPS as i128 / requirement).unwrap();
        lever(RISK, json!({ "pricing.max_leverage": max_leverage }))
    };
    let (healthy, stress) = (risk(left / 2), risk(left * 2));
    let locker = stressed(&graph, at, healthy, stress, "half its margin").await;
    liquidated(&locker, &graph, at, "twice its margin");

    let live_fee = u64_at(graph.data(&sol), custody::DECREASE_POSITION_BPS);
    let raised = live_fee + u64::try_from((left - required / 2) * BPS as i128 / size).unwrap();
    let close_fee = lever(TRADING_FEES, json!({ "decrease_position_bps": raised }));
    let locker = stressed(&graph, at, vec![], close_fee, "the live close fee").await;
    let what = format!("a {raised} BPS close fee");
    let closed = event(
        &liquidated(&locker, &graph, at, &what),
        "LiquidateFullPositionEvent",
    );
    let fee = (size as u128 * raised as u128).div_ceil(BPS) as u64;
    let impact = amount(&closed, "price_impact_fee_usd");
    assert_eq!(amount(&closed, "position_fee_usd"), fee + impact, "{what}");

    let yearly_bps = u64::try_from(2 * (left - required) * BPS as i128 * 365 / size).unwrap();
    let mut later = graph.clone();
    later.clock.unix_timestamp += DAY;
    let rates = json!({
        "jump_rate_state.min_rate_bps": yearly_bps,
        "jump_rate_state.target_rate_bps": yearly_bps,
        "jump_rate_state.max_rate_bps": yearly_bps,
    });
    let rates = lever(BORROW_RATE, rates);
    let locker = stressed(&later, at, vec![], rates, "a day at the live rate").await;
    let what = format!("a day at {yearly_bps} BPS a year");
    liquidated(&locker, &later, at, &what);
    let accrued = account(&locker, &sol).data;
    let accrued = u128_at(&accrued, custody::CUMULATIVE_INTEREST_RATE);
    let live = u128_at(graph.data(&sol), custody::CUMULATIVE_INTEREST_RATE);
    assert!(accrued > live, "{what}: the borrow fee did not accrue");
}

/// Opening pays `size x increase_position_bps / 10^4` and a price impact fee of
/// `size x ceil(size x 10^4 / pricing.trade_impact_fee_scalar) / 10^4`, out of the collateral. A
/// close fee larger than the collateral refuses the open.
#[tokio::test(flavor = "multi_thread")]
async fn custody_fees_set_the_opening_fee() {
    const SIZE: u64 = 1_000_000_000;
    let graph = perps_graph(&[]).await;
    let sol = custody_address(SOL_MINT);
    let increase_bps = u64_at(graph.data(&sol), custody::INCREASE_POSITION_BPS) + 50;
    // A scalar of 1,000 times the size charges 10 BPS of price impact.
    let scalar = SIZE * 1_000;
    let raised = json!({
        "increase_position_bps": increase_bps, "pricing.trade_impact_fee_scalar": scalar,
    });
    let locker = perps_vm(&graph).await;
    let raised = template_override(TRADING_FEES, sol, raised);
    play(&locker, vec![raised]).await;

    let trader = Pubkey::new_unique();
    let (sent, at) = open_position(&locker, &graph, &SOL_LONG, trader, SIZE).await;
    let opened = event(&sent.expect("the open failed"), "IncreasePositionEvent");
    let impact = SIZE * (SIZE as u128 * BPS).div_ceil(scalar as u128) as u64 / BPS as u64;
    let fee = SIZE * increase_bps / BPS as u64 + impact;
    assert_eq!(amount(&opened, "price_impact_fee_usd"), impact);
    assert_eq!(amount(&opened, "position_fee_usd"), fee);
    let collateral = u64_at(&account(&locker, &at).data, position::COLLATERAL_USD);
    assert_eq!(collateral, amount(&opened, "collateral_usd_delta") - fee);

    let close_fee = json!({ "decrease_position_bps": 2_000 });
    play(
        &locker,
        vec![template_override(TRADING_FEES, sol, close_fee)],
    )
    .await;
    let trader = Pubkey::new_unique();
    let (sent, _) = open_position(&locker, &graph, &SOL_LONG, trader, SIZE).await;
    expect_error(sent, MAX_LEVERAGE, "a $200 close fee on $100");
}

fn usdc_deposit(locker: &SurfnetSvmLocker, graph: &PerpsGraph) -> Sent {
    const DEPOSIT: u64 = 10_000_000;
    let wallet = depositor(locker, USDC_MINT, DEPOSIT);
    add_liquidity(locker, graph, wallet, USDC_MINT, DEPOSIT)
}

/// Only keep-alive publishes Doves prices on a surfnet. A copied feed refuses a keeper fill on a
/// request made after it (6045) and, 5 seconds on, a JLP deposit (6003), which reads every feed of
/// the pool. One price override per feed republishes each feed every slot at the Surfnet clock's
/// time and its own price, through produced slots and a time travel, until a later override with
/// `"keepAlive": []` lets its feed age.
#[tokio::test(flavor = "multi_thread")]
async fn price_overrides_keep_their_feeds_published_at_the_clock() {
    let graph = perps_graph(&[]).await;
    let now = graph.clock.unix_timestamp;
    let locker = perps_vm(&graph).await;
    copy_feeds_at(&locker, &graph, now);
    let trader = Pubkey::new_unique();
    let (requested, fill, _) = request_position(&locker, &graph, &SOL_LONG, trader, 1_000_000_000);
    requested.expect("the request");
    travel(&locker, now + 5).await;
    let refused = send(&locker, KEEPER, std::slice::from_ref(&fill));
    expect_error(
        refused,
        PULL_ORACLE_PUBLISH_TIME_TOO_EARLY,
        "a fill on the copied price",
    );
    let stale = usdc_deposit(&locker, &graph);
    expect_error(
        stale,
        STALE_ORACLE_PRICE,
        "a deposit 5 seconds after the copy",
    );

    // SOL 1% lower moves JLP well inside its 5% guard; the other feeds' overrides write only
    // `timestamp`, so they keep their live price.
    let sol_feed = graph.feed_of(custody_address(SOL_MINT));
    let sol_price = u64_at(graph.data(&sol_feed), feed::PRICE) * 99 / 100;
    let mut feeds = graph.feeds.clone();
    feeds.sort_unstable();
    feeds.dedup();
    let prices: Vec<(Pubkey, u64)> = feeds
        .iter()
        .map(|feed| {
            let live = u64_at(graph.data(feed), feed::PRICE);
            (*feed, if *feed == sol_feed { sol_price } else { live })
        })
        .collect();
    let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
    let overrides = feeds.iter().map(|feed| match *feed == sol_feed {
        true => price_override(sol_feed, sol_price),
        false => template_override(PRICE, *feed, json!({ "timestamp": clock.unix_timestamp })),
    });
    play(&locker, overrides.collect()).await;
    let timestamp = |feed: &Pubkey| {
        let data = account(&locker, feed).data;
        i64::from_le_bytes(
            data[feed::TIMESTAMP..feed::TIMESTAMP + 8]
                .try_into()
                .unwrap(),
        )
    };
    let published = |what: &str, prices: &[(Pubkey, u64)]| {
        let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
        for (feed, price) in prices {
            let data = account(&locker, feed).data;
            assert_eq!(
                timestamp(feed),
                clock.unix_timestamp,
                "{what}: {feed} timestamp"
            );
            assert_eq!(u64_at(&data, feed::PRICE), *price, "{what}: {feed} price");
        }
    };
    published("the slot the SOL price plays", &prices);
    send(&locker, KEEPER, &[fill]).expect("the fill on a price published after the request");
    usdc_deposit(&locker, &graph).expect("a deposit on published prices");

    for slot in 1..=3 {
        locker.confirm_current_block(&None).await.unwrap();
        published(&format!("{slot} slots on"), &prices);
    }
    travel(&locker, now + DAY).await;
    published("a time travel a day on", &prices);
    usdc_deposit(&locker, &graph).expect("a deposit a day on");

    let btc_feed = graph.feed_of(custody_address(BTC_MINT));
    let mut aging = price_override(btc_feed, u64_at(graph.data(&btc_feed), feed::PRICE));
    aging.keep_alive = Some(vec![]);
    play(&locker, vec![aging]).await;
    let stopped_at = timestamp(&btc_feed);
    travel(&locker, now + DAY + 20).await;
    assert_eq!(timestamp(&btc_feed), stopped_at, "keep-alive off: BTC ages");
    let others: Vec<_> = prices.into_iter().filter(|(f, _)| *f != btc_feed).collect();
    published("20 seconds after BTC's keep-alive stops", &others);
    let stale = usdc_deposit(&locker, &graph);
    expect_error(
        stale,
        STALE_ORACLE_PRICE,
        "a deposit on a 20-second-old BTC",
    );
}

/// Keep-alive republishes only the feeds overrides wrote, and a JLP deposit reads every feed of the
/// pool. Past the custodies' price age limit, a deposit fails (6003) with only SOL's price
/// overridden, mints JLP with one override per feed (SOL's price and every other feed's
/// `timestamp`), and fails again with BTC left out: a feed no override wrote stays stale.
#[tokio::test(flavor = "multi_thread")]
async fn a_jlp_deposit_needs_one_price_override_per_feed() {
    const DEPOSIT: u64 = 10_000_000;
    let graph = perps_graph(&[]).await;
    let now = graph.clock.unix_timestamp;
    let max_price_age = |custody: &Pubkey| {
        let oracle = &decode("Custody", &graph.data(custody)[8..])["oracle"];
        oracle["max_price_age_sec"].as_i64().unwrap()
    };
    let limit = graph.custodies.iter().map(max_price_age).max().unwrap();
    let past_limit = now + limit + 1;
    let mut feeds = graph.feeds.clone();
    feeds.sort_unstable();
    feeds.dedup();
    let sol_feed = graph.feed_of(custody_address(SOL_MINT));
    let btc_feed = graph.feed_of(custody_address(BTC_MINT));
    // SOL 1% lower moves JLP well inside its 5% guard.
    let sol_price = u64_at(graph.data(&sol_feed), feed::PRICE) * 99 / 100;
    let sol = price_override(sol_feed, sol_price);
    let every_feed_but = |left_out: Option<Pubkey>| {
        let others = feeds
            .iter()
            .filter(|feed| **feed != sol_feed && Some(**feed) != left_out);
        let published =
            others.map(|feed| template_override(PRICE, *feed, json!({ "timestamp": now })));
        [vec![sol.clone()], published.collect()].concat()
    };
    let cases = [
        ("SOL only", vec![sol.clone()], Some(STALE_ORACLE_PRICE)),
        ("one override per feed", every_feed_but(None), None),
        (
            "every feed but BTC",
            every_feed_but(Some(btc_feed)),
            Some(STALE_ORACLE_PRICE),
        ),
    ];

    for (what, overrides, refused) in cases {
        let locker = perps_vm(&graph).await;
        copy_feeds_at(&locker, &graph, now);
        let written: HashSet<Pubkey> = overrides
            .iter()
            .filter_map(|instance| match &instance.account {
                AccountAddress::Pubkey(address) => address.parse().ok(),
                _ => None,
            })
            .collect();
        play(&locker, overrides).await;
        let bound: HashSet<Pubkey> = locker.with_svm_reader(|svm| {
            let bindings = svm.keep_alive_bindings.iter();
            bindings.map(|binding| binding.address).collect()
        });
        assert_eq!(bound, written, "{what}: keep-alive binds the written feeds");
        travel(&locker, past_limit).await;
        let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
        for feed in &feeds {
            let data = account(&locker, feed).data;
            let at = &data[feed::TIMESTAMP..feed::TIMESTAMP + 8];
            let age = clock.unix_timestamp - i64::from_le_bytes(at.try_into().unwrap());
            let expected = if written.contains(feed) { 0 } else { limit + 1 };
            assert_eq!(age, expected, "{what}: {feed} age");
        }
        let wallet = depositor(&locker, USDC_MINT, DEPOSIT);
        let sent = add_liquidity(&locker, &graph, wallet, USDC_MINT, DEPOSIT);
        match refused {
            Some(code) => expect_error(sent, code, what),
            None => {
                sent.unwrap_or_else(|e| panic!("{what}: the deposit failed: {e:?}"));
                assert!(token_amount(&locker, wallet.2) > 0, "{what}: no JLP minted");
            }
        }
    }
}

/// A USDC deposit that works on the live pool, refused by each lever that guards deposits.
#[tokio::test(flavor = "multi_thread")]
async fn pool_limits_and_switches_refuse_a_deposit() {
    const DEPOSIT: u64 = 10_000_000_000;
    let graph = perps_graph(&[]).await;
    let sol_feed = graph.feed_of(custody_address(SOL_MINT));
    let aum = decode("Pool", &graph.data(&POOL)[8..])["aum_usd"].clone();
    let aum: u128 = aum.as_str().unwrap().parse().unwrap();
    let limits = |values: Value| template_override(POOL_LIMITS, POOL, values);
    // Every case lifts the weight limit, so only its own lever can refuse the deposit: a SOL crash
    // raises USDC's weight, and live weights alone can put USDC past its buffer.
    let any_weight = limits(json!({ "limit.token_weightage_buffer_bps": "100000" }));
    let cap = limits(json!({ "limit.max_aum_usd": (aum / 2).to_string() }));
    let guard = limits(json!({ "max_lp_token_price_change_bps": 10_000 }));
    let off = json!({ "permissions.allow_add_liquidity": false });
    let off = template_override(RISK, custody_address(USDC_MINT), off);
    // SOL is about half the pool, so this moves the JLP price by about 10%.
    let crashed = u64_at(graph.data(&sol_feed), feed::PRICE) * 8 / 10;
    let crash = || price_override(sol_feed, crashed);
    let lp_guard = Some(LP_TOKEN_PRICE_CHANGE_LIMIT_EXCEEDED);
    let cases = [
        ("the live pool", vec![live_price(&graph)], None),
        (
            "an AUM cap at half the pool",
            vec![cap],
            Some(POOL_AMOUNT_LIMIT),
        ),
        (
            "USDC deposits off",
            vec![off],
            Some(INSTRUCTION_NOT_ALLOWED),
        ),
        ("SOL down 20%", vec![crash()], lp_guard),
        (
            "SOL down 20% with the guard raised",
            vec![crash(), guard],
            None,
        ),
    ];

    for (what, overrides, refused) in cases {
        let locker = perps_vm(&graph).await;
        play(&locker, [vec![any_weight.clone()], overrides].concat()).await;
        let wallet = depositor(&locker, USDC_MINT, DEPOSIT);
        let sent = add_liquidity(&locker, &graph, wallet, USDC_MINT, DEPOSIT);
        match refused {
            Some(code) => expect_error(sent, code, what),
            None => {
                sent.unwrap_or_else(|e| panic!("{what}: the deposit failed: {e:?}"));
                assert!(token_amount(&locker, wallet.2) > 0, "{what}: no JLP minted");
            }
        }
    }
}

/// With both weight taxes at 0, deposits and withdrawals pay `fees.add_remove_liquidity_bps`,
/// swaps with a non-stable token `fees.swap_bps x (1 + external_swap_fee_multiplier_bps / 10^4)` of
/// that token's custody, and stable swaps `fees.stable_swap_bps`. A real deposit pays what its
/// quote said. A weight tax moves deposit and stable swap fees off their base.
#[tokio::test(flavor = "multi_thread")]
async fn pool_fees_set_deposit_withdrawal_and_swap_fees() {
    const DEPOSIT: u64 = 10_000_000_000;
    const SWAP_USD: u128 = 100_000_000;
    let graph = perps_graph(&[]).await;
    let pool = decode("Pool", &graph.data(&POOL)[8..]);
    let raised = |field: &str| amount(&pool["fees"], field) + 100;
    let liquidity_bps = raised("add_remove_liquidity_bps");
    let (swap_bps, stable_bps) = (raised("swap_bps"), raised("stable_swap_bps"));
    let sol = graph.data(&custody_address(SOL_MINT));
    let multiplier = BPS as u64 + u64_at(sol, custody::EXTERNAL_SWAP_FEE_MULTIPLIER_BPS);
    // The test reads fees, so the pool limits must not refuse a swap that moves a token past its
    // live target weight.
    let any_weight = json!({ "limit.token_weightage_buffer_bps": "100000" });
    let fees = |values: Value| {
        vec![
            template_override(POOL_FEES, POOL, values),
            template_override(POOL_LIMITS, POOL, any_weight.clone()),
        ]
    };
    let untaxed = json!({
        "fees.add_remove_liquidity_bps": liquidity_bps,
        "fees.swap_bps": swap_bps,
        "fees.stable_swap_bps": stable_bps,
        "fees.tax_bps": 0,
        "fees.stable_swap_tax_bps": 0,
    });
    let locker = perps_vm(&graph).await;
    play(&locker, fees(untaxed)).await;

    let named = [("custody", custody_address(USDC_MINT))];
    let params = json!({ "lp_amount_in": 10_000_000 });
    let name = "get_remove_liquidity_amount_and_fee2";
    let withdrawal = quote(&locker, &graph, name, &named, params).expect("withdrawal quote");
    assert_eq!(amount(&withdrawal, "fee_bps"), liquidity_bps, "withdrawal");
    let quoted = deposit_quote(&locker, &graph, USDC_MINT, DEPOSIT).expect("deposit quote");
    assert_eq!(amount(&quoted, "fee_bps"), liquidity_bps, "deposit");
    assert_eq!(amount(&quoted, "fee"), DEPOSIT * liquidity_bps / BPS as u64);
    let wallet = depositor(&locker, USDC_MINT, DEPOSIT);
    let deposited = add_liquidity(&locker, &graph, wallet, USDC_MINT, DEPOSIT);
    let deposited = event(&deposited.expect("the deposit failed"), "AddLiquidityEvent");
    assert_eq!(amount(&deposited, "fee_bps"), liquidity_bps);
    let kept = DEPOSIT - amount(&quoted, "fee");
    assert_eq!(amount(&deposited, "token_amount_after_fee"), kept);
    let minted = token_amount(&locker, wallet.2);
    assert_eq!(minted, amount(&quoted, "amount"), "JLP minted");
    let to_sol = swap_fee(&locker, &graph, USDC_MINT, SOL_MINT, SWAP_USD);
    assert_eq!(to_sol, swap_bps * multiplier / BPS as u64, "USDC to SOL");
    let stable_swaps = || {
        [(USDC_MINT, JUPUSD_MINT), (JUPUSD_MINT, USDC_MINT)]
            .map(|(from, to)| swap_fee(&locker, &graph, from, to, SWAP_USD))
    };
    assert_eq!(stable_swaps(), [stable_bps; 2], "USDC and JupUSD swaps");

    let taxed = json!({ "fees.tax_bps": 1_000, "fees.stable_swap_tax_bps": 1_000 });
    play(&locker, fees(taxed)).await;
    let taxed = [USDC_MINT, SOL_MINT, BTC_MINT].map(|mint| {
        let deposit = graph.tokens_worth(mint, DEPOSIT as u128);
        let quoted = deposit_quote(&locker, &graph, mint, deposit).expect("taxed quote");
        amount(&quoted, "fee_bps")
    });
    let moved = taxed.iter().any(|bps| *bps != liquidity_bps);
    assert!(moved, "taxed deposits: {taxed:?}");
    let swapped = stable_swaps();
    let moved = swapped.iter().any(|bps| *bps != stable_bps);
    assert!(moved, "taxed USDC and JupUSD swaps: {swapped:?}");
}

/// `pricing.max_leverage` also caps opening, on the line liquidation checks at the entry price:
/// the keeper's fill is refused (6022) once `size x 10^4 / (collateral - close fee)` is above it,
/// and passes at it. The trader's request does not check it.
#[tokio::test(flavor = "multi_thread")]
async fn max_leverage_caps_the_leverage_a_position_opens_at() {
    const SIZE: u64 = 1_000_000_000;
    let graph = perps_graph(&[]).await;
    let sol = custody_address(SOL_MINT);
    let locker = perps_vm(&graph).await;
    let (opened, at) = open_position(&locker, &graph, &SOL_LONG, Pubkey::new_unique(), SIZE).await;
    opened.expect("the open at the live max leverage");
    let collateral = u64_at(&account(&locker, &at).data, position::COLLATERAL_USD) as u128;
    let market = graph.data(&sol);
    let scalar = u64_at(market, custody::TRADE_IMPACT_FEE_SCALAR) as u128;
    let impact_bps = (SIZE as u128 * BPS).div_ceil(scalar);
    let close_bps = u64_at(market, custody::DECREASE_POSITION_BPS) as u128 + impact_bps;
    let margin = collateral - SIZE as u128 * close_bps / BPS;
    let leverage = u64::try_from((SIZE as u128 * BPS).div_ceil(margin)).unwrap();

    for (max_leverage, refused) in [(leverage - 1, Some(MAX_LEVERAGE)), (leverage, None)] {
        let cap = json!({ "pricing.max_leverage": max_leverage });
        play(&locker, vec![template_override(RISK, sol, cap)]).await;
        let trader = Pubkey::new_unique();
        let (requested, fill, _) = request_position(&locker, &graph, &SOL_LONG, trader, SIZE);
        let what = format!("a {leverage} BPS open under a {max_leverage} BPS cap");
        requested.unwrap_or_else(|e| panic!("{what}: the request failed: {e:?}"));
        let clock: Clock = locker.with_svm_reader(|svm| svm.inner.get_sysvar());
        travel(&locker, clock.unix_timestamp + 1).await;
        let filled = send(&locker, KEEPER, &[fill]);
        match refused {
            Some(code) => expect_error(filled, code, &what),
            None => {
                filled.unwrap_or_else(|e| panic!("{what}: the fill failed: {e:?}"));
            }
        }
    }
}

/// The borrow fee a `size_usd` position of `market` pays at its close, on a local VM where `step`
/// is added to the cumulative rate of `custody` after the open, and the pool's value before and
/// after the step.
async fn borrow_fee_at_close(
    graph: &PerpsGraph,
    market: &Market,
    (trader, size_usd): (Pubkey, u64),
    custody: Pubkey,
    step: u128,
) -> (u64, u128, u128) {
    let locker = perps_vm(graph).await;
    let (opened, at) = open_position(&locker, graph, market, trader, size_usd).await;
    opened.unwrap_or_else(|e| panic!("{}: the open failed: {e:?}", market.label));
    let before = aum(&locker, graph);
    let rate = u128_at(
        &account(&locker, &custody).data,
        custody::CUMULATIVE_INTEREST_RATE,
    );
    let charged = (rate + step).to_string();
    let charged = json!({ "funding_rate_state.cumulative_interest_rate": charged });
    play(
        &locker,
        vec![template_override(BORROW_FEE, custody, charged)],
    )
    .await;
    let after = aum(&locker, graph);
    let closed = close_position(&locker, graph, market, at).await;
    let closed = closed.unwrap_or_else(|e| panic!("{}: the close failed: {e:?}", market.label));
    let fee = amount(&event(&closed, "DecreasePositionEvent"), "funding_fee_usd");
    (fee, before, after)
}

/// A step on `funding_rate_state.cumulative_interest_rate` charges a position `step x size / 10^9`
/// at its close on top of what the unchanged custody charges: a long on its own custody, a short on
/// its USDC collateral custody and nothing on the traded token's. The pool's value does not count
/// the fee before a position pays it.
#[tokio::test(flavor = "multi_thread")]
async fn a_cumulative_rate_step_charges_the_borrow_fee_at_close() {
    const SIZE: u64 = 1_000_000_000;
    // 1% of the size.
    const STEP: u128 = 10_000_000;
    let graph = perps_graph(&[]).await;
    let trader = (Pubkey::new_unique(), SIZE);
    let step_fee = (STEP * SIZE as u128 / RATE_POWER) as u64;
    let cases = [
        (&SOL_LONG, SOL_MINT, step_fee),
        (&BTC_SHORT, USDC_MINT, step_fee),
        (&BTC_SHORT, BTC_MINT, 0),
    ];
    for (market, mint, charged) in cases {
        let custody = custody_address(mint);
        let (unchanged, ..) = borrow_fee_at_close(&graph, market, trader, custody, 0).await;
        let (paid, before, after) =
            borrow_fee_at_close(&graph, market, trader, custody, STEP).await;
        let what = format!("{}, the step on the {mint} custody", market.label);
        assert_eq!(paid, unchanged + charged, "{what}");
        assert_eq!(
            after, before,
            "{what}: the pool's value counts an unpaid fee"
        );
    }
}

/// The totals a position counts in: its collateral custody's `assets.locked`, its custody's
/// `assets.guaranteed_usd` and `assets.global_short_sizes`.
fn position_totals(locker: &SurfnetSvmLocker, market: &Market) -> [u64; 3] {
    let traded = account(locker, &custody_address(market.mint)).data;
    let collateral = account(locker, &custody_address(market.collateral_mint)).data;
    [
        u64_at(&collateral, custody::LOCKED),
        u64_at(&traded, custody::GUARANTEED_USD),
        u64_at(&traded, custody::GLOBAL_SHORT_SIZES),
    ]
}

/// What the position adds to `position_totals`.
fn share(position_data: &[u8]) -> [u64; 3] {
    let field = |at| u64_at(position_data, at);
    let (size, locked) = (field(position::SIZE_USD), field(position::LOCKED_AMOUNT));
    match position_data[position::SIDE] {
        position::LONG => [locked, size - field(position::COLLATERAL_USD), 0],
        _ => [locked, 0, size],
    }
}

fn less(totals: [u64; 3], share: [u64; 3]) -> [u64; 3] {
    [0, 1, 2].map(|i| totals[i] - share[i])
}

/// `A x old price x (S - old size) / (S x old price - old size x A)`: the average entry of the
/// custody's shorts once a short of `size` at `price` is taken out.
fn short_average_without(sizes: u128, average: u128, size: u128, price: u128) -> u128 {
    (sizes - size) * average * price / (sizes * price - size * average)
}

/// The live position forged to twice its size, an entry 2% against the live price and collateral
/// of 1% of its size, so a keeper may liquidate it: the `jupiter-perps-position` override, the
/// `jupiter-perps-position-totals` overrides computed as their `llm_context` says, how much the
/// change moves the pool's value, USD with 6 decimals, and the rounding the pool may add to that.
fn forged(
    graph: &PerpsGraph,
    market: &Market,
    at: Pubkey,
) -> (OverrideInstance, Vec<OverrideInstance>, i128, i128) {
    let custody = custody_address(market.mint);
    let collateral_custody = custody_address(market.collateral_mint);
    let old = graph.data(&at);
    let field = |at| u64_at(old, at) as u128;
    let (size, price) = (field(position::SIZE_USD), field(position::PRICE));
    let (collateral, locked) = (
        field(position::COLLATERAL_USD),
        field(position::LOCKED_AMOUNT),
    );
    let total = |custody: &Pubkey, at| u64_at(graph.data(custody), at) as u128;
    // A feed's mantissa is the USD price x 10^6 x `price_unit`, and tokens x mantissa /
    // `token_unit` is their value, USD with 6 decimals.
    let feed = |custody: Pubkey| {
        let data = graph.data(&graph.feed_of(custody));
        let decimals = graph.data(&custody)[custody::DECIMALS] as u32;
        let digits = (-(data[feed::EXPO] as i8)) as u32 - 6;
        let mantissa = u64_at(data, feed::PRICE) as u128;
        (mantissa, 10u128.pow(digits), 10u128.pow(decimals + digits))
    };
    let (mantissa, price_unit, token_unit) = feed(custody);
    let live = graph.feed_usd(graph.feed_of(custody));
    let (new_size, new_collateral) = (size * 2, size * 2 / 100);
    let (new_price, new_locked) = match market.side {
        position::LONG => {
            let new_price = live * 102 / 100;
            (new_price, new_size * (token_unit / price_unit) / new_price)
        }
        _ => {
            let (stable, _, stable_unit) = feed(collateral_custody);
            (live * 98 / 100, new_size * stable_unit / stable)
        }
    };
    let snapshot = total(&collateral_custody, custody::CUMULATIVE_INTEREST_RATE);
    let values = json!({
        "size_usd": new_size, "price": new_price, "collateral_usd": new_collateral,
        "locked_amount": new_locked, "cumulative_interest_snapshot": snapshot.to_string(),
    });
    let position = template_override(POSITION, at, values);
    let locked_total = (total(&collateral_custody, custody::LOCKED) + new_locked - locked) as u64;
    if market.side == position::LONG {
        let guaranteed = total(&custody, custody::GUARANTEED_USD) + new_size
            - new_collateral
            - (size - collateral);
        let values = json!({
            "assets.locked": locked_total, "assets.guaranteed_usd": guaranteed as u64,
        });
        let totals = vec![template_override(POSITION_TOTALS, custody, values)];
        let held = (new_locked as i128 - locked as i128) * mantissa as i128 / token_unit as i128;
        let moved = (new_size - new_collateral) as i128 - (size - collateral) as i128 - held;
        return (position, totals, moved, 2);
    }
    let sizes = total(&custody, custody::GLOBAL_SHORT_SIZES);
    let average = total(&custody, custody::GLOBAL_SHORT_AVERAGE_PRICES);
    let without = short_average_without(sizes, average, size, price);
    let (rest, new_sizes) = (sizes - size, sizes - size + new_size);
    let new_average = new_sizes * without * new_price / (rest * new_price + new_size * without);
    let shorts = json!({
        "assets.global_short_sizes": new_sizes as u64,
        "assets.global_short_average_prices": new_average as u64,
    });
    let totals = vec![
        template_override(
            POSITION_TOTALS,
            collateral_custody,
            json!({ "assets.locked": locked_total }),
        ),
        template_override(POSITION_TOTALS, custody, shorts),
    ];
    let pnl = |size: u128, entry: u128| {
        let entry = (entry * price_unit) as i128;
        size as i128 * (entry - mantissa as i128) / entry
    };
    // Each of the two rounded-down steps leaves the average up to 1 low, and the pool values the
    // shorts at sizes x (average - price) / average, which 1 of average moves by about
    // sizes x price / average^2.
    let slack = 2 * (new_sizes * live / new_average / new_average + 1) as i128 + 2;
    (
        position,
        totals,
        pnl(size, price) - pnl(new_size, new_price),
        slack,
    )
}

/// A position changed with `jupiter-perps-position` counts once `jupiter-perps-position-totals`
/// adds the change to the custody totals: the pool's value moves by the change in what it owes the
/// trader, and the keeper's liquidation takes the totals back to the live ones less the live
/// position's share. Without the totals the pool ignores the change, and the liquidation leaves
/// every total off by it.
#[tokio::test(flavor = "multi_thread")]
async fn a_forged_position_counts_in_the_pool_and_liquidates() {
    for market in [&SOL_LONG, &BTC_SHORT] {
        let (graph, at, _) = liquidation_candidate(market).await;
        let (position, totals, moves, slack) = forged(&graph, market, at);
        let custody = custody_address(market.mint);
        for with_totals in [false, true] {
            let what = match with_totals {
                true => format!("{} with the totals", market.label),
                false => format!("{} alone", market.label),
            };
            let locker = perps_vm(&graph).await;
            play(&locker, vec![live_price(&graph)]).await;
            let (value, before) = (aum(&locker, &graph), position_totals(&locker, market));
            let mut overrides = vec![position.clone()];
            if with_totals {
                overrides.extend(totals.clone());
            }
            play(&locker, overrides).await;
            let moved = aum(&locker, &graph) as i128 - value as i128;
            let forged_share = share(&account(&locker, &at).data);
            liquidated(&locker, &graph, at, &what);
            let after = position_totals(&locker, market);
            if !with_totals {
                assert_eq!(moved, 0, "{what}: the pool's value moved");
                assert_eq!(after, less(before, forged_share), "{what}: totals");
                continue;
            }
            let off = (moved - moves).abs();
            assert!(off <= slack, "{what}: the pool moved {moved}, not {moves}");
            assert_eq!(
                after,
                less(before, share(graph.data(&at))),
                "{what}: totals"
            );
            if market.side == position::SHORT {
                let data = graph.data(&at);
                let field = |data: &[u8], at| u64_at(data, at) as u128;
                let expected = short_average_without(
                    field(graph.data(&custody), custody::GLOBAL_SHORT_SIZES),
                    field(graph.data(&custody), custody::GLOBAL_SHORT_AVERAGE_PRICES),
                    field(data, position::SIZE_USD),
                    field(data, position::PRICE),
                );
                let average = field(
                    &account(&locker, &custody).data,
                    custody::GLOBAL_SHORT_AVERAGE_PRICES,
                );
                assert!(
                    average.abs_diff(expected) <= 1,
                    "{what}: average {average}, not {expected}"
                );
            }
        }
    }
}

/// The position lookup `jupiter-perps-position` teaches, sent to mainnet: Perps accounts of 216
/// bytes with the wallet at offset 8 are the wallet's positions, and they decode as `Position`.
#[tokio::test(flavor = "multi_thread")]
async fn the_position_lookup_finds_a_wallets_positions() {
    let registry = TemplateRegistry::new();
    let recipe = registry.get(POSITION).unwrap().llm_context.clone();
    let taught = r#"[{"dataSize":216},{"memcmp":{"offset":8,"bytes":"<wallet>"}}]"#;
    assert!(
        recipe.unwrap_or_default().contains(taught),
        "the lookup changed"
    );
    let none = || panic!("no eligible live candidate: no open {}", SOL_LONG.label);
    let at = *open_positions(&SOL_LONG, 1)
        .await
        .first()
        .unwrap_or_else(none);
    let wallet = pubkey_at(&fetch(&[at]).await[0].data, position::OWNER);
    let by_wallet = Memcmp::new_raw_bytes(position::OWNER, wallet.to_bytes().to_vec());
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::DataSize(position::LEN as u64),
            RpcFilterType::Memcmp(by_wallet),
        ]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        },
        ..Default::default()
    };
    let found = client()
        .client
        .get_program_ui_accounts_with_config(&PERPS, config)
        .await
        .unwrap_or_else(|e| panic!("the lookup for {wallet}: {e}"));
    assert!(
        found.iter().any(|(address, _)| *address == at),
        "{at} is missing from {wallet}'s positions"
    );
    let discriminator = &perps_idl().get_account("Position").unwrap().discriminator;
    for (address, account) in found {
        let data = account.data.decode().unwrap();
        assert_eq!(
            &data[..8],
            &discriminator[..],
            "{address} is not a Position"
        );
        let owner = &decode("Position", &data[8..])["owner"];
        assert_eq!(pubkey(owner), wallet, "{address}");
    }
}
