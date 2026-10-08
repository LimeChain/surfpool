# Phoenix Eternal

Surfpool bundles the Phoenix Eternal IDL and 21 override templates, so a scenario can put
Phoenix perpetuals into the state your code needs to see: a market moved to a new price, a trader
ready to be liquidated, several traders liquidatable at once, a paused market, throttled
withdrawals. The templates only prepare state. Liquidations and trades are sent by your own code.

Every field's meaning and units are on the template itself, visible in Studio and through
`get_override_templates`. This page covers what the templates cannot say on their own. For how
Phoenix itself works, see the [Phoenix docs](https://docs.phoenix.trade/).

## How the templates change state

Phoenix keeps one market's state across several accounts that must agree: the oracle readings and
mark in the PerpAssetMap, the makers' liquidity in the spline collection, resting orders in the
orderbook, and each trader's collateral and positions in its Trader account, the
GlobalTraderIndex and the ActiveTraderBuffer. Writing one of them alone leaves the others behind;
a mark moved without the liquidity, for example, lets a liquidation fill at the old price.

So every template runs Phoenix's own instructions, signed by whoever signs them on mainnet: the
oracle keys, the makers, the trader, or the GlobalConfig role that owns a setting. Play runs them
on a copy of the local VM with signature checks off, then writes back every account they changed
at once. The program keeps the accounts consistent itself, and refuses what it would refuse on
mainnet: a maintenance factor above the cancel order factor, a withdrawal beyond the free
margin, an order the margin cannot carry. A refused template is skipped with a warning naming the
program's reason.

The stop-loss and permission templates are the exception: they patch one field of one account
through the IDL.

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

## Templates

| Template                              | What it does                                                                                           | Signed by                    |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------ | ---------------------------- |
| `phoenix-market-move`                 | Moves a market's oracle readings, its makers' splines and its book to a price, and uncrosses the book. | oracle keys, makers          |
| `phoenix-liquidation-ready`           | Leaves one trader liquidatable, and not underwater, in one market.                                     | trader, oracle keys, makers  |
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

Phoenix ranks a trader by its effective collateral against the margins `view_margin` returns:

| Effective collateral                                                 | Tier                 |
| -------------------------------------------------------------------- | -------------------- |
| At or above `initial_margin_quote_lots`                              | Safe                 |
| Below the initial margin, above `cancel_margin_quote_lots`           | AtRisk               |
| At or below the cancel margin, above `maintenance_margin_quote_lots` | Cancellable          |
| Below the maintenance margin                                         | Liquidatable         |
| Below `backstop_margin_quote_lots`                                   | BackstopLiquidatable |
| Below `high_risk_margin_quote_lots`                                  | HighRisk             |

`liquidate_via_market_order` refuses a trader that still has risk-increasing resting orders on any
market, and one whose effective collateral is already below zero. The band between liquidatable
and underwater is narrow, which is why the liquidation templates pick the price themselves. Only
the trader can cancel its orders, so the templates cancel them for it, on every market it holds,
before they move the price.

- **One trader, ready now:** `phoenix-liquidation-ready` on the trader. It cancels its orders and
  moves the market to where effective collateral is half the maintenance margin. A liquidation is
  tried on a copy first, and nothing is written unless it goes through. Surfpool's log names the
  price.
- **Watch traders cross:** `phoenix-cancel-orders` on each trader at slot 0, then
  `phoenix-market-move` at slot 1. The traders are healthy until the move.
- **A cascade:** `phoenix-liquidation-cascade` with the side to liquidate. It looks at up to 24
  holders of that side, finds the price inside the most of their bands, and prepares every holder
  whose band covers it, leaving out any trader whose liquidation would fail once the others have
  gone first. How many traders that is depends on the market. Surfpool's log names the price and
  the traders, in the order their liquidations were tried.

## Keeping markets usable

Oracle updates keep each market's readings fresh on mainnet, and Phoenix refuses a market whose
readings are older than its stale threshold. Nothing updates them in the local VM, so every
Phoenix override also raises every market's stale thresholds in the local PerpAssetMap. Prices
and reading slots stay as they were.

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
- `create_scenario` accepts any template and refuses a market symbol `list_phoenix_markets` does
  not list.

## Troubleshooting

| Warning or error                                              | Meaning                                                                                                                           |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `Account ... not found in SVM for override ...`               | The override's own account is not in the local VM. Set `fetchBeforeUse: true` on it.                                              |
| `... accounts are missing locally and there is no datasource` | The template needs Phoenix accounts that are not in the local VM, and surfnet has no upstream datasource to fetch them from.      |
| `... is offline and missing locally`                          | The account is marked offline, so surfnet will not fetch it.                                                                      |
| `instruction N failed: ... logs: [...]`                       | Phoenix refused the template's instruction; the logs carry its reason, such as a factor out of range.                             |
| `... holds no ... position: moving its price changes nothing` | The trader has no position in that market.                                                                                        |
| `a liquidation of ... would still fail`                       | The prepared state still does not let a market-order liquidation through, usually because the book cannot absorb the size.        |
| `Cannot get mark price, staleness or validity check failed`   | No Phoenix scenario was played on this surfnet, so the oracle readings aged past the stale thresholds. Play any Phoenix scenario. |

## Tests against mainnet

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
