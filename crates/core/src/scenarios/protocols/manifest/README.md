# Manifest

Manifest is an order book. A market is one account: a 256-byte header followed by fixed-size blocks
that hold the resting bids and asks, the traders' seats and free space. There is no oracle, so a
market's price is nothing but its resting orders.

Surfpool bundles a small IDL for the header and three templates that rewrite the orders themselves.
They only prepare state; trades are sent by your own code.

# Template index

| Template | Does |
|---|---|
| `manifest-book-shift` | moves every order on one or both sides by the same ratio, like a price shock |
| `manifest-orders-expire` | makes the best price levels, or a whole side, expire, like makers pulling their quotes |
| `manifest-depth-cut` | shrinks every order on one or both sides by the same ratio without moving any price, like makers quoting smaller |

Each takes the market's address and reads only its own values; any other value sent with it is
ignored.

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `ratio_ppm` | new price over old price, in parts per million, as a decimal **string** | `"900000"` = 10% lower |
| `side` | which orders change | `"both"`, `"bids"` or `"asks"` |
| `levels` | distinct prices from the best one; `0` is the whole side | `3` |
| `expire_after_slots` | slots the orders keep matching, counting the one the override runs in | `0` = gone at once |
| `size_ppm` | new size over old size, in parts per million, as a decimal **string** from `"1"` to `"1000000"` | `"250000"` = a quarter of the depth |
| order price | quote atoms per base atom, times 10^18 | |

To turn an order price into a UI price: `price / 10^18 * 10^(base decimals - quote decimals)`.

## Picking a market

Markets are created from fresh keypairs, so their addresses cannot be derived. Find them by pair
with `getProgramAccounts`; the zero-length slice returns addresses only:

```json
["MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms", {
  "encoding": "base64",
  "dataSlice": { "offset": 0, "length": 0 },
  "filters": [
    { "memcmp": { "offset": 0, "bytes": "hFwv1prLTHL" } },
    { "memcmp": { "offset": 16, "bytes": "<base mint>" } },
    { "memcmp": { "offset": 48, "bytes": "<quote mint>" } }
  ]
}]
```

The first filter is the market discriminator. If nothing comes back, the pair is listed the other
way round.

Several markets can share a pair, and an inactive one can still hold large idle balances in its
vaults, so never pick by vault balance. Pick by recent activity: the market whose latest
transaction (`getSignaturesForAddress` with `limit: 1`) is newest, or, reading bytes, the one whose
`order_sequence_number` (a u64 at offset 144, bumped by every order and swap) grows fastest.

# Recipes

## Shock the price

```json
{
  "templateId": "manifest-book-shift",
  "scenarioRelativeSlot": 0, "enabled": true,
  "fetchBeforeUse": true,
  "account": { "pubkey": "<market>" },
  "values": { "ratio_ppm": "800000", "side": "both" }
}
```

**Why:** every bid and ask moves 20% down, so the next trade fills about 20% lower. Bids round down
and asks round up, so the book never crosses. Asks keep their size. A bid keeps its quote value, so
its size grows when its price falls, and it never locks more quote than its maker deposited.

A later shift with `fetchBeforeUse: false` compounds on an earlier one: `900000` twice is 0.81.

## Move one side

Use `"side": "bids"` or `"side": "asks"` for buying or selling pressure that the other side has not
followed yet. A one-sided shift that would cross the book is refused.

## Pull the best levels or a whole side

```json
{
  "templateId": "manifest-orders-expire",
  "scenarioRelativeSlot": 0, "enabled": true,
  "fetchBeforeUse": true,
  "account": { "pubkey": "<market>" },
  "values": { "side": "asks", "levels": 3, "expire_after_slots": 0 }
}
```

**Why:** the three best ask prices expire at once, so a buyer fills from the fourth.
Manifest itself removes an expired order the first time a trade reaches it and returns what the
order locked to its maker's seat, so every token stays accounted for. `levels: 0` empties the whole
side.

## Thin the book

```json
{
  "templateId": "manifest-depth-cut",
  "scenarioRelativeSlot": 0, "enabled": true,
  "fetchBeforeUse": true,
  "account": { "pubkey": "<market>" },
  "values": { "size_ppm": "250000", "side": "both" }
}
```

**Why:** every live order keeps its price and a quarter of its size, so the best price is unchanged
but a large trade runs through it sooner and fills more at worse prices. What each order no longer
locks goes back to its maker's balance on the market, rounded exactly as Manifest refunds a cancel,
so every token stays accounted for. Sizes round down but never below one atom; to remove orders
instead, expire them.

## Quotes that go stale later

Set `expire_after_slots` to the number of slots the quotes should survive. The orders keep matching
for that many slots, counting the one the override runs in, then disappear, as when a maker stops
refreshing its quotes.

# What the templates enforce

- The account must be a Manifest market: owned by `MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms`,
  with the market discriminator and a length that matches its header.
- Both order trees are walked before anything is written: every node must be an aligned block of
  the market, hold an order of its own side, sit in price order, and the cached best order must be
  the top of its tree.
- A shift is refused when a price would reach zero or overflow, or when an order's quote amount
  would no longer fit in 64 bits.
- An order that already expires sooner keeps its own expiry, and orders that already expired do not
  count as levels.
- A cut is refused for `size_ppm` 0 or above 1000000, since growing an order would need funds no
  maker deposited. It skips orders that already expired. An ask's maker gets back the base atoms
  the cut removes; a bid's maker gets the quote it locked (price times size, rounded up to a whole
  atom) minus what the smaller bid still locks. A global order shrinks but refunds nothing, since
  its backing sits in the Global account, not in the market. A cut is refused if an order's type
  is one Manifest never rests, if its trader is not a seat of the market, or if a seat balance
  would overflow.
- Only the price and size of orders (shift), their expiry slot (expire), or their size and their
  makers' seat balances (cut) change. The free list and the header stay as they were.

# Troubleshooting

| Symptom | Fix |
|---|---|
| The override had no effect and the log says the account was not found | The market is not in the local VM yet. Set `fetchBeforeUse: true` |
| Orders placed in the local VM disappeared after Play | `fetchBeforeUse: true` reloads the market from the upstream datasource first. Use `false` to keep local orders |
| A shift was skipped with "would cross the book" | A one-sided shift moved its side past the other. Shift both sides or use a smaller ratio |
| A cut left orders of one atom on the book | Sizes round down but never below one atom, since Manifest never rests an empty order. Use `manifest-orders-expire` to remove orders |
| A cut was skipped with "size_ppm must be between 1 and 1000000" | A cut only shrinks orders. To deepen the book, place orders in the local VM with your own code |
| A trade still fills at an expired price | It ran within the `expire_after_slots` slots the orders keep matching, which count the override's own slot. Use `0` to remove them at once |
| Fewer orders filled than the book shows | Global orders are backed by their maker's balance in a shared Global account, which can be smaller than the orders show. Manifest skips an order its maker cannot cover |

# Layout source

The layout comes from Manifest's published interface: its Shank IDL, `client/idl/manifest.json` at
tag `program-v3.0.26` of [Bonasa-Tech/manifest](https://github.com/Bonasa-Tech/manifest) (sha256
`366997a1359ff39179100de539fff24a0d2b99fe7f022a7eea701000cdbfa47a`), the release deployed at slot
459237697. The engine reads Anchor IDLs, and a Shank IDL names no
account discriminators, so `idl.json` restates that IDL's `MarketFixed` field for field in the
Anchor format, with Manifest's market discriminator. It covers only the 256-byte header.

The 80-byte blocks after the header, the trees that link them, and the `RestingOrder` and
`ClaimedSeat` payloads the templates rewrite follow the same IDL's types and the market reader of
Manifest's TypeScript SDK, `@bonasa-tech/manifest-sdk` 0.2.48 (MIT). No Manifest program code is
used.
