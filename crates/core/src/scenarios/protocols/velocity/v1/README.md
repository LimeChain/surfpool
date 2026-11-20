# Velocity

Velocity is a perpetuals exchange built on the Drift v2 program. Surfpool bundles its IDL and nine
override templates, so a scenario can move a market's oracle price, tighten a market's margin
requirements, change trading fees, collateral weights, borrow rates and liquidation fees, pause
actions across the exchange, charge funding, cap one account's leverage or give an account a
position. Once a scenario moves an oracle, Surfpool keeps every Velocity oracle posted on its own.
The templates only prepare state: trades, withdrawals and liquidations are sent by your own code.
For how scenarios work in general see the [scenarios README](../../../README.md).

# Template index

**Velocity** &middot; `vELoC1audYbSYVRXn1vPaV8Axoa9oU6BYmNGZZBDZ1P`

| Template | Account | Overrides |
|---|---|---|
| `velocity-oracle-price` | `PythLazerOracle` | the price a market reads and its confidence; Surfpool then keeps every oracle posted |
| `velocity-perp-market-risk` | `PerpMarket` | margin ratios, status, paused operations, the open interest cap and liquidation fees |
| `velocity-exchange-risk` | `State` | exchange-wide pauses, how long margin checks accept a price, the price guard rails liquidation checks and the liquidation margin buffer |
| `velocity-fees` | `State` | the taker fee and maker rebate every perp trader pays |
| `velocity-spot-market-risk` | `SpotMarket` | collateral and borrow weights, the borrow rate curve, status, paused operations and the deposit cap |
| `velocity-funding` | `PerpMarket` | the funding every open position of the market pays or earns when it next settles |
| `velocity-account-leverage` | `User` | the most leverage one account may open, in every market or in one |
| `velocity-perp-position` | `User` | the position an account holds in one market |
| `velocity-position-totals` | `PerpMarket` | the market's position totals, so the AMM holds the other side of a written position |

## Number formats

| Field | Meaning | Example |
|---|---|---|
| `price`, `conf` | USD x 10^-`exponent`, read from the oracle (-8 on every live oracle today) | `8500000000` = $85 at -8 |
| `posted_slot` | the slot the price counts as posted at; keep-alive writes the current slot | |
| `publish_time` | Pyth Lazer timestamp in microseconds; keep-alive writes the Clock time | unix seconds x `1000000` |
| `margin_ratio_maintenance`, `margin_ratio_initial` | share of position value, `10000` = 100% | `300` = 3% |
| `max_open_interest` | base units per side, `10^9` = one unit of the asset, as a decimal **string**; `0` = no cap | |
| `status` | enum object | `{"ReduceOnly": null}` |
| perp `paused_operations` | bits: `1` funding, `2` AMM fills, `4` fills, `8` settle PnL, `16` settle PnL with position, `32` liquidation, `64` AMM immediate fills, `128` settle revenue pool | `32` |
| `liquidator_fee`, `if_liquidation_fee` | share of the liquidated position's value, `1000000` = 100% | `7500` = 0.75% |
| `exchange_status` | bits: `1` deposits, `2` withdrawals, `4` AMM, `8` fills, `16` liquidations, `32` funding, `64` settle PnL, `128` AMM immediate fills | `16` |
| `slots_before_stale_for_margin` | units of 400 ms, converted with the State's `slot_duration_ms` (200 ms live, so two slots per unit) | `120` = 240 slots |
| `oracle_twap_5min_percent_divergence` | `1000000` = 100%; the program never uses less than 50% | `1000000` |
| `too_volatile_ratio` | a plain multiple of the market's TWAP | `5` |
| `liquidation_margin_buffer_ratio` | margin above maintenance a liquidation restores, `10000` = 100% | `200` = 2% |
| `promo_fee_tier` | the lowest perp fee tier any account pays; the program uses tiers `0` to `3` | `3` |
| `fee_numerator` / `fee_denominator` | taker fee as a share of the fill's quote value | `100` / `100000` = 0.1% |
| `maker_rebate_numerator` / `maker_rebate_denominator` | maker rebate as a share of the fill's quote value | `1` / `100000` = 0.001% |
| spot asset and liability weights | `10000` = 100% | `9000` |
| `optimal_utilization`, `optimal_borrow_rate`, `max_borrow_rate` | `1000000` = 100%; rates are yearly | `800000` |
| `min_borrow_rate` | yearly floor in steps of 0.5%, at most `255` | `200` = 100% |
| spot `paused_operations` | bits: `1` interest updates, `2` fills, `4` deposits, `8` withdrawals, `16` liquidation | `4` |
| `max_token_deposits` | the token's base units, as a JSON number; `0` = no cap | |
| `cumulative_funding_rate_long`, `cumulative_funding_rate_short` | funding per unit of base since the market opened, USD x 10^9, as a decimal **string** | a step of `72450000` = $0.07245 per unit |
| `net_unsettled_funding_pnl` | funding users owe and have not settled, USDT x 10^6 | |
| User `max_margin_ratio`, position `max_margin_ratio` | initial margin floor, `10000` = 100%; the most leverage is 10000 / ratio | `5000` = 2x |
| position `base_asset_amount` | base x 10^9, negative for a short, a multiple of the market's `order_step_size` | `100000000000` = 100 SOL |
| position `quote_asset_amount`, `quote_entry_amount`, `quote_break_even_amount` | -base x entry price / 10^9, USDT x 10^6 with the price at 10^6 | `-7000000000` = 100 SOL long at $70 |
| market `base_asset_amount_long`, `base_asset_amount_short`, `amm.base_asset_amount_with_amm`, `quote_asset_amount` | totals over every position, as decimal **strings** | |

Quote amounts (collateral, PnL, fees) are USDT with 6 decimals. Internally the program rescales
oracle prices to 6 decimals, so market TWAP fields are in that precision.

## Picking a market

Every account of 1560 bytes the program owns is a `PerpMarket`, and every account of 1064 bytes a
`SpotMarket`. List them with `getProgramAccounts` filtered by `dataSize` and a zero-length
`dataSlice`, then read them with `getMultipleAccounts` and `jsonParsed`: `name` is the market name
as ASCII bytes, `market_index` its index and `oracle` the `PythLazerOracle` it reads. In code, a
perp market is the PDA `["perp_market", market_index as u16 little-endian]` and a spot market
`["spot_market", market_index as u16 little-endian]`. Spot market 0 is USDT, the quote asset.

A spot market can read the same oracle as a perp market: SOL collateral is priced from the SOL-PERP
oracle, so a SOL price shock moves both.

The `State` is the PDA `["velocity_state"]`, `2etx5NvPNxeMZ7EfHE6GjJfW2imRYEUANehNS1WB4CVW`.

Set `fetchBeforeUse: true` to pull the account from upstream before the write, except for the
templates under [Picking an account](#picking-an-account) that build on what was read.

## Picking an account

A trader's account is a `User` of 4496 bytes with its wallet at offset 8, so `getProgramAccounts`
with `dataSize` 4496, a `memcmp` of the wallet at offset 8 and a zero-length `dataSlice` lists a
wallet's accounts. Read them with `getMultipleAccounts` and `jsonParsed`, which tells them apart by
`sub_account_id`: upstream RPCs cannot parse Velocity accounts, so `getProgramAccounts` with
`jsonParsed` returns them as base64. In code, an account is the PDA `["user", wallet,
sub_account_id as u16 little-endian]`. `perp_positions` holds 8 slots: a market's position is in the slot with its
`market_index` and a non-zero `base_asset_amount`, `quote_asset_amount` or `open_orders`, and the
program opens a new one in the first slot whose `base_asset_amount`, `quote_asset_amount`,
`open_orders`, `open_bids` and `open_asks` are all 0. Replace the `0` in a template's
`perp_positions.0` paths with the slot's index.

`velocity-funding`, `velocity-account-leverage`, `velocity-perp-position` and
`velocity-position-totals` take values worked out from the account as the surfnet holds it. Read
the account first with `getAccountInfo`, which loads it into the surfnet, and leave
`fetchBeforeUse: false`: a reload from upstream would bring back mainnet's values and drop the
account's changes on the surfnet, such as its trades.

## Prices on a fork

On mainnet Velocity's keeper posts every oracle continuously. Nobody runs it against a surfnet, so
from the slot a `velocity-oracle-price` override plays, Surfpool reposts each slot every Velocity
oracle the surfnet has loaded ([keep-alive](../../../README.md#keep-alive)), and from the next slot
each one a transaction loads later: `posted_slot` is the slot and `publish_time` the Clock time in
microseconds, while each price and confidence stay as they were or as the scenario set them. The
prices stay 0 slots old, the USDT price and other markets' oracles among them, through later slots
and time travel, until the network is reset. There is nothing to schedule.

Before such an override plays, and for an oracle an override writes with `keepAlive: false`, nothing
reposts the price, and the program ages it in slots, `clock.slot - posted_slot`. An account's margin
reads the oracles of all its markets and its collateral, so these windows apply to such a price:

- Margin checks, such as withdrawals, orders that add risk and matched fills, refuse a price older
  than the State's `oracle_guard_rails.validity.slots_before_stale_for_margin` (`InvalidOracle`,
  6035). It is 120 units of 400 ms, converted with the State's `slot_duration_ms` (200 ms live, so
  two slots per unit): 240 slots, and three times that for the USDT price. Raise it with
  `velocity-exchange-risk` to keep such a price usable longer; the program's admin path allows up
  to `1000000` (2,000,000 slots).
- AMM fills need a recent price. An order placed after the price fills from the AMM at once only
  within the market's `oracle_slot_delay_override` (the price's own slot when it is negative). An
  order placed in the price's slot or before it, and a PnL settle right after a price move, need
  the price posted within the State's `slots_before_stale_for_amm`, 20 slots, unless the market
  sets its own `oracle_low_risk_slot_delay_override`. The program reads both as `i8` units of
  400 ms, so neither reaches past about 254 slots, and a State `slots_before_stale_for_amm` above
  127 fails every fill.
- Liquidations never check a price's age.

`publish_time` decides which price fills read. A market keeps its own MM oracle price, and fills
read that instead when its `market_stats.mm_oracle_sequence_id`, also in microseconds, is newer than
`publish_time` and the two prices are within 1% of each other. A fetched market keeps the id it had
on mainnet, so for a move under 1% fills can read the MM price until the surfnet's clock passes it.
Keep-alive writes the Clock time to every oracle, so once the clock passes a market's id, that
market's fills read its oracle. To make such a move take effect at once, write
`market_stats.mm_oracle_sequence_id: 0` on the market with `velocity-perp-market-risk`.

# Recipes

## Make positions liquidatable with a price move

```
template: velocity-oracle-price            # account: the market's oracle
price: <live price x 0.8>                  # a 20% drop
```

Longs lose collateral as the price falls, shorts as it rises. Liquidation refuses a price more than
50% from the market's 5-minute TWAP (`PriceBandsBreached`, 6023) and one more than
`too_volatile_ratio` times away from its TWAP.

## Make positions liquidatable without a price move

```
template: velocity-perp-market-risk
margin_ratio_maintenance: 2500             # 25%
margin_ratio_initial: 3000                 # keep it above maintenance
```

Keep both ratios between 125 and 10000 with initial above maintenance; the program's admin path
enforces that and nothing else checks it.

## Make collateral count for less

```
template: velocity-spot-market-risk        # account: the SOL spot market
maintenance_asset_weight: 5000             # SOL deposits count for half against liquidation
initial_asset_weight: 4500                 # keep it at or below maintenance
```

Accounts backing positions with SOL become liquidatable without a price move. USDT, the quote
asset, keeps every weight at 10000.

## Crash deeper than the band

Combine both in one scenario:

```
template: velocity-exchange-risk
oracle_guard_rails.price_divergence.oracle_twap_5min_percent_divergence: 1000000   # 100%
oracle_guard_rails.validity.too_volatile_ratio: 10                                   # needed past 80% down

template: velocity-oracle-price
price: <live price x 0.15>                 # an 85% crash
```

## Change what a liquidation pays and closes

```
template: velocity-perp-market-risk
liquidator_fee: 20000                      # the liquidator earns 2% of the value it takes over
if_liquidation_fee: 15000                  # the account pays 1.5% to the insurance fund

template: velocity-exchange-risk
liquidation_margin_buffer_ratio: 2000      # restore maintenance plus 20%: closes more per liquidation
```

The liquidator fee applies to an account active in the last 10 minutes; for an idle account the
program raises it over time, up to 3 times `liquidator_fee` or the maintenance margin, whichever is
lower. The insurance fund fee is capped by the margin the account has left. Keep the two fees
together below `margin_ratio_maintenance` x 100, as the program's admin path does.

## Make trading more expensive

```
template: velocity-fees
promo_fee_tier: 3                          # every account pays tier 3
perp_fee_structure.fee_tiers.3.fee_numerator: 100
perp_fee_structure.fee_tiers.3.fee_denominator: 100000   # a 0.1% taker fee
```

An account pays the fees of the higher of its 30-day volume tier and `promo_fee_tier`, and the
program uses tiers 0 to 3 only, so with the promo tier at 3 every account pays tier 3. A taker pays
the fill's quote value times the fee, rounded up; a market's `taker_fee_addon_tenth_bps` and
`fee_adjustment` apply on top. Keep the maker rebate below the taker fee: a matched fill pays the
rebate out of the taker fee.

## Make borrowing expensive

```
template: velocity-spot-market-risk        # account: the USDT spot market
optimal_borrow_rate: 1000000
max_borrow_rate: 1000000
min_borrow_rate: 200                       # 100% a year at any utilization
```

The yearly borrow rate rises linearly to `optimal_borrow_rate` at `optimal_utilization`, then
towards `max_borrow_rate` at full utilization, and never goes below `min_borrow_rate` x 0.5%.
Interest accrues when anyone sends `update_spot_market_cumulative_interest` or moves the market's
balances.

## Charge funding

```
template: velocity-funding                 # account: the market
cumulative_funding_rate_long: "<live + 72450000>"    # longs pay $0.07245 per SOL
cumulative_funding_rate_short: "<live + 72450000>"   # shorts earn the same
net_unsettled_funding_pnl: <live - 72450000 x (base_asset_amount_long + base_asset_amount_short) / 10^12>
```

Each position keeps the total it last settled at, and when it settles its quote moves by
-(total - kept) x base / 10^12: a step up charges longs and pays shorts, and the two totals can take
different steps. A position settles when anyone sends `settle_funding_payment` for its account, and
before every fill of it. A step for a rate is rate x the oracle price at 10^6 x 1000, so 0.1% of
$72.45 is `72450000`. `net_unsettled_funding_pnl` takes its live value minus (long step x
`base_asset_amount_long` + short step x `base_asset_amount_short`) / 10^12, which is what a funding
update books; each settlement takes its payment back out.

The market's `last_funding_rate`, `last_funding_rate_long` and `last_funding_rate_short` record what
the last update charged; settlements read only the totals. `update_funding_rate`, which anyone can
send once each `funding_period` after `last_funding_rate_ts`, adds the market's own rate on top of
the step, settles the AMM's side of both and books only its own rate as unsettled.

## Cap an account's leverage

```
template: velocity-account-leverage        # account: the trader's User
max_margin_ratio: 5000                     # at most 2x in every market
```

An order that adds risk needs collateral of position value x the higher of the market's
`margin_ratio_initial` and the caps, or fails with `InsufficientCollateral` (6003). For one market
only, write `perp_positions.<slot>.max_margin_ratio` and, on a free slot, its `market_index` (see
[Picking an account](#picking-an-account)). On a free slot the program keeps the cap only in the
first free slot and only for the market written there. A cap below the market's ratio changes
nothing; to raise every account's leverage, lower `margin_ratio_initial` with
`velocity-perp-market-risk`.

## Give an account a position

```
template: velocity-perp-position           # account: the trader's User, slot 0 free
perp_positions.0.market_index: 0           # SOL-PERP
perp_positions.0.base_asset_amount: 100000000000                # 100 SOL long
perp_positions.0.quote_asset_amount: -7000000000                # entered at $70
perp_positions.0.quote_entry_amount: -7000000000
perp_positions.0.quote_break_even_amount: -7000000000
perp_positions.0.last_cumulative_funding_rate: <market cumulative_funding_rate_long>

template: velocity-position-totals         # account: the market, same slot
base_asset_amount_long: "<live + 100000000000>"
amm.base_asset_amount_with_amm: "<live + 100000000000>"
quote_asset_amount: "<live - 7000000000>"
```

The position counts in margin as one opened by a fill, and fills, PnL settles and liquidations take
it like any other. The account keeps its collateral; an entry away from the oracle price is PnL it
has at once, so a long entered far enough above the price, or a short far enough below, is
liquidatable at once. Stamp `last_cumulative_funding_rate` with the market's total for the side, or
the next settlement charges the difference x the base. For a position the account already holds,
add the new values minus the current ones to the totals.

The market's totals count every position, and the AMM holds the other side of what they say users
hold. The same change in base to the side's total and to `amm.base_asset_amount_with_amm` keeps
`base_asset_amount_long + base_asset_amount_short == amm.base_asset_amount_with_amm`, which every
fill checks first. Without `velocity-position-totals` nothing fails, but the position has no
counterparty: once it closes, the market's total for its side is short by its size. The market's
other counters, `quote_entry_amount_*`, `quote_break_even_amount_*`, `number_of_users` and
`number_of_users_with_base`, are left as they are; the program checks them only when a market is
deleted or its pools are settled at expiry.

## Halt liquidations

`exchange_status: 16` pauses them on every market (`ExchangePaused`, 6024);
`paused_operations: 32` on one market pauses them there (`InvalidLiquidation`, 6220). Add bits to
the live value rather than replacing it.

## Cap open interest or close a market to new risk

`max_open_interest` at or below a side's open position makes every order that adds to that side
fail with `MaxOpenInterest` (6154). `status: {"ReduceOnly": null}` refuses any order that adds risk
(6046).

# Troubleshooting

| Error | Meaning |
|---|---|
| `SufficientCollateral` (6004) on liquidation | the account still covers its maintenance requirement; move the price further, raise `margin_ratio_maintenance` or lower the collateral's `maintenance_asset_weight` |
| `PriceBandsBreached` (6023) | the price is more than 50% (or `oracle_twap_5min_percent_divergence`) from the 5-minute TWAP |
| `ExchangePaused` (6024) | the State is copied with mainnet's pause switches; write `exchange_status: 0` with `velocity-exchange-risk` |
| `InvalidOracle` (6035) on a withdrawal | an oracle nothing reposts, such as the USDT price before a `velocity-oracle-price` override plays or one written with `keepAlive: false`, is older than the margin window (240 slots, 720 for USDT); play `velocity-oracle-price`, turn keep-alive on for it again, or raise `slots_before_stale_for_margin` with `velocity-exchange-risk`. A fresh price is invalid for another reason, such as being more than `too_volatile_ratio` times from its TWAP |
| `PlaceAndTakeOrderSuccessConditionFailed` (6289) | nothing filled the order: a matched fill met a price past the margin window, or the AMM refused a price older than its window; both happen only to an oracle nothing reposts |
| `fill_perp_order` fills nothing and logs `taker does not cross amm` | the order's auction price has not reached the AMM's quote; on an oracle nothing reposts, a market order's auction can outlast the AMM's 20-slot window, so price a limit order through the quote |
| `taker does not cross amm` right after a scenario moves a price | nobody repegs the AMM on a fork, so it still quotes the price it was fetched with; send the permissionless `update_amms` for the market first, while its oracle is under 10 slots old |
| `AMMNotUpdatedInSameSlot` (6096) on `settle_pnl` | the price moved from the market's 5-minute TWAP; send `update_amms` for the market in the same transaction |
| `OracleStaleForAMM` (6266) or `OracleStaleForMargin` (6264) on `settle_pnl` | the price is more than 20 slots old for a settle after a move: nothing reposts the oracle yet, or an override turned keep-alive off for it; play `velocity-oracle-price` |
| `MaxDeposit` (6038) | the spot market's deposits would pass `max_token_deposits` |
| `InsufficientCollateral` (6003) on an order | the order would take the account past the higher of the market's `margin_ratio_initial` and the account's or position's `max_margin_ratio` |
| a per-market cap did nothing | it sits on a free slot other than the first, or the account opened another market in that slot first; write it on the market's slot or the first free one |
| `InvalidAmmDetected` (6168) on every fill | `base_asset_amount_long + base_asset_amount_short` no longer equals `amm.base_asset_amount_with_amm`; write the same change in base to both with `velocity-position-totals` |
| a written position's first settlement charges a large funding payment | its `last_cumulative_funding_rate` is not the market's total for its side |
| `status` override did nothing | it must be an object such as `{"Active": null}`, not a number or a bare string |
| a `u64` override did nothing | `u64` fields such as `max_token_deposits` take JSON numbers; only 128-bit fields take decimal strings |
| an earlier change to the State, a market or an oracle is gone, such as an unpause | a later override with `fetchBeforeUse: true` loaded the account from mainnet again, which drops every local change to it; give later overrides of the same account `fetchBeforeUse: false` |

# IDL

`idl.json` is the IDL Velocity publishes on chain in its Program Metadata account
`Bv2hvSEESQELxaqhtm7gGqGpquEVEgFcsArTWLif56ci`, unchanged (sha256
`1a746c36f818be772c8adfccc2393e197ab879c23093dcfab929c564608a62b2`). Velocity rewrites that account
with every upgrade; this copy was written in slot 464750765 together with the deployed release,
version 2.170.0 of [velocity-exchange/velocity-v1](https://github.com/velocity-exchange/velocity-v1).
The IDL in the npm package `@velocity-exchange/sdk` adds LP pool and devnet instructions and
accounts that the published one leaves out.

# Tests against mainnet

```sh
cargo test -p surfpool-core --features integration-tests velocity -- --test-threads=1
```

They run the deployed program from its ProgramData account. Set `SURFPOOL_TEST_RPC_URL` to use a
private endpoint if the public one rate-limits.
