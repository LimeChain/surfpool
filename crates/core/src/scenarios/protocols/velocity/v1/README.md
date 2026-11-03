# Velocity

Velocity is a perpetuals exchange built on the Drift v2 program. Surfpool bundles its IDL and five
override templates, so a scenario can move a market's oracle price, tighten a market's margin
requirements, change trading fees, collateral weights, borrow rates and liquidation fees, or pause
actions across the exchange. The templates only prepare state: trades, withdrawals and
liquidations are sent by your own code. For how scenarios work in general see the
[scenarios README](../../../README.md).

# Template index

**Velocity** &middot; `vELoC1audYbSYVRXn1vPaV8Axoa9oU6BYmNGZZBDZ1P`

| Template | Account | Overrides |
|---|---|---|
| `velocity-oracle-price` | `PythLazerOracle` | the price a market reads, with its confidence, posted slot and publish time |
| `velocity-perp-market-risk` | `PerpMarket` | margin ratios, status, paused operations, the open interest cap and liquidation fees |
| `velocity-exchange-risk` | `State` | exchange-wide pauses, how long margin checks accept a price, the price guard rails liquidation checks and the liquidation margin buffer |
| `velocity-fees` | `State` | the taker fee and maker rebate every perp trader pays |
| `velocity-spot-market-risk` | `SpotMarket` | collateral and borrow weights, the borrow rate curve, status, paused operations and the deposit cap |

## Number formats

| Field | Meaning | Example |
|---|---|---|
| `price`, `conf` | USD x 10^-`exponent`, read from the oracle (-8 on every live oracle today) | `8500000000` = $85 at -8 |
| `posted_slot` | the slot the price counts as posted at; write the current slot | |
| `publish_time` | Pyth Lazer timestamp in microseconds | unix seconds x `1000000` |
| `margin_ratio_maintenance`, `margin_ratio_initial` | share of position value, `10000` = 100% | `300` = 3% |
| `max_open_interest` | base units per side, `10^9` = one unit of the asset, as a decimal **string**; `0` = no cap | |
| `status` | enum object | `{"ReduceOnly": null}` |
| perp `paused_operations` | bits: `1` funding, `2` AMM fills, `4` fills, `8` settle PnL, `16` settle PnL with position, `32` liquidation, `64` AMM immediate fills, `128` settle revenue pool | `32` |
| `liquidator_fee`, `if_liquidation_fee` | share of the liquidated position's value, `1000000` = 100% | `7500` = 0.75% |
| `exchange_status` | bits: `1` deposits, `2` withdrawals, `4` AMM, `8` fills, `16` liquidations, `32` funding, `64` settle PnL, `128` AMM immediate fills | `16` |
| `slots_before_stale_for_margin` | units of 400 ms, two slots each at the program's 200 ms slot clock | `120` = 240 slots |
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

Set `fetchBeforeUse: true` to pull the account from upstream before the write.

## Prices on a fork

Nothing republishes Velocity's oracles on a surfnet, so a price only changes when a scenario writes
it, and a scenario that moves a price writes `posted_slot` at the current slot. The program ages a
price in slots, `clock.slot - posted_slot`:

- Margin checks, such as withdrawals, orders that add risk and matched fills, refuse a price older
  than the State's `oracle_guard_rails.validity.slots_before_stale_for_margin` (`InvalidOracle`,
  6035). It is 120 units of 400 ms, which is 240 slots at the program's 200 ms slot clock, and
  three times that for the USDT price. Raise it with `velocity-exchange-risk` to keep a moved price
  usable longer; the program's admin path allows up to `1000000` (2,000,000 slots).
- AMM fills need a recent price. An order placed after the price fills from the AMM at once only
  within the market's `oracle_slot_delay_override` (the price's own slot when it is negative). An
  order placed in the price's slot or before it, and a PnL settle right after a price move, need
  the price posted within the State's `slots_before_stale_for_amm`, 20 slots, unless the market
  sets its own `oracle_low_risk_slot_delay_override`. The program reads both as `i8` units of
  400 ms, so neither reaches past about 254 slots, and a State `slots_before_stale_for_amm` above
  127 fails every fill.
- Liquidations never check a price's age.

Keep-alive ([solana-foundation/surfpool#906](https://github.com/solana-foundation/surfpool/pull/906))
will keep prices published once it lands.

`publish_time` decides which price fills read. A market keeps its own MM oracle price, and fills
read that instead when its `market_stats.mm_oracle_sequence_id` is newer than `publish_time` and
the two prices are within 1% of each other. For a move under 1%, write `publish_time` above that
sequence id.

# Recipes

## Make positions liquidatable with a price move

```
template: velocity-oracle-price            # account: the market's oracle
price: <live price x 0.8>                  # a 20% drop
posted_slot: <current slot>
publish_time: <clock unix timestamp x 1000000>
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
posted_slot: <current slot>
publish_time: <clock unix timestamp x 1000000>
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
| `InvalidOracle` (6035) on a withdrawal | the price is older than the margin window (240 slots, 720 for USDT); write it again at the current slot or raise `slots_before_stale_for_margin` with `velocity-exchange-risk`. A fresh price is invalid for another reason, such as being more than `too_volatile_ratio` times from its TWAP |
| `PlaceAndTakeOrderSuccessConditionFailed` (6289) | nothing filled the order: a matched fill met a price past the margin window, or the AMM refused a price older than its window; write the price again at the current slot |
| `fill_perp_order` fills nothing and logs `taker does not cross amm` | the order's auction price has not reached the AMM's quote; a market order's auction can outlast the AMM's 20-slot window, so price a limit order through the quote |
| `AMMNotUpdatedInSameSlot` (6096) on `settle_pnl` | the price moved from the market's 5-minute TWAP; send `update_amms` for the market in the same transaction |
| `OracleStaleForAMM` (6266) or `OracleStaleForMargin` (6264) on `settle_pnl` | the price is more than 20 slots old for a settle after a move; write the price again at the current slot |
| `MaxDeposit` (6038) | the spot market's deposits would pass `max_token_deposits` |
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

They run the deployed program from its ProgramData account and pin the deploy slot, so a Velocity
upgrade fails them until the IDL is checked against the new release. Set `SURFPOOL_TEST_RPC_URL` to
use a private endpoint if the public one rate-limits.
