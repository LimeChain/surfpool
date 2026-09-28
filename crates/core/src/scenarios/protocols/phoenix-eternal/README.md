# Phoenix Eternal state preparation

This integration prepares deterministic Phoenix Eternal account state. Bots remain responsible
for submitting trades, arbitrage, and liquidation transactions.

## Supported state preparations

| Goal | Template | State changed |
| --- | --- | --- |
| Collateral stress | `phoenix-trader-collateral-stress` | Exact signed quote-lot collateral on one validated Trader account |
| Direct mark shock | `phoenix-direct-mark-risk-shock` | Mark-price ticks for one market in the current PerpAssetMap |
| Spot/perp divergence | `phoenix-reference-price-divergence` | Cached spot and external-perp reference ticks while preserving the mark price |
| Liquidation cascade | Two validated overrides in one scenario | Trader collateral at slot 0, then a direct market mark shock at slot 1 |

Tick inputs are Phoenix protocol ticks, not human-readable USD prices. Pass tick and collateral
values as decimal strings so values outside JavaScript's safe integer range remain exact.

Market templates identify the real `PerpAssetMap` account. Their `symbol` and tick properties
carry `value_type: string` so Studio can render scenario inputs without synthetic IDL accounts.

## Use from Studio

1. Start an online Surfpool fork and open Studio.
2. Open **Scenario presets**, choose **Phoenix state**, then select the state goal.
3. For price scenarios, select a market from the live dropdown. For collateral stress, enter a
   Phoenix Eternal Trader account.
4. Enter the target values and create the scenario.
5. Inspect the generated override, then press **Play** to activate it.
6. Send the bot, trade, arbitrage, or liquidation transaction you want to evaluate to the local
   Surfnet RPC, normally `http://127.0.0.1:8899`.

Play prepares account state; it does not submit a Phoenix transaction. The Transaction Inspector
remains empty until a client sends a transaction against the prepared state.

## Use through MCP

The collateral builder and live market catalog are available through MCP:

| Tool | Required parameters |
| --- | --- |
| `create_phoenix_collateral_scenario` | `trader`, `targetQuoteLots` |
| `list_phoenix_markets` | None; optional `surfnetPort` |

The collateral tool returns a Studio editor URL. The backend reads the live Trader account and refuses a target
above the trader’s effective collateral.

## Troubleshooting

| Error or observation | Meaning |
| --- | --- |
| `Phoenix GlobalConfig ... was not found` or `Phoenix PerpAssetMap ... was not found` | Neither the local fork nor its datasource holds the Phoenix account graph. Start Surfpool against a datasource that carries the deployment. |
| `Phoenix market ... was not found` | The symbol is not in the live PerpAssetMap. Symbols are exact, such as `BTC`. |
| `Phoenix collateral stress can only lower collateral` | The target exceeds what the global vault backs. Send a real deposit to raise collateral. |
| `Expected a valid Phoenix Eternal Trader account` | The supplied account is not a decodable Phoenix Eternal Trader owned by the deployed program. |
| Scenario is green but the Transaction Inspector is empty | The state is active, but no client transaction has been sent yet. |
| A transaction does not produce the expected economic result | Confirm its accounts and instruction path consume the field changed by the selected preparation. |

## Verification against mainnet

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits. The deployed
Phoenix Eternal and Hawkeye bytecode is cached under the system temp directory as
`surfpool-phoenix-$USER/eternal.so` and `surfpool-phoenix-$USER/hawkeye.so`; delete them to pick
up a program upgrade.
