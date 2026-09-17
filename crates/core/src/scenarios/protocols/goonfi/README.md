# GoonFi

GoonFi V2 uses raw account layouts rather than an IDL. Each market points to a 32-byte
oracle owned by a companion publisher program. The oracle stores bid/ask prices; the
market stores the reference prices that guard them. Surfpool prepares these accounts
before a user runs a strategy. Product scenarios do not construct or submit swaps.

## Pinned deployment

The live tests in `crates/core/src/tests/goonfi/mod.rs` pin both programs by ProgramData
size, deployment slot and ELF SHA-256 (the constants at the top of that file) before
replaying the program. A deployment change requires revalidation. These are test pins, not
an upgrade-monitoring service or a claim that every future deployment has the same layout.

## Layouts and templates

`market_overrides.yaml` and `oracle_overrides.yaml` carry the two layouts, every field
offset and the usage notes (`llm_context`) for the four templates: `goonfi-price` and
`goonfi-reference-band` move the oracle bid/ask and the market's reference band as one
invariant; `goonfi-stale-quote` and `goonfi-freshness` write the oracle's 4-byte slot with
a default lead of -2000 or 0. Both layouts declare their program owner; the shared
materializer and builders use the same owner predicate. The materializer then validates size,
optional magic bytes and write bounds. A failed owner check skips the override with a warning.

## Catalog and price scenario

The backend exposes three MCP tools, `list_goonfi_markets`, `create_goonfi_price_scenario`
and `create_goonfi_liquidity_scenario`; their `#[tool(description)]` and parameter
descriptions in `crates/mcp/src/surfpool/mod.rs` document the arguments and defaults. The
YAML files contain no market catalog, and discovery does not require a fixed count.

These tools accept optional `surfnetPort`, defaulting to 8899, with camelCase argument
names like the pump tool, and read through the local Surfnet RPC. Missing accounts fall
back to that Surfnet's datasource. The scenario tools stage through the shared Studio
scenario API; Play registers the scenario.

Direct library callers pass each vault and oracle as `(Pubkey, &Account)`, preserving
the address used to read its data. Builders compare these addresses with the market's
pointers before using any balance or preparing overrides. Owner, token mint and vault
authority checks validate the account graph; they do not authenticate arbitrary bytes
that a caller deliberately labels with an unrelated address. RPC reads remain outside
the pure builders, as in Pump's graduation preparation.

The price builder does not set `fetchBeforeUse`: the accounts read at creation retain
local edits, and only the specified fields are changed. Freshness writes the
materialization slot once, like every other override: a forked oracle is not
republished, so the stamp only has to be recent enough for the scenario's own slots.
A scenario that runs past the staleness window schedules another refresh at a later
slot. These settings do not establish transactional atomicity across all overrides in
a scenario.

## Composing other prepared states

The four templates remain available through the generic scenario editor and AI flow.
There are no dedicated GoonFi spread or delayed-event builders.

For a stale quote, target the oracle returned by `list_goonfi_markets` with
`goonfi-stale-quote`. Do not schedule a freshness override over the same interval:
it would erase the stale state. Recovery can use `goonfi-freshness` at a later relative
slot. The Studio AI chip requests a stale-quote scenario through this generic flow.

For depletion, `create_goonfi_liquidity_scenario` resolves the vaults from the market and
scales each balance for you; the AI chip calls it directly. Composing the same by hand
means reading the selected vault address from market offset 144 or 176, checking its token
program, and using `spl-token-account-balance` with an absolute amount, applied once. The
override does not recalculate percentages at execution time.

## Behavioral verification

The live suite fetches deployed account data and runs the pinned trading ELF in LiteSVM,
using a builtin wrapper for the Jupiter-shaped CPI. It checks:

- Coupled price/reference changes move the fill linearly in both directions.
- Raising only the bid or lowering only the ask rejects with `0x24` (reference-band guard).
- Quotes decay with slot age and eventually reject with `0x15`. Changing the multiplier
  changes decay in the tested fixture; stamping the slot restores freshness. Changing
  the wall-clock timestamp alone does not change the tested fill.
- An impossible minimum output rejects with `0xf`.
- A successful sell still fills with exactly enough quote inventory. One atomic unit less
  or an empty quote vault rejects with `0x1`, with the trade input held constant.
- The price builder's three overrides register and materialize through the production
  path on two markets, preserving unrelated bytes and refreshing the u32 slot afterwards.
- Live discovery returns valid market/oracle relationships without a fixed catalog count.

Behavior fixtures fund local vaults to at least 10,000 whole tokens and retain wrapped SOL
backing. This isolates price, ageing and inventory changes from fluctuating live liquidity;
it does not prove that the same trade currently has sufficient mainnet liquidity.

Run the suite serially with the command at the top of `crates/core/src/tests/goonfi/mod.rs`.
`SURFPOOL_TEST_RPC_URL` is optional and defaults to the public mainnet endpoint; a private
endpoint can avoid public RPC rate limits. Re-run after a program upgrade or account-layout
change.

## Known boundaries

The staleness window's on-chain source and exact decay formula remain unidentified.
Observed windows vary by market and time; historical slot ages are not fixed protocol
limits. The global account and other market fields are forked without assigned override
semantics. No enable/disable field is exposed. Direct top-level swaps are not covered by
the CPI replay, and the exact tolerance of the reference-band guard is not established
by these tests.
