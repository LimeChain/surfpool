use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use phoenix_rise_accounts::{
    PhoenixAccount,
    global_config::GlobalConfig,
    pda::derive_spline_collection_address,
    perp_asset_map::PerpAssetMap,
    trader::{TRADER_CAPABILITY_HOT, Trader, TraderHeader},
};
use solana_account::Account;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcAccountInfoConfig,
    filter::{Memcmp, RpcFilterType},
};
use solana_signer::Signer;
use solana_transaction::Transaction;
use surfpool_types::DEFAULT_MAINNET_RPC_URL;

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::phoenix_eternal::v1::state_builder::{
            PHOENIX_ETERNAL_PROGRAM_ID, PHOENIX_GLOBAL_TRADER_INDEX, PHOENIX_PERP_ASSET_MAP,
            build_phoenix_collateral_scenario, phoenix_market_symbols,
        },
    },
    surfnet::{locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    types::RemoteRpcResult,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const PHOENIX_GLOBAL_CONFIG: Pubkey =
    Pubkey::from_str_const("2zskx2iyCvb6Stg7RBZkt1f6MrF4dpYtMG3yMvKwqtUZ");

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

fn diff_indices(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.iter()
        .zip(b)
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i)
        .collect()
}

/// A live hot Trader with collateral and a long position, so a downward mark shock has
/// something to act on. Traders come and go, so the test discovers one through the program's
/// own account list rather than pinning an address that may be closed tomorrow.
async fn live_trader_with_position() -> Pubkey {
    // The list carries only collateral (offset 88) and the capability flags (96) of every
    // Trader; the full accounts, mostly 5 KB each, are read for the hot ones with collateral.
    let listed = client()
        .get_program_accounts(
            &PHOENIX_ETERNAL_PROGRAM_ID,
            RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                data_slice: Some(UiDataSliceConfig {
                    offset: 88,
                    length: 12,
                }),
                ..RpcAccountInfoConfig::default()
            },
            Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                0,
                &PhoenixAccount::Trader.discriminant(),
            ))]),
        )
        .await;
    let listed = match listed {
        Ok(RemoteRpcResult::Ok(accounts)) => accounts,
        // The protocol keeps its own trader index, but phoenix-rise-accounts exposes only the
        // arena metadata, so the program's account list is the reader we have.
        Ok(RemoteRpcResult::MethodNotSupported) => panic!(
            "environment: this endpoint does not support getProgramAccounts, which these tests \
             need to find a live trader. Set {RPC_URL_ENV} to an endpoint that supports it. \
             Nothing is proven or disproven about the integration."
        ),
        Err(error) => panic!("failed to list live Phoenix traders: {error}"),
    };
    let candidates: Vec<Pubkey> = listed
        .into_iter()
        .filter_map(|(pubkey, account)| {
            let data = account.to_account()?.data;
            let collateral = i64::from_le_bytes(data.get(..8)?.try_into().ok()?);
            let flags = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
            (collateral > 0 && flags & TRADER_CAPABILITY_HOT != 0).then_some(pubkey)
        })
        .collect();

    for chunk in candidates.chunks(100) {
        let accounts = client()
            .get_multiple_accounts(chunk, CommitmentConfig::confirmed())
            .await
            .unwrap_or_else(|e| panic!("failed to read live Phoenix traders: {e}"));
        for (pubkey, account) in chunk.iter().zip(accounts) {
            // A trader closed since the list was read is skipped, not an error.
            let Ok(account) = account.map_account() else {
                continue;
            };
            let Ok(trader) = Trader::try_from_account_bytes(&account.data) else {
                continue;
            };
            let state = &trader.header.trader_state;
            let holds_a_long = trader
                .positions()
                .any(|(_, position)| position.base_lot_position().as_inner() > 0);
            if state.quote_lot_collateral.as_inner() > 0 && state.is_hot() && holds_a_long {
                return *pubkey;
            }
        }
    }

    panic!("no eligible live candidate: no live Phoenix Trader is hot with collateral and a long")
}

/// A zero-copy layout cannot be round-tripped against itself, so drift shows up as an
/// invariant that stops holding on live bytes.
#[tokio::test(flavor = "multi_thread")]
async fn live_accounts_satisfy_the_typed_layout_invariants() {
    let graph = phoenix_live_graph().await;

    let global = GlobalConfig::try_from_account_bytes(&graph.account(&PHOENIX_GLOBAL_CONFIG).data)
        .expect("live GlobalConfig should decode through phoenix-rise-accounts");
    assert_eq!(
        Pubkey::new_from_array(global.account_key()),
        PHOENIX_GLOBAL_CONFIG,
        "GlobalConfig stores its own address, so a moved field shows up here first"
    );
    assert_eq!(
        (
            Pubkey::new_from_array(global.perp_asset_map_key()),
            Pubkey::new_from_array(global.global_trader_index_header_key()),
        ),
        (PHOENIX_PERP_ASSET_MAP, PHOENIX_GLOBAL_TRADER_INDEX),
        "the hardcoded Phoenix singletons moved; update them to what GlobalConfig points at"
    );
    let template_address = TemplateRegistry::new()
        .get("phoenix-direct-mark-risk-shock")
        .expect("the direct mark template exists")
        .address
        .resolve(None)
        .expect("the direct mark template carries a fixed address");
    assert_eq!(
        template_address, graph.perp_asset_map,
        "the template hardcodes the PerpAssetMap address; GlobalConfig says it moved"
    );

    let map_account = graph.account(&graph.perp_asset_map);
    assert_eq!(map_account.owner, PHOENIX_ETERNAL_PROGRAM_ID);
    let symbols = phoenix_market_symbols(graph.perp_asset_map, map_account)
        .expect("live PerpAssetMap should decode");
    let map = PerpAssetMap::try_from_account_bytes(&map_account.data)
        .expect("live PerpAssetMap should decode through phoenix-rise-accounts");
    for (symbol, _, spline) in &graph.markets {
        assert!(symbols.contains(symbol), "{symbol} is listed");
        let entry = map
            .find_by_symbol(symbol)
            .expect("symbol lookup should decode")
            .expect("the symbol came from this map");
        assert!(
            entry
                .metadata
                .oracle_price()
                .mark_price
                .price
                .ticks
                .as_inner()
                > 0,
            "{symbol} is listed with a zero mark price, which the risk engine cannot use"
        );
        // The spline address is derived, so a change in the seeds surfaces as an account the
        // program would no longer find.
        assert_eq!(
            graph.account(spline).owner,
            PHOENIX_ETERNAL_PROGRAM_ID,
            "the derived spline collection must belong to the Eternal program"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn collateral_idl_override_preserves_live_trader_layout() {
    let graph = phoenix_live_graph().await;
    let account = graph.account(&graph.trader);

    let idl: anchor_lang_idl::types::Idl =
        serde_json::from_str(crate::scenarios::registry::PHOENIX_ETERNAL_IDL_CONTENT)
            .expect("phoenix idl parses as an anchor idl");
    let (svm, _events_rx, _geyser_rx) =
        SurfnetSvm::new(crate::surfnet::svm::SurfnetSvmConfig::default()).unwrap();

    let target: i64 = 12_345;
    let overrides = HashMap::from([(
        "traderState.quoteLotCollateral".to_string(),
        serde_json::Value::from(target),
    )]);

    let forged = svm
        .get_forged_account_data(&graph.trader, &account.data, &idl, &overrides)
        .expect("idl override path forges the trader account");

    let header = TraderHeader::try_read_from_account_bytes(&forged).expect("forged header decodes");
    assert_eq!(header.trader_state.quote_lot_collateral.as_inner(), target);
    assert_eq!(forged.len(), account.data.len());
    let diffs = diff_indices(&forged, &account.data);
    assert!(
        diffs.iter().all(|index| (88..96).contains(index)),
        "collateral override changed bytes outside 88..96: {diffs:?}"
    );
}

const HAWKEYE_VIEW_MARGIN_DISCRIMINANT: [u8; 8] = [0xb2, 0x0a, 0x7c, 0xad, 0xec, 0xd2, 0x75, 0x06];
const HAWKEYE_VIEW_BBO_DISCRIMINANT: [u8; 8] = [0x37, 0x5f, 0x23, 0x2d, 0x53, 0xaf, 0x12, 0x52];
const ETERNAL_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("B5ayDaz9HegiNZqYeBtcFqfZBVSGwjB2CJgHshoSfMQg");
const HAWKEYE_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("Gv1WgG864CQqF5vedJVbpnhpRpRbTW1A7SyARzSw9B4Y");

/// The deployed bytecode, read from the upgradeable loader's ProgramData account once per test
/// process. The ELF starts 45 bytes in, past the loader's own header.
async fn deployed_program(programdata: Pubkey) -> Vec<u8> {
    static CACHE: std::sync::OnceLock<tokio::sync::Mutex<HashMap<Pubkey, Vec<u8>>>> =
        std::sync::OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().await;
    if let Some(bytes) = cache.get(&programdata) {
        return bytes.clone();
    }
    let bytes = fetch(&[programdata]).await.remove(0).data[45..].to_vec();
    cache.insert(programdata, bytes.clone());
    bytes
}

/// Refetching the graph per test is what exhausts a public endpoint: the perp asset map alone
/// is 1.6MB, so every test reads the same fork state from one cached fetch.
fn live_graph_cache() -> &'static tokio::sync::Mutex<Option<PhoenixLiveGraph>> {
    static CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<PhoenixLiveGraph>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

#[tokio::test(flavor = "multi_thread")]
async fn phoenix_state_preparation_changes_hawkeye_risk_outcomes() {
    let (collateral_locker, graph) = phoenix_behavior_locker().await;
    use crate::scenarios::protocols::phoenix_eternal::v1::collateral::index_trader_state_range;

    let account = |key: &Pubkey| {
        collateral_locker
            .with_svm_reader(|svm| svm.get_account(key))
            .unwrap()
            .unwrap()
    };
    let before_trader = account(&graph.trader);
    let before_index = account(&graph.global_trader_index);
    let header = TraderHeader::try_read_from_account_bytes(&before_trader.data).unwrap();
    assert!(
        header.trader_state.is_hot(),
        "the regression requires a hot trader"
    );
    let range = index_trader_state_range(&before_index, &header.key).unwrap();
    let before = hawkeye_margin(&collateral_locker, &graph);
    assert!(before.collateral_quote_lots > 1);
    assert!(
        before.position_count > 0,
        "the discovered trader must hold a position for margin to mean anything"
    );
    assert!(
        before.maintenance_margin_quote_lots > 0,
        "no eligible live candidate: the discovered trader's positions require no margin"
    );
    assert_eq!(before.is_liquidatable, 0, "the fork starts healthy");

    let scenario =
        build_phoenix_collateral_scenario(graph.trader, &before_trader, "1", Some(&before_index))
            .unwrap();
    collateral_locker
        .register_scenario(scenario, Some(graph.clock.slot))
        .unwrap();
    assert_eq!(account(&graph.trader), before_trader);
    assert_eq!(account(&graph.global_trader_index), before_index);
    collateral_locker
        .materialize_overrides_for_slot(&None, graph.clock.slot)
        .await
        .unwrap();
    let after_collateral = hawkeye_margin(&collateral_locker, &graph);
    assert_eq!(
        after_collateral.collateral_quote_lots, 1,
        "the program reads the collateral the preparation wrote"
    );
    assert!(
        after_collateral.effective_collateral_quote_lots < before.effective_collateral_quote_lots,
        "stressing collateral must lower what the risk engine can count on"
    );

    assert_eq!(after_collateral.is_liquidatable, 1);
    let mut expected_trader = before_trader;
    expected_trader.data[88..96].copy_from_slice(&1_i64.to_le_bytes());
    let mut expected_index = before_index;
    expected_index.data[range.start..range.start + 8].copy_from_slice(&1_i64.to_le_bytes());
    assert_eq!(account(&graph.trader), expected_trader);
    assert_eq!(account(&graph.global_trader_index), expected_index);

    // Whether the position liquidates depends on its side, so the cascade asserts what the
    // program reads, not the outcome.
    let (mark_locker, graph) = phoenix_behavior_locker().await;
    let (symbol, orderbook, spline) = graph.markets[0].clone();
    let trader_account = mark_locker
        .with_svm_reader(|svm| svm.get_account(&graph.trader))
        .unwrap()
        .unwrap();
    let prepared_collateral = hawkeye_margin(&mark_locker, &graph).collateral_quote_lots / 2;
    let index_account = mark_locker
        .with_svm_reader(|svm| svm.get_account(&graph.global_trader_index))
        .unwrap()
        .unwrap();
    let mut cascade = build_phoenix_collateral_scenario(
        graph.trader,
        &trader_account,
        &prepared_collateral.to_string(),
        Some(&index_account),
    )
    .unwrap();
    let mut shock = phoenix_market_scenario(
        "phoenix-direct-mark-risk-shock",
        graph.perp_asset_map,
        &[("symbol", symbol.as_str()), ("target_ticks", "1")],
    )
    .overrides
    .remove(0);
    shock.scenario_relative_slot = 1;
    cascade.add_override(shock);
    mark_locker
        .register_scenario(cascade, Some(graph.clock.slot))
        .unwrap();
    mark_locker
        .materialize_overrides_for_slot(&None, graph.clock.slot)
        .await
        .unwrap();
    let before_mark = hawkeye_bbo_for_market(&graph, &mark_locker, orderbook, spline);
    assert_eq!(
        hawkeye_margin(&mark_locker, &graph).collateral_quote_lots,
        prepared_collateral,
        "stage 0 prepares the collateral the cascade was built with"
    );
    assert_ne!(before_mark.mark_price_ticks, 1);
    mark_locker.with_svm_writer(|svm| {
        let mut clock = graph.clock.clone();
        clock.slot += 1;
        svm.inner.set_sysvar(&clock);
    });
    mark_locker
        .materialize_overrides_for_slot(&None, graph.clock.slot + 1)
        .await
        .unwrap();
    let after_mark = hawkeye_bbo_for_market(&graph, &mark_locker, orderbook, spline);
    assert_eq!(
        after_mark.mark_price_last_updated_slot,
        graph.clock.slot + 1
    );
    assert_eq!(
        after_mark.mark_price_ticks, 1,
        "stage 1 shocks the mark the program itself reads"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_margin_stress_raises_the_live_requirement() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let trader_account = graph.account(&graph.trader);
    let trader = Trader::try_from_account_bytes(&trader_account.data).unwrap();
    let (asset_id, _) = trader
        .positions()
        .next()
        .expect("the discovered trader holds a position");
    let map = PerpAssetMap::try_from_account_bytes(&graph.account(&graph.perp_asset_map).data)
        .expect("live PerpAssetMap decodes");
    let entry = map
        .iter()
        .map(|entry| entry.expect("live map entry decodes"))
        .find(|entry| u64::from(entry.metadata.static_market_params().asset_id()) == asset_id)
        .expect("the position's market is listed");
    let doubled = entry.metadata.risk_params().risk_factors[0].saturating_mul(2);

    let before = hawkeye_margin(&locker, &graph);
    locker
        .register_scenario(
            phoenix_market_scenario(
                "phoenix-maintenance-margin-stress",
                graph.perp_asset_map,
                &[
                    ("symbol", entry.symbol.as_str()),
                    ("maintenance_risk_factor_bps", &doubled.to_string()),
                ],
            ),
            Some(graph.clock.slot),
        )
        .unwrap();
    locker
        .materialize_overrides_for_slot(&None, graph.clock.slot)
        .await
        .unwrap();
    let after = hawkeye_margin(&locker, &graph);

    assert_eq!(after.collateral_quote_lots, before.collateral_quote_lots);
    assert!(
        after.maintenance_margin_quote_lots > before.maintenance_margin_quote_lots,
        "a stricter factor must raise the maintenance margin the program computes: {} -> {}",
        before.maintenance_margin_quote_lots,
        after.maintenance_margin_quote_lots
    );
}

async fn phoenix_behavior_locker() -> (SurfnetSvmLocker, PhoenixLiveGraph) {
    let eternal_program = deployed_program(ETERNAL_PROGRAMDATA).await;
    let hawkeye_program = deployed_program(HAWKEYE_PROGRAMDATA).await;
    let graph = phoenix_live_graph().await;

    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm_writer| {
        svm_writer.inner.set_sysvar(&graph.clock);
        svm_writer
            .inner
            .svm
            .add_program(PHOENIX_ETERNAL_PROGRAM_ID, &eternal_program)
            .unwrap();
        svm_writer
            .inner
            .svm
            .add_program(HAWKEYE_PROGRAM_ID, &hawkeye_program)
            .unwrap();
        for (address, account) in &graph.accounts {
            svm_writer.set_account(address, account.clone()).unwrap();
        }
    });

    (locker, graph)
}

async fn phoenix_live_graph() -> PhoenixLiveGraph {
    let mut cache = live_graph_cache().lock().await;
    if let Some(cached) = cache.as_ref() {
        return cached.clone();
    }

    let global_account = fetch(&[PHOENIX_GLOBAL_CONFIG]).await.remove(0);
    let global = GlobalConfig::try_from_account_bytes(&global_account.data)
        .expect("live GlobalConfig decodes");
    let perp_asset_map = Pubkey::new_from_array(global.perp_asset_map_key());
    let global_trader_index = Pubkey::new_from_array(global.global_trader_index_header_key());
    let active_trader_buffer = Pubkey::new_from_array(global.active_trader_buffer_header_key());

    let map_account = fetch(&[perp_asset_map]).await.remove(0);
    let map =
        PerpAssetMap::try_from_account_bytes(&map_account.data).expect("live PerpAssetMap decodes");
    let mut markets = Vec::new();
    let mut addresses = vec![
        PHOENIX_GLOBAL_CONFIG,
        perp_asset_map,
        global_trader_index,
        active_trader_buffer,
    ];
    for symbol in ["SOL", "BTC"] {
        let entry = map
            .find_by_symbol(symbol)
            .expect("symbol lookup")
            .expect("live SOL/BTC market");
        let orderbook =
            Pubkey::new_from_array(entry.metadata.static_market_params().market_account);
        let spline = derive_spline_collection_address(&PHOENIX_ETERNAL_PROGRAM_ID, &orderbook);
        addresses.extend([orderbook, spline]);
        markets.push((symbol.to_string(), orderbook, spline));
    }

    let trader = live_trader_with_position().await;
    addresses.push(trader);
    addresses.push(Pubkey::from_str_const(
        "SysvarC1ock11111111111111111111111111111111",
    ));
    // Read the clock and all dependencies from one bank, after address discovery.
    let mut fresh = fetch(&addresses).await;
    let clock: Clock = bincode::deserialize(&fresh.pop().unwrap().data).unwrap();
    assert!(clock.slot > 0);
    let accounts = addresses.into_iter().zip(fresh).collect();

    let graph = PhoenixLiveGraph {
        clock,
        accounts,
        global_trader_index,
        active_trader_buffer,
        perp_asset_map,
        trader,
        markets,
    };
    *cache = Some(graph.clone());

    graph
}

#[derive(Clone)]
struct PhoenixLiveGraph {
    clock: Clock,
    accounts: Vec<(Pubkey, Account)>,
    global_trader_index: Pubkey,
    active_trader_buffer: Pubkey,
    perp_asset_map: Pubkey,
    trader: Pubkey,
    /// Symbol, orderbook and spline for each market the Hawkeye BBO view reads.
    markets: Vec<(String, Pubkey, Pubkey)>,
}

impl PhoenixLiveGraph {
    fn account(&self, address: &Pubkey) -> &Account {
        self.accounts
            .iter()
            .find_map(|(key, account)| (key == address).then_some(account))
            .unwrap_or_else(|| panic!("{address} is not in the live graph"))
    }
}

fn hawkeye_view(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixLiveGraph,
    discriminant: [u8; 8],
    extra_accounts: &[Pubkey],
) -> Vec<u8> {
    let payer = Keypair::new();
    let accounts = [
        PHOENIX_ETERNAL_PROGRAM_ID,
        PHOENIX_GLOBAL_CONFIG,
        graph.global_trader_index,
        graph.active_trader_buffer,
        graph.perp_asset_map,
    ]
    .iter()
    .chain(extra_accounts)
    .map(|address| AccountMeta::new_readonly(*address, false))
    .collect();
    locker.with_svm_writer(|svm| {
        svm.inner.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
        let transaction = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                Instruction {
                    program_id: HAWKEYE_PROGRAM_ID,
                    accounts,
                    data: discriminant.to_vec(),
                },
            ],
            Some(&payer.pubkey()),
            &[&payer],
            svm.inner.svm.latest_blockhash(),
        );
        svm.inner
            .send_transaction(transaction)
            .unwrap()
            .return_data
            .data
    })
}

fn hawkeye_margin(locker: &SurfnetSvmLocker, graph: &PhoenixLiveGraph) -> HawkeyeMarginView {
    let data = hawkeye_view(
        locker,
        graph,
        HAWKEYE_VIEW_MARGIN_DISCRIMINANT,
        &[graph.trader],
    );
    let margin = bytemuck::pod_read_unaligned::<HawkeyeMarginView>(&data);
    assert_eq!(margin.magic, HAWKEYE_MARGIN_RETURN_MAGIC);
    margin
}

fn hawkeye_bbo_for_market(
    graph: &PhoenixLiveGraph,
    locker: &SurfnetSvmLocker,
    orderbook: Pubkey,
    spline: Pubkey,
) -> HawkeyeBboView {
    let data = hawkeye_view(
        locker,
        graph,
        HAWKEYE_VIEW_BBO_DISCRIMINANT,
        &[orderbook, spline],
    );
    let bbo = bytemuck::pod_read_unaligned::<HawkeyeBboView>(&data);
    assert_eq!(bbo.magic, HAWKEYE_BBO_RETURN_MAGIC);
    bbo
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct HawkeyeMarginView {
    magic: u64,
    version: u16,
    position_count: u16,
    risk_state: u8,
    risk_tier: u8,
    is_liquidatable: u8,
    padding: u8,
    collateral_quote_lots: i64,
    effective_collateral_quote_lots: i64,
    free_collateral_quote_lots: i64,
    withdrawable_collateral_quote_lots: u64,
    initial_margin_quote_lots: u64,
    maintenance_margin_quote_lots: u64,
    cancel_margin_quote_lots: u64,
    backstop_margin_quote_lots: u64,
    high_risk_margin_quote_lots: u64,
    unrealized_pnl_quote_lots: i64,
    discounted_unrealized_pnl_quote_lots: i64,
    unsettled_funding_quote_lots: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct HawkeyeBboView {
    magic: u64,
    version: u16,
    flags: u8,
    padding: [u8; 5],
    best_bid_ticks: u64,
    best_ask_ticks: u64,
    mark_price_ticks: u64,
    index_price_ticks: u64,
    mark_price_last_updated_slot: u64,
    index_price_last_updated_slot: u64,
}

const HAWKEYE_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("RiSeVw3ZjNfsaXPRb4mgaqYaEEt41pNNJoDvVh7pgQj");

const HAWKEYE_MARGIN_RETURN_MAGIC: u64 = 0x955f5b9d3dff253f;

const HAWKEYE_BBO_RETURN_MAGIC: u64 = 0xefca1fa31fa74171;

fn phoenix_market_scenario(
    template_id: &str,
    perp_asset_map: Pubkey,
    values: &[(&str, &str)],
) -> surfpool_types::Scenario {
    let mut scenario = surfpool_types::Scenario::new(
        template_id.to_string(),
        "Phoenix market override".to_string(),
    );
    scenario.add_override(
        surfpool_types::OverrideInstance::new(
            template_id.to_string(),
            0,
            surfpool_types::AccountAddress::Pubkey(perp_asset_map.to_string()),
        )
        .with_values(
            values
                .iter()
                .map(|(field, value)| (field.to_string(), serde_json::Value::from(*value)))
                .collect(),
        ),
    );
    scenario
}
