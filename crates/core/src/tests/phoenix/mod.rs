//! Phoenix Eternal integration tests.
//!
//! These fetch the real accounts from mainnet rather than embedding captured copies, so they
//! need a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests phoenix
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! What these cover that unit tests cannot: Phoenix is zero-copy, so decoding a synthetic
//! account and decoding the bytes it was built from can never disagree. Only live accounts
//! carry the real header values, populated market entries and the dynamic tail, so a program
//! upgrade that moves a field shows up here as a failed invariant or a stray byte diff.
//!
//! Nothing here hunts for a market that happens to sit in an interesting state: the live
//! accounts are the raw material, and the production builders prepare the scene.

use phoenix_rise_accounts::{
    PhoenixAccount,
    global_config::GlobalConfig,
    pda::derive_spline_collection_address,
    perp_asset_map::PerpAssetMap,
    trader::{Trader, TraderHeader},
};
use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
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

use surfpool_types::AccountAddress;

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::phoenix_eternal::v1::state_builder::{
            PHOENIX_ETERNAL_PROGRAM_ID, PHOENIX_GLOBAL_CONFIG, build_phoenix_collateral_scenario,
            forge_phoenix_override, phoenix_market_symbols, phoenix_perp_asset_map_address,
        },
    },
    surfnet::{locker::SurfnetSvmLocker, svm::SurfnetSvm},
    tests::live::{RPC_URL_ENV, client, diff_indices, fetch},
    types::RemoteRpcResult,
};

type LiveMarketGraph = (Account, Pubkey, Account, String);

/// One fetch of the fork state per process. The perp asset map alone is 1.6MB, and running the
/// tests in parallel against a public endpoint is what exhausts it.
fn market_graph_cache() -> &'static tokio::sync::Mutex<Option<LiveMarketGraph>> {
    static CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<LiveMarketGraph>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

fn trader_cache() -> &'static tokio::sync::Mutex<HashMap<bool, (Pubkey, Account)>> {
    static CACHE: std::sync::OnceLock<tokio::sync::Mutex<HashMap<bool, (Pubkey, Account)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// The live Phoenix account graph: GlobalConfig, the perp asset map it points at, and the
/// symbol of a market that is actually listed right now.
async fn live_market_graph() -> LiveMarketGraph {
    let mut cache = market_graph_cache().lock().await;
    if let Some(cached) = cache.as_ref() {
        return cached.clone();
    }
    let global_account = fetch(&[PHOENIX_GLOBAL_CONFIG]).await.remove(0);
    assert_eq!(
        global_account.owner, PHOENIX_ETERNAL_PROGRAM_ID,
        "GlobalConfig must be owned by the deployed Eternal program"
    );

    let perp_asset_map = phoenix_perp_asset_map_address(&global_account)
        .expect("live GlobalConfig should resolve its perp asset map");
    let map_account = fetch(&[perp_asset_map]).await.remove(0);
    let symbols = phoenix_market_symbols(perp_asset_map, &map_account)
        .expect("live PerpAssetMap should decode");
    let symbol = symbols
        .first()
        .cloned()
        .expect("no eligible live candidate: the Phoenix PerpAssetMap lists no markets");

    let graph = (global_account, perp_asset_map, map_account, symbol);
    *cache = Some(graph.clone());

    graph
}

/// A live Trader account with collateral on it. Traders are per-user accounts that come and go,
/// so the test discovers one through the program's own account list rather than pinning an
/// address that may be closed tomorrow.
pub(crate) async fn live_trader_with_position() -> (Pubkey, Account) {
    live_candidate(true).await
}

async fn live_trader() -> (Pubkey, Account) {
    live_candidate(false).await
}

/// Walks the program's own account list for a Trader that carries collateral, and an open
/// position when the caller needs something for a mark shock to act on.
async fn live_candidate(needs_position: bool) -> (Pubkey, Account) {
    let mut cache = trader_cache().lock().await;
    if let Some(hit) = cache.get(&needs_position) {
        return hit.clone();
    }
    let listed = client()
        .get_program_accounts(
            &PHOENIX_ETERNAL_PROGRAM_ID,
            RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..RpcAccountInfoConfig::default()
            },
            Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                0,
                &PhoenixAccount::Trader.discriminant(),
            ))]),
        )
        .await;
    let candidates = match listed {
        Ok(RemoteRpcResult::Ok(accounts)) => accounts,
        // The protocol keeps its own trader index, but 0.3.4 exposes only the arena
        // metadata, so the program's account list is the reader we have.
        Ok(RemoteRpcResult::MethodNotSupported) => panic!(
            "environment: this endpoint does not support getProgramAccounts, which these tests \
             need to find a live trader. Set {RPC_URL_ENV} to an endpoint that supports it. \
             Nothing is proven or disproven about the integration."
        ),
        Err(error) => panic!("failed to list live Phoenix traders: {error}"),
    };

    for (pubkey, account) in candidates {
        let account = account.to_account().expect("live Trader account decodes");
        let Ok(trader) = Trader::try_from_account_bytes(&account.data) else {
            continue;
        };
        let has_collateral = trader.header.trader_state.quote_lot_collateral.as_inner() > 0;
        // A downward mark shock only threatens a long, so the risk scenarios need one:
        // the shock direction is fixed, the trader is what we go looking for.
        let holds_a_long = trader
            .positions()
            .any(|(_, position)| position.base_lot_position().as_inner() > 0);
        if has_collateral
            && (!needs_position || (holds_a_long && trader.header.trader_state.is_hot()))
        {
            cache.insert(needs_position, (pubkey, account.clone()));
            return (pubkey, account);
        }
    }

    panic!(
        "no eligible live candidate: no live Phoenix Trader read carries collateral{}",
        if needs_position {
            " and is hot with a long position"
        } else {
            ""
        }
    )
}

/// A zero-copy layout cannot be round-tripped against itself, so drift shows up as an
/// invariant that stops holding on live bytes.
#[tokio::test(flavor = "multi_thread")]
async fn live_accounts_satisfy_the_typed_layout_invariants() {
    let (global_account, perp_asset_map, map_account, symbol) = live_market_graph().await;

    let global = GlobalConfig::try_from_account_bytes(&global_account.data)
        .expect("live GlobalConfig should decode through phoenix-rise-accounts");
    assert_eq!(
        Pubkey::new_from_array(global.account_key()),
        PHOENIX_GLOBAL_CONFIG,
        "GlobalConfig stores its own address, so a moved field shows up here first"
    );
    assert_eq!(
        Pubkey::new_from_array(global.perp_asset_map_key()),
        perp_asset_map
    );
    assert_ne!(
        Pubkey::new_from_array(global.global_trader_index_header_key()),
        Pubkey::default()
    );
    assert_ne!(
        Pubkey::new_from_array(global.active_trader_buffer_header_key()),
        Pubkey::default()
    );

    assert_eq!(map_account.owner, PHOENIX_ETERNAL_PROGRAM_ID);
    let map = PerpAssetMap::try_from_account_bytes(&map_account.data)
        .expect("live PerpAssetMap should decode through phoenix-rise-accounts");
    let entry = map
        .find_by_symbol(&symbol)
        .expect("symbol lookup should decode")
        .expect("the symbol came from this map");
    let market = Pubkey::new_from_array(entry.metadata.static_market_params().market_account);
    assert_ne!(
        market,
        Pubkey::default(),
        "a listed market needs an address"
    );

    let price = entry.metadata.oracle_price();
    assert!(
        price.mark_price.price.ticks.as_inner() > 0,
        "{symbol} is listed with a zero mark price, which the risk engine cannot use"
    );

    // The spline address is derived, so a change in the seeds surfaces as an account the
    // program would no longer find.
    let spline = derive_spline_collection_address(&PHOENIX_ETERNAL_PROGRAM_ID, &market);
    let spline_account = fetch(&[spline]).await.remove(0);
    assert_eq!(
        spline_account.owner, PHOENIX_ETERNAL_PROGRAM_ID,
        "the derived spline collection must belong to the Eternal program"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_on_live_accounts_touch_only_their_target_bytes() {
    let (_global_account, perp_asset_map, map_account, symbol) = live_market_graph().await;

    let map = PerpAssetMap::try_from_account_bytes(&map_account.data).expect("live map decodes");
    let entry = map
        .find_by_symbol(&symbol)
        .expect("symbol lookup")
        .expect("listed symbol");
    let live_mark = entry
        .metadata
        .oracle_price()
        .mark_price
        .price
        .ticks
        .as_inner();

    let materialization_slot = entry.metadata.oracle_price().mark_price.price.slot + 1;
    let shocked = forge_phoenix_override(
        &perp_asset_map,
        &map_account,
        &HashMap::from([
            ("symbol".to_string(), serde_json::json!(symbol)),
            (
                "target_ticks".to_string(),
                serde_json::json!((live_mark / 2 + 1).to_string()),
            ),
        ]),
        materialization_slot,
    )
    .expect("direct mark patch on the live map");
    assert_eq!(
        shocked.len(),
        map_account.data.len(),
        "the map's dynamic tail must survive"
    );
    let mark_diffs = diff_indices(&shocked, &map_account.data);
    assert!(
        !mark_diffs.is_empty() && mark_diffs.len() <= 16,
        "a mark shock writes its ticks and slot, got {} changed bytes",
        mark_diffs.len()
    );

    let diverged = forge_phoenix_override(
        &perp_asset_map,
        &map_account,
        &HashMap::from([
            ("symbol".to_string(), serde_json::json!(symbol)),
            (
                "spot_ticks".to_string(),
                serde_json::json!((live_mark * 2).to_string()),
            ),
            (
                "perp_ticks".to_string(),
                serde_json::json!((live_mark * 3).to_string()),
            ),
        ]),
        materialization_slot,
    )
    .expect("reference price patch on the live map");
    assert_eq!(diverged.len(), map_account.data.len());
    let reference_diffs = diff_indices(&diverged, &map_account.data);
    assert!(
        reference_diffs
            .iter()
            .all(|index| !mark_diffs.contains(index)),
        "reference divergence must preserve the mark price it diverges from"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn collateral_idl_override_preserves_live_trader_layout() {
    let (trader, account) = live_trader().await;

    let idl: anchor_lang_idl::types::Idl =
        serde_json::from_str(crate::scenarios::registry::PHOENIX_ETERNAL_IDL_CONTENT)
            .expect("phoenix idl parses as an anchor idl");
    let (svm, _events_rx, _geyser_rx) =
        SurfnetSvm::new(crate::surfnet::svm::SurfnetSvmConfig::default()).unwrap();

    let target: i64 = 12_345;
    let mut overrides = HashMap::new();
    overrides.insert(
        "traderState.quoteLotCollateral".to_string(),
        crate::scenarios::protocols::phoenix_eternal::v1::collateral::collateral_override_value(
            &serde_json::Value::String(target.to_string()),
        )
        .unwrap(),
    );

    let forged = svm
        .get_forged_account_data(&trader, &account.data, &idl, &overrides)
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

#[tokio::test(flavor = "multi_thread")]
async fn the_market_templates_address_the_live_perp_asset_map() {
    let (_global_account, perp_asset_map, _map_account, _symbol) = live_market_graph().await;
    let registry = TemplateRegistry::new();

    for template_id in [
        "phoenix-direct-mark-risk-shock",
        "phoenix-reference-price-divergence",
    ] {
        let template = registry
            .get(template_id)
            .unwrap_or_else(|| panic!("{template_id} should be registered"));
        let addressed = match &template.address {
            AccountAddress::Pubkey(value) => Pubkey::from_str_const(value),
            other => panic!("{template_id} should address a fixed pubkey, got {other:?}"),
        };
        assert_eq!(
            addressed, perp_asset_map,
            "{template_id} writes to a hardcoded map; a Phoenix migration moves the one \
             GlobalConfig points at, and nothing else here would notice"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn collateral_stress_refuses_to_outrun_the_live_vault() {
    let (trader, account) = live_trader().await;
    let global = fetch(&[PHOENIX_GLOBAL_CONFIG]).await.remove(0);
    let index_key = crate::scenarios::protocols::phoenix_eternal::v1::state_builder::phoenix_global_trader_index_address(&global).unwrap();
    let index = fetch(&[index_key]).await.remove(0);
    let header = TraderHeader::try_read_from_account_bytes(&account.data).unwrap();
    let live_collateral =
        crate::scenarios::protocols::phoenix_eternal::v1::collateral::effective_collateral(
            &header,
            Some(&index),
        )
        .unwrap();

    let lowered = build_phoenix_collateral_scenario(trader, &account, "1", Some(&index))
        .expect("lowering collateral is state preparation");
    assert_eq!(
        lowered.overrides[0].values["traderState.quoteLotCollateral"],
        "1"
    );

    let raised = build_phoenix_collateral_scenario(
        trader,
        &account,
        &(live_collateral + 1).to_string(),
        Some(&index),
    )
    .unwrap_err();
    assert!(
        raised.to_string().contains("can only lower collateral"),
        "raising collateral past its vault backing must be refused, got: {raised}"
    );
}

const HAWKEYE_VIEW_MARGIN_DISCRIMINANT: [u8; 8] = [0xb2, 0x0a, 0x7c, 0xad, 0xec, 0xd2, 0x75, 0x06];
const HAWKEYE_VIEW_BBO_DISCRIMINANT: [u8; 8] = [0x37, 0x5f, 0x23, 0x2d, 0x53, 0xaf, 0x12, 0x52];
const ETERNAL_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("B5ayDaz9HegiNZqYeBtcFqfZBVSGwjB2CJgHshoSfMQg");
const HAWKEYE_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("Gv1WgG864CQqF5vedJVbpnhpRpRbTW1A7SyARzSw9B4Y");

/// The deployed bytecode, read from the upgradeable loader's ProgramData account. The ELF
/// starts 45 bytes in, past the loader's own header.
async fn deployed_program(programdata: Pubkey, name: &str) -> Vec<u8> {
    let cache = std::env::temp_dir().join(format!("surfpool-phoenix-{name}.so"));
    match std::fs::read(&cache) {
        Ok(bytes) if bytes.len() > 200_000 => bytes,
        _ => {
            let bytes = fetch(&[programdata]).await.remove(0).data[45..].to_vec();
            let _ = std::fs::write(&cache, &bytes);
            bytes
        }
    }
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
    // Collateral stress produces the risk condition: a trader with an open position and
    // almost no collateral is liquidatable whichever way the position points.
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
    // The program itself says how much collateral this trader's positions require, so the
    // stress target is derived from live state rather than picked.
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

    // The cascade prepares the same collateral at slot 0 and a mark shock at slot 1. What
    // the deployed program reads is asserted; whether this particular position liquidates
    // depends on its side, which the discovery does not choose.
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
    // The cascade is the two templates across slots: the collateral tool's scenario at slot 0
    // and the mark shock at slot 1, which is what a user composes in the editor.
    let mut cascade = build_phoenix_collateral_scenario(
        graph.trader,
        &trader_account,
        &prepared_collateral.to_string(),
        Some(&index_account),
    )
    .unwrap();
    let mut shock = phoenix_direct_mark_scenario(graph.perp_asset_map, &symbol, "1", false)
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

    // The second live market proves the preparations are not market-specific.
    let (second_locker, graph) = phoenix_behavior_locker().await;
    let (symbol, orderbook, spline) = graph.markets[1].clone();
    let before_second = hawkeye_bbo_for_market(&graph, &second_locker, orderbook, spline);
    second_locker
        .register_scenario(
            phoenix_direct_mark_scenario(graph.perp_asset_map, &symbol, "1", false),
            Some(graph.clock.slot),
        )
        .unwrap();
    second_locker
        .materialize_overrides_for_slot(&None, graph.clock.slot)
        .await
        .unwrap();
    let after_second = hawkeye_bbo_for_market(&graph, &second_locker, orderbook, spline);
    assert_ne!(before_second.mark_price_ticks, 1);
    assert_eq!(after_second.mark_price_ticks, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn reference_prices_change_hawkeye_index_on_live_markets() {
    let graph = phoenix_live_graph().await;
    for (symbol, orderbook, spline) in &graph.markets {
        let (locker, _) = phoenix_behavior_locker().await;
        let before = hawkeye_bbo_for_market(&graph, &locker, *orderbook, *spline);
        locker
            .register_scenario(
                phoenix_reference_price_scenario(
                    graph.perp_asset_map,
                    symbol,
                    "80000",
                    "79000",
                    false,
                ),
                Some(graph.clock.slot),
            )
            .unwrap();
        locker
            .materialize_overrides_for_slot(&None, graph.clock.slot)
            .await
            .unwrap();
        let after = hawkeye_bbo_for_market(&graph, &locker, *orderbook, *spline);
        println!(
            "{symbol}: mark {} -> {}, index {} -> {} (target spot 80000, perp 79000)",
            before.mark_price_ticks,
            after.mark_price_ticks,
            before.index_price_ticks,
            after.index_price_ticks
        );
        assert_eq!(after.mark_price_ticks, before.mark_price_ticks, "{symbol}");
        assert_eq!(after.index_price_ticks, 80000, "{symbol}");
    }
}

/// A surfnet holding the live Phoenix account graph and a discovered live trader, with the
/// two SBF programs loaded. Nothing here looks for a market or trader in an interesting
/// state: the collateral stress below produces the risk condition the assertions check.
async fn phoenix_behavior_locker() -> (SurfnetSvmLocker, PhoenixLiveGraph) {
    let eternal_program = deployed_program(ETERNAL_PROGRAMDATA, "eternal").await;
    let hawkeye_program = deployed_program(HAWKEYE_PROGRAMDATA, "hawkeye").await;
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

    let (trader, _) = live_trader_with_position().await;
    addresses.push(trader);
    addresses.push(Pubkey::from_str_const(
        "SysvarC1ock11111111111111111111111111111111",
    ));
    // Read the clock and all dependencies from one bank, after address discovery.
    let mut fresh = fetch(&addresses).await;
    let clock: Clock = bincode::deserialize(&fresh.pop().unwrap().data).unwrap();
    assert!(clock.slot > 0);
    let accounts = addresses.into_iter().zip(fresh).collect();
    println!(
        "Phoenix live fork: slot={}, unix_timestamp={}",
        clock.slot, clock.unix_timestamp
    );

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

/// The live accounts a Phoenix behavioral run needs, with the addresses the Hawkeye
/// margin view expects to be passed alongside them.
#[derive(Clone)]
struct PhoenixLiveGraph {
    clock: Clock,
    accounts: Vec<(Pubkey, Account)>,
    global_trader_index: Pubkey,
    active_trader_buffer: Pubkey,
    perp_asset_map: Pubkey,
    trader: Pubkey,
    /// Two live markets, as symbol plus the orderbook and spline accounts the
    /// Hawkeye reader wants for it.
    markets: Vec<(String, Pubkey, Pubkey)>,
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

fn phoenix_collateral_scenario(
    trader: Pubkey,
    collateral: serde_json::Value,
    fetch_before_use: bool,
) -> surfpool_types::Scenario {
    let mut scenario = surfpool_types::Scenario::new(
        "Phoenix collateral stress".to_string(),
        "Phoenix Trader collateral override".to_string(),
    );
    let mut instance = surfpool_types::OverrideInstance::new(
        "phoenix-trader-collateral-stress".to_string(),
        0,
        surfpool_types::AccountAddress::Pubkey(trader.to_string()),
    )
    .with_values(HashMap::from([(
        "traderState.quoteLotCollateral".to_string(),
        collateral,
    )]));
    instance.fetch_before_use = fetch_before_use;
    scenario.add_override(instance);
    scenario
}

fn phoenix_direct_mark_scenario(
    perp_asset_map: Pubkey,
    symbol: &str,
    target_ticks: &str,
    fetch_before_use: bool,
) -> surfpool_types::Scenario {
    let mut scenario = surfpool_types::Scenario::new(
        "Phoenix direct mark risk shock".to_string(),
        "Phoenix direct mark override".to_string(),
    );
    let mut instance = surfpool_types::OverrideInstance::new(
        "phoenix-direct-mark-risk-shock".to_string(),
        0,
        surfpool_types::AccountAddress::Pubkey(perp_asset_map.to_string()),
    )
    .with_values(HashMap::from([
        ("symbol".to_string(), serde_json::json!(symbol)),
        ("target_ticks".to_string(), serde_json::json!(target_ticks)),
    ]));
    instance.fetch_before_use = fetch_before_use;
    scenario.add_override(instance);
    scenario
}

fn phoenix_reference_price_scenario(
    perp_asset_map: Pubkey,
    symbol: &str,
    spot_ticks: &str,
    perp_ticks: &str,
    fetch_before_use: bool,
) -> surfpool_types::Scenario {
    let mut scenario = surfpool_types::Scenario::new(
        "Phoenix spot/perp reference divergence".to_string(),
        "Phoenix reference-price override".to_string(),
    );
    let mut instance = surfpool_types::OverrideInstance::new(
        "phoenix-reference-price-divergence".to_string(),
        0,
        surfpool_types::AccountAddress::Pubkey(perp_asset_map.to_string()),
    )
    .with_values(HashMap::from([
        ("symbol".to_string(), serde_json::json!(symbol)),
        ("spot_ticks".to_string(), serde_json::json!(spot_ticks)),
        ("perp_ticks".to_string(), serde_json::json!(perp_ticks)),
    ]));
    instance.fetch_before_use = fetch_before_use;
    scenario.add_override(instance);
    scenario
}

#[tokio::test(flavor = "multi_thread")]
async fn materialize_patches_only_phoenix_trader_collateral() {
    let trader = Pubkey::new_unique();
    let base = phoenix_trader_fixture(6_996_825_500);
    let scenario = phoenix_collateral_scenario(trader, serde_json::json!("371499999"), false);
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm_writer| {
        svm_writer
            .set_account(
                &trader,
                Account {
                    lamports: 1,
                    data: base.clone(),
                    owner: PHOENIX_ETERNAL_PROGRAM_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    });

    locker.register_scenario(scenario, Some(100)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 100)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm_reader| svm_reader.get_account(&trader))
        .unwrap()
        .unwrap();
    let header = TraderHeader::try_read_from_account_bytes(&after.data).unwrap();
    assert_eq!(
        header.trader_state.quote_lot_collateral.as_inner(),
        371_499_999
    );
    assert_eq!(after.data.len(), base.len());
    assert!(
        base.iter()
            .zip(&after.data)
            .enumerate()
            .filter(|(_, (before, after))| before != after)
            .all(|(offset, _)| (88..96).contains(&offset))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn materialize_applies_a_phoenix_direct_mark_override() {
    let perp_asset_map = Pubkey::new_unique();
    let base = crate::scenarios::protocols::phoenix_eternal::v1::state_builder::tests::perp_asset_map_fixture();
    let scenario = phoenix_direct_mark_scenario(perp_asset_map, "SOL", "1", false);
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm_writer| {
        svm_writer
            .set_account(
                &perp_asset_map,
                Account {
                    lamports: 1,
                    data: base.clone(),
                    owner: PHOENIX_ETERNAL_PROGRAM_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    });

    locker.register_scenario(scenario, Some(100)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 100)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm_reader| svm_reader.get_account(&perp_asset_map))
        .unwrap()
        .unwrap();
    let map = PerpAssetMap::try_from_account_bytes(&after.data).unwrap();
    let mark_price = map
        .find_by_symbol("SOL")
        .unwrap()
        .unwrap()
        .metadata
        .oracle_price()
        .mark_price
        .price;
    assert_eq!(mark_price.ticks.as_inner(), 1);
    assert_eq!(mark_price.slot, 100);
    assert_eq!(after.data.len(), base.len());
    assert!(
        base.iter()
            .zip(&after.data)
            .filter(|(before, after)| before != after)
            .count()
            <= 16
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn materialize_refreshes_phoenix_reference_price_slots() {
    let perp_asset_map = Pubkey::new_unique();
    let base = crate::scenarios::protocols::phoenix_eternal::v1::state_builder::tests::perp_asset_map_fixture();
    let before = PerpAssetMap::try_from_account_bytes(&base).unwrap();
    let before_mark = before
        .find_by_symbol("SOL")
        .unwrap()
        .unwrap()
        .metadata
        .oracle_price()
        .mark_price
        .price
        .ticks
        .as_inner();
    let scenario = phoenix_reference_price_scenario(perp_asset_map, "SOL", "8000", "7000", false);
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm_writer| {
        svm_writer
            .set_account(
                &perp_asset_map,
                Account {
                    lamports: 1,
                    data: base,
                    owner: PHOENIX_ETERNAL_PROGRAM_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    });

    locker.register_scenario(scenario, Some(100)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 100)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm_reader| svm_reader.get_account(&perp_asset_map))
        .unwrap()
        .unwrap();
    let map = PerpAssetMap::try_from_account_bytes(&after.data).unwrap();
    let entry = map.find_by_symbol("SOL").unwrap().unwrap();
    let price = entry.metadata.oracle_price();

    assert_eq!(price.mark_price.price.ticks.as_inner(), before_mark);
    assert!(
        price
            .mark_price
            .spot_price_component
            .last_exchange_spot_price
            .iter()
            .all(|value| value.slot == 100 && value.ticks.as_inner() == 8_000)
    );
    assert!(
        price
            .mark_price
            .perp_price_component
            .last_exchange_perp_price
            .iter()
            .all(|value| value.slot == 100 && value.ticks.as_inner() == 7_000)
    );
}

fn phoenix_trader_fixture(collateral: i64) -> Vec<u8> {
    let header_len = core::mem::size_of::<TraderHeader>();
    let mut data = vec![0_u8; header_len + 16 + 80];
    data[..8].copy_from_slice(&PhoenixAccount::Trader.discriminant());
    data[88..96].copy_from_slice(&collateral.to_le_bytes());
    data[112..116].copy_from_slice(&2_u32.to_le_bytes());
    data[header_len..header_len + 8].copy_from_slice(&1_u64.to_le_bytes());
    data[header_len + 8..header_len + 16].copy_from_slice(&2_u64.to_le_bytes());
    data[header_len + 16..header_len + 24].copy_from_slice(&42_u64.to_le_bytes());
    data
}
