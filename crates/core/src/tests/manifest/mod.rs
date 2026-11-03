//! Manifest tests on live mainnet markets and the deployed program. They need a network connection
//! and `--features integration-tests`; `SURFPOOL_TEST_RPC_URL` overrides the endpoint.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use base64::{Engine, prelude::BASE64_STANDARD};
use serde_json::json;
use solana_account::Account;
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use surfpool_types::{AccountAddress, DEFAULT_MAINNET_RPC_URL, OverrideInstance, Scenario};

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::manifest::v1::market::{
            GLOBAL_ORDER, MANIFEST_PROGRAM_ID, ORDER_TRADER, ORDER_TYPE, Order, SEAT_BASE,
            SEAT_QUOTE, read_book,
        },
    },
    surfnet::{locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    types::TokenAccount,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const PROGRAMDATA: Pubkey = Pubkey::from_str_const("G92J4ZFggKZ9LbsQ2zBpajRvRto7ed7uRuJJqWsX4BhJ");
const DEPLOYED_SLOT: u64 = 459_237_697;
const SOL_USDC: Pubkey = Pubkey::from_str_const("ENhU8LsaR7vDD2G1CsWcsuSGNrih9Cv5WZEk7q9kPapQ");
const USDT_USDC: Pubkey = Pubkey::from_str_const("8sjV1AqBFvFuADBCQHhotaRq5DFFYSjjg1jMyVWMqXvZ");

/// The depth cut the live tests apply: 2/5 of every size.
const CUT_PPM: &str = "400000";
const WITHDRAW_INSTRUCTION: u8 = 3;
const SWAP_INSTRUCTION: u8 = 4;
const BATCH_UPDATE_INSTRUCTION: u8 = 6;
const FILL_LOG: [u8; 8] = [58, 230, 242, 3, 75, 113, 4, 169];
const ORDER_SEQUENCE: usize = 40;
const SEAT_TRADER: usize = 16;
const HEADER_LEN: usize = 256;
const PRICE_SCALE: u128 = 1_000_000_000_000_000_000;

fn client() -> SurfnetRemoteClient {
    SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_MAINNET_RPC_URL.to_string()),
    )
}

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[Pubkey]) -> Vec<Option<Account>> {
    client()
        .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .map(|result| result.map_account().ok())
        .collect()
}

/// The deployed bytecode, read from ProgramData once per test process. The ELF starts 45 bytes
/// in, past the upgradeable loader's header, whose bytes 4..12 are the deploy slot.
async fn deployed_program() -> Arc<Vec<u8>> {
    static PROGRAM: tokio::sync::OnceCell<Arc<Vec<u8>>> = tokio::sync::OnceCell::const_new();
    PROGRAM
        .get_or_init(|| async {
            let programdata = fetch(&[PROGRAMDATA])
                .await
                .remove(0)
                .expect("the Manifest ProgramData exists");
            let deployed_slot = u64::from_le_bytes(programdata.data[4..12].try_into().unwrap());
            assert_eq!(
                deployed_slot, DEPLOYED_SLOT,
                "Manifest was redeployed at slot {deployed_slot}; revalidate the market layout \
                 and the matching rules the templates rely on, then move the pin"
            );
            Arc::new(programdata.data[45..].to_vec())
        })
        .await
        .clone()
}

struct MarketFixture {
    clock: Clock,
    market: Pubkey,
    data: Vec<u8>,
    /// Base, then quote.
    mints: [Pubkey; 2],
    vaults: [Pubkey; 2],
    accounts: Vec<(Pubkey, Account)>,
}

fn global_and_vault(mint: &Pubkey) -> [Pubkey; 2] {
    [b"global".as_slice(), b"global-vault"].map(|prefix| {
        Pubkey::find_program_address(&[prefix, mint.as_ref()], &MANIFEST_PROGRAM_ID).0
    })
}

/// A market with its mints, vaults, both Global accounts and the clock, all read at one slot.
async fn market_fixture(market: Pubkey) -> MarketFixture {
    let header = fetch(&[market])
        .await
        .remove(0)
        .unwrap_or_else(|| {
            panic!("{market} no longer exists on mainnet; the test needs a new market")
        })
        .data;
    let key = |at: usize| Pubkey::try_from(&header[at..at + 32]).unwrap();
    let (mints, vaults) = ([key(16), key(48)], [key(80), key(112)]);
    let mut keys = vec![market, solana_sdk_ids::sysvar::clock::ID];
    keys.extend(mints.iter().chain(&vaults));
    keys.extend(mints.iter().flat_map(global_and_vault));
    let mut fetched = fetch(&keys).await;
    let clock = bincode::deserialize(&fetched[1].take().expect("the clock sysvar").data).unwrap();
    let data = fetched[0].as_ref().expect("the market").data.clone();
    let accounts = keys
        .into_iter()
        .zip(fetched)
        .filter_map(|(key, account)| Some((key, account?)))
        .collect();
    MarketFixture {
        clock,
        market,
        data,
        mints,
        vaults,
        accounts,
    }
}

async fn local_vm(fixture: &MarketFixture) -> SurfnetSvmLocker {
    let program = deployed_program().await;
    let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    svm.inner.set_sysvar(&fixture.clock);
    svm.inner
        .svm
        .add_program(MANIFEST_PROGRAM_ID, &program)
        .unwrap();
    for (address, account) in &fixture.accounts {
        svm.set_account(address, account.clone()).unwrap();
    }
    SurfnetSvmLocker::new(svm)
}

fn market_data(locker: &SurfnetSvmLocker, market: &Pubkey) -> Vec<u8> {
    locker
        .with_svm_reader(|svm| svm.get_account(market))
        .unwrap()
        .expect("the market is in the local VM")
        .data
}

/// Runs one template on the fixture's market through scenario registration and the materializer.
async fn materialize(
    locker: &SurfnetSvmLocker,
    fixture: &MarketFixture,
    template: &str,
    values: serde_json::Value,
) {
    let values = serde_json::from_value(values).unwrap();
    let address = AccountAddress::Pubkey(fixture.market.to_string());
    let mut scenario = Scenario::new(template.to_string(), "Manifest live test".to_string());
    scenario
        .add_override(OverrideInstance::new(template.to_string(), 0, address).with_values(values));
    let slot = fixture.clock.slot;
    locker.register_scenario(scenario, Some(slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, slot)
        .await
        .unwrap();
}

fn token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let rent = 2_039_280;
    let native = (mint == spl_token_interface::native_mint::ID).then_some(rent);
    let mut token = TokenAccount::new(&spl_token_interface::ID, owner, mint, native);
    token.set_amount(amount);
    Account {
        lamports: rent + native.map_or(0, |_| amount),
        data: token.pack_into_vec(),
        owner: spl_token_interface::ID,
        ..Account::default()
    }
}

fn token_balance(locker: &SurfnetSvmLocker, address: &Pubkey) -> u64 {
    let account = locker.with_svm_reader(|svm| svm.get_account(address));
    TokenAccount::unpack(&account.unwrap().unwrap().data)
        .unwrap()
        .amount()
}

/// Sends `instructions` paid by `payer`, funded here, and returns the logs. Other signers do not
/// sign, since the local VM does not verify signatures.
fn send(locker: &SurfnetSvmLocker, payer: &Keypair, instructions: &[Instruction]) -> Vec<String> {
    locker.with_svm_writer(|svm| {
        svm.inner.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
        let blockhash = svm.inner.svm.latest_blockhash();
        let message = Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &blockhash);
        let mut transaction = Transaction::new_unsigned(message);
        transaction.partial_sign(&[payer], blockhash);
        svm.inner
            .send_transaction(transaction)
            .unwrap_or_else(|failed| {
                panic!("{:?} failed, logs: {:?}", failed.err, failed.meta.logs)
            })
            .logs
    })
}

struct Fill {
    price: u128,
    base_atoms: u64,
    maker_sequence: u64,
}

struct SwapOutcome {
    received: u64,
    fills: Vec<Fill>,
}

/// An exact-in Swap from a fresh taker with no seat: base for quote when `base_in`, else quote for
/// base.
fn swap(
    locker: &SurfnetSvmLocker,
    fixture: &MarketFixture,
    amount: u64,
    base_in: bool,
) -> SwapOutcome {
    let taker = Keypair::new();
    let tokens = [Pubkey::new_unique(), Pubkey::new_unique()];
    let (input, output) = if base_in { (0, 1) } else { (1, 0) };
    for (side, balance) in [(input, amount), (output, 0)] {
        let account = token_account(fixture.mints[side], taker.pubkey(), balance);
        locker.with_svm_writer(|svm| svm.set_account(&tokens[side], account).unwrap());
    }
    // The taker matches the other side, whose global orders are backed by the mint it receives.
    let [global, global_vault] = global_and_vault(&fixture.mints[output]);
    let mut data = vec![SWAP_INSTRUCTION];
    data.extend(amount.to_le_bytes());
    data.extend(0u64.to_le_bytes());
    data.extend([u8::from(base_in), 1]);
    let accounts = vec![
        AccountMeta::new(taker.pubkey(), true),
        AccountMeta::new(fixture.market, false),
        AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
        AccountMeta::new(tokens[0], false),
        AccountMeta::new(tokens[1], false),
        AccountMeta::new(fixture.vaults[0], false),
        AccountMeta::new(fixture.vaults[1], false),
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new(global, false),
        AccountMeta::new(global_vault, false),
    ];
    let instructions = [
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
        Instruction::new_with_bytes(MANIFEST_PROGRAM_ID, &data, accounts),
    ];
    let fills = send(locker, &taker, &instructions)
        .iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| BASE64_STANDARD.decode(data).ok())
        .filter(|event| event.starts_with(&FILL_LOG))
        .map(|event| Fill {
            price: u128::from_le_bytes(event[168..184].try_into().unwrap()),
            base_atoms: read_u64(&event, 184),
            maker_sequence: read_u64(&event, 200),
        })
        .collect();
    SwapOutcome {
        received: token_balance(locker, &tokens[output]),
        fills,
    }
}

fn read_u64(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// The account offset of the seat that placed `order`.
fn seat_of(data: &[u8], order: &Order) -> usize {
    let index = u32::from_le_bytes(data[order.node + ORDER_TRADER..][..4].try_into().unwrap());
    HEADER_LEN + index as usize
}

/// A seat's base and quote balances.
fn seat_balances(data: &[u8], seat: usize) -> [u64; 2] {
    [SEAT_BASE, SEAT_QUOTE].map(|balance| read_u64(data, seat + balance.start))
}

fn is_global(data: &[u8], order: &Order) -> bool {
    data[order.node + ORDER_TYPE] == GLOBAL_ORDER
}

/// The quote an order is worth, rounded up as Manifest locks and refunds a bid's quote.
fn quote(order: &Order) -> u128 {
    (order.price * u128::from(order.size)).div_ceil(PRICE_SCALE)
}

/// What a non-global order holds back: its size for an ask, its quote for a bid.
fn locked(order: &Order, is_bid: bool) -> u128 {
    if is_bid {
        quote(order)
    } else {
        u128::from(order.size)
    }
}

fn cut(size: u64) -> u64 {
    (u128::from(size) * 2 / 5).max(1) as u64
}

/// Both sides, bids first, each order with whether it is a bid.
fn book(data: &[u8]) -> Vec<(Order, bool)> {
    let (bids, asks) = read_book(data).unwrap();
    let bids = bids.into_iter().map(|bid| (bid, true));
    bids.chain(asks.into_iter().map(|ask| (ask, false)))
        .collect()
}

/// Per token, what the non-global orders lock, and that plus their makers' seat balances: what the
/// market owes those makers.
fn claims(data: &[u8]) -> ([u128; 2], [u128; 2]) {
    let mut seats = BTreeSet::new();
    let mut held = [0u128; 2];
    for (order, is_bid) in book(data) {
        if !is_global(data, &order) {
            seats.insert(seat_of(data, &order));
            held[usize::from(is_bid)] += locked(&order, is_bid);
        }
    }
    let mut owed = held;
    for seat in seats {
        for (side, balance) in seat_balances(data, seat).into_iter().enumerate() {
            owed[side] += u128::from(balance);
        }
    }
    (held, owed)
}

/// The seat's maker cancels the orders at `nodes` and withdraws its whole seat balance through the
/// deployed program. Returns the base and quote its token accounts received.
fn cancel_and_withdraw(
    locker: &SurfnetSvmLocker,
    fixture: &MarketFixture,
    seat: usize,
    nodes: &[usize],
) -> [u64; 2] {
    let market = market_data(locker, &fixture.market);
    let maker = Pubkey::try_from(&market[seat + SEAT_TRADER..][..32]).unwrap();
    let index = |at: usize| ((at - HEADER_LEN) as u32).to_le_bytes();
    let mut data = vec![BATCH_UPDATE_INSTRUCTION, 1];
    data.extend(index(seat));
    data.extend((nodes.len() as u32).to_le_bytes());
    for node in nodes {
        data.extend(&market[node + ORDER_SEQUENCE..][..8]);
        data.push(1);
        data.extend(index(*node));
    }
    data.extend(0u32.to_le_bytes());
    let accounts = vec![
        AccountMeta::new(maker, true),
        AccountMeta::new(fixture.market, false),
        AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
    ];
    let cancel = Instruction::new_with_bytes(MANIFEST_PROGRAM_ID, &data, accounts);
    send(locker, &Keypair::new(), &[cancel]);

    let balances = seat_balances(&market_data(locker, &fixture.market), seat);
    [0, 1].map(|side| {
        if balances[side] == 0 {
            return 0;
        }
        let token = Pubkey::new_unique();
        let account = token_account(fixture.mints[side], maker, 0);
        locker.with_svm_writer(|svm| svm.set_account(&token, account).unwrap());
        let mut data = vec![WITHDRAW_INSTRUCTION];
        data.extend(balances[side].to_le_bytes());
        data.push(1);
        data.extend(index(seat));
        let accounts = vec![
            AccountMeta::new(maker, true),
            AccountMeta::new(fixture.market, false),
            AccountMeta::new(token, false),
            AccountMeta::new(fixture.vaults[side], false),
            AccountMeta::new_readonly(spl_token_interface::ID, false),
            AccountMeta::new_readonly(fixture.mints[side], false),
        ];
        let withdraw = Instruction::new_with_bytes(MANIFEST_PROGRAM_ID, &data, accounts);
        send(locker, &Keypair::new(), &[withdraw]);
        token_balance(locker, &token)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn deployed_program_is_the_pinned_build() {
    let program = deployed_program().await;
    assert_eq!(&program[..4], b"\x7fELF");
}

/// The codec walks every order on both live books, and the bundled IDL round-trips each market.
#[tokio::test(flavor = "multi_thread")]
async fn live_markets_pass_the_codec_guards() {
    let registry = TemplateRegistry::new();
    let idl = registry.get("manifest-book-shift").unwrap().idl();
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    for market in [SOL_USDC, USDT_USDC] {
        let data = market_fixture(market).await.data;
        let (bids, asks) =
            read_book(&data).unwrap_or_else(|e| panic!("{market} fails the codec guards: {e}"));
        assert!(
            !bids.is_empty() && !asks.is_empty() && bids[0].price < asks[0].price,
            "{market} needs both sides, uncrossed"
        );
        let round_trip = svm.get_forged_account_data(&market, &data, idl, &HashMap::new());
        assert_eq!(round_trip.unwrap(), data, "{market}");
    }
}

/// After a 10% drop the same quote buys 10/9 of the base and the same base sells for 9/10 of the
/// quote.
#[tokio::test(flavor = "multi_thread")]
async fn the_deployed_program_trades_at_the_shifted_price() {
    for market in [SOL_USDC, USDT_USDC] {
        let fixture = market_fixture(market).await;
        let now = fixture.clock.slot as u32;
        let (bids, asks) = read_book(&fixture.data).unwrap();
        // Small enough that every swap below fills from one order, where the output is linear in
        // the price, even when a global order at the top is short of balance and skipped.
        let smallest = |orders: &[Order], amount: fn(&Order) -> u128| {
            let live = orders.iter().filter(|order| order.is_live(now)).take(3);
            live.map(amount).min().expect("live orders on both sides") / 8
        };
        let quote_in = smallest(&asks, quote).clamp(1, 1_000_000) as u64;
        let base_in = smallest(&bids, |bid| bid.size.into()).clamp(1, 10_000_000) as u64;

        // Each swap runs on its own local VM: a fill of a reverse order rests a new order on the
        // other side, which a later swap can cross.
        for (amount, sells_base, ratio) in [(quote_in, false, (10, 9)), (base_in, true, (9, 10))] {
            let before = swap(&local_vm(&fixture).await, &fixture, amount, sells_base);
            let shifted = local_vm(&fixture).await;
            let values = json!({"ratio_ppm": "900000", "side": "both"});
            materialize(&shifted, &fixture, "manifest-book-shift", values).await;
            let after = swap(&shifted, &fixture, amount, sells_base);
            assert!(
                before.fills.len() == 1 && after.fills.len() == 1,
                "{market}: a test swap crossed orders"
            );
            // Every rounding on the way (the shifted price, the base an exact quote buys, the
            // quote a fill pays) is at most one atom, so three atoms bound the difference.
            let expected = i128::from(before.received) * ratio.0 / ratio.1;
            assert!(
                (i128::from(after.received) - expected).abs() <= 3,
                "{market}: {} after a 10% drop, {} before",
                after.received,
                before.received
            );
        }
    }
}

/// In the slot the override runs in, a buy skips the expired levels, and Manifest refunds each
/// locked ask it passes to its maker's seat.
#[tokio::test(flavor = "multi_thread")]
async fn expired_asks_are_skipped_and_refunded_to_their_makers() {
    let fixture = market_fixture(SOL_USDC).await;
    let data = &fixture.data;
    let now = fixture.clock.slot as u32;
    let (_, asks) = read_book(data).unwrap();
    let mut prices: Vec<u128> = asks
        .iter()
        .filter(|ask| ask.is_live(now))
        .map(|ask| ask.price)
        .collect();
    prices.dedup();
    // The level that should fill next must hold an order backed by locked funds, since a
    // global order can be short of balance and be skipped too.
    let levels = (2..prices.len())
        .find(|&levels| {
            asks.iter()
                .any(|ask| ask.is_live(now) && ask.price == prices[levels] && !is_global(data, ask))
        })
        .expect("no eligible live candidate: SOL/USDC has no locked ask");

    let locker = local_vm(&fixture).await;
    let values = json!({"side": "asks", "levels": levels, "expire_after_slots": 0});
    materialize(&locker, &fixture, "manifest-orders-expire", values).await;
    let expired = market_data(&locker, &SOL_USDC);
    // Matching removes every expired order it passes and refunds a locked one to its maker's
    // seat, up to the first order that can fill.
    let (_, expired_asks) = read_book(&expired).unwrap();
    let mut refunds: HashMap<usize, u64> = HashMap::new();
    for ask in expired_asks.iter().take_while(|ask| !ask.is_live(now)) {
        if !is_global(&expired, ask) {
            *refunds.entry(seat_of(&expired, ask)).or_default() += ask.size;
        }
    }
    assert!(
        !refunds.is_empty(),
        "no eligible live candidate: every expired ask is a global order"
    );

    let outcome = swap(&locker, &fixture, 1_000, false);
    let first = outcome.fills.first().expect("the swap fills");
    assert!(first.base_atoms > 0);
    assert_eq!(
        first.price, prices[levels],
        "the first fill must come from the first level left after {levels} expired"
    );
    let traded = market_data(&locker, &SOL_USDC);
    for (seat, refund) in refunds {
        assert_eq!(
            seat_balances(&traded, seat)[0] - seat_balances(&expired, seat)[0],
            refund,
            "seat {seat} must get back exactly what its expired asks locked"
        );
    }
}

/// A buy worth the two best ask levels sweeps the best one. After the cut that level holds 2/5 of
/// its size, so less fills at the best price and the same quote buys less base.
#[tokio::test(flavor = "multi_thread")]
async fn depth_cut_leaves_less_to_buy_at_the_best_price() {
    let fixture = market_fixture(SOL_USDC).await;
    let now = fixture.clock.slot as u32;
    let (_, asks) = read_book(&fixture.data).unwrap();
    let asks: Vec<Order> = asks.into_iter().filter(|ask| ask.is_live(now)).collect();
    let second = asks
        .iter()
        .find(|ask| ask.price != asks[0].price)
        .expect("no eligible live candidate: two ask levels")
        .price;
    let quote_in = asks
        .iter()
        .take_while(|ask| ask.price <= second)
        .map(quote)
        .sum::<u128>() as u64;
    let baseline = swap(&local_vm(&fixture).await, &fixture, quote_in, false);
    let locker = local_vm(&fixture).await;
    let values = json!({"size_ppm": CUT_PPM, "side": "asks"});
    materialize(&locker, &fixture, "manifest-depth-cut", values).await;
    let outcome = swap(&locker, &fixture, quote_in, false);

    let best = baseline.fills[0].price;
    assert!(
        baseline.fills.iter().any(|fill| fill.price > best),
        "no eligible live candidate: the baseline buy did not sweep its best level"
    );
    // Both buys go past the best price, so every order there fills whole: in full before the cut,
    // at its cut size after.
    let at_best = |outcome: &SwapOutcome, cut_sizes: bool| -> u64 {
        let fills = outcome.fills.iter().filter(|fill| fill.price == best);
        fills
            .map(|fill| {
                let ask = asks.iter().find(|ask| {
                    read_u64(&fixture.data, ask.node + ORDER_SEQUENCE) == fill.maker_sequence
                });
                let size = ask.expect("a fill from a live ask").size;
                assert_eq!(fill.base_atoms, if cut_sizes { cut(size) } else { size });
                fill.base_atoms
            })
            .sum()
    };
    let (before_cut, after_cut) = (at_best(&baseline, false), at_best(&outcome, true));
    assert!(
        after_cut < before_cut,
        "{after_cut} base at the best price after the cut, {before_cut} before"
    );
    assert!(
        outcome.received < baseline.received,
        "{quote_in} quote bought {} base after the cut, {} before",
        outcome.received,
        baseline.received
    );
}

/// The cut moves tokens from orders to seats without creating or losing any, and a cut maker that
/// cancels and withdraws through the deployed program gets exactly what its orders locked, as
/// without the cut.
#[tokio::test(flavor = "multi_thread")]
async fn depth_cut_keeps_every_claim_and_its_refund_withdraws() {
    let fixture = market_fixture(SOL_USDC).await;
    let data = &fixture.data;
    let locker = local_vm(&fixture).await;
    let values = json!({"size_ppm": CUT_PPM, "side": "both"});
    materialize(&locker, &fixture, "manifest-depth-cut", values).await;
    let (held, owed) = claims(data);
    let (held_after, owed_after) = claims(&market_data(&locker, &SOL_USDC));
    assert_eq!(owed_after, owed, "the cut created or lost tokens");
    assert!(held_after[0] < held[0] && held_after[1] < held[1]);

    // A maker refunded on both sides, with a bid whose cut quote rounds up, and the fewest
    // locked orders, so one batch cancels them all.
    let now = fixture.clock.slot as u32;
    let mut by_seat: BTreeMap<usize, Vec<(Order, bool)>> = BTreeMap::new();
    for (order, is_bid) in book(data) {
        if order.is_live(now) && !is_global(data, &order) {
            let seat = seat_of(data, &order);
            by_seat.entry(seat).or_default().push((order, is_bid));
        }
    }
    let (seat, orders) = by_seat
        .into_iter()
        .filter(|(_, orders)| {
            orders.iter().any(|(order, is_bid)| {
                *is_bid && order.price * u128::from(cut(order.size)) % PRICE_SCALE != 0
            }) && orders
                .iter()
                .any(|(order, is_bid)| !is_bid && cut(order.size) < order.size)
        })
        .min_by_key(|(_, orders)| orders.len())
        .expect("no eligible live candidate: a maker with a rounding bid and an ask");
    let nodes: Vec<usize> = orders.iter().map(|(order, _)| order.node).collect();
    let mut expected = seat_balances(data, seat).map(u128::from);
    for (order, is_bid) in &orders {
        expected[usize::from(*is_bid)] += locked(order, *is_bid);
    }

    let with_cut = cancel_and_withdraw(&locker, &fixture, seat, &nodes);
    let without_cut = cancel_and_withdraw(&local_vm(&fixture).await, &fixture, seat, &nodes);
    assert_eq!(with_cut, without_cut);
    assert_eq!(with_cut.map(u128::from), expected);
}
