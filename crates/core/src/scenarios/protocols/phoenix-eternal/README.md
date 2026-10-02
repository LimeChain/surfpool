# Phoenix Eternal state preparation

This integration prepares deterministic Phoenix Eternal account state. Bots remain responsible
for submitting trades, arbitrage, and liquidation transactions.

## Supported state preparations

| Goal | Template | State changed |
| --- | --- | --- |
| Collateral stress | `phoenix-trader-collateral-stress` | Exact signed quote-lot collateral on one validated Trader account |
| Direct mark shock | `phoenix-direct-mark-risk-shock` | Mark-price ticks for one market in the current PerpAssetMap |
| Maintenance margin stress | `phoenix-maintenance-margin-stress` | Maintenance margin risk factor for one market in the current PerpAssetMap |
| Liquidation cascade | Two validated overrides in one scenario | Trader collateral at slot 0, then a direct market mark shock at slot 1 |
| Market fees | `phoenix-market-fees` | Default taker and maker fee on one market's orderbook |
| Exchange status | `phoenix-exchange-status` | Exchange-wide status bits in GlobalConfiguration, such as maintenance |
| Withdraw limits | `phoenix-withdraw-limits` | Withdrawal budget, its refill per slot and the withdrawal and queueing fees |
| Trader capabilities | `phoenix-trader-capabilities` | Capability bits of one cold Trader: activate, reduce-only or frozen |
| Stop-loss trigger | `phoenix-stop-loss-trigger` | Trigger and execution prices of a Trader's existing stop loss on one market |
| Delegated permission | `phoenix-permission-limits` | Expiry and remaining signer actions of one delegated permission |

Tick inputs are Phoenix protocol ticks, not human-readable USD prices. Pass the market templates'
tick values and collateral as decimal strings so values outside JavaScript's safe integer range
remain exact.
The fee, exchange status, withdraw limit, capability, stop-loss and permission templates
edit their account through the IDL like the Kamino templates, so their values are JSON
numbers.

Market templates identify the real `PerpAssetMap` account. Their `symbol` and tick properties
carry `value_type: string` so Studio can render scenario inputs without synthetic IDL accounts.

`idl.json` is the on-chain IDL of the program, with instructions, events and errors removed like
the Kamino IDLs. One edit: `maxPositions` is split into `maxPositions: u32` and
`traderPreferenceBits: u32` to match the Rise SDK; the on-chain IDL shows them as one `u64`.

## Use from Studio

1. Start an online Surfpool fork and open Studio.
2. Open **Scenario presets**, choose **Phoenix state**, then select the state goal.
3. For market scenarios, select a market from the live dropdown. For collateral stress, enter a
   Phoenix Eternal Trader account.
4. Enter the target values and create the scenario.
5. Inspect the generated override, then press **Play** to activate it.
6. Send the bot, trade, arbitrage, or liquidation transaction you want to evaluate to the local
   Surfnet RPC, normally `http://127.0.0.1:8899`.

## Use through MCP

The collateral builder and live market catalog are available through MCP:

| Tool | Required parameters |
| --- | --- |
| `create_phoenix_collateral_scenario` | `trader`, `targetQuoteLots` |
| `list_phoenix_markets` | None; optional `surfnetPort` |

`list_phoenix_markets` returns every listed market with its symbol, orderbook address, current `markTicks`, `tickSize`,
`baseLotDecimals`, `maintenanceRiskFactorBps` and `backstopRiskFactorBps`, so a market given by orderbook address resolves to its symbol,
a mark converts to USD per base unit as `markTicks * tickSize * 10^(baseLotDecimals - 6)`,
relative changes start from live values, and a maintenance factor can be kept above the backstop one. The collateral tool returns a Studio editor URL. The tool reads the live Trader account; Play skips a target above
the trader’s effective collateral with a warning.

## Troubleshooting

| Error or observation | Meaning |
| --- | --- |
| `Phoenix PerpAssetMap ... was not found` or `Phoenix dependency ... is missing locally` | Neither the local fork nor its datasource holds the Phoenix account graph. Start Surfpool against a datasource that carries the deployment. |
| `Phoenix market ... was not found` | The symbol is not in the live PerpAssetMap. Symbols are exact, such as `BTC`. |
| `Phoenix collateral stress can only lower collateral` | The target exceeds what the global vault backs. Send a real deposit to raise collateral. |
| `Expected a valid Phoenix Eternal Trader account` | The supplied account is not a decodable Phoenix Eternal Trader owned by the deployed program. |
| `Cannot get mark price, staleness or validity check failed` | The fork's PerpAssetMap aged since it was loaded. Market templates refresh it with `fetchBeforeUse: true`; for collateral-only scenarios add a market override or restart the fork. |
| `Global configuration must be active` when a trade, withdrawal or liquidation is sent after Play | Surfnet starts the `LastRestartSlot` sysvar at 0, and Phoenix only trades while it equals the restart slot its GlobalConfig acknowledged (246464040 on mainnet). Until Surfpool copies the sysvar from the datasource, call `surfnet_setAccount` after every start with the mainnet `SysvarLastRestartS1ot1111111111111111111111` account. Views such as Hawkeye are not affected. |
| `sendTransaction` waits 30 s and fails with `Failed to fetch accounts from remote` | The Phoenix log authority `GdxfTLSsdSY37G6fZoYtdGDSfgFnbT2EmRpuePZxWShS` does not exist on mainnet. Create it locally as an empty system account with `surfnet_setAccount` before sending. |

## Verification against mainnet

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits. Each run
reads the deployed Phoenix Eternal and Hawkeye bytecode once, so a program upgrade is picked up
on the next run.
