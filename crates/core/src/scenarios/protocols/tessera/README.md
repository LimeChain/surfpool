# Tessera

Tessera is a proprietary market maker that publishes no IDL. Surfpool writes its market accounts
through the raw byte layout in `v1/overrides.yaml`. It prepares state; it does not construct or
submit a swap.

## Deployment

- Program: `TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH`
- ProgramData: `BzSXM6KLDpHQQChzr7Fdgbzwp8r8zRYWFFrHK2uZmDYV`
- Upgrade authority: `7bJ9xu9UGVZPtYzH1fMwdaKdvfhqeSJtoFc2eGrXBPhK`
- Deploy slot: `446053401`
- ELF SHA-256: `82fd37995fcece47a253b1a00c1dd7e4c3fff706b2e42384110e2cbabf4201b3`

The live suite checks the layout and behavior against this pinned deployment and fails when
its identity changes. A redeploy voids the layout evidence — re-verify
before trusting the templates again.

## The guard, and what it does not cover

The manifest requires 1264 bytes and `05 00 00 00 00 00 00 00` at offset 96, then discovery
validates ownership and mint identities. These bytes are a conservative observed-state filter,
not a version discriminator: the deployed program also reads this field during age adjustment.
A different value may exclude a valid market, and a matching value does not authenticate a future
layout. Deployment revalidation remains necessary.

The shared raw-layout schema has no owner predicate, so a foreign account of the same size carrying
the same eight bytes would pass a raw template. `TesseraMarket` validation adds the ownership
check used by discovery and both scenario tools; the depth builder also validates its input
account. Composing the raw template against an arbitrary address does not. That is a property of the shared schema, not of
this integration, and the raw scenario API is unvalidated by contract.

## Templates

| Template | Prepared state |
|---|---|
| `tessera-fair-value` | both directional atomic-ratio fields |
| `tessera-depth` | all twenty directional capacities on both ladders |
| `tessera-curve` | all twenty directional output factors on both ladders |
| `tessera-halt` | all twenty enabled flags on both ladders |
| `tessera-stale-quote` | offset 120, aged by the lead you pass (default -20) |
| `tessera-freshness` | offset 120, the current materialization slot |

The direct field at offset 128 is quote atomic units per base atomic unit multiplied by `10^15`.
The reciprocal at offset 144 uses the same scale, so their product is approximately `10^30` after
integer-floor rounding. Changing only one of them moves one quote direction and leaves the other
where it was, which is why they are one invariant.

The sell ladder occupies bytes 160 through 639 and the buy ladder 640 through 1119. Each holds
twenty 24-byte records: directional capacity at `+0`, marginal-price factor at `+8`, enabled flag
at `+16`. Capacity and factor changes affect only their active quote direction. For a fill
starting and ending in level zero, the modeled output is
`floor(input_atoms * directional_price * first_level_factor / 10^21)`. The live tests check exact
atomic output after clearing the captured flow counters at 0/8 and neutralizing the five
selectable configurations at 1136 + 12*i (ppm adjustment 0, factor scale 1,000,000, no skipped
levels) in their local fixtures. A small input alone cannot
establish this condition: prior flow or the selected configuration can start at a later level.
These fixture controls are not exposed as scenario properties.

`tessera-halt` writes zero to every enabled flag using two strided byte properties. Its existing
property names are retained, but each now covers twenty levels. Clearing only level zero can
leave later levels tradable. The live regression checks both captured state and an explicit
one-level skip, including a control where clearing only level zero still allows a swap.

Offset 88 stores the age at which the program rejects a quote. Age 19 succeeds and age 20 fails
with custom error 65535 on a market configured at 20.

Both slot templates take the lead from the caller: the value supplied for `last_update_slot` is
added to the materialization slot, and only `null` falls back to the template's own lead. One stale
template therefore covers every market, including one configured at a limit nobody has seen yet.
`list_tessera_markets` returns each market's limit alongside its address; callers pass its negation. Passing a number where you meant the default is the one trap: `0` on the stale
template writes a perfectly fresh quote.

## Live market discovery

`list_tessera_markets` queries Tessera program accounts through the selected Surfnet RPC.
It filters by the manifest's account size and pinned bytes, validates ownership and mint identities,
and reads decimals from the referenced mint accounts. The freshness limit comes from offset 88.
The existing Surfnet account resolver merges remote discovery with local accounts, preferring local
state. Discovery needs a datasource that supports `getProgramAccounts`; offline instances can list
only their local accounts.

The six shared templates retain the SOL/USDC default address for callers that omit an account.

Labels use mint symbols from Surfpool's existing token metadata. An unknown mint is displayed by
its full address, so missing symbol metadata never hides a discovered market. Addresses are the
identities; symbols are not unique. Market membership, decimals and freshness limits are not taken
from the token metadata catalog.

## Builders and tools

The fair-value builder converts a human price into reciprocal atomic ratios using both mints'
decimals. It is a pure function over account data. `create_tessera_fair_value_scenario` reads the market and both mints through the
surfnet's own RPC, so local state wins and only missing accounts fall back to the datasource, then
stages the scenario through the shared path.

Builder-created overrides keep `fetchBeforeUse: false`: creation has already read and hydrated
the target account, and the scenario must use that prepared local snapshot. When composing a
direct template scenario, set `fetchBeforeUse: true` on the first override for each account not
yet in local state. Freshness overrides pass `last_update_slot: null` to write the materialization
slot itself. They are applied once; a scenario that runs past the market's freshness window sets
`persist` itself.

The `create_tessera_depth_scenario` tool reads current Surfnet state and takes remaining basis points
per direction: 1000 retains 10%, 10000 leaves that direction unchanged. It scales only enabled
capacities, with integer-floor rounding, and rejects zero capacities or increases. Prices, factors
and disabled levels are preserved. The scenario combines `tessera-depth` with a freshness override,
both applied once. Creating another scenario reads the then-current state again.
Curve changes remain available through the raw template.

## Behavioral evidence

The live suite loads the pinned ELF into LiteSVM and exercises price direction isolation,
depth reductions, curve factors, freshness boundaries, halted ladders, vault bindings,
invalid sentinel/global account metas, and live market discovery. Raw writes are checked against
complete expected buffers or permitted byte ranges.

Run it serially. The public endpoint sheds queued requests right after a `getProgramAccounts` scan,
sometimes as a 413 that looks like a request-size error:

```bash
SURFPOOL_TEST_RPC_URL=<rpc-url> cargo test -p surfpool-core --features integration-tests \
  tests::tessera -- --test-threads=1 --nocapture
```

`SURFPOOL_TEST_RPC_URL` is optional and defaults to the public mainnet endpoint. Set it to a
private endpoint when the public one rate-limits.

## Known boundaries

No separate fee field is exposed. Exact controlled first-level output does not establish how
Tessera decomposes its price factor into spread, fee, or another adjustment. The region from 1120
onward includes configuration-dependent quote adjustments and leading-level selection; its full
economic meaning remains unmodeled and it is not exposed by the templates. Vault depletion is
not exposed by the Tessera templates. The generic SPL Token balance template uses the shared
typed token-account writer, but Tessera vault depletion behavior is outside this suite's coverage.
