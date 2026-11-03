use std::{collections::HashMap, ops::Range, str::FromStr};

use solana_account::Account;
use solana_pubkey::Pubkey;

pub const MANIFEST_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms");
pub const MARKET_DISCRIMINATOR: [u8; 8] = [240, 175, 97, 143, 147, 159, 113, 67];

const RATIO_FIELD: &str = "ratio_ppm";
const SIDE_FIELD: &str = "side";
const LEVELS_FIELD: &str = "levels";
const EXPIRE_FIELD: &str = "expire_after_slots";
const SIZE_FIELD: &str = "size_ppm";

const HEADER_LEN: usize = 256;
const NODE_LEN: usize = 80;
const NIL: u32 = u32::MAX;
const SEAT_NODE: u8 = 1;
const ORDER_NODE: u8 = 2;
const PPM: u128 = 1_000_000;
/// Prices are quote atoms per base atom, times 10^18.
const PRICE_SCALE: u128 = 1_000_000_000_000_000_000;

const ALLOCATED_AT: usize = 152;
const BIDS: (usize, usize) = (156, 160);
const ASKS: (usize, usize) = (164, 168);

const RIGHT_CHILD: usize = 4;
const PAYLOAD_TYPE: usize = 13;
const ORDER_PRICE: Range<usize> = 16..32;
const ORDER_SIZE: Range<usize> = 32..40;
pub(crate) const ORDER_TRADER: usize = 48;
const ORDER_LAST_VALID_SLOT: Range<usize> = 52..56;
const ORDER_IS_BID: usize = 56;
pub(crate) const ORDER_TYPE: usize = 57;
pub(crate) const GLOBAL_ORDER: u8 = 3;
/// Limit, post-only, reverse and reverse-tight orders lock their backing in the market. An
/// immediate-or-cancel order never rests.
const LOCKING_ORDERS: [u8; 4] = [0, 2, 4, 5];
pub(crate) const SEAT_BASE: Range<usize> = 48..56;
pub(crate) const SEAT_QUOTE: Range<usize> = 56..64;

/// A resting order; `node` is the account offset of its tree node.
pub(crate) struct Order {
    pub node: usize,
    pub price: u128,
    pub size: u64,
    pub last_valid_slot: u32,
}

impl Order {
    /// Manifest drops an order once `last_valid_slot < clock slot`, and 0 never expires.
    pub(crate) fn is_live(&self, now: u32) -> bool {
        self.size > 0 && (self.last_valid_slot == 0 || self.last_valid_slot >= now)
    }
}

#[derive(Clone, Copy)]
enum Side {
    Both,
    Bids,
    Asks,
}

impl Side {
    fn from_values(values: &HashMap<String, serde_json::Value>) -> Result<Self, String> {
        match values.get(SIDE_FIELD).and_then(serde_json::Value::as_str) {
            Some("both") => Ok(Self::Both),
            Some("bids") => Ok(Self::Bids),
            Some("asks") => Ok(Self::Asks),
            _ => Err(format!("{SIDE_FIELD} must be both, bids or asks")),
        }
    }

    fn includes(self, is_bid: bool) -> bool {
        matches!(
            (self, is_bid),
            (Self::Both, _) | (Self::Bids, true) | (Self::Asks, false)
        )
    }
}

/// The new bytes for a Manifest market override, or `None` when the account is not a Manifest
/// market or the values carry none of `ratio_ppm`, `expire_after_slots` and `size_ppm`. Every
/// other key is ignored. `clock_slot` is the slot Manifest checks order expiry against.
pub fn prepare_manifest_override(
    account: &Account,
    values: &HashMap<String, serde_json::Value>,
    clock_slot: u64,
) -> Result<Option<Vec<u8>>, String> {
    if account.owner != MANIFEST_PROGRAM_ID || !account.data.starts_with(&MARKET_DISCRIMINATOR) {
        return Ok(None);
    }
    let mut data = account.data.clone();
    match [RATIO_FIELD, EXPIRE_FIELD, SIZE_FIELD].map(|field| values.get(field)) {
        [Some(ratio), None, None] => shift_book(
            &mut data,
            parse(ratio, RATIO_FIELD)?,
            Side::from_values(values)?,
        )?,
        [None, None, Some(size)] => cut_depth(
            &mut data,
            parse(size, SIZE_FIELD)?,
            Side::from_values(values)?,
            // Manifest truncates the slot the same way.
            clock_slot as u32,
        )?,
        [None, Some(slots), None] => {
            // Manifest matches an order up to and including its last valid slot, so 0 slots ends
            // it before the clock's slot.
            let last_valid_slot = clock_slot
                .checked_add(parse(slots, EXPIRE_FIELD)?)
                .and_then(|slot| slot.checked_sub(1))
                .and_then(|slot| u32::try_from(slot).ok())
                .filter(|slot| *slot != 0)
                .ok_or_else(|| {
                    format!(
                        "{EXPIRE_FIELD} must keep the expiry slot between 1 and {}",
                        u32::MAX
                    )
                })?;
            let levels = values
                .get(LEVELS_FIELD)
                .ok_or_else(|| format!("{LEVELS_FIELD} is required with {EXPIRE_FIELD}"))?;
            expire_orders(
                &mut data,
                Side::from_values(values)?,
                parse(levels, LEVELS_FIELD)?,
                last_valid_slot,
                // Manifest truncates the slot the same way.
                clock_slot as u32,
            )?
        }
        [None, None, None] => return Ok(None),
        _ => {
            return Err(format!(
                "a Manifest override takes one of {RATIO_FIELD}, {EXPIRE_FIELD} or {SIZE_FIELD}"
            ));
        }
    }
    Ok(Some(data))
}

fn parse<T: FromStr>(value: &serde_json::Value, field: &str) -> Result<T, String> {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) if number.is_u64() => number.to_string(),
        _ => String::new(),
    };
    text.parse()
        .map_err(|_| format!("{field} must be a whole number, as a decimal string or a number"))
}

fn read<const N: usize>(data: &[u8], at: usize) -> [u8; N] {
    data[at..at + N].try_into().unwrap()
}

/// Bids and asks, best first, after checking the market's length and both order trees.
pub(crate) fn read_book(data: &[u8]) -> Result<(Vec<Order>, Vec<Order>), String> {
    if data.len() < HEADER_LEN {
        return Err(format!("a Manifest market is at least {HEADER_LEN} bytes"));
    }
    let allocated = u32::from_le_bytes(read(data, ALLOCATED_AT)) as usize;
    if data.len() != HEADER_LEN + allocated {
        return Err(format!(
            "the market is {} bytes but its header allocates {}",
            data.len(),
            HEADER_LEN + allocated
        ));
    }
    let mut nodes_left = allocated / NODE_LEN;
    let bids = walk_side(data, BIDS, true, &mut nodes_left)?;
    let asks = walk_side(data, ASKS, false, &mut nodes_left)?;
    Ok((bids, asks))
}

/// Walks one order tree from its maximum down, the order matching takes.
fn walk_side(
    data: &[u8],
    (root_at, best_at): (usize, usize),
    is_bid: bool,
    nodes_left: &mut usize,
) -> Result<Vec<Order>, String> {
    let side = if is_bid { "bid" } else { "ask" };
    let mut orders: Vec<Order> = Vec::new();
    let mut stack = Vec::new();
    let mut next = u32::from_le_bytes(read(data, root_at));
    loop {
        while next != NIL {
            let node = HEADER_LEN + next as usize;
            if !(next as usize).is_multiple_of(NODE_LEN) || node + NODE_LEN > data.len() {
                return Err(format!(
                    "{side} node index {next} is not a block of this market"
                ));
            }
            *nodes_left = nodes_left
                .checked_sub(1)
                .ok_or("the order trees hold more nodes than the market allocates")?;
            stack.push(node);
            next = u32::from_le_bytes(read(data, node + RIGHT_CHILD));
        }
        let Some(node) = stack.pop() else {
            break;
        };
        if data[node + PAYLOAD_TYPE] != ORDER_NODE || data[node + ORDER_IS_BID] != u8::from(is_bid)
        {
            return Err(format!("the {side} tree holds a node of another kind"));
        }
        let order = Order {
            node,
            price: u128::from_le_bytes(read(data, node + ORDER_PRICE.start)),
            size: u64::from_le_bytes(read(data, node + ORDER_SIZE.start)),
            last_valid_slot: u32::from_le_bytes(read(data, node + ORDER_LAST_VALID_SLOT.start)),
        };
        let unsorted = orders.last().is_some_and(|better| {
            if is_bid {
                better.price < order.price
            } else {
                better.price > order.price
            }
        });
        if unsorted {
            return Err(format!("the {side} tree is not sorted by price"));
        }
        orders.push(order);
        next = u32::from_le_bytes(read(data, node));
    }
    let top = orders
        .first()
        .map_or(NIL, |order| (order.node - HEADER_LEN) as u32);
    if u32::from_le_bytes(read(data, best_at)) != top {
        return Err(format!(
            "the market's best {side} is not the top of its tree"
        ));
    }
    Ok(orders)
}

/// Every price moves the same way, so both trees stay sorted where they are.
fn shift_book(data: &mut [u8], ratio_ppm: u128, side: Side) -> Result<(), String> {
    if ratio_ppm == 0 {
        return Err(format!("{RATIO_FIELD} must be above 0"));
    }
    let (bids, asks) = read_book(data)?;
    let mut writes = Vec::new();
    let mut tops = [None, None];
    for ((orders, is_bid), top) in [(&bids, true), (&asks, false)].into_iter().zip(&mut tops) {
        if !side.includes(is_bid) {
            *top = orders.first().map(|order| (order.price, order.price));
            continue;
        }
        for order in orders {
            let (price, size) = shifted(order, ratio_ppm, is_bid)?;
            top.get_or_insert((order.price, price));
            writes.push((order.node, price, size));
        }
    }
    if writes.is_empty() {
        return Err("the chosen side has no orders".to_string());
    }
    if let [Some((bid, new_bid)), Some((ask, new_ask))] = tops
        && bid < ask
        && new_bid >= new_ask
    {
        return Err("the shift would cross the book".to_string());
    }
    for (node, price, size) in writes {
        data[node + ORDER_PRICE.start..node + ORDER_PRICE.end]
            .copy_from_slice(&price.to_le_bytes());
        data[node + ORDER_SIZE.start..node + ORDER_SIZE.end].copy_from_slice(&size.to_le_bytes());
    }
    Ok(())
}

/// Bids round down and asks round up, so a shift of both sides cannot cross the book. A bid's
/// size is divided by the ratio, so it never locks more quote than its maker deposited; a bid
/// too small to keep any size refuses the shift, since Manifest never rests an empty order.
fn shifted(order: &Order, ratio_ppm: u128, is_bid: bool) -> Result<(u128, u64), String> {
    let scaled = order
        .price
        .checked_mul(ratio_ppm)
        .ok_or("the shift overflows a price")?;
    let (price, size) = if is_bid {
        let size = u128::from(order.size) * PPM / ratio_ppm;
        if size == 0 {
            return Err(format!(
                "the shift takes a bid of {} at price {} to zero size; shift the asks only or use \
                 a smaller ratio",
                order.size, order.price
            ));
        }
        (
            scaled / PPM,
            u64::try_from(size).map_err(|_| "the shift overflows a bid's size")?,
        )
    } else {
        (scaled.div_ceil(PPM), order.size)
    };
    if price == 0 {
        return Err("the shift takes a price to zero".to_string());
    }
    quote_for(price, size)?;
    Ok((price, size))
}

/// The quote Manifest charges a bid when it rests and refunds when it is cancelled, rounded up.
fn quote_for(price: u128, size: u64) -> Result<u64, String> {
    price
        .checked_mul(u128::from(size))
        .and_then(|quote| u64::try_from(quote.div_ceil(PRICE_SCALE)).ok())
        .ok_or_else(|| "an order's quote amount would not fit in 64 bits".to_string())
}

/// Every live order keeps its price and shrinks to `size * size_ppm / 10^6`, rounded down but
/// never to zero, since Manifest never rests an empty order. What an order no longer locks goes
/// back to its maker's seat as a cancel would refund it; a global order locks nothing here.
fn cut_depth(data: &mut [u8], size_ppm: u128, side: Side, now: u32) -> Result<(), String> {
    if size_ppm == 0 || size_ppm > PPM {
        return Err(format!("{SIZE_FIELD} must be between 1 and {PPM}"));
    }
    let (bids, asks) = read_book(data)?;
    let mut sizes = Vec::new();
    let mut refunds: HashMap<usize, u128> = HashMap::new();
    for (orders, is_bid) in [(&bids, true), (&asks, false)] {
        if !side.includes(is_bid) {
            continue;
        }
        for order in orders.iter().filter(|order| order.is_live(now)) {
            let size = (u128::from(order.size) * size_ppm / PPM).max(1) as u64;
            sizes.push((order.node, size));
            let order_type = data[order.node + ORDER_TYPE];
            if order_type == GLOBAL_ORDER {
                continue;
            }
            if !LOCKING_ORDERS.contains(&order_type) {
                return Err(format!(
                    "an order has type {order_type}, which Manifest never rests"
                ));
            }
            let refund = if is_bid {
                quote_for(order.price, order.size)? - quote_for(order.price, size)?
            } else {
                order.size - size
            };
            *refunds
                .entry(seat_balance(data, order, is_bid)?)
                .or_default() += u128::from(refund);
        }
    }
    if sizes.is_empty() {
        return Err("the chosen side has no live orders".to_string());
    }
    let mut balances = Vec::new();
    for (at, refund) in refunds {
        let balance = u128::from(u64::from_le_bytes(read(data, at))) + refund;
        let balance = u64::try_from(balance).map_err(|_| "a seat balance would overflow")?;
        balances.push((at, balance));
    }
    for (node, size) in sizes {
        data[node + ORDER_SIZE.start..node + ORDER_SIZE.end].copy_from_slice(&size.to_le_bytes());
    }
    for (at, balance) in balances {
        data[at..at + 8].copy_from_slice(&balance.to_le_bytes());
    }
    Ok(())
}

/// The account offset of the seat balance a cancel of `order` refunds: quote for a bid, base for
/// an ask.
fn seat_balance(data: &[u8], order: &Order, is_bid: bool) -> Result<usize, String> {
    let index = u32::from_le_bytes(read(data, order.node + ORDER_TRADER)) as usize;
    let seat = HEADER_LEN + index;
    if !index.is_multiple_of(NODE_LEN)
        || seat + NODE_LEN > data.len()
        || data[seat + PAYLOAD_TYPE] != SEAT_NODE
    {
        return Err("an order's trader is not a seat of this market".to_string());
    }
    Ok(seat
        + if is_bid {
            SEAT_QUOTE.start
        } else {
            SEAT_BASE.start
        })
}

/// Orders that already expired count as no level, and an earlier expiry is kept.
fn expire_orders(
    data: &mut [u8],
    side: Side,
    levels: u32,
    last_valid_slot: u32,
    now: u32,
) -> Result<(), String> {
    let (bids, asks) = read_book(data)?;
    let mut expired = 0;
    for (orders, is_bid) in [(&bids, true), (&asks, false)] {
        if !side.includes(is_bid) {
            continue;
        }
        let mut level = 0;
        let mut level_price = None;
        for order in orders.iter().filter(|order| order.is_live(now)) {
            if level_price != Some(order.price) {
                level += 1;
                level_price = Some(order.price);
            }
            if levels != 0 && level > levels {
                break;
            }
            let slot = match order.last_valid_slot {
                0 => last_valid_slot,
                earlier => earlier.min(last_valid_slot),
            };
            let at = order.node + ORDER_LAST_VALID_SLOT.start;
            data[at..at + 4].copy_from_slice(&slot.to_le_bytes());
            expired += 1;
        }
    }
    if expired == 0 {
        return Err("the chosen side has no live orders".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{scenarios::TemplateRegistry, surfnet::svm::SurfnetSvm};

    const P: u128 = PRICE_SCALE;

    /// A market whose orders are given best first. Each side is a chain from its worst order up,
    /// followed by one seat and one free block.
    fn market(bids: &[(u128, u64)], asks: &[(u128, u64)]) -> Vec<u8> {
        let blocks = bids.len() + asks.len() + 2;
        let mut data = vec![0u8; HEADER_LEN + blocks * NODE_LEN];
        data[..8].copy_from_slice(&MARKET_DISCRIMINATOR);
        set_u32(&mut data, ALLOCATED_AT, (blocks * NODE_LEN) as u32);
        let seat = ((blocks - 2) * NODE_LEN) as u32;
        let free = seat + NODE_LEN as u32;
        set_u32(&mut data, 172, seat);
        set_u32(&mut data, 176, free);
        let node_of = |index: u32| HEADER_LEN + index as usize;
        data[node_of(seat) + PAYLOAD_TYPE] = SEAT_NODE;
        for at in [0, 4, 8] {
            set_u32(&mut data, node_of(seat) + at, NIL);
            set_u32(&mut data, node_of(free) + at, NIL);
        }
        let mut first = 0u32;
        for (orders, is_bid, (root_at, best_at)) in [(bids, true, BIDS), (asks, false, ASKS)] {
            let indices: Vec<u32> = (0..orders.len() as u32)
                .map(|i| (first + i) * NODE_LEN as u32)
                .collect();
            first += orders.len() as u32;
            set_u32(&mut data, root_at, *indices.last().unwrap_or(&NIL));
            set_u32(&mut data, best_at, *indices.first().unwrap_or(&NIL));
            for (i, (&(price, size), index)) in orders.iter().zip(&indices).enumerate() {
                let node = node_of(*index);
                let better = if i == 0 { NIL } else { indices[i - 1] };
                set_u32(&mut data, node, NIL);
                set_u32(&mut data, node + RIGHT_CHILD, better);
                data[node + PAYLOAD_TYPE] = ORDER_NODE;
                set_order(&mut data, node, price, size);
                set_u32(&mut data, node + ORDER_TRADER, seat);
                data[node + ORDER_IS_BID] = u8::from(is_bid);
            }
        }
        data
    }

    fn set_u32(data: &mut [u8], at: usize, value: u32) {
        data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_u64(data: &mut [u8], at: usize, value: u64) {
        data[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn set_order(data: &mut [u8], node: usize, price: u128, size: u64) {
        data[node + ORDER_PRICE.start..node + ORDER_PRICE.end]
            .copy_from_slice(&price.to_le_bytes());
        set_u64(data, node + ORDER_SIZE.start, size);
    }

    fn account(data: Vec<u8>) -> Account {
        Account {
            lamports: 1,
            data,
            owner: MANIFEST_PROGRAM_ID,
            ..Account::default()
        }
    }

    fn to_map(values: serde_json::Value) -> HashMap<String, serde_json::Value> {
        serde_json::from_value(values).unwrap()
    }

    fn apply(data: &[u8], values: serde_json::Value, slot: u64) -> Result<Vec<u8>, String> {
        prepare_manifest_override(&account(data.to_vec()), &to_map(values), slot)
            .map(|written| written.expect("a Manifest market with codec keys"))
    }

    fn book() -> Vec<u8> {
        market(
            &[
                (108 * P + 7, 3_000_000_001),
                (107 * P + P / 2, 10),
                (99 * P + 1, 1),
            ],
            &[
                (109 * P - 3, 5),
                (109 * P - 3, 6),
                (110 * P, 7),
                (130 * P, 8),
            ],
        )
    }

    /// Bids round down and asks round up, a bid's size grows so its quote never does, and only the
    /// chosen side's prices and sizes change.
    #[test]
    fn shift_moves_only_the_chosen_side_and_never_crosses() {
        let before = book();
        let (bids, asks) = read_book(&before).unwrap();
        for (ratio, side) in [
            ("900000", "both"),
            ("1", "both"),
            ("1050000", "asks"),
            ("1100000", "asks"),
        ] {
            let after = apply(&before, json!({"ratio_ppm": ratio, "side": side}), 1).unwrap();
            let ratio: u128 = ratio.parse().unwrap();
            let mut expected = before.clone();
            let shifted_bids = if side == "both" { &bids[..] } else { &[] };
            for bid in shifted_bids {
                let (price, size) = (bid.price * ratio / PPM, u128::from(bid.size) * PPM / ratio);
                set_order(&mut expected, bid.node, price, size as u64);
                assert!(quote_for(price, size as u64) <= quote_for(bid.price, bid.size));
            }
            for ask in &asks {
                let price = (ask.price * ratio).div_ceil(PPM);
                set_order(&mut expected, ask.node, price, ask.size);
            }
            assert_eq!(after, expected, "{ratio} on {side}");
            let (new_bids, new_asks) = read_book(&after).unwrap();
            assert!(new_bids[0].price < new_asks[0].price, "{ratio} crossed");
        }
    }

    #[test]
    fn identity_values_are_byte_identical() {
        let mut before = book();
        let (_, asks) = read_book(&before).unwrap();
        before[asks[0].node + ORDER_TYPE] = GLOBAL_ORDER;
        for values in [
            json!({"ratio_ppm": "1000000", "side": "both"}),
            json!({"size_ppm": "1000000", "side": "both"}),
        ] {
            assert_eq!(apply(&before, values, 1).unwrap(), before);
        }
    }

    /// Orders that already expired count as no level, an earlier expiry is kept, and levels 0 is
    /// the whole side.
    #[test]
    fn expiry_writes_only_the_expiry_of_the_best_live_levels() {
        let mut before = book();
        let (bids, asks) = read_book(&before).unwrap();
        let expiry = |order: &Order| order.node + ORDER_LAST_VALID_SLOT.start;
        set_u32(&mut before, expiry(&asks[0]), 99);
        set_u32(&mut before, expiry(&asks[2]), 101);
        let mut expected = before.clone();
        set_u32(&mut expected, expiry(&asks[1]), 104);
        let values = json!({"side": "asks", "levels": 2, "expire_after_slots": "5"});
        assert_eq!(apply(&before, values, 100).unwrap(), expected);

        let mut expected = before.clone();
        for bid in &bids {
            set_u32(&mut expected, expiry(bid), 99);
        }
        let values = json!({"side": "bids", "levels": "0", "expire_after_slots": 0});
        assert_eq!(apply(&before, values, 100).unwrap(), expected);
    }

    #[test]
    fn depth_cut_refunds_each_seat_what_its_orders_no_longer_lock() {
        let mut before = book();
        let (bids, asks) = read_book(&before).unwrap();
        let seat = HEADER_LEN + u32::from_le_bytes(read(&before, 172)) as usize;
        set_u32(&mut before, bids[2].node + ORDER_LAST_VALID_SLOT.start, 99);
        before[bids[1].node + ORDER_TYPE] = 4;
        before[asks[3].node + ORDER_TYPE] = GLOBAL_ORDER;
        let mut expected = before.clone();
        // The expired bid stays; every ask rounds to zero and keeps one atom.
        set_u64(&mut expected, bids[0].node + ORDER_SIZE.start, 300_000_000);
        for order in [&bids[1], &asks[0], &asks[1], &asks[2], &asks[3]] {
            set_u64(&mut expected, order.node + ORDER_SIZE.start, 1);
        }
        // Bids: ceil(p * 3_000_000_001) - ceil(p * 300_000_000) = 324_000_000_109 - 32_400_000_001,
        // plus 1_075 - 108 for the reverse bid, whose new quote alone rounds up. Asks: 4 + 5 + 6;
        // the global ask locks nothing.
        set_u64(&mut expected, seat + SEAT_QUOTE.start, 291_600_001_075);
        set_u64(&mut expected, seat + SEAT_BASE.start, 15);
        let values = json!({"size_ppm": "100000", "side": "both"});
        assert_eq!(apply(&before, values, 100).unwrap(), expected);
    }

    #[test]
    fn values_that_break_an_order_are_refused() {
        let good = book();
        let (bids, asks) = read_book(&good).unwrap();
        let seat = HEADER_LEN + u32::from_le_bytes(read(&good, 172)) as usize;
        let mut immediate = good.clone();
        immediate[asks[1].node + ORDER_TYPE] = 1;
        let mut unknown = good.clone();
        unknown[bids[0].node + ORDER_TYPE] = 6;
        let mut not_a_seat = good.clone();
        let order_index = (bids[0].node - HEADER_LEN) as u32;
        set_u32(&mut not_a_seat, asks[0].node + ORDER_TRADER, order_index);
        let mut full_seat = good.clone();
        set_u64(&mut full_seat, seat + SEAT_QUOTE.start, u64::MAX);
        let one_bid = market(&[(P, 1)], &[]);
        let tiny = market(&[(1, 1)], &[]);
        let huge = market(&[(u128::MAX / 2, 1)], &[]);
        let big_bid = market(&[(P, u64::MAX)], &[]);
        let big_ask = market(&[], &[(P, u64::MAX)]);
        let no_dust = market(&[(108 * P + 7, 3_000_000_001)], &[(109 * P - 3, 5)]);
        let shift = |ratio: &str, side: &str| json!({"ratio_ppm": ratio, "side": side});
        let cut = |size: &str, side: &str| json!({"size_ppm": size, "side": side});
        let no_levels = json!({"side": "both", "expire_after_slots": 1});
        let two_keys = json!({"ratio_ppm": "1", "size_ppm": "1", "side": "both"});
        for (data, values, expected) in [
            (&good, shift("0", "both"), "above 0"),
            (&tiny, shift("999999", "bids"), "to zero"),
            (&huge, shift("2000000", "bids"), "overflows a price"),
            (&big_ask, shift("2000000", "asks"), "64 bits"),
            (&big_bid, shift("999999", "bids"), "a bid's size"),
            (&one_bid, shift("900000", "asks"), "no orders"),
            (&no_dust, shift("1010000", "bids"), "cross the book"),
            (&good, shift("1100000", "both"), "to zero size"),
            (&good, shift("123456789", "both"), "to zero size"),
            (&good, no_levels, "levels is required"),
            (&good, cut("0", "both"), "between 1 and"),
            (&good, cut("1000001", "asks"), "between 1 and"),
            (&good, cut("half", "asks"), "whole number"),
            (&good, cut("500000", "top"), "both, bids or asks"),
            (&one_bid, cut("500000", "asks"), "no live orders"),
            (&immediate, cut("500000", "asks"), "never rests"),
            (&unknown, cut("500000", "bids"), "never rests"),
            (&not_a_seat, cut("500000", "asks"), "not a seat"),
            (&full_seat, cut("500000", "bids"), "overflow"),
            (&good, two_keys, "one of"),
        ] {
            let error = apply(data, values, 1).unwrap_err();
            assert!(error.contains(expected), "{expected}: {error}");
        }
        for (slot, after_slots) in [(0, 0), (1, 0), (u64::from(u32::MAX), 2), (1, u64::MAX)] {
            let after_slots = after_slots.to_string();
            let values = json!({"side": "both", "levels": 1, "expire_after_slots": after_slots});
            let error = apply(&good, values, slot).unwrap_err();
            assert!(error.contains("expiry slot"), "{error}");
        }
    }

    #[test]
    fn damaged_markets_are_refused_before_any_write() {
        let good = book();
        let (bids, asks) = read_book(&good).unwrap();
        let root = u32::from_le_bytes(read(&good, BIDS.0));
        let seat = u32::from_le_bytes(read(&good, 172));
        let outside = (good.len() - HEADER_LEN) as u32;
        let second_bid = (bids[1].node - HEADER_LEN) as u32;
        let with = |writes: &[(usize, u32)]| {
            let mut data = good.clone();
            for &(at, value) in writes {
                set_u32(&mut data, at, value);
            }
            data
        };
        let mut short = good.clone();
        short.pop();
        let mut wrong_side = good.clone();
        wrong_side[asks[0].node + ORDER_IS_BID] = 1;
        let mut unsorted = good.clone();
        set_order(&mut unsorted, bids[1].node, 200 * P, bids[1].size);
        for (data, expected) in [
            (short, "allocates"),
            (with(&[(BIDS.0, root + 1)]), "not a block"),
            (with(&[(BIDS.0, outside)]), "not a block"),
            (with(&[(BIDS.0, seat), (BIDS.1, seat)]), "another kind"),
            (wrong_side, "another kind"),
            (with(&[(bids[0].node + RIGHT_CHILD, root)]), "more nodes"),
            (with(&[(BIDS.1, second_bid)]), "best bid"),
            (unsorted, "not sorted"),
        ] {
            for values in [
                json!({"ratio_ppm": "900000", "side": "both"}),
                json!({"side": "both", "levels": 0, "expire_after_slots": 1}),
                json!({"size_ppm": "500000", "side": "both"}),
            ] {
                let error = apply(&data, values, 1).unwrap_err();
                assert!(error.contains(expected), "{expected}: {error}");
            }
        }
    }

    /// Each template reads only its own values. Other accounts, and values with no codec key, are
    /// left to the generic IDL path.
    #[test]
    fn writer_reads_only_its_own_keys() {
        let shift = json!({"ratio_ppm": "800000", "side": "both"});
        let echoed = json!({
            "ratio_ppm": "800000", "side": "both", "version": 3, "order_sequence_number": "1",
            "baseMint": Pubkey::new_unique().to_string(), "bidsBestIndex": 80, "quote_volume": 5
        });
        assert_eq!(apply(&book(), echoed, 1), apply(&book(), shift.clone(), 1));
        let mut foreign = account(book());
        foreign.owner = Pubkey::new_unique();
        let mut global = account(book());
        global.data[..8].copy_from_slice(&[1, 170, 151, 47, 187, 160, 180, 149]);
        let header_only = json!({"quote_volume": 5});
        for (target, values) in [
            (foreign, shift.clone()),
            (global, shift),
            (account(book()), header_only),
        ] {
            let written = prepare_manifest_override(&target, &to_map(values), 1);
            assert_eq!(written, Ok(None));
        }
    }

    #[test]
    fn market_lookup_in_llm_context_matches_the_codec() {
        let registry = TemplateRegistry::new();
        let discriminator = bs58::encode(MARKET_DISCRIMINATOR).into_string();
        for id in [
            "manifest-book-shift",
            "manifest-orders-expire",
            "manifest-depth-cut",
        ] {
            let context = registry
                .get(id)
                .and_then(|template| template.llm_context.as_deref())
                .unwrap_or_else(|| panic!("{id} explains how to find a market"));
            assert!(context.contains(&format!("\"{discriminator}\"")));
            assert!(context.contains(&MANIFEST_PROGRAM_ID.to_string()));
        }
    }

    #[test]
    fn idl_header_matches_the_market_layout() {
        let registry = TemplateRegistry::new();
        let idl = registry
            .get("manifest-book-shift")
            .and_then(|template| template.idl.as_ref())
            .expect("the Manifest template carries its IDL");
        assert_eq!(idl.address, MANIFEST_PROGRAM_ID.to_string());
        assert_eq!(idl.accounts[0].discriminator, MARKET_DISCRIMINATOR);
        let before = book();
        let global = Pubkey::new_unique();
        let header = to_map(json!({"quote_volume": 7, "quote_global": global.to_string()}));
        let mut after = before[..8].to_vec();
        after.extend(
            SurfnetSvm::get_forged_idl_type_data(&before[8..], idl, "MarketFixed", &header)
                .unwrap(),
        );
        let mut expected = before.clone();
        set_u64(&mut expected, 184, 7);
        expected[224..256].copy_from_slice(global.as_ref());
        assert_eq!(after, expected);
    }
}
