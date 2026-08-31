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

A market is 1264 bytes with the eight-byte layout tag `05 00 00 00 00 00 00 00` at offset 96. All
26 live markets carry that tag, and no other Tessera account is 1264 bytes, so size alone already
separates markets from the program's other accounts today. The tag is the version half of the
guard: it is what rejects a future market layout that reuses the size.

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
Each market's limit travels with its address in the catalog, so a caller reads it there and passes
its negation. Passing a number where you meant the default is the one trap: `0` on the stale
template writes a perfectly fresh quote.

## The market catalog

`v1/overrides.yaml` carries a `market` constant listing every live market with its mints, their
decimals, and its freshness limit. The UI constrains the choice to it and `search_constant_options`
resolves it for models. It is a snapshot, captured 2026-08-31 (26 markets:
fourteen with freshness limit 20, eleven at 25, one at 55): Tessera lists markets continuously,
a new one reaches the catalog on the next refresh, and until then the raw scenario API still
accepts its address directly.

Refresh it by reading the live set and rewriting the `options` block:

```bash
curl -s -X POST "$RPC_URL" -H 'Content-Type: application/json' -d '{
  "jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH",
    {"encoding":"base64","commitment":"confirmed",
     "filters":[{"dataSize":1264}],
     "dataSlice":{"offset":24,"length":72}}]}'
```

Each result yields the base mint at `+0`, the quote mint at `+32`, and the freshness limit at
`+64`. Decimals come from the two mint accounts. `tessera_catalog_matches_live_markets` fails when
the catalog and the chain disagree, so a stale catalog is caught rather than shipped.

## Builder and tool

One builder exists, for the one thing a template cannot express: turning a human price into the
pair of reciprocal atomic ratios, which needs both mints' decimals. It is a pure function over
account data. `create_tessera_fair_value_scenario` reads the market and both mints through the
surfnet's own RPC, so local state wins and only missing accounts fall back to the datasource, then
stages the scenario through the shared path.

The price override deliberately does not set `fetchBeforeUse`. Reading the accounts at creation
hydrates them into local state, so the values apply to the same bytes they were derived from; a
Play-time refetch would reinstall remote bytes over any local edit and patch a different read.

The paired freshness override is persisted. Its slot encoder writes the slot it materializes at,
so the prepared price stays inside the market's freshness window however long the scenario runs.

Depth and curve have no builder here. Their templates expose every field, and the scaling helpers
that read a live ladder and preserve its ordering are parked until a product flow asks for them.

## Behavioral evidence

The live suite loads the pinned deployed ELF from ProgramData into LiteSVM and fails if the
ProgramData address, deploy slot, or ELF hash changes. It proves each price field controls only its
matching direction, that active-side capacities and factors alter large fills while the opposite
side stays byte-for-byte identical, that first-level output in both directions equals the
price-times-factor formula to the atom, that age 19 succeeds and age 20 fails with error 65535,
that disabling both required first levels fails both directions, that an unordered single curve
factor fails with error 8, and that the catalog matches the live market set.

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
