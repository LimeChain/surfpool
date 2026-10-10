use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
};

use phoenix_rise_accounts::{
    global_config::GlobalConfig,
    orderbook::Orderbook,
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
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use surfpool_types::DEFAULT_MAINNET_RPC_URL;

use crate::{
    scenarios::protocols::phoenix_eternal::v1::{
        market::{MarketContext, move_market, phoenix_markets},
        state_builder::{
            Exchange, LIQUIDATION_READY_TEMPLATE_ID, PHOENIX_GLOBAL_CONFIG, PHOENIX_PERP_ASSET_MAP,
            PHOENIX_PROGRAM_ID, prepare_phoenix_override, run_instructions,
            tests::template_addresses,
        },
        trader::{
            BboView, HAWKEYE_PROGRAM_ID, MarginView, Side, VIEW_BBO, VIEW_MARGIN, deposit,
            holdings, index_trader_state_ranges, liquidation, place_market_order, prepare_cascade,
            prepare_cross_margin, tests::index_trader_state_range, trader_positions, withdraw,
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
    for (id, account_type, address) in template_addresses() {
        let named = match account_type {
            "PerpAssetMap" => global.perp_asset_map_key(),
            "GlobalConfiguration" => global.account_key(),
            "WithdrawQueueHeader" => global.withdraw_queue_key(),
            other => panic!("{id} carries an address for {other}, which nothing checks"),
        };
        assert_eq!(
            address,
            Pubkey::new_from_array(named),
            "{id} carries the address GlobalConfig names"
        );
    }

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
    play(
        &locker,
        graph.clock.slot,
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
    )
    .await;
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
    // A surfnet loads this sysvar from its datasource; the bare test VM has none.
    let restart = fetch(&[LAST_RESTART_SLOT]).await.remove(0);
    locker.with_svm_writer(|svm| svm.set_account(&LAST_RESTART_SLOT, restart).unwrap());
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

fn hawkeye_margin(locker: &SurfnetSvmLocker, graph: &PhoenixMainnetGraph) -> MarginView {
    try_hawkeye_margin(locker, graph)
        .unwrap_or_else(|e| panic!("Hawkeye margin view failed for {}: {e}", graph.trader))
}

/// Why `graph.trader` cannot be a candidate: already liquidatable, or a margin view that failed.
fn healthy(locker: &SurfnetSvmLocker, graph: &PhoenixMainnetGraph) -> Result<(), String> {
    match try_hawkeye_margin(locker, graph) {
        Ok(margin) if margin.is_liquidatable == 0 => Ok(()),
        Ok(_) => Err("already liquidatable on mainnet".to_string()),
        Err(e) => Err(format!("margin view failed: {e}")),
    }
}

fn try_hawkeye_margin(
    locker: &SurfnetSvmLocker,
    graph: &PhoenixMainnetGraph,
) -> Result<MarginView, String> {
    let data = try_hawkeye_view(locker, graph, VIEW_MARGIN, &[graph.trader])?;
    MarginView::from_return_data(&data).map_err(|e| e.to_string())
}

fn hawkeye_bbo_for_market(
    graph: &PhoenixMainnetGraph,
    locker: &SurfnetSvmLocker,
    orderbook: Pubkey,
    spline: Pubkey,
) -> BboView {
    let data = hawkeye_view(locker, graph, VIEW_BBO, &[orderbook, spline]);
    BboView::from_return_data(&data).unwrap()
}

fn phoenix_override(
    template_id: &str,
    account: Pubkey,
    values: &[(&str, &str)],
) -> surfpool_types::OverrideInstance {
    surfpool_types::OverrideInstance::new(
        template_id.to_string(),
        0,
        surfpool_types::AccountAddress::Pubkey(account.to_string()),
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

/// One uncross crank matches at most 64 crossed orders, so a move past more resting bids than that
/// needs several before the book is uncrossed.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_past_more_resting_bids_than_one_crank_matches_uncrosses_the_book() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let remote = Some((client(), CommitmentConfig::confirmed()));
    for (symbol, orderbook, spline) in graph.markets.clone() {
        let bids: Vec<u64> = Orderbook::try_from_account_bytes(&graph.account(&orderbook).data)
            .unwrap()
            .bid_orders()
            .map(|bid| bid.price_in_ticks().as_inner())
            .collect();
        // Just below the 81st best bid, at least 81 resting bids cross the re-centred splines.
        let Some(target) = bids.get(80).map(|price| price - 1) else {
            continue;
        };
        let writes = {
            let mut svm = locker.0.write().await;
            move_market(&mut svm, &remote, &symbol, target)
                .await
                .unwrap_or_else(|e| panic!("{symbol}: {e}"))
        };
        apply(&locker, writes).await;
        let after = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
        assert!(
            after.best_bid_ticks < after.best_ask_ticks,
            "{symbol}: the book is uncrossed at {target} ticks: {after:?}"
        );
        let book = locker
            .with_svm_reader(|svm| svm.get_account(&orderbook))
            .unwrap()
            .unwrap();
        let crossed = Orderbook::try_from_account_bytes(&book.data)
            .unwrap()
            .bid_orders()
            .filter(|bid| bid.price_in_ticks().as_inner() >= target)
            .count();
        assert_eq!(
            crossed, 0,
            "{symbol}: no resting bid is left at or above {target}"
        );
        return;
    }
    panic!(
        "no eligible mainnet candidate: no book of {:?} rests 81 bids",
        graph.markets
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
    scenario.add_override(phoenix_override(template_id, account, values));
    locker.register_scenario(scenario, Some(slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&Some((client(), CommitmentConfig::confirmed())), slot)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trader_crosses_into_liquidatable_between_two_slots() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let remote = Some((client(), CommitmentConfig::confirmed()));
    // Where the liquidation-ready fixture would move SOL for some long, found on a copy.
    let (trader, target) = {
        let mut svm = locker.0.write().await;
        let ready = crate::scenarios::protocols::phoenix_eternal::v1::trader::prepare_cascade(
            &mut svm, &remote, "SOL", true,
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
    let (_, orderbook, _) = graph.markets[0].clone();
    let before = sol_metadata(&locker, &graph);
    let slot = graph.clock.slot;

    // Studio's risk factor preset sends only the maintenance factor; the others, left out or
    // empty, keep their values.
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
    let [_, backstop, high_risk] = before.risk_params().risk_factors;
    assert_eq!(
        sol_metadata(&locker, &graph).risk_params().risk_factors,
        [7000, backstop, high_risk],
        "the risk authority set SOL's factors"
    );

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
    let global_data = || {
        locker
            .with_svm_reader(|svm| svm.get_account(&PHOENIX_GLOBAL_CONFIG))
            .unwrap()
            .unwrap()
            .data
    };
    assert_eq!(
        GlobalConfig::try_from_account_bytes(&global_data())
            .unwrap()
            .deposit_cooldown_period_in_slots(),
        300
    );

    play(
        &locker,
        slot + 1,
        "phoenix-market-status",
        graph.perp_asset_map,
        &[("symbol", "SOL"), ("nextMarketStatus", "Paused")],
    )
    .await;
    let paused = {
        let mut svm = locker.0.write().await;
        place_market_order(
            &mut svm,
            &Some((client(), CommitmentConfig::confirmed())),
            graph.trader,
            "SOL",
            Side::Bid,
            1,
        )
        .await
    };
    let refused = paused
        .expect_err("a paused SOL market refuses a market order")
        .to_string();
    assert!(
        refused.contains("Market status Paused does not satisfy"),
        "{refused}"
    );

    let global_before = global_data();
    play(
        &locker,
        slot + 2,
        "phoenix-exchange-status",
        PHOENIX_GLOBAL_CONFIG,
        &[("maintenance", "true")],
    )
    .await;
    assert_ne!(
        global_data(),
        global_before,
        "the root authority switched a status flag"
    );
}

/// Hot traders, in address order, whose account `keep` accepts. The account copy can lag, so the
/// program's own view decides later whether a candidate really qualifies.
async fn indexed_traders(
    graph: &PhoenixMainnetGraph,
    keep: impl Fn(&Trader) -> bool,
) -> Vec<Pubkey> {
    let mut listed: Vec<Pubkey> =
        index_trader_state_ranges(graph.account(&graph.global_trader_index))
            .expect("mainnet GlobalTraderIndex should walk")
            .into_iter()
            .map(|(trader, _)| trader)
            .collect();
    listed.sort_unstable();
    let fetched = client()
        .get_multiple_accounts(&listed, CommitmentConfig::confirmed())
        .await
        .unwrap();
    let mut kept = Vec::new();
    for (trader, result) in listed.into_iter().zip(fetched) {
        let Ok(account) = result.map_account() else {
            continue;
        };
        if Trader::try_from_account_bytes(&account.data).is_ok_and(|view| keep(&view)) {
            kept.push(trader);
        }
    }
    kept
}

fn sol_asset_id(graph: &PhoenixMainnetGraph) -> u64 {
    let map = PerpAssetMap::try_from_account_bytes(&graph.account(&graph.perp_asset_map).data)
        .expect("mainnet PerpAssetMap decodes");
    let sol = map
        .find_by_symbol("SOL")
        .unwrap()
        .expect("mainnet lists SOL");
    u64::from(sol.metadata.static_market_params().asset_id())
}

/// Hot traders whose account lists a SOL position.
async fn sol_position_holders(graph: &PhoenixMainnetGraph) -> Vec<Pubkey> {
    let sol = sol_asset_id(graph);
    indexed_traders(graph, |view| {
        view.positions()
            .any(|(asset, position)| asset == sol && position.base_lot_position().as_inner() != 0)
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_liquidation_ready_scenario_plays_through_the_materializer() {
    let (locker, graph) = phoenix_behavior_locker().await;
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
    let sol = sol_asset_id(&graph);

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
        if hawkeye_margin(&locker, &traded).is_liquidatable == 1 {
            skipped.push(format!("{trader}: already liquidatable on mainnet"));
            continue;
        }

        play(
            &locker,
            clock.slot + round as u64,
            LIQUIDATION_READY_TEMPLATE_ID,
            trader,
            &[("symbols", "SOL")],
        )
        .await;
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
        let lots = position_lots(&locker, &remote, &trader, "SOL").await;
        send_liquidation(&locker, instruction)
            .await
            .unwrap_or_else(|e| panic!("{trader}: the keeper's liquidation failed: {e}"));
        let left = position_lots(&locker, &remote, &trader, "SOL").await;
        assert!(
            left.abs() < lots.abs(),
            "{trader}: the liquidation took {lots} SOL base lots down to {left}"
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

/// Sends a keeper's liquidation on the test VM. Its liquidator wallet has no key, so it runs
/// unsigned on a copy and the copy's writes are applied.
async fn send_liquidation(
    locker: &SurfnetSvmLocker,
    liquidation: Instruction,
) -> Result<(), String> {
    let writes = locker
        .with_svm_reader(|svm| run_instructions(svm, &[liquidation]))
        .map_err(|e| e.to_string())?;
    apply(locker, writes).await;
    Ok(())
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
    let token_program = locker
        .with_svm_reader(|svm| svm.get_account(&mint))
        .unwrap()
        .expect("withdraw put the quote mint in the local VM")
        .owner;
    let wallet = get_associated_token_address_with_program_id(&authority, &mint, &token_program);
    let (vault_before, mint_before) = (token_amount(&locker, &vault), mint_supply(&locker, &mint));
    let wallet_before = token_amount(&locker, &wallet);
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
    assert_eq!(
        token_amount(&locker, &wallet),
        wallet_before + amount,
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
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let markets = phoenix_markets(graph.perp_asset_map, graph.account(&graph.perp_asset_map))
        .expect("mainnet PerpAssetMap decodes");
    let candidates =
        indexed_traders(&graph, |view| view.header().trader_subaccount_index == 0).await;
    let mut chosen = None;
    for trader in candidates.into_iter().take(12) {
        let account = fetch(&[trader]).await.remove(0);
        let max_positions = Trader::try_from_account_bytes(&account.data)
            .unwrap()
            .max_positions() as usize;
        locker.with_svm_writer(|svm| svm.set_account(&trader, account).unwrap());
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        if !try_hawkeye_margin(&locker, &traded).is_ok_and(|m| m.free_collateral_quote_lots > 0) {
            continue;
        }
        // A hot trader's account copy lags, so Hawkeye decides what it holds and the room left.
        let held = held_positions(&locker, &remote, &trader).await;
        let unheld = markets
            .iter()
            .find(|market| held.iter().all(|(symbol, _)| *symbol != market.symbol));
        if let Some(market) = unheld.filter(|_| held.len() < max_positions) {
            chosen = Some((traded, market.symbol.clone()));
            break;
        }
    }
    let (graph, symbol) =
        chosen.expect("no eligible mainnet candidate: no trader with room and free collateral");

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
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let (_, orderbook, spline) = graph
        .markets
        .iter()
        .find(|(symbol, _, _)| symbol == "SOL")
        .cloned()
        .unwrap();

    let ready = {
        let mut svm = locker.0.write().await;
        prepare_cascade(&mut svm, &remote, "SOL", true)
            .await
            .unwrap_or_else(|e| panic!("no eligible mainnet candidate: {e}"))
    };
    assert!(
        ready.liquidations.len() >= 2,
        "no eligible mainnet candidate: only {} long fits one price",
        ready.liquidations.len()
    );
    apply(&locker, ready.writes).await;

    for (trader, instruction) in ready.liquidations {
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        assert_eq!(
            hawkeye_margin(&locker, &traded).is_liquidatable,
            1,
            "{trader}: liquidatable after Play"
        );
        let lots = position_lots(&locker, &remote, &trader, "SOL").await;
        send_liquidation(&locker, instruction)
            .await
            .unwrap_or_else(|e| panic!("{trader}: liquidation in turn failed: {e}"));
        let left = position_lots(&locker, &remote, &trader, "SOL").await;
        assert!(
            left.abs() < lots.abs(),
            "{trader}: its liquidation took {lots} SOL base lots down to {left}"
        );
    }
    let book = hawkeye_bbo_for_market(&graph, &locker, orderbook, spline);
    assert!(
        book.best_bid_ticks < book.best_ask_ticks,
        "the book stays uncrossed after the cascade"
    );
}

/// Hot traders on their cross-margin subaccount, quoting no spline, whose account shows a number of
/// positions in `positions`.
async fn cross_margin_candidates(
    graph: &PhoenixMainnetGraph,
    positions: RangeInclusive<usize>,
) -> Vec<Pubkey> {
    indexed_traders(graph, |view| {
        let header = view.header();
        let held = view
            .positions()
            .filter(|(_, position)| position.base_lot_position().as_inner() != 0)
            .count();
        header.trader_subaccount_index == 0
            && header.num_markets_with_splines == 0
            && positions.contains(&held)
    })
    .await
}

/// The symbols of the markets where Hawkeye reports a position of `trader`, with its base lots.
async fn held_positions(
    locker: &SurfnetSvmLocker,
    remote: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: &Pubkey,
) -> Vec<(String, i64)> {
    let mut svm = locker.0.write().await;
    let exchange = Exchange::load(&mut svm, remote).await.unwrap();
    holdings(&svm, &exchange, trader)
        .unwrap()
        .into_iter()
        .filter(|held| held.base_lots != 0)
        .map(|held| (held.market.symbol, held.base_lots))
        .collect()
}

/// The base lots of `trader`'s position in `symbol`, as Hawkeye reports it; 0 when it has none.
async fn position_lots(
    locker: &SurfnetSvmLocker,
    remote: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: &Pubkey,
    symbol: &str,
) -> i64 {
    held_positions(locker, remote, trader)
        .await
        .into_iter()
        .find_map(|(held, lots)| (held == symbol).then_some(lots))
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn several_positions_of_one_trader_are_liquidated_in_turn() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let clock = trail_mainnet_clock(&locker, &graph);

    let mut skipped = Vec::new();
    for trader in cross_margin_candidates(&graph, 2..=3)
        .await
        .into_iter()
        .take(12)
    {
        let account = fetch(&[trader]).await.remove(0);
        locker.with_svm_writer(|svm| svm.set_account(&trader, account.clone()).unwrap());
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        if let Err(reason) = healthy(&locker, &traded) {
            skipped.push(format!("{trader}: {reason}"));
            continue;
        }
        let held = held_positions(&locker, &remote, &trader).await;
        if !(2..=4).contains(&held.len()) {
            skipped.push(format!("{trader}: Hawkeye reports {held:?}"));
            continue;
        }
        let symbols: Vec<&str> = held.iter().map(|(symbol, _)| symbol.as_str()).collect();

        let played = {
            let mut svm = locker.0.write().await;
            prepare_phoenix_override(
                &mut svm,
                LIQUIDATION_READY_TEMPLATE_ID,
                &trader,
                &account,
                &HashMap::from([("symbols".to_string(), serde_json::json!(symbols.join(",")))]),
                &remote,
                clock.slot,
            )
            .await
        };
        let mut writes = match played {
            Ok(writes) => writes.expect("the template has its own writer"),
            Err(e) => {
                skipped.push(format!("{trader} on {symbols:?}: {e}"));
                continue;
            }
        };
        // The template only logs its order, so the same preparation is run again for it.
        let ready = {
            let mut svm = locker.0.write().await;
            prepare_cross_margin(&mut svm, &remote, trader, &symbols)
                .await
                .unwrap()
        };
        let mut expected = ready.writes.clone();
        writes.sort_by_key(|(pubkey, _)| *pubkey);
        expected.sort_by_key(|(pubkey, _)| *pubkey);
        assert!(
            writes == expected,
            "{trader}: the template wrote what the preparation returns"
        );
        apply(&locker, writes).await;

        for ((symbol, _), instruction) in ready.moves.iter().zip(ready.liquidations) {
            assert_eq!(
                hawkeye_margin(&locker, &traded).is_liquidatable,
                1,
                "{trader}: liquidatable before its {symbol} position goes"
            );
            let lots = position_lots(&locker, &remote, &trader, symbol).await;
            send_liquidation(&locker, instruction)
                .await
                .unwrap_or_else(|e| panic!("{trader}: liquidating {symbol} in turn failed: {e}"));
            let left = position_lots(&locker, &remote, &trader, symbol).await;
            assert!(
                left.abs() < lots.abs(),
                "{trader}: its {symbol} liquidation took {lots} base lots down to {left}"
            );
        }
        return;
    }
    panic!("no eligible mainnet candidate: {skipped:#?}");
}

/// The makers on `symbol` requote 10% below the mark while its oracle stays, and the crank
/// matches what that crossed. Returns whether the book's mid now sits at least 5% below the mark.
async fn lower_the_book(
    locker: &SurfnetSvmLocker,
    remote: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    symbol: &str,
) -> bool {
    let writes = {
        let mut svm = locker.0.write().await;
        let Ok(mut market) = MarketContext::load(&mut svm, remote, symbol).await else {
            return false;
        };
        let mark = market.market.mark_ticks;
        let Ok(mut instructions) = market.spline_moves(mark * 9 / 10) else {
            return false;
        };
        instructions.push(market.uncross().unwrap());
        match run_instructions(&svm, &instructions) {
            Ok(writes) => writes,
            Err(_) => return false,
        }
    };
    apply(locker, writes).await;
    let map = locker
        .with_svm_reader(|svm| svm.get_account(&PHOENIX_PERP_ASSET_MAP))
        .unwrap()
        .unwrap();
    let market = PerpAssetMap::try_from_account_bytes(&map.data)
        .unwrap()
        .find_by_symbol(symbol)
        .unwrap()
        .unwrap();
    let price = market.metadata.oracle_price().mark_price;
    let book = price.book_price_component;
    let mid = (book.last_best_bid.ticks.as_inner() + book.last_best_ask.ticks.as_inner()) / 2;
    mid * 100 <= price.price.ticks.as_inner() * 95
}

/// On a map 260 slots older than the Clock, with a long's book 10% under its oracle, a deep search
/// move has no positive price unless the readings are re-stamped to the Clock.
#[tokio::test(flavor = "multi_thread")]
async fn a_cross_margin_run_works_on_a_map_older_than_the_clock() {
    let mut skipped = Vec::new();
    for trader in cross_margin_candidates(&phoenix_mainnet_graph().await, 2..=6)
        .await
        .into_iter()
        .take(12)
    {
        let (locker, graph) = phoenix_behavior_locker().await;
        let remote = Some((client(), CommitmentConfig::confirmed()));
        let account = fetch(&[trader]).await.remove(0);
        locker.with_svm_writer(|svm| svm.set_account(&trader, account.clone()).unwrap());
        let traded = PhoenixMainnetGraph {
            trader,
            ..graph.clone()
        };
        if let Err(reason) = healthy(&locker, &traded) {
            skipped.push(format!("{trader}: {reason}"));
            continue;
        }
        let held = held_positions(&locker, &remote, &trader).await;
        if !(2..=6).contains(&held.len()) {
            skipped.push(format!("{trader}: Hawkeye reports {held:?}"));
            continue;
        }
        let Some((lowered, _)) = held.iter().find(|(_, lots)| *lots > 0) else {
            skipped.push(format!("{trader}: no long in {held:?}"));
            continue;
        };
        if !lower_the_book(&locker, &remote, lowered).await {
            skipped.push(format!(
                "{trader}: the {lowered} makers could not lower the book"
            ));
            continue;
        }
        let mut clock = graph.clock.clone();
        clock.slot += 260;
        clock.unix_timestamp += 104;
        locker.with_svm_writer(|svm| svm.inner.set_sysvar(&clock));
        let symbols: Vec<&str> = held.iter().map(|(symbol, _)| symbol.as_str()).collect();

        let played = {
            let mut svm = locker.0.write().await;
            prepare_phoenix_override(
                &mut svm,
                LIQUIDATION_READY_TEMPLATE_ID,
                &trader,
                &account,
                &HashMap::from([("symbols".to_string(), serde_json::json!(symbols.join(",")))]),
                &remote,
                clock.slot,
            )
            .await
        };
        let writes = match played {
            Ok(writes) => writes.expect("the template has its own writer"),
            Err(e) if e.to_string().contains("staleness or validity check failed") => {
                panic!("{trader}: Phoenix refused a mark with the {lowered} book lowered: {e}")
            }
            Err(e) => {
                skipped.push(format!("{trader} on {symbols:?}: {e}"));
                continue;
            }
        };
        apply(&locker, writes).await;
        assert_eq!(
            hawkeye_margin(&locker, &traded).is_liquidatable,
            1,
            "{trader}: liquidatable after Play"
        );
        return;
    }
    panic!("no eligible mainnet candidate: {skipped:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_load_brings_the_index_with_the_buffer_and_every_book() {
    let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let remote = Some((client(), CommitmentConfig::confirmed()));
    let exchange = Exchange::load(&mut svm, &remote).await.unwrap();

    let map = svm
        .inner
        .get_account(&exchange.perp_asset_map)
        .unwrap()
        .unwrap();
    let books: Vec<Pubkey> = phoenix_markets(exchange.perp_asset_map, &map)
        .unwrap()
        .into_iter()
        .map(|market| market.orderbook)
        .collect();
    assert!(!books.is_empty());
    for address in [exchange.global_trader_index, exchange.active_trader_buffer]
        .into_iter()
        .chain(books)
    {
        assert!(
            svm.inner.get_account(&address).unwrap().is_some(),
            "{address} did not come with the index"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn positions_of_an_address_without_a_trader_say_so() {
    // `Pubkey::new_unique` counts up from low addresses, and some of those exist on mainnet.
    let address = Keypair::new().pubkey();
    let error = trader_positions(client(), address)
        .await
        .err()
        .expect("an address with no account has no positions")
        .to_string();
    assert!(
        error.contains(&format!("there is no Phoenix Trader at {address}")),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_liquidation_ready_run_refuses_a_market_the_trader_does_not_hold() {
    let (locker, graph) = phoenix_behavior_locker().await;
    let remote = Some((client(), CommitmentConfig::confirmed()));
    // The graph's trader can hold resting orders alone, so this picks one with a position.
    let mut picked = None;
    for trader in cross_margin_candidates(&graph, 1..=4)
        .await
        .into_iter()
        .take(12)
    {
        let account = fetch(&[trader]).await.remove(0);
        locker.with_svm_writer(|svm| svm.set_account(&trader, account.clone()).unwrap());
        let held = held_positions(&locker, &remote, &trader).await;
        if !held.is_empty() {
            picked = Some((trader, account, held));
            break;
        }
    }
    let (trader, account, held) = picked.expect("a mainnet trader holds a position");
    let (holding, _) = &held[0];
    let markets = phoenix_markets(graph.perp_asset_map, graph.account(&graph.perp_asset_map))
        .expect("mainnet PerpAssetMap decodes");
    let missing = markets
        .iter()
        .map(|market| market.symbol.as_str())
        .find(|symbol| held.iter().all(|(held, _)| held != symbol))
        .expect("a market the trader does not hold");

    let error = {
        let mut svm = locker.0.write().await;
        prepare_phoenix_override(
            &mut svm,
            LIQUIDATION_READY_TEMPLATE_ID,
            &trader,
            &account,
            &HashMap::from([(
                "symbols".to_string(),
                serde_json::json!(format!("{holding},{missing}")),
            )]),
            &remote,
            graph.clock.slot,
        )
        .await
        .unwrap_err()
        .to_string()
    };
    assert!(
        error.contains(&format!("{trader} holds no {missing} position")),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trader_configuration_reaches_what_phoenix_reads() {
    let (locker, graph) = phoenix_behavior_locker().await;
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
    let refused = withdraw_one()
        .await
        .expect_err("WithdrawCollateral false blocks withdrawals")
        .to_string();
    assert!(refused.contains("Trader cannot withdraw"), "{refused}");
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
