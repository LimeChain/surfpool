# Save

Save, formerly Solend, is a pool lender. Program `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo`,
build 2.0.2. Six templates: four on reserves and two on obligations.

Save publishes no IDL and its accounts carry no discriminator, so the templates write bytes at fixed
offsets. They describe a 619-byte Reserve and a 1300-byte Obligation. Point them only at accounts of
that kind; the account is the caller's responsibility.

Save stores settings someone chose (`liquidation_threshold`) and values it computed from them
(`market_price`, the obligation's `borrowed_value`). Every borrow, withdraw and liquidation needs
`refresh_reserve` and `refresh_obligation` in the same slot, and those recompute every computed
value. So override inputs, not results:

| Want to change | Override this | Not this |
|---|---|---|
| A price | `pyth-price-feed-v2` on the reserve's oracle | the reserve's `market_price` |
| Position health | `save-reserve-risk` → `config.liquidation_threshold` | the obligation's `*_value` fields |

# Template index

| Template | Overrides |
|---|---|
| `save-reserve-risk` | loan to value, liquidation threshold and bonus, borrow weight |
| `save-reserve-rates` | the borrow rate curve |
| `save-reserve-limits` | deposit and borrow caps |
| `save-reserve-liquidity` | how much the reserve can lend or pay out |
| `save-obligation-position` | a whole position: one collateral deposit and one borrow |
| `save-obligation-owner` | the wallet that owns an existing position |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `*_wads` | value x 10^18, as a decimal **string** | 500 USDC of debt → `"500000000000000000000000000"` |
| percent fields | whole percent in one byte | `72` = 72% |
| `*_bps` | basis points | `10000` = 100% |
| `deposited_amount` | cTokens, the reserve's collateral mint, in its smallest unit | |
| token amounts | the mint's smallest unit | 1 USDC → `1000000`, 1 SOL → `1000000000` |

u128 values exceed what a JSON number holds exactly, so they must be quoted.

## Picking a market

Every template takes the account it writes; callers are responsible for passing a reserve or an
obligation. The main pool is `4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY`. A token's reserve
and the price account it reads come from one `getProgramAccounts` call on the program: filter on
`dataSize` 619, the market at offset 10 and the liquidity mint at offset 42, with a `dataSlice` of
32 bytes at offset 107. The templates' `llm_context` spells the call out. For example, the main
pool's SOL reserve is `8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36` and its USDC reserve
`BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw`.

**An obligation is not a PDA.** Its address is
`createWithSeed(owner, seed, So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo)`, where `seed` is the first
32 characters of the market's address. For the main pool that is `4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtf`.
A wallet's obligations can also be listed with `getProgramAccounts`, filtered on `dataSize: 1300` and
the owner at offset 42.

Each reserve names its price oracle at offset 107. In the main pool these are Pyth PriceUpdateV2
accounts, the same accounts `pyth-price-feed-v2` writes:

| Reserve | Oracle | `pyth-price-feed-v2` feed |
|---|---|---|
| SOL | `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` | SOL/USD |
| USDC | `Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX` | USDC/USD |

**Feeds are shared.** SOL/USD also prices other main-pool reserves, most of them liquid-staking
tokens priced as SOL times a fixed multiplier.

# Recipes

## Make a position liquidatable

Two independent levers. Either one works.

```
save-reserve-risk   on the SOL reserve
  config.liquidation_threshold: 50     # was 72
```

```
pyth-price-feed-v2  feed_id: SOL/USD, fetchBeforeUse: true
  price_message.price:        5900000000      # $59, 8 decimals
  price_message.ema_price:    5900000000
```

**Why:** a position is liquidatable once its borrowed value reaches its collateral's value times the
threshold. Lowering the threshold shrinks the limit; halving the price halves the collateral's
value. Both survive `refresh_obligation`.

## Build a position from scratch

```
save-obligation-position   on an existing obligation
  owner:                                  <your wallet>
  lending_market:                         4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY
  deposits_len:                           1
  borrows_len:                            1
  deposits.0.deposit_reserve:             <the SOL reserve>
  deposits.0.deposited_amount:            100000000000                        # 100 cSOL
  borrows.0.borrow_reserve:               <the USDC reserve>
  borrows.0.cumulative_borrow_rate_wads:  "<the USDC reserve's value at offset 195>"
  borrows.0.borrowed_amount_wads:         "5000000000000000000000000000"      # 5000 USDC
```

**Set every property.** The account's old entries are not cleared, so a field left out keeps
whatever was there.

- `deposited_amount` is in cTokens. One cToken is worth `(available_amount + borrowed_amount_wads /
  10^18 - accumulated_protocol_fees_wads / 10^18) / collateral.mint_total_supply` of the underlying
  token, all read from the collateral reserve.
- `cumulative_borrow_rate_wads` must be copied from the debt reserve. A higher value fails with
  `NegativeInterestRate`; a lower one charges the difference as interest on the next refresh.

The obligation must already exist. Pull a mainnet obligation from upstream with
`fetchBeforeUse: true`, or create one for your wallet with `init_obligation` first.

## Take over a real position

```
save-obligation-owner   on a mainnet obligation
  owner: <your wallet>
```

The position keeps its deposits, debt and address. Your wallet can now withdraw or borrow against
it. A client that finds obligations by deriving the address from the wallet will not find it; one
that searches by owner will.

## Block an action to test the rejection

```
save-reserve-limits   config.borrow_limit: 0      # no new borrows on this reserve
save-reserve-limits   config.deposit_limit: 0     # no new deposits
```

## Run a reserve dry

```
save-reserve-liquidity   liquidity.available_amount: 0
```

**Lower, never raise.** The field mirrors the reserve's liquidity vault, which the template does not
touch. Lowering it makes large borrows and withdrawals fail. Raising it above the vault's real
balance promises tokens the vault cannot pay, and the transaction fails when it settles.

## Make borrowing expensive

```
save-reserve-rates
  config.min_borrow_rate:       50
  config.optimal_borrow_rate:   50
  config.max_borrow_rate:       50
  config.super_max_borrow_rate: 50
```

Rates are APR in whole percent, compounded per slot on the next `refresh_reserve`.

## Keep the price fresh

Save accepts a Pyth price for **120 seconds** after its `publish_time`, measured against the Surfnet
clock, not the real one. Playing a scenario with an enabled Save or Pyth override marks every Pyth
price already loaded as published at that moment (see the
[scenarios README](../../../README.md#scenario-registration)), so leave `publish_time` unset.
Prices age again while the clock runs; play the scenario again to refresh them. A `publish_time`
written into the scenario is kept, which is how to make a price stale on purpose.

# Troubleshooting

| Symptom | Fix |
|---|---|
| A price override on SOL had no effect | The Pyth price aged past 120 s while the clock ran, and SOL fell back to its Switchboard feed. Play the scenario again - see "Keep the price fresh" |
| `InvalidOracleConfig` on refresh_reserve | Same cause on a reserve with no fallback, such as USDC; play the scenario again. It also fires when `conf x 10 > price`, so lower `conf` along with a crashed price |
| `Math operation overflow` on refresh_reserve | The reserve was loaded from upstream after the Surfnet clock stopped, so its `last_update.slot` is ahead of the clock. Move the clock to the current time with `surfnet_timeTravel`: `[{"absoluteTimestamp": <now in milliseconds>}]` |
| A health or `market_price` override disappeared | Recomputed by `refresh_reserve` or `refresh_obligation`. Override the oracle or the reserve's settings instead |
| `NegativeInterestRate` | The borrow entry's `cumulative_borrow_rate_wads` is above the reserve's. Copy the reserve's current value |
| `ReserveStale` or `ObligationStale` | A reserve or obligation is fresh only in the slot it was refreshed. Put `refresh_reserve` for every reserve the obligation lists, then `refresh_obligation`, in the same transaction |
| A liquidation says the obligation is healthy | The borrow value is still below the threshold value. Lower `config.liquidation_threshold` further or crash the price further |
| The position override did nothing | The obligation account does not exist on the surfnet yet. Pull a mainnet one from upstream with `fetchBeforeUse: true` or create it with `init_obligation` |
| Unexpected entries after a position override | The old account had more entries and a property was left out. Set every property of `save-obligation-position` |
| The entries swapped places after a refresh | Not a bug. `refresh_obligation` moves the highest-weighted borrow first and drops empty entries |
