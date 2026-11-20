# Pyth

Pyth runs two price receivers on Solana, both deployments of the `pyth_solana_receiver` program, as
listed on [Pyth's Solana contract addresses](https://docs.pyth.network/price-feeds/contract-addresses/solana).
Surfpool bundles an IDL, an override template and a keep-alive for each:

| Receiver | Program | Folder | Template |
|---|---|---|---|
| Pyth Core | `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` | `v2/` | `pyth-price-feed-v2` |
| Pyth Core Compatible | `rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` | `core-compatible/v2/` | `pyth-core-compatible-price-feed-v2` |

They hold separate price accounts for the same feeds, and a consumer reads the accounts of one of
them. Take the price account from the consumer's own account that names it.

For how scenarios work in general see the [scenarios README](../../README.md).

# Template index

**Pyth Core** &middot; `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` &middot; `v2/`

| Template | Overrides |
|---|---|
| `pyth-price-feed-v2` | the price account of a feed picked from a list |

**Pyth Core Compatible** &middot; `rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` &middot; `core-compatible/v2/`

| Template | Overrides |
|---|---|
| `pyth-core-compatible-price-feed-v2` | a price account of this receiver, given by address |

Both templates write the same fields of a `PriceUpdateV2` account: `price_message.price`,
`price_message.conf`, `price_message.exponent`, `price_message.publish_time`,
`price_message.prev_publish_time`, `price_message.ema_price`, `price_message.ema_conf` and
`posted_slot`. `pyth-price-feed-v2` also takes `feed_id`, which picks the account and is not
written. Both templates import a [keep-alive](#keep-alive).

## Picking a price account

Set `fetchBeforeUse: true` to pull the price account from upstream first.

### Pyth Core

Pick the feed in the template's **Price Feed** list, the `price_feed` constant in
`v2/overrides.yaml`. Its value is the feed id, and the template derives the price account from it:
a PDA of `pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT` with the seeds shard `0` as a little-endian
`u16` and the 32-byte feed id. For SOL/USD, feed id
`0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d`, that is
`7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`, an account owned by the Pyth Core receiver.

For a feed the list does not have, name the price account in the override's account as
`{"pubkey": "<price account>"}`.

### Pyth Core Compatible

This template has no feed list. It writes the account the override names:

1. Take the price account from the consumer's account that names it.
2. Check that its owner is `rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp`. For an account owned by
   the Pyth Core receiver, use `pyth-price-feed-v2`.
3. Put it in the override's account as `{"pubkey": "<price account>"}`.

The SOL/USD price account of this receiver is `7AviUf9nL62mcxNbQGKm4nKDQnPjswo6c5MX4D57HmyE`.

## Number formats

Every price field is a scaled integer. The scale is the account's own `price_message.exponent`,
normally `-8` and `-10` on some feeds.

| You'll see | It means | Example |
|---|---|---|
| `price_message.price` | the USD price x 10^-exponent | $145.00 at exponent `-8` → `14500000000` |
| `price_message.conf` | the uncertainty band around the price, in the same scale | $2 at exponent `-8` → `200000000` |
| `price_message.ema_price`, `price_message.ema_conf` | moving averages of price and conf, in the same scale | |
| `price_message.exponent` | the decimal exponent; it rescales every price field | `-8` |
| `price_message.publish_time`, `price_message.prev_publish_time` | Unix seconds | |
| `posted_slot` | the slot the update was posted at | |

More examples at exponent `-8`: $97,000 BTC → `9700000000000`, $3,500 ETH → `350000000000`. At
exponent `-10` (BONK, PEPE and SHIB in the Pyth Core list): $0.00002 BONK → `200000`, $0.000012
PEPE → `120000`.

Leave `price_message.exponent` alone unless the scenario is about an exponent error. If you change it,
restate every scaled field to match.

# Keep-alive

Nobody publishes Pyth prices on a surfnet, so both templates import a
[keep-alive](../../README.md#keep-alive): from the slot an override of a template plays until
the network is reset, Surfpool writes the Clock's unix seconds to `price_message.publish_time` and
the slot to `posted_slot` every slot, time travels included. It does so for every price account of
that template's receiver the surfnet has loaded, including feeds no override names and, from the
next slot, feeds a transaction loads later, so a liquidation that reads the borrower's other feeds
finds them fresh. The price, confidence and EMA fields keep their values. To keep one feed stale on
purpose, override it with `keepAlive: false`; it then ages against the Surfnet clock.

Each `overrides.yaml` imports, at its top level, the publication template of its folder's
`keep-alive.yaml`:

| Template | Keep-alive template | Rewrites |
|---|---|---|
| `pyth-price-feed-v2` | `pyth-price-publication` | every loaded `PriceUpdateV2` account owned by `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` |
| `pyth-core-compatible-price-feed-v2` | `pyth-core-compatible-price-publication` | every loaded `PriceUpdateV2` account owned by `rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` |

```yaml
keep_alive:
  - template: pyth-price-publication
    address: { type: owned, owner: rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ, account: PriceUpdateV2 }
    values: { price_message.publish_time: $unix_s, posted_slot: $slot }
```

The keep-alive templates are never offered in Studio or MCP. Because keep-alive rewrites
`price_message.publish_time` and `posted_slot` every slot, a value an override sets for them holds
only with `keepAlive: false`.

# Recipes

## Drop a price

"SOL drops to $20" on an account with exponent `-8`, live price `p` and live EMA price `q`:

```
price_message.price: 2000000000
price_message.ema_price: 2000000000
price_message.conf: <live conf x 2000000000 / p>
price_message.ema_conf: <live ema_conf x 2000000000 / q>
```

Move `ema_price` with `price`: consumers use the EMA for some checks and the spot price for others.
Scale `conf` and `ema_conf` by the same factor as the price: consumers refuse a price whose
confidence is too wide a share of it, so a large drop that keeps the live `conf` can be refused.

## Keep one feed stale

Keep-alive starts only when an override with `keepAlive: true` writes a price account, and an
override with no values writes nothing. For `pyth-price-feed-v2`, `feed_id` does not count as a
value. To keep one feed stale while the others stay fresh, add two overrides of the receiver's
template, both with `fetchBeforeUse: true` and `price_message.price` set to the account's current
price, so no price changes:

- on the stale feed's price account, `keepAlive: false`. It then ages against the Surfnet clock and
  turns stale once the consumer's age limit has passed;
- on any other price account the same consumer reads, `keepAlive: true`. This starts the
  republishing of every loaded price account except the stale one.

To make it stale at once, `surfnet_timeTravel` past the consumer's limit. Its `absoluteTimestamp`
is in milliseconds, while `publish_time` and the Clock sysvar are in seconds.

# Troubleshooting

| Symptom | Fix |
|---|---|
| The consumer still reads the old price | The override wrote the other receiver's account. Check the price account's owner and use that receiver's template |
| A large price drop is refused | Scale `conf` and `ema_conf` with the price. See [Drop a price](#drop-a-price) |
| A `publish_time` or `posted_slot` value did not hold | Keep-alive rewrites both every slot. Set `keepAlive: false` on that override |
| Other feeds of the consumer turn stale | Keep-alive starts only when an override with `keepAlive: true` writes a price account. Give that override a value, such as the current price |
| A feed meant to be stale is still fresh after `surfnet_timeTravel` | `absoluteTimestamp` is in milliseconds, and `publish_time` is in seconds |
| Prices are off by a power of ten after an exponent change | `exponent` rescales every price field. Restate `price`, `conf`, `ema_price` and `ema_conf` to match |

# Files

| File | Holds |
|---|---|
| `v2/overrides.yaml` | the `pyth-price-feed-v2` template and its `price_feed` list |
| `v2/idl.json` | the Pyth Core IDL |
| `v2/keep-alive.yaml` | the `pyth-price-publication` keep-alive template |
| `v2/generate-price-feeds.js` | the script that regenerates the `price_feed` list |
| `core-compatible/v2/overrides.yaml` | the `pyth-core-compatible-price-feed-v2` template |
| `core-compatible/v2/idl.json` | the Pyth Core Compatible IDL |
| `core-compatible/v2/keep-alive.yaml` | the `pyth-core-compatible-price-publication` keep-alive template |

[`registry.rs`](../../registry.rs) compiles these files in. `load_pyth_overrides` loads both
template collections under the protocol `Pyth`, each with the IDL beside it, and both keep-alive
files.

## Regenerating the Pyth Core feed list

`v2/generate-price-feeds.js` fetches the feeds from the Pyth Hermes API
(`https://hermes.pyth.network/v2/price_feeds`) and rewrites the `price_feed` options in
`v2/overrides.yaml`. From `v2/`:

```
node generate-price-feeds.js
```

It keeps the `Crypto` feeds quoted in USD whose base is in the script's `POPULAR_SYMBOLS`, plus
ETH/BTC, and puts SOL, BTC, ETH, USDC and USDT first. The "(N decimals)" in each option's
description is the feed's exponent without its sign, read from
`https://hermes.pyth.network/v2/updates/price/latest`, or 8 when Hermes returns none. The script
expects the `constants` block to end right before `templates:`.

## Tests

The Pyth integration tests in `crates/core/src/tests/pyth/` fetch real price accounts from mainnet,
so they need a network connection and are compiled only behind a feature:

```
cargo test -p surfpool-core --features integration-tests pyth
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits. The unit tests
in `registry.rs` check the Pyth Core address derivation and both keep-alive imports without a
network.

# IDL

## Pyth Core

`v2/idl.json` is an Anchor IDL with the address `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`. It
has no instructions and defines one account, `PriceUpdateV2`, with the types `PriceFeedMessage`,
`PriceUpdateV2` and `VerificationLevel`. Its `PriceUpdateV2` account has the same discriminator and
fields as the one in the Pyth Core Compatible IDL.

| IDL | sha256 |
|---|---|
| Bundled `v2/idl.json` | `93b29b6f60d6c50c39de3d3f075b0d9acf948633edef7234e6b1e2f9b397aa02` |

## Pyth Core Compatible

`core-compatible/v2/idl.json` is the IDL the program publishes in its Anchor IDL account
`raeUSn8gguFWVFs4iw3YathYM3mdb95W5Bfiu3tqE7F` (`pyth_solana_receiver` 0.2.1, legacy format, sha256
`9ec4718f66a71fc524ecdd533770ca5a8a3984f8e1fb603f01f5e31ff0ab0caa`), converted with
`anchor idl convert --program-id rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` (anchor-cli 0.31.1,
output sha256 `29ab79ef705a3429b3ced2b1222e1b5efdcd4b40fed547177d31c16ebd8473fc`).

One change is made before the conversion: the account names `priceUpdateV2` and `twapUpdate` are
written `PriceUpdateV2` and `TwapUpdate`, as the program's Rust types are named. Anchor derives an
account's discriminator from its name, and only the capitalized names give the discriminator the
live price accounts carry.
