# Tessera

Tessera is a proprietary market maker that publishes no IDL. Surfpool writes its market accounts
through the raw byte layout in `v1/overrides.yaml`. It prepares state; it does not construct or
submit a swap.

## Deployment

- Program: `TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH`
- ProgramData: `BzSXM6KLDpHQQChzr7Fdgbzwp8r8zRYWFFrHK2uZmDYV`
- Upgrade authority: `7bJ9xu9UGVZPtYzH1fMwdaKdvfhqeSJtoFc2eGrXBPhK`
- Deploy slot: `438800691`
- ELF SHA-256: `433f2a857ffe2045310a478b4aca0fd824308d01f283719275baec60e2aecb3b`

Every offset and behavior below was proven against exactly this deployment; the live suite pins
it and fails when any of these values move. A redeploy voids the layout evidence — re-verify
before trusting the templates again.

## The guard, and what it does not cover

A market is 1264 bytes with the eight-byte layout tag `05 00 00 00 00 00 00 00` at offset 96.
Discovery selects accounts with this size and tag, then validates the mint identities. The tag is
the version half of the guard: it is what rejects a future market layout that reuses the size.

The shared raw-layout schema has no owner predicate, so a foreign account of the same size carrying
the same eight bytes would pass a raw template. `validate_tessera_market_layout` adds the ownership
check, and every scenario made through the fair-value builder goes through it. Composing the raw
template against an arbitrary address does not. That is a property of the shared schema, not of
this integration, and the raw scenario API is unvalidated by contract.

## Templates

| Template | Prepared state |
|---|---|
| `tessera-fair-value` | both directional atomic-ratio fields |
| `tessera-depth` | all twenty directional capacities on both ladders |
| `tessera-curve` | all twenty directional output factors on both ladders |
| `tessera-halt` | both required first-level enabled flags |
| `tessera-stale-quote` | offset 120, aged by the lead you pass (default -20) |
| `tessera-freshness` | offset 120, the current materialization slot |

The direct field at offset 128 is quote atomic units per base atomic unit multiplied by `10^15`.
The reciprocal at offset 144 uses the same scale, so their product is approximately `10^30` after
integer-floor rounding. Changing only one of them moves one quote direction and leaves the other
where it was, which is why they are one invariant.

The sell ladder occupies bytes 160 through 639 and the buy ladder 640 through 1119. Each holds
twenty 24-byte records: directional capacity at `+0`, marginal-price factor at `+8`, enabled flag
at `+16`. Capacity and factor changes affect only their active quote direction. For a fill
contained in the first level, both directions match
`floor(input_atoms * directional_price * first_level_factor / 10^21)` exactly.

Offset 88 stores the age at which the program rejects a quote. Age 19 succeeds and age 20 fails
with custom error 65535 on a market configured at 20.

Both slot templates take the lead from the caller: the value supplied for `last_update_slot` is
added to the materialization slot, and only `null` falls back to the template's own lead. One stale
template therefore covers every market, including one configured at a limit nobody has seen yet.
`list_tessera_markets` returns each market's limit alongside its address; callers pass its negation. Passing a number where you meant the default is the one trap: `0` on the stale
template writes a perfectly fresh quote.

## Live market discovery

`list_tessera_markets` queries Tessera program accounts through the selected Surfnet RPC.
It filters by the manifest's account size and layout tag, validates ownership and mint identities,
and reads decimals from the referenced mint accounts. The freshness limit comes from offset 88.
The existing Surfnet account resolver merges remote discovery with local accounts, preferring local
state. Discovery needs a datasource that supports `getProgramAccounts`; offline instances can list
only their local accounts.

Studio loads this list when opening the fair-value dialog. The model uses the same tool to select
an override account and its freshness limit. There is no market list in `overrides.yaml`; the six
shared templates retain the SOL/USDC default address for callers that omit an account.

Labels use mint symbols from Surfpool's existing token metadata. An unknown mint is displayed by
its full address, so missing symbol metadata never hides a discovered market. Addresses are the
identities; symbols are not unique. Market membership, decimals and freshness limits are not taken
from the token metadata catalog.

`tessera_discovers_live_markets` exercises the production discovery function and checks the returned
mint identities, decimals and freshness limits against fetched accounts. It does not pin a market
count, so newly listed markets are included without changing the test or templates.

## Builders and tools

The fair-value builder converts a human price into reciprocal atomic ratios using both mints'
decimals. It is a pure function over account data. `create_tessera_fair_value_scenario` reads the market and both mints through the
surfnet's own RPC, so local state wins and only missing accounts fall back to the datasource, then
stages the scenario through the shared path.

The price override deliberately does not set `fetchBeforeUse`. Reading the accounts at creation
hydrates them into local state, so the values apply to the same bytes they were derived from; a
Play-time refetch would reinstall remote bytes over any local edit and patch a different read.

The paired freshness override is persisted. Its slot encoder writes the slot it materializes at,
so the prepared price stays inside the market's freshness window however long the scenario runs.

The `Tessera Depth Stress` AI chip requests a 90% reduction in both directions. The
`create_tessera_depth_scenario` tool reads current Surfnet state and takes remaining basis points
per direction: 1000 retains 10%, 10000 leaves that direction unchanged. It scales only enabled
capacities, with integer-floor rounding, and rejects zero capacities or increases. Prices, factors
and disabled levels are preserved. The scenario combines `tessera-depth` with persisted freshness;
depth itself is applied once. Creating another scenario reads the then-current state again.
Curve changes remain available through the raw template.

## Behavioral evidence

The live suite loads the pinned deployed ELF from ProgramData into LiteSVM and fails if the
ProgramData address, deploy slot, or ELF hash changes. It proves each price field controls only its
matching direction, that active-side capacities and factors alter large fills while the opposite
side stays byte-for-byte identical, that first-level output in both directions equals the
price-times-factor formula to the atom, that age 19 succeeds and age 20 fails with error 65535,
that disabling both required first levels fails both directions, that an unordered single curve
factor fails with error 8, and that live market discovery returns valid mint metadata.

Run it serially. The public endpoint sheds queued requests right after a `getProgramAccounts` scan,
sometimes as a 413 that looks like a request-size error:

```bash
SURFPOOL_TEST_RPC_URL=<rpc-url> cargo test -p surfpool-core --features integration-tests \
  tests::tessera -- --test-threads=1 --nocapture
```

`SURFPOOL_TEST_RPC_URL` is optional and defaults to the public mainnet endpoint. Set it to a
private endpoint when the public one rate-limits.

## Known boundaries

The remaining header and trailing fields carry no assigned semantics. No separate fee field is
exposed: the proven first-level output has no deduction beyond its directional price and factor,
but that does not establish how Tessera decomposes the factor into spread, fee, or another price
adjustment. The structured region from 1120 onward stays unexposed because its economic meaning has
not been behaviorally proven. Vault depletion is not exposed either; the generic SPL Token balance
template follows the Anchor discriminator path and does not materialize a non-Anchor token account.
