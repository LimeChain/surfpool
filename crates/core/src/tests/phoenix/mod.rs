use std::collections::{HashMap, HashSet};

use bytemuck::{Pod, Zeroable};
use phoenix_rise_accounts::{
    global_config::GlobalConfig,
    pda::derive_spline_collection_address,
    perp_asset_map::PerpAssetMap,
    trader::{Trader, TraderHeader},
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
use surfpool_types::DEFAULT_MAINNET_RPC_URL;

use crate::{
    scenarios::protocols::phoenix_eternal::v1::{
        market::{MarketContext, move_market, phoenix_markets},
        state_builder::{PHOENIX_GLOBAL_CONFIG, PHOENIX_PERP_ASSET_MAP, PHOENIX_PROGRAM_ID},
        trader::{
            Side, deposit, index_trader_state_range, index_trader_state_ranges, liquidation,
            place_market_order, prepare_cascade, prepare_liquidation, withdraw,
        },
    },
    surfnet::{locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const LAST_RESTART_SLOT: Pubkey =
    Pubkey::from_str_const("SysvarLastRestartS1ot1111111111111111111111");
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

/// Hawkeye checks it, because it reads a hot Trader's collateral from the GlobalTraderIndex and
/// its positions from the ActiveTraderBuffer, and the Trader account's copies of both can lag.
/// A margin view that fails rejects the trader, and its error is returned.
fn trader_is_eligible(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixMainnetGraph,
) -> Result<bool, String> {
    let header = TraderHeader::try_read_from_account_bytes(&graph.account(&graph.trader).data)
        .unwrap_or_else(|e| {
            panic!(
                "{} is in the GlobalTraderIndex but is not a valid Trader: {e}",
                graph.trader
            )
        });
    if !header.trader_state.is_hot() {
        return Ok(false);
    }
    let margin = try_hawkeye_margin(locker, graph)?;
    Ok(margin.collateral_quote_lots > 0
        && margin.position_count > 0
        && margin.maintenance_margin_quote_lots > 0
        && margin.is_liquidatable == 0)
}

/// A zero-copy layout cannot be round-tripped against itself, so drift shows up as an
/// invariant that stops holding on mainnet bytes.
#[tokio::test(flavor = "multi_thread")]
async fn mainnet_accounts_satisfy_the_typed_layout_invariants() {
    let graph = phoenix_mainnet_graph().await;

    let global = GlobalConfig::try_from_account_bytes(&graph.account(&PHOENIX_GLOBAL_CONFIG).data)
        .expect("mainnet GlobalConfig should decode through phoenix-rise-accounts");
    assert_eq!(
        Pubkey::new_from_array(global.account_key()),
        PHOENIX_GLOBAL_CONFIG,
        "GlobalConfig stores its own address, so a moved field shows up here first"
    );
    assert_eq!(
        Pubkey::new_from_array(global.perp_asset_map_key()),
        PHOENIX_PERP_ASSET_MAP,
        "the PerpAssetMap the templates carry moved; update it to what GlobalConfig points at"
    );

    let map_account = graph.account(&graph.perp_asset_map);
    assert_eq!(map_account.owner, PHOENIX_PROGRAM_ID);
    let markets = phoenix_markets(graph.perp_asset_map, map_account)
        .expect("mainnet PerpAssetMap should decode");
    let map = PerpAssetMap::try_from_account_bytes(&map_account.data)
        .expect("mainnet PerpAssetMap should decode through phoenix-rise-accounts");
    for (symbol, orderbook, spline) in &graph.markets {
        assert!(
            markets
                .iter()
                .any(|market| &market.symbol == symbol && market.orderbook == *orderbook),
            "{symbol} is listed with the orderbook the BBO view reads"
        );
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
            PHOENIX_PROGRAM_ID,
            "the derived spline collection must belong to the Eternal program"
        );
    }
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

fn mainnet_graph_cache() -> &'static tokio::sync::Mutex<Option<Result<PhoenixMainnetGraph, String>>>
{
    static CACHE: std::sync::OnceLock<
        tokio::sync::Mutex<Option<Result<PhoenixMainnetGraph, String>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

#[tokio::test(flavor = "multi_thread")]
async fn scenario_keeps_the_trader_usable_as_the_clock_moves_on() {
    let later = |graph: &PhoenixMainnetGraph| {
        let mut clock = graph.clock.clone();
        clock.slot += 1_000_000;
        clock
    };

    let (unplayed, graph) = phoenix_behavior_locker().await;
    unplayed.with_svm_writer(|svm| svm.inner.set_sysvar(&later(&graph)));
    let refused = try_hawkeye_margin(&unplayed, &graph)
        .expect_err("with no Phoenix scenario played, the aged readings are refused");
    assert!(
        refused.contains("staleness or validity check failed"),
        "{refused}"
    );

    let (locker, graph) = phoenix_behavior_locker().await;
    // Any Phoenix override keeps the markets usable; this one leaves SOL's fees as they are.
    let (_, orderbook, _) = graph.markets[0].clone();
    let book = phoenix_rise_accounts::orderbook::Orderbook::try_from_account_bytes(
        &graph.account(&orderbook).data,
    )
    .unwrap()
    .header;
    let mut scenario = surfpool_types::Scenario::new(
        "phoenix-market-fees".to_string(),
        "Leave SOL's fees as they are".to_string(),
    );
    scenario.add_override(phoenix_market_override(
        "phoenix-market-fees",
        graph.perp_asset_map,
        &[
            ("symbol", "SOL"),
            (
                "defaultTakerFeeMicro",
                &book.default_taker_fee_micro().to_string(),
            ),
            (
                "defaultMakerFeeMicro",
                &book.default_maker_fee_micro().to_string(),
            ),
        ],
    ));
    locker
        .register_scenario(scenario, Some(graph.clock.slot))
        .unwrap();
    locker
        .materialize_overrides_for_slot(
            &Some((client(), CommitmentConfig::confirmed())),
            graph.clock.slot,
        )
        .await
        .unwrap();
    let prepared = hawkeye_margin(&locker, &graph);

    locker.with_svm_writer(|svm| svm.inner.set_sysvar(&later(&graph)));
    let kept = hawkeye_margin(&locker, &graph);
    assert_eq!(
        (
            kept.collateral_quote_lots,
            kept.effective_collateral_quote_lots,
            kept.initial_margin_quote_lots,
            kept.maintenance_margin_quote_lots,
            kept.unrealized_pnl_quote_lots,
        ),
        (
            prepared.collateral_quote_lots,
            prepared.effective_collateral_quote_lots,
            prepared.initial_margin_quote_lots,
            prepared.maintenance_margin_quote_lots,
            prepared.unrealized_pnl_quote_lots,
        ),
        "the trader is priced as when the scenario was played"
    );
}

async fn phoenix_behavior_locker() -> (SurfnetSvmLocker, PhoenixMainnetGraph) {
    let eternal_program = deployed_program(ETERNAL_PROGRAMDATA).await;
    let hawkeye_program = deployed_program(HAWKEYE_PROGRAMDATA).await;
    let graph = phoenix_mainnet_graph().await;
    let locker = phoenix_surfnet(&graph, &eternal_program, &hawkeye_program);
    (locker, graph)
}

/// A local VM holding the deployed programs and every graph account, at the graph's clock.
fn phoenix_surfnet(
    graph: &PhoenixMainnetGraph,
    eternal_program: &[u8],
    hawkeye_program: &[u8],
) -> SurfnetSvmLocker {
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm_writer| {
        svm_writer.inner.set_sysvar(&graph.clock);
        svm_writer
            .inner
            .svm
            .add_program(PHOENIX_PROGRAM_ID, eternal_program)
            .unwrap();
        svm_writer
            .inner
            .svm
            .add_program(HAWKEYE_PROGRAM_ID, hawkeye_program)
            .unwrap();
        for (address, account) in &graph.accounts {
            svm_writer.set_account(address, account.clone()).unwrap();
        }
    });
    locker
}

async fn phoenix_mainnet_graph() -> PhoenixMainnetGraph {
    let mut cache = mainnet_graph_cache().lock().await;
    match cache.as_ref() {
        Some(Ok(graph)) => return graph.clone(),
        Some(Err(reason)) => panic!("{reason}"),
        None => {}
    }

    let global_account = fetch(&[PHOENIX_GLOBAL_CONFIG]).await.remove(0);
    let global = GlobalConfig::try_from_account_bytes(&global_account.data)
        .expect("mainnet GlobalConfig decodes");
    let perp_asset_map = Pubkey::new_from_array(global.perp_asset_map_key());
    let global_trader_index = Pubkey::new_from_array(global.global_trader_index_header_key());
    let active_trader_buffer = Pubkey::new_from_array(global.active_trader_buffer_header_key());

    // One request carries both the market list and the trader index discovery walks.
    let mut discovery = fetch(&[perp_asset_map, global_trader_index]).await;
    let map_account = discovery.remove(0);
    let index_account = discovery.remove(0);
    let map = PerpAssetMap::try_from_account_bytes(&map_account.data)
        .expect("mainnet PerpAssetMap decodes");
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
            .expect("mainnet SOL/BTC market");
        let orderbook =
            Pubkey::new_from_array(entry.metadata.static_market_params().market_account);
        let spline = derive_spline_collection_address(&PHOENIX_PROGRAM_ID, &orderbook);
        addresses.extend([orderbook, spline]);
        markets.push((symbol.to_string(), orderbook, spline));
    }

    let eternal_program = deployed_program(ETERNAL_PROGRAMDATA).await;
    let hawkeye_program = deployed_program(HAWKEYE_PROGRAMDATA).await;
    let clock_address = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");
    // Every hot Trader is a candidate, in address order, so the pick moves only when it or a
    // trader ahead of it changes.
    let mut candidates: Vec<Pubkey> = index_trader_state_ranges(&index_account)
        .expect("mainnet GlobalTraderIndex should walk")
        .into_iter()
        .map(|(trader, _)| trader)
        .collect();
    candidates.sort_unstable();
    // Every batch re-reads the dependencies and the clock, so each fills a request to the
    // 100-account cap.
    let batch_len = 100 - addresses.len() - 1;
    let mut last_error = None;
    for batch in candidates.chunks(batch_len) {
        // Read the clock, every dependency and this batch of traders from one bank, so a trader
        // is checked, and then tested, on state the programs accept together.
        let keys: Vec<Pubkey> = addresses
            .iter()
            .chain(batch)
            .chain([&clock_address])
            .copied()
            .collect();
        let mut fetched = client()
            .get_multiple_accounts(&keys, CommitmentConfig::confirmed())
            .await
            .unwrap_or_else(|e| panic!("failed to fetch {keys:?} from mainnet: {e}"))
            .into_iter();
        let dependencies: Vec<(Pubkey, Account)> = addresses
            .iter()
            .zip(fetched.by_ref())
            .map(|(address, result)| {
                let account = result.map_account().unwrap_or_else(|_| {
                    panic!("{address} no longer exists on mainnet; the test needs a new address")
                });
                (*address, account)
            })
            .collect();
        // A trader closed since the index was read is skipped, not an error.
        let traders: Vec<(Pubkey, Account)> = batch
            .iter()
            .zip(fetched.by_ref())
            .filter_map(|(trader, result)| Some((*trader, result.map_account().ok()?)))
            .collect();
        let clock_account = fetched
            .next()
            .and_then(|result| result.map_account().ok())
            .expect("the clock is read with the graph");
        let clock: Clock = bincode::deserialize(&clock_account.data).unwrap();
        assert!(clock.slot > 0);

        let mut graph = PhoenixMainnetGraph {
            clock,
            accounts: dependencies.iter().chain(&traders).cloned().collect(),
            global_trader_index,
            active_trader_buffer,
            perp_asset_map,
            trader: Pubkey::default(),
            markets: markets.clone(),
        };
        // A trader that left the index since it was listed is no longer hot.
        let indexed: HashSet<Pubkey> =
            index_trader_state_ranges(graph.account(&global_trader_index))
                .expect("mainnet GlobalTraderIndex should walk")
                .into_iter()
                .map(|(trader, _)| trader)
                .collect();
        let locker = phoenix_surfnet(&graph, &eternal_program, &hawkeye_program);
        for (trader, account) in traders
            .iter()
            .filter(|(trader, _)| indexed.contains(trader))
        {
            graph.trader = *trader;
            match trader_is_eligible(&locker, &graph) {
                Ok(false) => {}
                Err(error) => last_error = Some(format!("{trader}: {error}")),
                Ok(true) => {
                    // The rest of the batch was read only to be checked.
                    graph.accounts = dependencies
                        .into_iter()
                        .chain([(*trader, account.clone())])
                        .collect();
                    *cache = Some(Ok(graph.clone()));
                    return graph;
                }
            }
        }
    }

    let reason = format!(
        "no eligible mainnet candidate: no hot Phoenix Trader in the GlobalTraderIndex has \
         collateral, a position Hawkeye margins and a healthy account{}",
        last_error
            .map(|error| format!("; the last margin view that failed was {error}"))
            .unwrap_or_default()
    );
    *cache = Some(Err(reason.clone()));
    panic!("{reason}")
}

#[derive(Clone)]
struct PhoenixMainnetGraph {
    clock: Clock,
    accounts: Vec<(Pubkey, Account)>,
    global_trader_index: Pubkey,
    active_trader_buffer: Pubkey,
    perp_asset_map: Pubkey,
    trader: Pubkey,
    /// Symbol, orderbook and spline for each market the Hawkeye BBO view reads.
    markets: Vec<(String, Pubkey, Pubkey)>,
}

impl PhoenixMainnetGraph {
    fn account(&self, address: &Pubkey) -> &Account {
        self.accounts
            .iter()
            .find_map(|(key, account)| (key == address).then_some(account))
            .unwrap_or_else(|| panic!("{address} is not in the mainnet graph"))
    }
}

fn hawkeye_view(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixMainnetGraph,
    discriminant: [u8; 8],
    extra_accounts: &[Pubkey],
) -> Vec<u8> {
    try_hawkeye_view(locker, graph, discriminant, extra_accounts)
        .unwrap_or_else(|e| panic!("Hawkeye view failed: {e}"))
}

/// The view's return data, or the transaction error and logs when the view fails.
fn try_hawkeye_view(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixMainnetGraph,
    discriminant: [u8; 8],
    extra_accounts: &[Pubkey],
) -> Result<Vec<u8>, String> {
    let payer = Keypair::new();
    let accounts = [
        PHOENIX_PROGRAM_ID,
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
            .map(|meta| meta.return_data.data)
            .map_err(|failed| format!("{:?}, logs: {:?}", failed.err, failed.meta.logs))
    })
}

fn hawkeye_margin(locker: &SurfnetSvmLocker, graph: &PhoenixMainnetGraph) -> HawkeyeMarginView {
    try_hawkeye_margin(locker, graph)
        .unwrap_or_else(|e| panic!("Hawkeye margin view failed for {}: {e}", graph.trader))
}

fn try_hawkeye_margin(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixMainnetGraph,
) -> Result<HawkeyeMarginView, String> {
    let data = try_hawkeye_view(
        locker,
        graph,
        HAWKEYE_VIEW_MARGIN_DISCRIMINANT,
        &[graph.trader],
    )?;
    let margin = bytemuck::try_pod_read_unaligned::<HawkeyeMarginView>(&data)
        .map_err(|e| format!("the margin view returned {} bytes: {e:?}", data.len()))?;
    if margin.magic != HAWKEYE_MARGIN_RETURN_MAGIC {
        return Err(format!(
            "the margin view returned magic {:#x}",
            margin.magic
        ));
    }
    Ok(margin)
}

fn hawkeye_bbo_for_market(
    graph: &PhoenixMainnetGraph,
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

fn phoenix_market_override(
    template_id: &str,
    perp_asset_map: Pubkey,
    values: &[(&str, &str)],
) -> surfpool_types::OverrideInstance {
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
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_market_move_reprices_the_oracles_and_the_book_together() {
    let (locker, graph) = phoenix_behavior_locker().await;
    // A surfnet loads this sysvar from its datasource; the bare test VM has none.
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));

    for (symbol, orderbook, spline) in graph.markets.clone() {
        let before = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
        let target = before.mark_price_ticks * 93 / 100;
        let map_before = locker
            .with_svm_reader(|svm| svm.get_account(&graph.perp_asset_map))
            .unwrap();

        let writes = {
            let mut svm = locker.0.write().await;
            move_market(&mut svm, &remote, &symbol, target)
                .await
                .unwrap_or_else(|e| panic!("{symbol}: {e}"))
        };
        assert_eq!(
            locker
                .with_svm_reader(|svm| svm.get_account(&graph.perp_asset_map))
                .unwrap(),
            map_before,
            "{symbol}: the move runs on a copy of the local VM"
        );
        let written: Vec<Pubkey> = writes.iter().map(|(pubkey, _)| *pubkey).collect();
        assert!(
            written.contains(&graph.perp_asset_map) && written.contains(&spline),
            "{symbol}: the oracles and the splines move together, wrote {written:?}"
        );
        locker.with_svm_writer(|svm| {
            for (pubkey, account) in writes {
                svm.set_account(&pubkey, account).unwrap();
            }
        });

        let after = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
        println!(
            "{symbol}: {} -> {target}: bid {} ask {} mark {} index {}",
            before.mark_price_ticks,
            after.best_bid_ticks,
            after.best_ask_ticks,
            after.mark_price_ticks,
            after.index_price_ticks
        );
        assert_eq!(
            after.index_price_ticks, target,
            "{symbol}: the index is the oracle reports"
        );
        // The mark also weighs in the book, which now quotes around the target.
        assert!(
            after.mark_price_ticks.abs_diff(target) <= (target / 1_000).max(2),
            "{symbol}: the mark follows the oracles and the book to the target"
        );
        assert!(
            after.best_bid_ticks < after.best_ask_ticks,
            "{symbol}: resting orders the move crossed are matched"
        );
        let band = target / 100;
        assert!(
            after.best_bid_ticks + band >= target && after.best_ask_ticks <= target + band,
            "{symbol}: the makers quote around the new price"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_market_move_scenario_plays_through_the_materializer() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let (symbol, orderbook, spline) = graph.markets[0].clone();
    let target =
        hawkeye_bbo_for_market(&graph, &locker, orderbook, spline).mark_price_ticks * 90 / 100;
    let clock = trail_mainnet_clock(&locker, &graph);

    let mut scenario = surfpool_types::Scenario::new(
        "phoenix-market-move".to_string(),
        format!("Move {symbol} to {target} ticks"),
    );
    scenario.add_override(phoenix_market_override(
        "phoenix-market-move",
        graph.perp_asset_map,
        &[
            ("symbol", symbol.as_str()),
            ("target_ticks", &target.to_string()),
        ],
    ));
    locker
        .register_scenario(scenario, Some(clock.slot))
        .unwrap();
    locker
        .materialize_overrides_for_slot(
            &Some((client(), CommitmentConfig::confirmed())),
            clock.slot,
        )
        .await
        .unwrap();

    // A skipped override only logs, so the program's own view decides.
    let after = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
    assert_eq!(
        after.index_price_ticks, target,
        "{symbol}: Play moved the oracles"
    );
    assert!(
        after.best_bid_ticks < after.best_ask_ticks
            && after.best_bid_ticks + target / 100 >= target
            && after.best_ask_ticks <= target + target / 100,
        "{symbol}: Play moved the book with them: bid {} ask {}",
        after.best_bid_ticks,
        after.best_ask_ticks
    );
}

/// A running surfnet's clock trails mainnet, so the accounts it fetches carry funding and spline
/// updates from after its time and slot. This puts the test VM's clock as far behind.
fn trail_mainnet_clock(locker: &SurfnetSvmLocker, graph: &PhoenixMainnetGraph) -> Clock {
    let mut clock = graph.clock.clone();
    clock.unix_timestamp -= 120;
    clock.slot -= 300;
    locker.with_svm_writer(|svm| svm.inner.set_sysvar(&clock));
    clock
}

/// Plays a one-override scenario at `slot` through the materializer.
async fn play(
    locker: &SurfnetSvmLocker,
    slot: u64,
    template_id: &str,
    account: Pubkey,
    values: &[(&str, &str)],
) {
    let mut scenario =
        surfpool_types::Scenario::new(template_id.to_string(), template_id.to_string());
    scenario.add_override(
        surfpool_types::OverrideInstance::new(
            template_id.to_string(),
            0,
            surfpool_types::AccountAddress::Pubkey(account.to_string()),
        )
        .with_values(
            values
                .iter()
                .map(|(field, value)| (field.to_string(), serde_json::json!(value)))
                .collect(),
        ),
    );
    locker.register_scenario(scenario, Some(slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&Some((client(), CommitmentConfig::confirmed())), slot)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_templates_run_phoenix_own_instructions() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let map_before =
        PerpAssetMap::try_from_account_bytes(&graph.account(&graph.perp_asset_map).data)
            .unwrap()
            .find_by_symbol("SOL")
            .unwrap()
            .unwrap()
            .metadata;
    let [_, backstop, high_risk] = map_before.risk_params().risk_factors;
    let slot = graph.clock.slot;

    // A risk factor, signed by the risk authority. Studio's preset sends only the maintenance
    // factor; the others, left out or empty, keep their values.
    play(
        &locker,
        slot,
        "phoenix-market-risk-factors",
        graph.perp_asset_map,
        &[
            ("symbol", "SOL"),
            ("maintenanceRiskFactor", "7000"),
            ("backstopRiskFactor", ""),
        ],
    )
    .await;
    let map = locker
        .with_svm_reader(|svm| svm.get_account(&graph.perp_asset_map))
        .unwrap()
        .unwrap();
    let factors = PerpAssetMap::try_from_account_bytes(&map.data)
        .unwrap()
        .find_by_symbol("SOL")
        .unwrap()
        .unwrap()
        .metadata
        .risk_params()
        .risk_factors;
    assert_eq!(
        factors,
        [7000, backstop, high_risk],
        "the risk authority set SOL's factors"
    );

    // A market status, signed by the market authority: a paused market refuses orders.
    let (_, orderbook, spline) = graph.markets[0].clone();
    play(
        &locker,
        slot + 1,
        "phoenix-market-status",
        graph.perp_asset_map,
        &[("symbol", "SOL"), ("nextMarketStatus", "Paused")],
    )
    .await;
    let book_status = locker
        .with_svm_reader(|svm| svm.get_account(&orderbook))
        .unwrap()
        .unwrap();
    let paused = {
        let mut svm = locker.0.write().await;
        crate::scenarios::protocols::phoenix_eternal::v1::trader::place_market_order(
            &mut svm,
            &Some((client(), CommitmentConfig::confirmed())),
            graph.trader,
            "SOL",
            crate::scenarios::protocols::phoenix_eternal::v1::trader::Side::Bid,
            1,
        )
        .await
    };
    assert!(
        paused.is_err(),
        "a paused SOL market refuses a market order, orderbook {} bytes",
        book_status.data.len()
    );
    let _ = spline;

    // An exchange status, signed by the root authority.
    let global_before = graph.account(&PHOENIX_GLOBAL_CONFIG).data.clone();
    play(
        &locker,
        slot + 2,
        "phoenix-exchange-status",
        PHOENIX_GLOBAL_CONFIG,
        &[("maintenance", "true")],
    )
    .await;
    let global_after = locker
        .with_svm_reader(|svm| svm.get_account(&PHOENIX_GLOBAL_CONFIG))
        .unwrap()
        .unwrap()
        .data;
    assert_ne!(
        global_after, global_before,
        "the root authority switched a status flag"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trader_crosses_into_liquidatable_between_two_slots() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    // Where the liquidation-ready fixture would move SOL for some long, found on a copy.
    let (trader, target) = {
        let mut svm = locker.0.write().await;
        let ready = crate::scenarios::protocols::phoenix_eternal::v1::trader::prepare_cascade(
            &mut svm, &remote, "SOL", true, 1,
        )
        .await
        .unwrap_or_else(|e| panic!("no eligible mainnet candidate: {e}"));
        (ready.liquidations[0].0, ready.target_ticks)
    };
    let traded = PhoenixMainnetGraph {
        trader,
        ..graph.clone()
    };

    play(
        &locker,
        graph.clock.slot,
        "phoenix-cancel-orders",
        trader,
        &[("symbol", "SOL")],
    )
    .await;
    assert_eq!(
        hawkeye_margin(&locker, &traded).is_liquidatable,
        0,
        "{trader}: still healthy after slot 0"
    );
    play(
        &locker,
        graph.clock.slot + 1,
        "phoenix-market-move",
        graph.perp_asset_map,
        &[("symbol", "SOL"), ("target_ticks", &target.to_string())],
    )
    .await;
    assert_eq!(
        hawkeye_margin(&locker, &traded).is_liquidatable,
        1,
        "{trader}: liquidatable once SOL moves to {target} at slot 1"
    );
}

/// Runs a template through the Phoenix dispatch the materializer calls and applies its writes,
/// returning the program's refusal when there is one.
async fn configure(
    locker: &SurfnetSvmLocker,
    template_id: &str,
    account: Pubkey,
    values: &[(&str, String)],
) -> Result<(), String> {
    let values: std::collections::HashMap<String, serde_json::Value> = values
        .iter()
        .map(|(field, value)| (field.to_string(), serde_json::json!(value)))
        .collect();
    let mut svm = locker.0.write().await;
    let target = svm.inner.get_account(&account).unwrap().unwrap_or_default();
    let writes =
        crate::scenarios::protocols::phoenix_eternal::v1::state_builder::prepare_phoenix_override(
            &mut svm,
            template_id,
            &account,
            &target,
            &values,
            &Some((client(), CommitmentConfig::confirmed())),
            false,
            0,
        )
        .await
        .map_err(|e| format!("{template_id}: {e}"))?
        .unwrap_or_default();
    for (pubkey, written) in writes {
        svm.set_account(&pubkey, written).unwrap();
    }
    Ok(())
}

fn sol(field: &'static str, value: String) -> Vec<(&'static str, String)> {
    vec![("symbol", "SOL".to_string()), (field, value)]
}

fn sol_metadata(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixMainnetGraph,
) -> phoenix_rise_accounts::perp_asset_map::PerpAssetMetadata {
    let map = locker
        .with_svm_reader(|svm| svm.get_account(&graph.perp_asset_map))
        .unwrap()
        .unwrap();
    PerpAssetMap::try_from_account_bytes(&map.data)
        .unwrap()
        .find_by_symbol("SOL")
        .unwrap()
        .unwrap()
        .metadata
}

#[tokio::test(flavor = "multi_thread")]
async fn market_and_exchange_configuration_reaches_the_accounts_phoenix_reads() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let (_, orderbook, _) = graph.markets[0].clone();
    let before = sol_metadata(&locker, &graph);

    configure(
        &locker,
        "phoenix-market-cancel-risk-factor",
        graph.perp_asset_map,
        &sol("cancelOrderRiskFactor", "8000".into()),
    )
    .await
    .unwrap();
    assert_eq!(
        sol_metadata(&locker, &graph)
            .risk_params()
            .cancel_order_risk_factor,
        8000
    );

    configure(
        &locker,
        "phoenix-market-max-liquidation-size",
        graph.perp_asset_map,
        &sol("maxLiquidationSize", "123456".into()),
    )
    .await
    .unwrap();
    assert_eq!(
        sol_metadata(&locker, &graph)
            .risk_params()
            .max_liquidation_size
            .as_inner(),
        123456
    );

    let cap = before.open_interest_params().open_interest_cap.as_inner() * 2;
    configure(
        &locker,
        "phoenix-market-open-interest-cap",
        graph.perp_asset_map,
        &sol("openInterestCap", cap.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(
        sol_metadata(&locker, &graph)
            .open_interest_params()
            .open_interest_cap
            .as_inner(),
        cap
    );

    let max_rate = before.funding_accumulator().max_funding_rate.as_inner() / 2;
    configure(
        &locker,
        "phoenix-market-funding",
        graph.perp_asset_map,
        &sol("maxFundingRate", max_rate.to_string()),
    )
    .await
    .unwrap();
    let funding = *sol_metadata(&locker, &graph).funding_accumulator();
    assert_eq!(funding.max_funding_rate.as_inner(), max_rate);
    assert_eq!(
        funding.funding_period_seconds,
        before.funding_accumulator().funding_period_seconds,
        "a funding field left out keeps its value"
    );

    configure(
        &locker,
        "phoenix-market-fees",
        graph.perp_asset_map,
        &[
            ("symbol", "SOL".to_string()),
            ("defaultTakerFeeMicro", "1000".to_string()),
            ("defaultMakerFeeMicro", "-50".to_string()),
        ],
    )
    .await
    .unwrap();
    let book = locker
        .with_svm_reader(|svm| svm.get_account(&orderbook))
        .unwrap()
        .unwrap();
    let header = phoenix_rise_accounts::orderbook::Orderbook::try_from_account_bytes(&book.data)
        .unwrap()
        .header;
    assert_eq!(
        (
            header.default_taker_fee_micro(),
            header.default_maker_fee_micro()
        ),
        (1000, -50)
    );

    let global =
        GlobalConfig::try_from_account_bytes(&graph.account(&PHOENIX_GLOBAL_CONFIG).data).unwrap();
    let queue = Pubkey::new_from_array(global.withdraw_queue_key());
    configure(
        &locker,
        "phoenix-withdraw-limits",
        queue,
        &[
            ("maxBudget", "1000000000".to_string()),
            ("replenishAmountPerSlot", "0".to_string()),
        ],
    )
    .await
    .unwrap();
    configure(
        &locker,
        "phoenix-withdraw-parameters",
        queue,
        &[
            ("depositCooldownPeriodInSlots", "300".to_string()),
            ("withdrawalFee", "1000000".to_string()),
            ("enqueueingFee", "2000000".to_string()),
        ],
    )
    .await
    .unwrap();
    let queue_account = locker
        .with_svm_reader(|svm| svm.get_account(&queue))
        .unwrap()
        .unwrap();
    let queue_header =
        phoenix_rise_accounts::withdraw_queue::WithdrawQueueHeader::try_from_account_bytes(
            &queue_account.data,
        )
        .unwrap();
    assert_eq!(
        (
            queue_header.withdraw_throttle().max_budget().as_inner(),
            queue_header
                .withdraw_throttle()
                .replenish_amount_per_slot()
                .as_inner(),
            queue_header.withdrawal_fee().as_inner(),
            queue_header.enqueueing_fee().as_inner(),
        ),
        (1_000_000_000, 0, 1_000_000, 2_000_000)
    );
    let global_after = locker
        .with_svm_reader(|svm| svm.get_account(&PHOENIX_GLOBAL_CONFIG))
        .unwrap()
        .unwrap();
    assert_eq!(
        GlobalConfig::try_from_account_bytes(&global_after.data)
            .unwrap()
            .deposit_cooldown_period_in_slots(),
        300
    );
}

/// Hot traders whose account lists a SOL position. The account copy can lag, so the program's own
/// view decides later whether a candidate really qualifies.
async fn sol_position_holders(graph: &PhoenixMainnetGraph) -> Vec<Pubkey> {
    let map = PerpAssetMap::try_from_account_bytes(&graph.account(&graph.perp_asset_map).data)
        .expect("mainnet PerpAssetMap decodes");
    let sol = u64::from(
        map.find_by_symbol("SOL")
            .unwrap()
            .expect("mainnet lists SOL")
            .metadata
            .static_market_params()
            .asset_id(),
    );
    let mut candidates: Vec<Pubkey> =
        index_trader_state_ranges(graph.account(&graph.global_trader_index))
            .expect("mainnet GlobalTraderIndex should walk")
            .into_iter()
            .map(|(trader, _)| trader)
            .collect();
    candidates.sort_unstable();
    let mut holders = Vec::new();
    for batch in candidates.chunks(100) {
        let fetched = client()
            .get_multiple_accounts(batch, CommitmentConfig::confirmed())
            .await
            .unwrap();
        for (trader, result) in batch.iter().zip(fetched) {
            let Ok(account) = result.map_account() else {
                continue;
            };
            let Ok(view) = Trader::try_from_account_bytes(&account.data) else {
                continue;
            };
            if view.positions().any(|(asset, position)| {
                asset == sol && position.base_lot_position().as_inner() != 0
            }) {
                holders.push(*trader);
            }
        }
    }
    holders
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prepared_trader_is_liquidated_by_a_market_order() {
    let (locker, graph) = phoenix_behavior_locker().await;
    // A surfnet loads this sysvar from its datasource; the bare test VM has none.
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let (_, orderbook, spline) = graph
        .markets
        .iter()
        .find(|(symbol, _, _)| symbol == "SOL")
        .cloned()
        .unwrap();

    let mut skipped = Vec::new();
    for trader in sol_position_holders(&graph).await.into_iter().take(8) {
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        let account = fetch(&[trader]).await.remove(0);
        locker.with_svm_writer(|svm| svm.set_account(&trader, account).unwrap());
        if try_hawkeye_margin(&locker, &traded).map(|m| m.is_liquidatable) != Ok(0) {
            skipped.push(format!("{trader}: not healthy on mainnet"));
            continue;
        }
        let prepared = {
            let mut svm = locker.0.write().await;
            prepare_liquidation(&mut svm, &remote, trader, "SOL").await
        };
        let ready = match prepared {
            Ok(ready) => ready,
            Err(e) => {
                skipped.push(format!("{trader}: {e}"));
                continue;
            }
        };
        locker.with_svm_writer(|svm| {
            for (pubkey, account) in ready.writes {
                svm.set_account(&pubkey, account).unwrap();
            }
        });
        let before = hawkeye_margin(&locker, &traded);
        assert_eq!(
            before.is_liquidatable, 1,
            "{trader}: Play leaves it liquidatable"
        );
        assert!(
            before.effective_collateral_quote_lots > 0,
            "{trader}: and not underwater"
        );

        let payer = Keypair::new();
        let result = locker.with_svm_writer(|svm| {
            svm.inner.set_sigverify(false);
            svm.inner.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
            let mut transaction = Transaction::new_with_payer(
                &[
                    ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                    ready.liquidation.clone(),
                ],
                Some(&payer.pubkey()),
            );
            transaction.partial_sign(&[&payer], svm.inner.svm.latest_blockhash());
            svm.inner
                .send_transaction(transaction)
                .map(|_| ())
                .map_err(|failed| format!("{:?}: {:?}", failed.err, failed.meta.logs))
        });
        result.unwrap_or_else(|e| panic!("{trader}: the liquidation failed: {e}"));

        let after = hawkeye_margin(&locker, &traded);
        let book = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
        println!(
            "{trader}: SOL moved to {}, effective {} -> {}, positions {} -> {}, book {} / {} mark {}",
            ready.target_ticks,
            before.effective_collateral_quote_lots,
            after.effective_collateral_quote_lots,
            before.position_count,
            after.position_count,
            book.best_bid_ticks,
            book.best_ask_ticks,
            book.mark_price_ticks
        );
        assert_eq!(
            after.is_liquidatable, 0,
            "{trader}: the liquidation restored it"
        );
        // The fill came from liquidity at the new price, not from the book's old one.
        assert!(
            after.effective_collateral_quote_lots < before.effective_collateral_quote_lots * 2,
            "{trader}: the liquidation filled near the moved price"
        );
        assert!(book.best_bid_ticks < book.best_ask_ticks);
        return;
    }
    panic!("no eligible mainnet candidate: {skipped:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_liquidation_ready_scenario_plays_through_the_materializer() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    // A market moved by an earlier scenario, after Phoenix last validated its mark, and this
    // scenario some slots later, both behind mainnet's time.
    let mut clock = graph.clock.clone();
    clock.slot += 50;
    clock.unix_timestamp -= 120;
    locker.with_svm_writer(|svm| svm.inner.set_sysvar(&clock));
    let (_, orderbook, spline) = graph
        .markets
        .iter()
        .find(|(symbol, _, _)| symbol == "SOL")
        .cloned()
        .unwrap();
    let moved =
        hawkeye_bbo_for_market(&graph, &locker, orderbook, spline).mark_price_ticks * 98 / 100;
    play(
        &locker,
        clock.slot,
        "phoenix-market-move",
        graph.perp_asset_map,
        &[("symbol", "SOL"), ("target_ticks", &moved.to_string())],
    )
    .await;
    clock.slot += 160;
    locker.with_svm_writer(|svm| svm.inner.set_sysvar(&clock));
    let map = PerpAssetMap::try_from_account_bytes(&graph.account(&graph.perp_asset_map).data)
        .expect("mainnet PerpAssetMap decodes");
    let sol = u64::from(
        map.find_by_symbol("SOL")
            .unwrap()
            .expect("mainnet lists SOL")
            .metadata
            .static_market_params()
            .asset_id(),
    );

    let mut skipped = Vec::new();
    for (round, trader) in sol_position_holders(&graph)
        .await
        .into_iter()
        .take(8)
        .enumerate()
    {
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        let account = fetch(&[trader]).await.remove(0);
        let long = Trader::try_from_account_bytes(&account.data)
            .unwrap()
            .positions()
            .find(|(asset, _)| *asset == sol)
            .map(|(_, position)| position.base_lot_position().as_inner() > 0)
            .unwrap();
        locker.with_svm_writer(|svm| svm.set_account(&trader, account).unwrap());
        if try_hawkeye_margin(&locker, &traded).map(|m| m.is_liquidatable) != Ok(0) {
            skipped.push(format!("{trader}: not healthy on mainnet"));
            continue;
        }

        let slot = clock.slot + round as u64;
        let mut scenario = surfpool_types::Scenario::new(
            "phoenix-liquidation-ready".to_string(),
            format!("Leave {trader} ready for liquidation"),
        );
        scenario.add_override(
            surfpool_types::OverrideInstance::new(
                "phoenix-liquidation-ready".to_string(),
                0,
                surfpool_types::AccountAddress::Pubkey(trader.to_string()),
            )
            .with_values(HashMap::from([(
                "symbol".to_string(),
                serde_json::json!("SOL"),
            )])),
        );
        locker.register_scenario(scenario, Some(slot)).unwrap();
        locker
            .materialize_overrides_for_slot(&remote, slot)
            .await
            .unwrap();
        // A skipped override only logs, so the program's own view decides.
        let prepared = hawkeye_margin(&locker, &traded);
        if prepared.is_liquidatable != 1 {
            skipped.push(format!("{trader}: Play left it healthy"));
            continue;
        }
        assert!(
            prepared.effective_collateral_quote_lots > 0,
            "{trader}: not underwater"
        );

        let mark = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline).mark_price_ticks;
        let instruction = {
            let mut svm = locker.0.write().await;
            let context = MarketContext::load(&mut svm, &remote, "SOL").await.unwrap();
            liquidation(&context, trader, long, mark).unwrap()
        };
        let payer = Keypair::new();
        locker
            .with_svm_writer(|svm| {
                svm.inner.set_sigverify(false);
                svm.inner.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
                let mut transaction = Transaction::new_with_payer(
                    &[
                        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                        instruction,
                    ],
                    Some(&payer.pubkey()),
                );
                transaction.partial_sign(&[&payer], svm.inner.svm.latest_blockhash());
                svm.inner
                    .send_transaction(transaction)
                    .map(|_| ())
                    .map_err(|failed| format!("{:?}: {:?}", failed.err, failed.meta.logs))
            })
            .unwrap_or_else(|e| panic!("{trader}: the keeper's liquidation failed: {e}"));
        assert_eq!(
            hawkeye_margin(&locker, &traded).is_liquidatable,
            0,
            "{trader}: the liquidation restored it"
        );
        return;
    }
    panic!("no eligible mainnet candidate: {skipped:#?}");
}

fn token_amount(locker: &SurfnetSvmLocker, account: &Pubkey) -> u64 {
    locker
        .with_svm_reader(|svm| svm.get_account(account))
        .unwrap()
        .map(|account| u64::from_le_bytes(account.data[64..72].try_into().unwrap()))
        .unwrap_or(0)
}

fn mint_supply(locker: &SurfnetSvmLocker, mint: &Pubkey) -> u64 {
    let account = locker
        .with_svm_reader(|svm| svm.get_account(mint))
        .unwrap()
        .unwrap();
    u64::from_le_bytes(account.data[36..44].try_into().unwrap())
}

async fn apply(locker: &SurfnetSvmLocker, writes: Vec<(Pubkey, Account)>) {
    locker.with_svm_writer(|svm| {
        for (pubkey, account) in writes {
            svm.set_account(&pubkey, account).unwrap();
        }
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn collateral_moves_through_real_withdrawals_and_deposits() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let global =
        GlobalConfig::try_from_account_bytes(&graph.account(&PHOENIX_GLOBAL_CONFIG).data).unwrap();
    let vault = Pubkey::new_from_array(global.global_vault_key());
    let mint = Pubkey::new_from_array(global.canonical_token_mint_key());
    let authority = Pubkey::new_from_array(
        TraderHeader::try_read_from_account_bytes(&graph.account(&graph.trader).data)
            .unwrap()
            .authority,
    );
    let before = hawkeye_margin(&locker, &graph);
    assert!(
        before.withdrawable_collateral_quote_lots > 0,
        "no eligible mainnet candidate: the discovered trader has nothing to withdraw"
    );
    let amount = before.withdrawable_collateral_quote_lots.min(1_000_000);

    let writes = {
        let mut svm = locker.0.write().await;
        withdraw(&mut svm, &remote, graph.trader, amount)
            .await
            .unwrap()
    };
    let wallet = writes
        .iter()
        .map(|(pubkey, _)| *pubkey)
        .find(|pubkey| {
            *pubkey != vault && *pubkey != graph.trader && token_amount(&locker, pubkey) == 0
        })
        .unwrap_or_default();
    let (vault_before, mint_before) = (token_amount(&locker, &vault), mint_supply(&locker, &mint));
    apply(&locker, writes).await;
    let withdrawn = hawkeye_margin(&locker, &graph);
    // Phoenix settles accrued funding into collateral before it pays a withdrawal out.
    assert_eq!(
        withdrawn.collateral_quote_lots,
        before.collateral_quote_lots + before.unsettled_funding_quote_lots - amount as i64,
        "the withdrawal took exactly {amount} quote lots"
    );
    assert_eq!(
        token_amount(&locker, &vault),
        vault_before - amount,
        "the vault paid it out"
    );
    assert!(
        wallet == Pubkey::default() || token_amount(&locker, &wallet) >= amount,
        "the wallet {authority} received it"
    );

    let writes = {
        let mut svm = locker.0.write().await;
        deposit(&mut svm, &remote, graph.trader, amount)
            .await
            .unwrap()
    };
    apply(&locker, writes).await;
    let deposited = hawkeye_margin(&locker, &graph);
    assert_eq!(
        deposited.collateral_quote_lots,
        withdrawn.collateral_quote_lots + amount as i64,
        "the deposit added exactly {amount} quote lots"
    );
    assert_eq!(
        token_amount(&locker, &vault),
        vault_before,
        "the vault holds the deposit"
    );
    assert_eq!(
        mint_supply(&locker, &mint),
        mint_before + amount,
        "the deposited tokens were minted, not conjured into the vault"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_market_order_opens_a_position() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let map = PerpAssetMap::try_from_account_bytes(&graph.account(&graph.perp_asset_map).data)
        .expect("mainnet PerpAssetMap decodes");
    let held: Vec<u64> = Trader::try_from_account_bytes(&graph.account(&graph.trader).data)
        .unwrap()
        .positions()
        .filter(|(_, position)| position.base_lot_position().as_inner() != 0)
        .map(|(asset, _)| asset)
        .collect();
    let symbol = map
        .iter()
        .filter_map(Result::ok)
        .find(|entry| !held.contains(&u64::from(entry.metadata.static_market_params().asset_id())))
        .map(|entry| entry.symbol.as_str().to_string())
        .expect("no eligible mainnet candidate: the trader holds every listed market");

    let before = hawkeye_margin(&locker, &graph);
    let writes = {
        let mut svm = locker.0.write().await;
        place_market_order(&mut svm, &remote, graph.trader, &symbol, Side::Bid, 1)
            .await
            .unwrap_or_else(|e| panic!("{symbol}: {e}"))
    };
    apply(&locker, writes).await;
    let after = hawkeye_margin(&locker, &graph);
    assert_eq!(
        after.position_count,
        before.position_count + 1,
        "buying 1 {symbol} base lot opened a position"
    );
    assert!(after.initial_margin_quote_lots > before.initial_margin_quote_lots);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cascade_liquidates_several_traders_one_after_another() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let (_, orderbook, spline) = graph
        .markets
        .iter()
        .find(|(symbol, _, _)| symbol == "SOL")
        .cloned()
        .unwrap();

    let ready = {
        let mut svm = locker.0.write().await;
        prepare_cascade(&mut svm, &remote, "SOL", true, 3)
            .await
            .unwrap_or_else(|e| panic!("no eligible mainnet candidate: {e}"))
    };
    println!(
        "SOL moved to {} for {} longs",
        ready.target_ticks,
        ready.liquidations.len()
    );
    assert!(
        ready.liquidations.len() >= 2,
        "no eligible mainnet candidate: only {} long fits one price",
        ready.liquidations.len()
    );
    apply(&locker, ready.writes).await;

    let payer = Keypair::new();
    for (trader, instruction) in ready.liquidations {
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        let before = hawkeye_margin(&locker, &traded);
        assert_eq!(
            before.is_liquidatable, 1,
            "{trader}: liquidatable after Play"
        );
        locker
            .with_svm_writer(|svm| {
                svm.inner.set_sigverify(false);
                svm.inner.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
                let mut transaction = Transaction::new_with_payer(
                    &[
                        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                        instruction,
                    ],
                    Some(&payer.pubkey()),
                );
                transaction.partial_sign(&[&payer], svm.inner.svm.latest_blockhash());
                svm.inner
                    .send_transaction(transaction)
                    .map(|_| ())
                    .map_err(|failed| format!("{:?}: {:?}", failed.err, failed.meta.logs))
            })
            .unwrap_or_else(|e| panic!("{trader}: liquidation in turn failed: {e}"));
        let after = hawkeye_margin(&locker, &traded);
        println!(
            "  {trader}: effective {} -> {}",
            before.effective_collateral_quote_lots, after.effective_collateral_quote_lots
        );
        assert_eq!(
            after.is_liquidatable, 0,
            "{trader}: restored by its liquidation"
        );
    }
    let book = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
    assert!(
        book.best_bid_ticks < book.best_ask_ticks,
        "the book stays uncrossed after the cascade"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trader_configuration_reaches_what_phoenix_reads() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let withdraw_one = || async {
        let mut svm = locker.0.write().await;
        withdraw(&mut svm, &remote, graph.trader, 1).await
    };
    assert!(
        withdraw_one().await.is_ok(),
        "the trader may withdraw on mainnet"
    );

    // A capability, signed by the risk authority: a blocked withdrawal is refused.
    configure(
        &locker,
        "phoenix-trader-capabilities",
        graph.trader,
        &[("WithdrawCollateral", "false".to_string())],
    )
    .await
    .unwrap();
    assert!(
        withdraw_one().await.is_err(),
        "WithdrawCollateral false blocks withdrawals"
    );
    configure(
        &locker,
        "phoenix-trader-capabilities",
        graph.trader,
        &[("WithdrawCollateral", "true".to_string())],
    )
    .await
    .unwrap();
    assert!(withdraw_one().await.is_ok(), "true allows them again");

    // Fee multipliers, signed by the market authority, land in the record Phoenix reads.
    configure(
        &locker,
        "phoenix-trader-fees",
        graph.trader,
        &[
            ("makerFeeOverrideMultiplier", "2".to_string()),
            ("takerFeeOverrideMultiplier", "3".to_string()),
        ],
    )
    .await
    .unwrap();
    let index = locker
        .with_svm_reader(|svm| svm.get_account(&graph.global_trader_index))
        .unwrap()
        .unwrap();
    let range = index_trader_state_range(&index, &graph.trader.to_bytes()).unwrap();
    let state = bytemuck::pod_read_unaligned::<phoenix_rise_accounts::trader::TraderState>(
        &index.data
            [range.start..range.start + size_of::<phoenix_rise_accounts::trader::TraderState>()],
    );
    assert_eq!(
        (
            state.maker_fee_override_multiplier,
            state.taker_fee_override_multiplier
        ),
        (2, 3)
    );
}
