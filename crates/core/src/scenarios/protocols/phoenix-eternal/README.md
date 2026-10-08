# Phoenix Eternal

Surfpool bundles the Phoenix Eternal IDL and eight override templates, so a scenario can put
Phoenix perpetuals into the state your code needs to see: a trader close to liquidation, a moved
mark price, throttled withdrawals. The templates only prepare state. Trades and liquidations are
sent by your own code.

Every field's meaning and units are on the template itself, visible in Studio and through
`get_override_templates`. This page covers what the templates cannot say on their own. For how
Phoenix itself works, see the [Phoenix docs](https://docs.phoenix.trade/).

## Terms

| Term                 | Meaning                                                                                                                                                                                                                                                                                   |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trader               | A trader's account, with its USDC collateral and capability flags in `traderState`.                                                                                                                                                                                                       |
| GlobalTraderIndex    | Phoenix keeps the state of some traders here instead of in their Trader account: a 16-byte record per listed trader, with its positions in the ActiveTraderBuffer. Phoenix reads that record instead of the Trader. A listed trader has the HOT bit (value 1) of `traderState.flags` set. |
| PerpAssetMap         | The single account that holds every market's mark price and risk factors.                                                                                                                                                                                                                 |
| Hawkeye              | A Phoenix program that returns margin and risk views, such as `view_margin`.                                                                                                                                                                                                              |
| Effective collateral | What Phoenix measures margin against: USDC collateral, plus the discounted value of SOL collateral and unrealized gains, minus unrealized losses, plus unsettled funding. `view_margin` returns it as `effective_collateral_quote_lots`.                                                  |
| Ticks                | Phoenix's price unit. USD per base unit = `markTicks * tickSize * 10^(baseLotDecimals - 6)`, all three from `list_phoenix_markets`.                                                                                                                                                       |
| Quote lots           | Units of Phoenix USDC (mint `PhUsd11YkbjSaWjFncfAAmatntsjx3MgDR9B6g1ks3A`), which has 6 decimals. `1000000` is 1 USDC.                                                                                                                                                                    |
| Backstop factor      | Each market's risk factor for the BackstopLiquidatable tier (`backstopRiskFactorBps`).                                                                                                                                                                                                    |

## Number formats

| Value                                          | Unit                               | Example                  |
| ---------------------------------------------- | ---------------------------------- | ------------------------ |
| Mark, take-profit and stop-loss prices         | Ticks                              |                          |
| Collateral, withdraw budgets and withdraw fees | Quote lots                         | `1000000` = 1 USDC       |
| Risk factors                                   | Basis points of the initial margin | `5000` = 50%             |
| Trading fees                                   | Millionths of the filled notional  | `350` = 0.035% (3.5 bps) |
| Permission expiry                              | Unix seconds                       | `0` = never expires      |

The collateral, mark and maintenance templates take their numbers as decimal strings, so large
values stay exact in JavaScript. The other templates take JSON numbers.

Set `fetchBeforeUse: true` on every override, so the account is fetched from the upstream
datasource before its bytes are changed. Use `false` only for a later override that builds on
state an earlier override of the same scenario prepared.

Every Phoenix override also keeps the markets usable for the rest of the session. Oracle updates
keep each market's readings fresh, and Phoenix refuses a market once its readings are older than
its stale threshold times its hard-stale multiplier, or than the threshold alone when the
multiplier is 0. Nothing refreshes them locally, so the override raises every market's
threshold in the local PerpAssetMap instead, fetching the map first when it is not local yet.
Prices and reading slots stay as they were.

## Traders in the GlobalTraderIndex

Traders join and leave the GlobalTraderIndex all the time. The `GlobalTraderIndex` will be pulled
from the upstream datasource once, so it can potentially disagree with a Trader that is fetched
later. Hawkeye and the program read a trader from its index record whenever the local
GlobalTraderIndex lists it, whatever the HOT bit says. The templates follow the program:

- If the local GlobalTraderIndex lists the Trader, its collateral is written to both its record
  there and its Trader account. Other TraderState fields are refused for it, since only
  collateral is mirrored into the record.
- If the Trader's HOT bit is set but the local GlobalTraderIndex does not list it, the Trader
  joined the index after it was pulled. Phoenix rejects every transaction for that Trader with
  `TradersViewError::TraderNotFound`, so the collateral override is refused. Restart surfnet to
  pull the current GlobalTraderIndex.

## What the templates enforce

- **Collateral stress** sets the Trader's USDC collateral (`quoteLotCollateral`) and only lowers
  it, since raising it needs a real deposit. The ceiling is the amount on mainnet with
  `fetchBeforeUse`, otherwise the amount in the local VM, and for a listed trader it is its index
  record. A target above the ceiling is skipped at Play with a warning.
- Lowering `quoteLotCollateral` by N quote lots lowers effective collateral by N. SOL collateral
  is separate and still counts toward effective collateral.
- **Mark shock** sets the market's mark and its oracle readings to the target ticks, stamped with
  the current slot. The orderbook does not move, so a trade can pull the mark back toward the
  book. Compute a relative move from `markTicks` just before Play.
- **Maintenance margin stress** needs a factor above the market's backstop factor and at most
  10000. The maintenance margin scales linearly with it.
- **Trader capabilities** only applies to a Trader the GlobalTraderIndex does not list. The HOT
  bit cannot be set: Phoenix would look the trader up in the GlobalTraderIndex, which does not
  list it, and reject its transactions. `62` grants every capability except HOT, `54` is
  reduce-only and `6` is frozen.
- **Stop-loss trigger** edits the two legs of a StopLosses account placed with `PlaceStopLoss`.
  `stopLosses.0` is the greater-than leg (the take-profit of a long, the stop loss of a short) and
  `stopLosses.1` the less-than leg. It does not place, resize or execute a leg, and it does not
  touch position conditional orders (`PlacePositionConditionalOrder`). Only an active leg fires:
  bit 0 of its flags byte, at account byte 49 for `stopLosses.0` and 129 for `stopLosses.1`.
  Leave `positionSequenceNumber` as it is.
- **Withdraw limits**: a USDC withdrawal is paid at once only when the queue is empty and it fits
  the remaining budget. Otherwise it is queued, never partially filled, until someone calls the
  permissionless `ConsumeWithdrawQueue` instruction. SOL withdrawals never queue.
- **Delegated permission**: a PermissionAccount lets a delegated key sign certain Phoenix actions
  for an authority. An `expiresAtTimestamp` in the past expires it now, and
  `numSignerActionsRemaining` of `0` leaves no actions. Trading delegation through a Trader's
  `position_authority` is separate.

## Finding accounts

| Account           | How to find it                                                                                                                                                    |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trader            | PDA `["trader", authority, [pda index, subaccount index]]`. The pda index is 0; subaccount 0 is the cross-margin account and 1 and up are isolated accounts.      |
| Market orderbook  | The `orderbook` of the market in `list_phoenix_markets`.                                                                                                          |
| StopLosses        | PDA `["stoploss", trader account, asset id as u64 little-endian]`, or `getProgramAccounts` with `dataSize: 328` and a memcmp of the trader account at offset 224. |
| PermissionAccount | PDA `["permission", authority, delegated key]`, or `getProgramAccounts` with `dataSize: 168` and a memcmp of the authority at offset 8.                           |
| PerpAssetMap      | Fixed address, carried by its templates.                                                                                                                          |
| WithdrawQueue     | Fixed address, carried by its template.                                                                                                                           |

## Recipe: liquidation cascade

1. Pick a trader whose Hawkeye `view_margin` shows a position and `is_liquidatable` 0.
2. At slot 0, use `phoenix-trader-collateral-stress` with this target, all three values from
   `view_margin`:
   ```
   collateral_quote_lots - effective_collateral_quote_lots + maintenance_margin_quote_lots / 2
   ```
   Effective collateral then sits at half the maintenance margin.
3. At slot 1, use `phoenix-direct-mark-risk-shock` on the trader's market, moving the mark
   against the position.
4. Send the liquidation.

Phoenix ranks a trader by its effective collateral against the margins `view_margin` returns:

| Effective collateral                                                 | Tier                 |
| -------------------------------------------------------------------- | -------------------- |
| At or above `initial_margin_quote_lots`                              | Safe                 |
| Below the initial margin, above `cancel_margin_quote_lots`           | AtRisk               |
| At or below the cancel margin, above `maintenance_margin_quote_lots` | Cancellable          |
| Below the maintenance margin                                         | Liquidatable         |
| Below `backstop_margin_quote_lots`                                   | BackstopLiquidatable |
| Below `high_risk_margin_quote_lots`                                  | HighRisk             |

## Studio and MCP

- In Studio, **Scenario presets → Phoenix state** builds the collateral, mark and maintenance
  scenarios with USD and percent inputs. The other templates are in the scenario editor.
- `list_phoenix_markets` returns every market with its symbol, orderbook, `markTicks`,
  `tickSize`, `baseLotDecimals` and risk factors, read from the local VM.
- `create_phoenix_collateral_scenario` takes `trader` and `targetQuoteLots` and returns a Studio
  editor URL.
- `create_scenario` accepts any template and refuses a market symbol `list_phoenix_markets` does
  not list.

## Troubleshooting

| Error                                                                                   | Meaning                                                                                                                                                                           |
| --------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Phoenix PerpAssetMap ... was not found` or `Phoenix dependency ... is missing locally` | Neither the local VM nor the upstream datasource holds the Phoenix accounts. Start surfnet with a datasource that has the Phoenix deployment.                                     |
| `Hot Phoenix Trader has no reachable GlobalTraderIndex entry`                           | The trader joined the GlobalTraderIndex after it was pulled; Phoenix rejects its transactions too. Restart surfnet.                                                               |
| `Cannot get mark price, staleness or validity check failed`                             | No Phoenix scenario was played on this surfnet, so the PerpAssetMap kept its fetched stale thresholds and its oracle readings aged past them. Play any Phoenix scenario.          |

## Tests against mainnet

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
