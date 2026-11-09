# Jupiter Perps

Templates for the Jupiter Perps exchange and the JLP pool behind it, across two programs:

| Program | Address | Collection |
|---|---|---|
| Jupiter Perps | `PERPHjGBqRHArX4DySjwM6UJHiR3sWAatqfdBS2qQJu` | `jupiter-perps` (this folder) |
| Doves oracle | `DoVEsk76QybCEHQGzkvYPWLQu9gzNoZZZt3TPiL597e` | `jupiter-perps-doves` (`doves/v1`) |

Perps reads every price from a Doves `AgPriceFeed`. It never reads the custody's Pyth account, so
`pyth-price-feed-v2` does not move Perps prices. Surfpool keeps a feed published on its own once a
scenario overrides it.

# Template index

| Template | Overrides |
|---|---|
| `jupiter-perps-doves-price` | the Doves price Perps reads for one asset |
| `jupiter-perps-custody-risk` | a market's liquidation leverage, size caps and action switches |
| `jupiter-perps-custody-borrow-rate` | the borrow-rate curve a custody charges positions |
| `jupiter-perps-custody-fees` | the fees a market charges to open and close positions |
| `jupiter-perps-pool-limits` | the JLP pool's AUM cap, weight buffer and price guard |
| `jupiter-perps-pool-fees` | the JLP pool's deposit, withdrawal and swap fees |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `_usd` fields, position prices | USD with 6 decimals | $80 -> `80000000` |
| Doves `price` | USD x 10^-expo, expo -8 | $80 -> `8000000000` |
| `_bps` | basis points, 10^4 = 100% | `500` = 5% |
| `pricing.max_leverage` | leverage x 10^4 | `5000000` = 500x |
| `pricing.trade_impact_fee_scalar` | USD with 6 decimals; the price impact fee rate is size / scalar, rounded up to a whole BPS | `1000000000000` charges a $1,000 trade 10 BPS |
| `target_utilization_rate`, `cumulative_interest_rate` | scaled by 10^9 | `800000000` = 80% |

`limit.max_aum_usd` and `limit.token_weightage_buffer_bps` are `u128`: pass them as decimal strings.

## Finding a market

Nothing but the pool is hardcoded; custodies and feeds are read live.

1. The JLP Pool `5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq` lists its custodies in `custodies`.
2. Each custody holds one token: its `mint`, and its feed in `doves_ag_oracle`.

A `getAccountInfo` or `getMultipleAccounts` with `jsonParsed` encoding returns these accounts
decoded. Positions live on the custody of the traded token, longs and shorts alike; a short's
collateral sits in the USDC or USDT custody.

# Recipes

## Crash a price

```
template: jupiter-perps-doves-price      address: the custody's doves_ag_oracle
price: 8000000000                        # $80
```

Perps refuses a stale feed: a JLP deposit once a feed is the custody's `oracle.max_price_age_sec`
old (5 seconds), a quote once it is three times that, at most 15, and a keeper fill unless the
price was published after the position request. JLP deposits, withdrawals, swaps and quotes read
every custody's feed; positions read their own feeds; liquidations do not check the age.

## Keep-alive (Surfpool handles it)

On mainnet the Doves publisher republishes every feed's price within seconds. Nobody runs it
against a surfnet, so from the slot a `jupiter-perps-doves-price` override plays, Surfpool
republishes every Doves feed the surfnet has loaded every slot
([keep-alive](../../../README.md#keep-alive)), including feeds a transaction loads later: each
`timestamp` follows the Surfnet clock and each price stays the scenario's. A JLP deposit,
withdrawal, swap or quote, or a position, finds every custody's feed fresh, and a keeper fill finds
a price published after its request. Keep-alive runs until the network is reset.

To keep one feed stale on purpose, override it with `keepAlive: false`; a later override of it with
`keepAlive: true` republishes it again.

## Make a position liquidatable

A keeper liquidates a position once `collateral + PnL - close and borrow fees` falls below
`size x 10^4 / pricing.max_leverage`. Three levers reach it without touching the position:

```
jupiter-perps-doves-price    price: <past the position's liquidation price>
jupiter-perps-custody-risk   pricing.max_leverage: 200000     # liquidate everything above 20x
jupiter-perps-custody-fees   decrease_position_bps: <above the position's spare margin>
```

Liquidation is keeper-only: the deployed program accepts one hardcoded keeper key. On a surfnet
running `--skip-signature-verification`, a transaction can be sent as that keeper.

A cascade is a price override per asset in the same slot.

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

# Troubleshooting

| Error | Meaning | Fix |
|---|---|---|
| 6003 `StaleOraclePrice` | a feed the instruction read is too old: 5 seconds for a deposit, 15 for a quote | expected before any `jupiter-perps-doves-price` override plays, or on a feed overridden with `keepAlive: false`: override one feed, at its live price if the scenario does not move it - see "Keep-alive" |
| 6009 `InvalidPositionState` | the position is not liquidatable | move the price further, or lower `pricing.max_leverage` |
| 6014 `InvalidCustodyBalance` | a custody's vault and staked lamports hold less than its `assets.owned`: the two were copied from mainnet at different moments, as when Studio's market list or a lookup read the custody first, or `fetchBeforeUse` reloaded only the custody | `surfnet_resetAccount` on the custody and on its `token_account`; the next transaction loads them together |
| 6022 `MaxLeverage` | the position would open past its leverage limit, as when its close fee exceeds its collateral | lower `decrease_position_bps` or add collateral |
| 6024 `PoolAmountLimit` | the deposit would exceed `limit.max_aum_usd` | raise the cap |
| 6027 `InstructionNotAllowed` | a `permissions` switch is off | set it back to `true` |
| 6034 `InvalidKeeper` | the liquidator is not the keeper the program accepts | sign as the keeper |
| 6043 `ExceedTokenWeightage` | the deposit leaves the token too far above its target weight | raise `limit.token_weightage_buffer_bps` |
| 6045 `PullOraclePublishTimeTooEarly` | a keeper filled a position request with a price published no later than the request | override a feed before the fill, and fill in a later second than the request - see "Keep-alive" |
| 6066 `LpTokenPriceChangeLimitExceeded` | the JLP price moved past the guard | raise `max_lp_token_price_change_bps` |
| 6000 `MathOverflow` on a stablecoin custody | its borrow-lend debt overflowed a large rate or time jump | lower the rate or the time step |
| an earlier change to a custody, the pool or a feed is gone | a later override with `fetchBeforeUse: true` loaded the account from mainnet again, which drops every local change to it | give later overrides of the same account `fetchBeforeUse: false` |

# IDL provenance

Perps publishes a legacy Anchor IDL on chain, in account `38GK1i4cQPAxqrbfKX4RRMNfXpKRn5PgLFHVJeXm1C8Y`,
which the engine cannot read. `idl.json` is that IDL after `anchor idl convert` (anchor-cli
0.31.1), unchanged.

| File | sha256 |
|---|---|
| on-chain legacy IDL | `1292d2ecec3f6c17501704a423f1e0f4c2cde25b7821cabeeb2b98eb52168861` |
| `idl.json` | `8d9860d8baf41478d8a44f9507611043ffa685c8d1e801ee032c16817a46b3f3` |

Doves publishes a current Anchor IDL in account `4D2RLxRVWjGUp7E45noWC6x7BfmSQbQ4mQevHvTV5ai6`.
`doves/v1/idl.json` is that IDL as fetched, unchanged (sha256
`4945d1b7f0782cf0221e0fc9953ec2f8560604ea8aeb79fd76962a2417e71b2c`).
