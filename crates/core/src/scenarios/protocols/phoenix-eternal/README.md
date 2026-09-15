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

Collateral overrides use `traderState.quoteLotCollateral`. The former `quote_lot_collateral`
name is rejected by MCP scenario creation; an existing scenario using it is skipped at Play with
an explicit warning naming the replacement. Update that scenario's values before playing it.
On a hot Trader only `traderState.quoteLotCollateral` can be overridden: the GlobalTraderIndex
mirror carries just that field, so other `traderState` writes are rejected at creation and skipped
at Play. A Trader whose header key differs from its address is skipped at Play as well. The
GlobalConfig and GlobalTraderIndex a hot override needs are fetched once and then kept as local
state: a later `fetchBeforeUse` on another Trader does not reinstall them, so mirrors prepared by
earlier scenarios survive.
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

## Where scenarios are built

Phoenix has no HTTP routes of its own. The perp asset map address is a GlobalConfig field rather
than a PDA, but both market templates carry that address directly, so those two scenarios need no
tool. Only the collateral scenario needs one, because it must read and validate the live Trader
account first. Studio's preset calls that tool for collateral and posts
the market templates directly.

Accounts are read through the Surfnet's own RPC, so local state wins and only missing accounts fall
back to the fork's datasource. A scenario therefore computes from the state you see, including
accounts you changed locally.

## Use through MCP

The collateral builder and live market catalog are available through MCP:

| Tool | Required parameters |
| --- | --- |
| `create_phoenix_collateral_scenario` | `trader`, `targetQuoteLots` |
| `list_phoenix_markets` | None; optional `surfnetPort` |

The collateral tool returns a Studio editor URL. The backend reads the live Trader account and refuses a target
above the trader’s effective collateral.

## What each preparation guarantees

- Collateral stress uses the IDL for `Trader.traderState.quoteLotCollateral`. For a hot trader,
  Phoenix reads its effective collateral from `GlobalTraderIndex`, so Play also locates the
  reachable entry by the trader key and encodes its `TraderState.quoteLotCollateral` with the
  same IDL. Both accounts retain their length and every unrelated byte. A cold trader needs
  only its Trader account. The builder's increase guard uses effective index collateral for
  hot traders, rather than the stale copy in their Trader account.
- Hot collateral preparations currently support a single-arena `GlobalTraderIndex`. Missing,
  corrupt, or multi-arena indexes skip the override with a warning before any collateral write;
  like every other override, a rejected value never stops block production. Node offsets are resolved
  again at Play; no trader address or node offset is pinned in the scenario.
- Direct mark shock changes only the selected market's mark-price ticks and the mark-price slot,
  which is stamped with the slot the override materializes at so the program reads the new mark
  as fresh.
- Reference divergence changes all five cached spot-reference ticks and all five cached
  external-perp-reference ticks for the selected market, stamping their slots the same way. It
  preserves the mark price, orderbook, spline liquidity, account length, and every unrelated byte.
- Liquidation cascade is the collateral and direct-mark overrides in one scenario: the collateral
  override activates at slot 0 and the mark shock at slot 1.
- Collateral stress only lowers collateral when created through the MCP tool or the Studio
  preset: the field is a claim on the global vault's real tokens, which an override cannot
  create, so the builder refuses a raise; deposit first. A scenario posted straight to
  `/v1/scenarios` bypasses that check by design — writers write what the scenario says — and
  owns the consequences.
- Generated overrides leave `fetchBeforeUse` off: creation already read the accounts the plan
  applies to, and a refresh at Play time would patch a different version of them. An override that
  asks for a refresh anyway takes the shared refresh path, like every other protocol: the fork
  serves the rest of the account graph lazily when the program reads it.

These guarantees describe state preparation. Whether a particular transaction trades, arbitrages,
or liquidates depends on the transaction, the selected account, and the rest of the forked state.

## Troubleshooting

| Error or observation | Meaning |
| --- | --- |
| `Phoenix GlobalConfig ... was not found` or `Phoenix PerpAssetMap ... was not found` | Neither the local fork nor its datasource holds the Phoenix account graph. Start Surfpool against a datasource that carries the deployment. |
| `Phoenix market ... was not found` | The symbol is not in the live PerpAssetMap. Symbols are exact, such as `BTC`. |
| `Phoenix collateral stress can only lower collateral` | The target exceeds what the global vault backs. Send a real deposit to raise collateral. |
| `Expected a valid Phoenix Eternal Trader account` | The supplied account is not a decodable Phoenix Eternal Trader owned by the deployed program. |
| Scenario is green but the Transaction Inspector is empty | The state is active, but no client transaction has been sent yet. |
| A transaction does not produce the expected economic result | Confirm its accounts and instruction path consume the field changed by the selected preparation. |

Check the same market before applying an override. A cached `ActiveTraderBuffer` can lack positions
referenced by a more recently fetched orderbook, causing BBO to fail even before price validation.
Price-slot refreshes cannot repair those missing references. Behavioral tests fetch the account graph
and Clock together with `getMultipleAccounts` after discovering the addresses; they do not rewrite
oracle timestamps or fabricate trader positions. An old fork can also fail price-staleness guards
independently of whether the override wrote the correct values.

## Verification against mainnet

Phoenix is zero-copy, so decoding an account built by these tests can never disagree with the
decoder that built it. Layout drift and behavior are both checked against live mainnet accounts:

```sh
cargo test -p surfpool-core --features integration-tests tests::phoenix
```

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits. The suite
resolves the live account graph, asserts the typed invariants the program relies on, proves an
override on a live account changes only its target bytes, and runs the deployed Phoenix Eternal
and Hawkeye bytecode in the test VM. The bytecode is read from the ProgramData accounts
(`B5ayDaz9HegiNZqYeBtcFqfZBVSGwjB2CJgHshoSfMQg` and `Gv1WgG864CQqF5vedJVbpnhpRpRbTW1A7SyARzSw9B4Y`)
and cached under the system temp directory as `surfpool-phoenix-eternal.so` and
`surfpool-phoenix-hawkeye.so`; delete those files to pick up a program upgrade.

The behavioral run discovers a live hot Trader that carries collateral and a long position, and
fails loudly with `no eligible live candidate` rather than skipping if none exists. The risk
condition is produced by the preparations, not found pre-existing. Through real Phoenix and
Hawkeye execution it proves that:

- collateral stress lands in the account the program reads, lowers the collateral its risk
  engine can count on, and a Hawkeye margin view reports the trader liquidatable;
- the two cascade stages arrive in order: the prepared collateral at the first slot, and the
  mark the program reads shocked at the next;
- spot/perp divergence moves the cached index away from the mark on both live markets while
  the mark itself stands.

The market and spline accounts are never written by any Phoenix preparation; the byte-level
assertions on live accounts enforce that directly, and Hawkeye reads the resulting book state
in the behavioral run.
