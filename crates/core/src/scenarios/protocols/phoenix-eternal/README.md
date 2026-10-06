# Phoenix Eternal

Surfpool bundles the Phoenix Eternal IDL and eight override templates, so a scenario can put
Phoenix perpetuals into the state your code needs to see: a trader close to liquidation, a moved
mark price, throttled withdrawals. The templates only prepare state. Trades and liquidations are
sent by your own code.

Every field's meaning and units are on the template itself, visible in Studio and through
`get_override_templates`. This page covers what the templates cannot say on their own.

## Terms

| Term              | Meaning                                                                                                                                         |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| Trader            | A trader's account, with its collateral and capability flags in `traderState`.                                                                  |
| GlobalTraderIndex | Phoenix's list of active traders. It keeps each listed trader's state in a 16-byte record, and Phoenix reads that record instead of the Trader. |
| Hot, cold         | A hot trader is listed in the GlobalTraderIndex and has the HOT bit (value 1) of `traderState.flags` set. A cold trader is not listed.          |
| PerpAssetMap      | The single account that holds every market's mark price and risk factors.                                                                       |
| Hawkeye           | A Phoenix program that returns margin and risk views, such as `view_margin`.                                                                    |
| Ticks             | Phoenix's price unit. USD per base unit = `markTicks * tickSize * 10^(baseLotDecimals - 6)`, all three from `list_phoenix_markets`.             |
| Quote lots        | The collateral unit: PhUSD, Phoenix's USD quote token, with 6 decimals. `1000000` is 1 PhUSD.                                                   |
| Backstop factor   | Each market's risk factor for the tier just below Liquidatable (`backstopRiskFactorBps`).                                                       |

## Number formats

| Value                                          | Unit                               | Example             |
| ---------------------------------------------- | ---------------------------------- | ------------------- |
| Mark and stop-loss prices                      | Ticks                              |                     |
| Collateral, withdraw budgets and withdraw fees | Quote lots                         | `1000000` = 1 PhUSD |
| Risk factors                                   | Basis points of the initial margin | `5000` = 50%        |
| Trading fees                                   | Millionths of the filled notional  | `350` = 0.035%      |
| Permission expiry                              | Unix seconds                       | `0` = never expires |

The collateral, mark and maintenance templates take their numbers as decimal strings, so large
values stay exact in JavaScript. The other templates take JSON numbers.

Set `fetchBeforeUse: true` on every override, so the account is fetched from the upstream
datasource before its bytes are changed. Use `false` only for a later override that builds on
state an earlier override of the same scenario prepared.

## Hot and cold traders

On mainnet the HOT bit of a Trader and the GlobalTraderIndex agree, and traders join and leave
the hot set all the time.

The `GlobalTraderIndex` will be pulled from the upstream datasource once, so it can potentially
disagree with a Trader that is fetched later. Hawkeye and the program read a trader from its index
record whenever the local GlobalTraderIndex lists it, whatever the HOT bit says. The templates
follow the program:

- If the local GlobalTraderIndex lists the Trader, its collateral is written to both its record
  there and its Trader account. Other TraderState fields are refused for it, since only
  collateral is mirrored into the record.
- If the Trader's HOT bit is set but the local GlobalTraderIndex does not list it, the Trader
  became hot after the index was pulled. Phoenix rejects every transaction for that Trader with
  `TradersViewError::TraderNotFound`, so the collateral override is refused. Restart surfnet to
  pull the current GlobalTraderIndex.

## What the templates enforce

- **Collateral stress** only lowers collateral, since raising it needs a real deposit. The
  ceiling is the amount on mainnet with `fetchBeforeUse`, otherwise the amount in the local VM,
  and for a listed trader it is its index record. A target above the ceiling is skipped at Play
  with a warning.
- The collateral target is `quoteLotCollateral`, which leaves out the unrealized PnL and funding
  Phoenix adds for effective collateral. Lowering it by N quote lots lowers effective collateral
  by N.
- **Mark shock** sets the market's mark and its oracle readings to the target ticks, stamped with
  the current slot. The orderbook does not move, so a trade can pull the mark back toward the
  book. Compute a relative move from `markTicks` just before Play.
- **Maintenance margin stress** needs a factor above the market's backstop factor and at most
  10000. The maintenance margin scales linearly with it.
- **Trader capabilities** only applies to a cold trader. The HOT bit cannot be set: Phoenix would
  look the trader up in the GlobalTraderIndex, which does not list it, and reject its
  transactions. `62` is an active trader, `54` reduce-only and `6` frozen.
- **Stop-loss trigger** edits a stop loss the trader already placed. It does not place, resize or
  execute one. Only an active stop loss fires: bit 0 of its flags byte, at account byte 49 for
  stop loss 0 and 129 for stop loss 1. Leave `positionSequenceNumber` as it is.
- **Withdraw limits**: a withdrawal above the remaining budget is queued until someone calls the
  permissionless `ConsumeWithdrawQueue` instruction.
- **Delegated permission**: an `expiresAtTimestamp` in the past expires it now, and
  `numSignerActionsRemaining` of `0` leaves no actions.

## Finding accounts

| Account           | How to find it                                                                                                                                                    |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trader            | PDA `["trader", authority, [pda index, subaccount index]]`, usually `[0, 0]`.                                                                                     |
| Market orderbook  | The `orderbook` of the market in `list_phoenix_markets`.                                                                                                          |
| StopLosses        | PDA `["stoploss", trader account, asset id as u64 little-endian]`, or `getProgramAccounts` with `dataSize: 328` and a memcmp of the trader account at offset 224. |
| PermissionAccount | PDA `["permission", authority, delegated key]`, or `getProgramAccounts` with `dataSize: 168` and a memcmp of the authority at offset 8.                           |
| PerpAssetMap      | Fixed address, carried by its templates.                                                                                                                          |
| WithdrawQueue     | Fixed address, carried by its template.                                                                                                                           |

## Recipe: liquidation cascade

1. Pick a trader whose Hawkeye `view_margin` shows a position and `is_liquidatable` 0.
2. At slot 0, use `phoenix-trader-collateral-stress` with
   `target = collateral - effective_collateral + maintenance_margin / 2`, all three from
   `view_margin`. Effective collateral then sits at half the maintenance margin.
3. At slot 1, use `phoenix-direct-mark-risk-shock` on the trader's market, moving the mark
   against the position.
4. Send the liquidation.

Phoenix ranks a trader by its effective collateral against the margins `view_margin` returns:

| Effective collateral is below    | Tier                 |
| -------------------------------- | -------------------- |
| The high-risk margin, or below 0 | HighRisk             |
| The backstop requirement         | BackstopLiquidatable |
| The maintenance margin           | Liquidatable         |
| The cancel margin                | Cancellable          |
| The at-risk margin               | AtRisk               |
| None of them                     | Safe                 |

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
| `Hot Phoenix Trader has no reachable GlobalTraderIndex entry`                           | The trader turned hot after the GlobalTraderIndex was pulled; Phoenix rejects its transactions too. Restart surfnet.                                                              |
| `Cannot get mark price, staleness or validity check failed`                             | The local PerpAssetMap aged since it was pulled. Market templates refresh it with `fetchBeforeUse: true`; for collateral-only scenarios add a market override or restart surfnet. |

## Tests against mainnet

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
