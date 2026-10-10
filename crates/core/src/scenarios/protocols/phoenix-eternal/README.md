# Phoenix Eternal

Surfpool bundles the Phoenix Eternal IDL and 21 override templates, so a scenario can put
Phoenix perpetuals into the state your code needs to see: a market moved to a new price, a trader's
positions ready to be liquidated one after another, several traders liquidatable at once, a paused
market, throttled withdrawals. The templates only prepare state. Liquidations and trades are sent
by your own code.

Every field's meaning and units are on the template itself, visible in Studio and through
`get_override_templates`. This page covers what the templates cannot say on their own. For how
Phoenix itself works, see the [Phoenix docs](https://docs.phoenix.trade/).

## How the templates change state

Phoenix keeps one market's state across several accounts that must agree: the oracle readings and
mark in the PerpAssetMap, the makers' liquidity in the spline collection, resting orders in the
orderbook, and each trader's collateral and positions in its Trader account, the
GlobalTraderIndex and the ActiveTraderBuffer. Writing one of them alone leaves the others behind;
a mark moved without the liquidity, for example, lets a liquidation fill at the old price.

The instruction templates run Phoenix's own instructions on a copy of the local VM. They name the
same signer accounts as on mainnet: the oracle keys, the makers, the trader, or the GlobalConfig
role that owns a setting. Their private keys are not used: signature checks are disabled in the
copy, and only a temporary local fee payer signs. Phoenix still checks the instructions' account
and state constraints: a maintenance factor above the cancel order factor, a withdrawal beyond
the free margin, or an order the margin cannot carry is refused. A refused template is skipped
with a warning naming the program's reason.

After the instructions succeed, Play writes back the accounts they changed. These writes are
applied sequentially by the shared materializer, not as an atomic group: a storage error can leave
earlier writes applied. Successful preparation in the copy does not guarantee atomic writeback.

The stop-loss and permission templates patch one field of one account through the IDL. The
maintenance described under "Keeping markets usable" also patches the PerpAssetMap directly.

## Terms

| Term                 | Meaning                                                                                                                                                                                                                       |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trader               | A trader's account, with its USDC collateral and capability flags in `traderState`.                                                                                                                                           |
| GlobalTraderIndex    | Phoenix keeps the state of active traders here instead of in their Trader account, with their positions in the ActiveTraderBuffer. Phoenix reads those, so the templates check traders through Hawkeye, not the account copy. |
| PerpAssetMap         | The single account that holds every market's oracle readings, mark price and risk parameters.                                                                                                                                 |
| Spline               | A market maker's liquidity curve around its mid price, in the market's spline collection. Liquidations mostly fill against splines.                                                                                           |
| Hawkeye              | A Phoenix program that returns margin and risk views, such as `view_margin`.                                                                                                                                                  |
| Effective collateral | What Phoenix measures margin against: USDC collateral, plus the discounted value of SOL collateral and unrealized gains, minus unrealized losses, plus unsettled funding.                                                     |
| Ticks                | Phoenix's price unit. USD per base unit = `markTicks * tickSize * 10^(baseLotDecimals - 6)`, all three from `list_phoenix_markets`.                                                                                           |
| Quote lots           | Units of Phoenix USDC (mint `PhUsd11YkbjSaWjFncfAAmatntsjx3MgDR9B6g1ks3A`), which has 6 decimals. `1000000` is 1 USDC.                                                                                                        |

## Number formats

| Value                                          | Unit                               | Example                  |
| ---------------------------------------------- | ---------------------------------- | ------------------------ |
| Prices                                         | Ticks                              |                          |
| Collateral, withdraw budgets and withdraw fees | Quote lots                         | `1000000` = 1 USDC       |
| Position and liquidation sizes                 | Base lots                          |                          |
| Risk factors                                   | Basis points of the initial margin | `5000` = 50%             |
| Trading fees                                   | Millionths of the filled notional  | `350` = 0.035% (3.5 bps) |

The instruction templates take numbers as decimal strings, so large values stay exact in
JavaScript; the stop-loss and permission templates take JSON numbers. A configuration field left
out or left empty keeps its current value, unless its template says the field is required.

## fetchBeforeUse

`fetchBeforeUse: true` makes Play fetch the override's own account from the upstream datasource
first, replacing the local copy. Use it on the first Phoenix scenario played on a surfnet: an
override whose account is not in the local VM yet is skipped.

Use `false` on every later Phoenix scenario. Phoenix keeps one market across several accounts,
and a refetch replaces only the override's own account. Refetching the PerpAssetMap, for example,
puts every market's price back to mainnet's, while the order books, market status and traders keep
what earlier scenarios prepared, and the two no longer agree.

Surfnet copies each account from mainnet the first time it is read, while mainnet keeps trading.
Order books and the ActiveTraderBuffer name traders by their node in the GlobalTraderIndex, so the
three must come from the same moment. The first Phoenix template or tool that needs the index on a
surfnet loads it with the buffer and every market's book in one request. Splines, Trader accounts
and the PerpAssetMap are still copied when first needed, which can leave a collateral or a price
slightly behind mainnet but breaks no reference.

**Known limitation:** that does not help when your own code read Phoenix accounts on a fresh
surfnet before any Phoenix template or tool ran, or once Phoenix lists more than 98 markets and the
request splits. It shows up as the uncross-crank message, the refusal of a hot trader the
GlobalTraderIndex does not list, or a skipped override failing with `InvalidAccountData` in Hawkeye
before any program log. Reset the Phoenix accounts (`surfnet_resetAccount` on the Phoenix program
with `includeOwnedAccounts: true`) or restart the surfnet, and play again.

## Templates

| Template                              | What it does                                                                                           | Signer accounts              |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------ | ---------------------------- |
| `phoenix-market-move`                 | Moves a market's oracle readings, its makers' splines and its book to a price, and uncrosses the book. | oracle keys, makers          |
| `phoenix-liquidation-ready`           | Leaves one or more positions of one trader liquidatable one after another.                             | trader, oracle keys, makers  |
| `phoenix-liquidation-cascade`         | Leaves several traders liquidatable at one price in one market.                                        | traders, oracle keys, makers |
| `phoenix-open-position`               | Sends a market order for the trader.                                                                   | trader                       |
| `phoenix-cancel-orders`               | Cancels the trader's resting orders in a market.                                                       | trader                       |
| `phoenix-withdraw`, `phoenix-deposit` | Moves collateral through the global vault; a deposit mints the quote token first.                      | trader                       |
| `phoenix-market-risk-factors`         | Maintenance, backstop and high-risk factors.                                                           | risk authority               |
| `phoenix-market-cancel-risk-factor`   | The resting-order factor, the ceiling for the maintenance factor.                                      | risk authority               |
| `phoenix-market-max-liquidation-size` | The most base lots one liquidation may close.                                                          | risk authority               |
| `phoenix-market-open-interest-cap`    | The most open interest the market accepts.                                                             | risk authority               |
| `phoenix-market-funding`              | Funding interval, period and maximum rate.                                                             | market authority             |
| `phoenix-market-fees`                 | Default taker and maker fees.                                                                          | market authority             |
| `phoenix-market-status`               | Active, PostOnly, Paused or Closed.                                                                    | market authority             |
| `phoenix-exchange-status`             | The exchange-wide active, gated and maintenance flags.                                                 | root authority               |
| `phoenix-withdraw-limits`             | The withdrawal budget and its refill per slot.                                                         | root authority               |
| `phoenix-withdraw-parameters`         | Deposit cooldown and withdrawal and queueing fees.                                                     | risk authority               |
| `phoenix-trader-fees`                 | A trader's fee override multipliers.                                                                   | market authority             |
| `phoenix-trader-capabilities`         | Allows or blocks a trader's orders, risk-increasing trades, deposits and withdrawals.                  | risk authority               |
| `phoenix-stop-loss-trigger`           | Edits the two legs of a StopLosses account placed with `PlaceStopLoss`.                                | IDL write                    |
| `phoenix-permission-limits`           | A delegated permission's expiry and remaining actions.                                                 | IDL write                    |

## Recipes

`liquidate_via_market_order` refuses a trader that still has risk-increasing resting orders on any
market, and one whose effective collateral is already below zero. The band between liquidatable
and underwater is narrow, which is why the liquidation templates pick the price themselves. Only
the trader can cancel its orders, so the templates cancel them for it, on every market where
Hawkeye reports a position or orders of the trader, before they move the price.

- **One trader:** `phoenix-liquidation-ready` on the trader, with one or more of the markets
  `list_phoenix_trader_positions` lists for it. See below.
- **Watch traders cross:** `phoenix-cancel-orders` on each trader at slot 0, then
  `phoenix-market-move` at slot 1. The traders are healthy until the move.
- **A cascade:** `phoenix-liquidation-cascade` with the side to liquidate. It looks at the first 24
  holders of that side, in address order, in Phoenix's active trader index (cold holders are not
  examined), finds the price inside the most of their bands, and prepares every holder whose band
  covers it, leaving out any trader whose liquidation would fail once the others have gone first.
  After dropping a holder, it prepares the remaining holders again from the initial copy at the same
  price. Each unsuccessful round removes at least one holder, so there are at most 24 rounds. An
  empty set is refused. How many traders remain depends on the market. Surfpool's log names the
  price and the traders, in the order their liquidations were tried.

Market moves run uncross cranks until the book is uncrossed or neither its best prices nor its
resting-order count changes. They stop with an error after 256 cranks. This is an execution budget,
not a guarantee that every possible book can be uncrossed within that many calls.

### Liquidation-ready positions

`phoenix-liquidation-ready` cancels the trader's orders, then moves every listed market by the
same share of its mark against the position: longs down, shorts up. The positions run largest
maintenance margin last. Each liquidation releases margin, so the account can recover before the
last position. The template tries the run on a copy and prepares the state in the middle of the
moves where it goes through. Surfpool's log names the order and each market's price:

```text
Phoenix liquidation-ready: <trader> liquidatable in turn: SOL, BTC (SOL moved to 15446 ticks, BTC to 112194 ticks)
```

Send one liquidation per position, in that order. A trader that quotes splines is refused.

## Keeping markets usable

Oracle updates keep each market's readings fresh on mainnet, and Phoenix refuses a market whose
readings are older than its stale threshold. Nothing updates them in the local VM, so every
Phoenix override also raises every market's stale thresholds in the local PerpAssetMap. Prices
stay as they were.

This maintenance runs before template validation, including for the two generic IDL templates,
and writes the map only when its data changes. A refused template can therefore leave these
maintenance changes applied.

Every Phoenix override also moves readings older than the local clock up to its slot. An oracle
report folds the gap between book and oracle into the price, weighted by the slots since the last
reading, so on a map fetched long before the clock a deep move would leave no positive price.

A running surfnet's clock trails mainnet, so a PerpAssetMap fetched from the upstream datasource
records funding updates from after the local time. Phoenix refuses to update funding, and with it
every price, before the last update, so every Phoenix override also moves those funding
timestamps back to the local clock. Funding then accrues from the local time.

## Finding accounts

| Account           | How to find it                                                                                                                                                    |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trader            | PDA `["trader", authority, [pda index, subaccount index]]`. The pda index is 0; subaccount 0 is the cross-margin account and 1 and up are isolated accounts.      |
| Market orderbook  | The `orderbook` of the market in `list_phoenix_markets`.                                                                                                          |
| StopLosses        | PDA `["stoploss", trader account, asset id as u64 little-endian]`, or `getProgramAccounts` with `dataSize: 328` and a memcmp of the trader account at offset 224. |
| PermissionAccount | PDA `["permission", authority, delegated key]`, or `getProgramAccounts` with `dataSize: 168` and a memcmp of the authority at offset 8.                           |
| PerpAssetMap      | Fixed address, carried by its templates.                                                                                                                          |
| WithdrawQueue     | Fixed address, carried by its templates.                                                                                                                          |
| GlobalConfig      | Fixed address, carried by its template.                                                                                                                           |

## MCP

- `list_phoenix_markets` returns every market with its symbol, orderbook, `markTicks`,
  `tickSize`, `baseLotDecimals` and risk factors, read from the local VM.
- `list_phoenix_trader_positions` returns the markets where a trader holds a position, with its
  side, size in base lots and maintenance margin. Hawkeye's views run on a copy of the surfnet's
  state, with its markets kept usable the way Play keeps them, and write nothing back. The reads
  go through the surfnet, which loads any account it is missing.
- `create_scenario` accepts any template and refuses a market symbol `list_phoenix_markets` does
  not list.

## Troubleshooting

| Warning or error                                              | Meaning                                                                                                                           |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `Account ... not found in SVM for override ...`               | The override's own account is not in the local VM. Set `fetchBeforeUse: true` on it.                                              |
| `... accounts are missing locally and there is no datasource` | The template needs Phoenix accounts that are not in the local VM, and surfnet has no upstream datasource to fetch them from.      |
| `... is offline and missing locally`                          | The account is marked offline, so surfnet will not fetch it.                                                                      |
| `instruction N failed: ... logs: [...]`                       | Phoenix refused the template's instruction; the logs carry its reason, such as a factor out of range.                             |
| `... holds no ... position`                                   | The trader has no position in that market.                                                                                        |
| `no common move leaves ... liquidatable in turn on ...`       | No share of the marks lets each position be liquidated in turn, largest maintenance margin last. Try fewer or other markets.      |
| `... quotes splines on ... markets; ...`                      | The trader is a spline market maker, and Phoenix did not liquidate such traders anywhere in the band.                             |
| `the ... book stayed crossed after ... uncross cranks`        | The book was still crossed when the execution budget was exhausted. Try a smaller move; this error alone does not establish that the local accounts are inconsistent. |
| `InvalidAccountData` before any program log                   | The order book and the trader index were copied from mainnet at different moments; see the known limitation.                      |
| `Cannot get mark price, staleness or validity check failed`   | No Phoenix scenario was played on this surfnet, so the oracle readings aged past the stale thresholds. Play any Phoenix scenario. |

## Tests against mainnet

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
