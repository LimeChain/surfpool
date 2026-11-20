# Jupiter

Templates for three Jupiter programs: the v6 swap aggregator, and the Jupiter Perps exchange with
the JLP pool behind it, which takes its prices from the Doves oracle.

| Program | Address | Collection |
|---|---|---|
| Jupiter v6 | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` | `Jupiter` (`v6`) |
| Jupiter Perps | `PERPHjGBqRHArX4DySjwM6UJHiR3sWAatqfdBS2qQJu` | `jupiter-perps` (`perps/v1`) |
| Doves oracle | `DoVEsk76QybCEHQGzkvYPWLQu9gzNoZZZt3TPiL597e` | `jupiter-perps-doves` (`doves/v1`) |

Perps reads every price from a Doves `AgPriceFeed`. It never reads the custody's Pyth account, so
`pyth-price-feed-v2` does not move Perps prices. Surfpool keeps each Doves feed a scenario overrides
published.

# Template index

## Jupiter v6

| Template | Overrides |
|---|---|
| `jupiter-token-ledger-override` | the intermediate token amounts of a multi-hop swap |

It writes both `TokenLedger` fields: the token account and the amount. The template has no fixed
address: each override names the `TokenLedger` account by its pubkey.

## Jupiter Perps and Doves

| Template | Overrides |
|---|---|
| `jupiter-perps-doves-price` | the Doves price Perps reads for one asset |
| `jupiter-perps-custody-risk` | a market's leverage limit, size caps and action switches |
| `jupiter-perps-custody-borrow-rate` | the borrow-rate curve a custody charges positions |
| `jupiter-perps-custody-borrow-fee` | a borrow fee charged to a custody's positions at once |
| `jupiter-perps-custody-fees` | the fees a market charges to open and close positions |
| `jupiter-perps-pool-limits` | the JLP pool's AUM cap, weight buffer and price guard |
| `jupiter-perps-pool-fees` | the JLP pool's deposit, withdrawal and swap fees |
| `jupiter-perps-position` | one position's size, entry price and collateral |
| `jupiter-perps-position-totals` | the custody totals that count a changed position |

### Number formats

| You'll see | It means | Example |
|---|---|---|
| `_usd` fields, position prices | USD with 6 decimals | $80 -> `80000000` |
| Doves `price` | USD x 10^-expo, expo -8 | $80 -> `8000000000` |
| `_bps` | basis points, 10^4 = 100% | `500` = 5% |
| `pricing.max_leverage` | leverage x 10^4 | `5000000` = 500x |
| `pricing.trade_impact_fee_scalar` | USD with 6 decimals; the price impact fee rate is size / scalar, rounded up to a whole BPS | `1000000000000` charges a $1,000 trade 10 BPS |
| `target_utilization_rate`, `cumulative_interest_rate` | scaled by 10^9 | `800000000` = 80% |
| `locked_amount`, `assets.locked` | tokens of the collateral custody, in its smallest unit | 1 SOL -> `1000000000` |

`limit.max_aum_usd` and `limit.token_weightage_buffer_bps` are `u128`: pass them as decimal strings.

### Finding a market

Nothing but the pool is hardcoded; custodies and feeds are read live.

1. The JLP Pool `5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq` lists its custodies in `custodies`.
2. Each custody holds one token: its `mint`, and its feed in `doves_ag_oracle`.

A `getAccountInfo` or `getMultipleAccounts` with `jsonParsed` encoding returns these accounts
decoded. Positions live on the custody of the traded token, longs and shorts alike; a short's
collateral sits in the USDC or USDT custody.

### Finding a position

`getProgramAccounts` on the Perps program with `base64` encoding, the filters
`[{"dataSize":216},{"memcmp":{"offset":8,"bytes":"<wallet>"}}]` and a zero-length `dataSlice`
lists a wallet's positions, one per market, collateral token and side. Read them with
`getMultipleAccounts` and `jsonParsed` through the surfnet, which decodes them with the bundled IDL;
mainnet cannot decode them. A closed position keeps its account with `size_usd` 0.

# Perps recipes

## Crash a price

```
template: jupiter-perps-doves-price      address: the custody's doves_ag_oracle
price: 8000000000                        # $80
```

Perps refuses a stale feed: a JLP deposit once a feed is the custody's `oracle.max_price_age_sec`
old (5 seconds), a quote once it is three times that, at most 15, and a keeper fill unless the
price was published after the position request. JLP deposits, withdrawals, swaps and quotes read
every custody's feed; positions read their own feeds; liquidations do not check the age.

## Keep prices published

On mainnet the Doves publisher republishes every feed's price within seconds. Nobody runs it
against a surfnet, so from the slot a `jupiter-perps-doves-price` override plays, Surfpool
republishes the feed it wrote every slot ([keep-alive](../../README.md#keep-alive)): its
`timestamp` follows the Surfnet clock and its price stays the scenario's, until the network is
reset. A feed no override wrote ages against the Surfnet clock.

Give every feed the transaction reads one override, in the same scenario and slot. A JLP deposit,
withdrawal, swap or quote reads every custody's feed. On a feed whose price does not move, write
only `timestamp`: keep-alive writes the clock time over it in the same slot.

```
template: jupiter-perps-doves-price      address: each custody's doves_ag_oracle
price: 8000000000                        # the feed that moves
timestamp: 0                             # each other feed
```

To keep one feed stale, leave it out. A feed an earlier override keeps published ages from the
slot of a later override of it with `"keepAlive": []` and a value, such as its `timestamp`.

## Make a position liquidatable

A keeper liquidates a position once `collateral + PnL - close and borrow fees` falls below
`size x 10^4 / pricing.max_leverage`. Three levers reach it without touching the position:

```
jupiter-perps-doves-price    price: <past the position's liquidation price>
jupiter-perps-custody-risk   pricing.max_leverage: 200000     # liquidate everything above 20x
jupiter-perps-custody-fees   decrease_position_bps: <above the position's spare margin>
```

Only a keeper can send the liquidation; see "Keepers" under "Perps limits".

A cascade is a price override per asset in the same slot.

## Cap leverage

`pricing.max_leverage` is also the most a position may open at. A keeper's fill of an open or
increase fails with 6022 once `size x 10^4 / (collateral after the open fee - close fee)` is above
it; the trader's request does not check it.

```
template: jupiter-perps-custody-risk      address: the market's custody
pricing.max_leverage: 100000              # opens above 10x fail, positions above 10x are liquidated
```

## Charge positions faster

```
template: jupiter-perps-custody-borrow-rate
jump_rate_state.min_rate_bps: 100000         # 1000% a year, flat
jump_rate_state.target_rate_bps: 100000
jump_rate_state.max_rate_bps: 100000
```

The fee accrues over the time since the custody's last update, whenever a transaction touches the
custody. A long pays its own custody's rate; a short's fee follows its
stablecoin collateral custody.

## Charge a borrow fee now

```
template: jupiter-perps-custody-borrow-fee     address: the custody that charges the positions
funding_rate_state.cumulative_interest_rate: "<live value + 10000000>"    # 1% of each position's size
```

Each position pays `(cumulative_interest_rate - its cumulative_interest_snapshot) x size_usd / 10^9`
when it closes, decreases or is liquidated, and counts it against its margin until then. Read the
custody as the surfnet holds it and leave `fetchBeforeUse` false. A long pays its own custody; a
short pays its USDC or USDT collateral custody, never the traded token's. Transactions keep adding
the ordinary rate on top. The pool's value, and so the JLP price, counts the fee only once a
position pays it.

## Change a position

```
jupiter-perps-position          address: the position
size_usd, price, collateral_usd, locked_amount, cumulative_interest_snapshot
jupiter-perps-position-totals   address: each custody the position names, same slot
```

The pool's value and every close or liquidation read the custodies' totals, never the positions,
so every change to a position needs the totals override in the same slot. Without it the JLP price
ignores the change, and closing or liquidating the position leaves the totals off by it.

| Side | Custody | Totals to add the change to |
|---|---|---|
| long | its custody | `assets.locked` (+ change in `locked_amount`), `assets.guaranteed_usd` (+ change in `size_usd - collateral_usd`) |
| short | its collateral custody | `assets.locked` (+ change in `locked_amount`) |
| short | its custody | `assets.global_short_sizes` (+ change in `size_usd`), `assets.global_short_average_prices` (the size-weighted harmonic mean, in the template's `llm_context`) |

A long's `locked_amount` is `size_usd x 10^decimals / price`; a short's is `size_usd` in its
stablecoin at that coin's Doves price. A long's collateral is part of the pool's tokens: the pool
pays for an increase and keeps a decrease. A short's collateral sits outside them, so keep it at or
below its current value.

## Make trading expensive

```
template: jupiter-perps-custody-fees     address: the market's custody
increase_position_bps: 100                # 1% of size to open
decrease_position_bps: 100                # 1% of size to close or liquidate
```

Fees come out of the position's collateral. Closing rounds the fee up. Both directions also pay a
price impact fee of `size x ceil(size x 10^4 / pricing.trade_impact_fee_scalar) / 10^4`. A higher
close fee counts against the margin, so it makes positions liquidatable at an unchanged price.

## Raise JLP and swap fees

```
template: jupiter-perps-pool-fees
fees.add_remove_liquidity_bps: 100        # 1% on deposits and withdrawals
fees.tax_bps: 0                           # no weight tax: every deposit pays exactly 1%
```

`fees.swap_bps` is the base fee on swaps that involve a non-stable token, and `fees.stable_swap_bps`
on swaps between two stablecoins. A non-stable custody multiplies its swap fee by
`1 + external_swap_fee_multiplier_bps / 10^4`. The weight tax (`fees.tax_bps`,
`fees.stable_swap_tax_bps`) raises the fee of a deposit or swap that moves a token further from its
target weight and lowers it, never below 0, for one that moves it back.
`getAddLiquidityAmountAndFee2` and `getRemoveLiquidityAmountAndFee2` return the fee in BPS.

## Refuse JLP deposits

```
jupiter-perps-pool-limits    limit.max_aum_usd: "1"                       # 6024
jupiter-perps-custody-risk   permissions.allow_add_liquidity: false        # 6027, one token
```

## Keep JLP open after a crash

A shock that moves the JLP price more than `max_lp_token_price_change_bps` (500 = 5%) from the
pool's recorded value refuses deposits, so a large shock on a heavily weighted token trips it.

```
jupiter-perps-pool-limits    max_lp_token_price_change_bps: 10000
```

A large crash also shifts every token's weight; raise `limit.token_weightage_buffer_bps` if
deposits then fail with 6043.

# Perps limits

## Keepers

The deployed Perps program accepts position fills and liquidations only from four keeper keys
compiled into it:

- `DFZcDnmEYNUK1khquZzx5dQYiEyjJ3N5STqaDVLZ88ZU`
- `A2rCQdCqQB4wu72x9Q7iq1QugtdZmcyWsiSPfgfZzu2u`
- `Ag28fGtwtpnqassHURUBsQ1WfiyaWWzDDNs4Q28qHRjv`
- `H3dDE6K6uqBp5kBKctH6w8hzpV5eAmxUiqvcdHxuE9i4`

No account holds them, so no override adds a keeper. On a surfnet, a fill or a liquidation runs
only when the surfnet starts with `--skip-signature-verification` and the transaction is sent as
one of these keys. User instructions, such as JLP deposits and position requests, are not affected.

# Perps troubleshooting

| Error | Meaning | Fix |
|---|---|---|
| 6003 `StaleOraclePrice` | a feed the instruction read is too old: 5 seconds for a deposit, 15 for a quote | expected on a feed no `jupiter-perps-doves-price` override wrote, or one a `"keepAlive": []` override stopped: give every feed the instruction reads an override, with only `timestamp` on a feed whose price does not move - see "Keep prices published" |
| 6009 `InvalidPositionState` | the position is not liquidatable | move the price further, or lower `pricing.max_leverage` |
| 6014 `InvalidCustodyBalance` | a custody's vault and staked lamports hold less than its `assets.owned`: the two were copied from mainnet at different moments, as when Studio's market list or a lookup read the custody first, or `fetchBeforeUse` reloaded only the custody | `surfnet_resetAccount` on the custody and on its `token_account`; the next transaction loads them together |
| 6022 `MaxLeverage` | a keeper's fill would open the position above `pricing.max_leverage`, counting the collateral after the open fee less the close fee | raise `pricing.max_leverage`, lower `decrease_position_bps` or add collateral |
| 6024 `PoolAmountLimit` | the deposit would exceed `limit.max_aum_usd` | raise the cap |
| 6027 `InstructionNotAllowed` | a `permissions` switch is off | set it back to `true` |
| 6034 `InvalidKeeper` | the fill or liquidation was not sent by one of the keeper keys | send it as a keeper key on a surfnet started with `--skip-signature-verification` - see "Keepers" |
| 6043 `ExceedTokenWeightage` | the deposit leaves the token too far above its target weight | raise `limit.token_weightage_buffer_bps` |
| 6045 `PullOraclePublishTimeTooEarly` | a keeper filled a position request with a price published no later than the request | override the position's feeds before the fill, and fill in a later second than the request - see "Keep prices published" |
| 6066 `LpTokenPriceChangeLimitExceeded` | the JLP price moved past the guard | raise `max_lp_token_price_change_bps` |
| 6000 `MathOverflow` on a stablecoin custody | its borrow-lend debt overflowed a large rate or time jump | lower the rate or the time step |
| a changed position does not move the JLP price, or its close leaves the custody totals wrong | `jupiter-perps-position` played without `jupiter-perps-position-totals` | add the totals override in the same slot |
| an earlier change to a custody, the pool or a feed is gone | a later override with `fetchBeforeUse: true` loaded the account from mainnet again, which drops every local change to it | give later overrides of the same account `fetchBeforeUse: false` |

# IDL provenance

Perps publishes a legacy Anchor IDL on chain, in account `38GK1i4cQPAxqrbfKX4RRMNfXpKRn5PgLFHVJeXm1C8Y`,
which the engine cannot read. `perps/v1/idl.json` is that IDL after `anchor idl convert` (anchor-cli
0.31.1), unchanged.

| File | sha256 |
|---|---|
| on-chain legacy IDL | `1292d2ecec3f6c17501704a423f1e0f4c2cde25b7821cabeeb2b98eb52168861` |
| `perps/v1/idl.json` | `8d9860d8baf41478d8a44f9507611043ffa685c8d1e801ee032c16817a46b3f3` |

Doves publishes a current Anchor IDL in account `4D2RLxRVWjGUp7E45noWC6x7BfmSQbQ4mQevHvTV5ai6`.
`doves/v1/idl.json` is that IDL as fetched, unchanged (sha256
`4945d1b7f0782cf0221e0fc9953ec2f8560604ea8aeb79fd76962a2417e71b2c`).
