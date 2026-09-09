# GoonFi

GoonFi V2 uses raw account layouts rather than an IDL. Each market points to a 32-byte
oracle owned by a companion publisher program. The oracle stores bid/ask prices; the
market stores the reference prices that guard them. Surfpool prepares these accounts
before a user runs a strategy. Product scenarios do not construct or submit swaps.

## Pinned deployment

The live tests in `crates/core/src/tests/goonfi/mod.rs` check these ProgramData sizes,
deployment slots and ELF hashes before replaying the program:

| | Trading program | Oracle publisher |
|---|---|---|
| Program | `goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE` | `dijkbkCAKfFTCxQg3u1pg82gVU1jJGHBBRcteD11mBu` |
| ProgramData | `124gUYwjVnJQ4sJsFug9gHPzPLEtwCbAQC5LkbaDgx9s` | `7btzN5NEjnZqdQECwT88XhixeGnZjz5YKqjYGYKxKE5z` |
| ProgramData bytes | 252,429 | 557 |
| Deployment slot | 438563879 | 404369628 |
| ELF SHA-256 | `73e580830356c7a086d8bec422790b2600108a8129faebdfc055bd46d8936c2e` | `0fc545beb6abd12682ae68a27fa1e2a22d86d5d1dbbbe6d1e8f49e53ef762695` |

A deployment change requires revalidation. These are test pins, not an upgrade-monitoring
service or a claim that every future deployment has the same layout.

## Layouts and templates

A market is 2048 bytes with magic `30 bc 2f 35 34 58 32 9a` at offset 0. Its base/quote
mints are at offsets 80/112, vaults at 144/176, and oracle pointer at 208. The oracle is
32 bytes with no discriminator. Both YAML layouts declare their expected program owner;
the shared materializer checks ownership before writing, then validates size, optional
magic bytes and write bounds. A failed owner check skips the override with a warning.

| Template | Account | Fields |
|---|---|---|
| `goonfi-price` | Oracle | Bid and ask, u64 at offsets 0 and 8 |
| `goonfi-stale-quote` | Oracle | Freshness slot, u32 at offset 16; default lead -2000 |
| `goonfi-freshness` | Oracle | Freshness slot, u32 at offset 16; default lead 0 |
| `goonfi-reference-band` | Market | Reference prices, u64 at offsets 1712 and 1720 |

Prices use the human pair price multiplied by `10^6`, independent of mint decimals.
For example, 99.74 quote tokens per base token becomes the integer string `"99740000"`.
Use strings for u64 price values to preserve precision in JSON and Studio.

Slot templates write exactly four bytes. The u32 multiplier at offset 20 and the
millisecond timestamp at offset 24 remain untouched. A slot value of `null` selects the
template's default lead; an integer specifies a lead relative to the materialization
slot. The resulting slot must fit u32.

## Catalog and price scenario

The backend exposes three GoonFi MCP tools:

- `list_goonfi_markets` discovers program accounts and validates market, oracle and mint
  relationships. It returns market/oracle addresses, labels, mint addresses and decimals.
  The YAML files contain no market catalog, and discovery does not require a fixed count.
- `create_goonfi_price_scenario` accepts a market address and a positive human price with
  up to six decimal places. It resolves the oracle from the market account, validates
  both accounts, and composes three overrides: equal oracle bid/ask, equal market reference
  prices, and persistent freshness. An omitted market selects the default SOL/USDC market.
- `create_goonfi_liquidity_scenario` accepts a market address and per-vault remaining basis
  points. It resolves both token vaults from the market's own pointers (offsets 144 and
  176), reads each current balance, validates the vault and oracle owners, and scales each
  vault through `spl-token-account-balance`: 0 drains a vault so a swap rejects with `0x1`,
  10000 leaves it unchanged. A persistent freshness override keeps the rejection about
  liquidity rather than a stale quote. Both default to 0; an omitted market selects the
  default SOL/USDC market.

These tools accept optional `surfnet_port`, defaulting to 8899, and read through the local
Surfnet RPC. Missing accounts fall back to that Surfnet's datasource. The price tool
stages through the shared Studio scenario API; Play registers the scenario.

Studio's PMM fair-value dialog selects a protocol, a live market and a human price. It
calls these tools through Studio MCP without forwarding `rpcUrl` or `surfnet_port`,
matching the Tessera dialog convention. Consequently, these Studio GoonFi calls use the
backend's default RPC port. Studio retains only each catalog entry's market address and
label; the backend resolves the oracle when creating a price scenario.

The price builder does not set `fetchBeforeUse`: the accounts read at creation retain
local edits, and only the specified fields are changed. Freshness uses `persist: true`
to stamp each subsequent materialization slot. These settings do not establish
transactional atomicity across all overrides in a scenario.

## Composing other prepared states

The four templates remain available through the generic scenario editor and AI flow.
There are no dedicated GoonFi spread or delayed-event builders.

For a stale quote, target the oracle returned by `list_goonfi_markets` with
`goonfi-stale-quote`. Do not run a persistent freshness override over the same interval:
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

- Unchanged encoding produces the same fill; coupled price/reference changes alter output.
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
it does not prove that the same trade currently has sufficient mainnet liquidity. Layout
and discovery checks use unfunded fetched accounts. Owner-predicate unit tests live in
`crates/types/src/scenarios.rs`. This suite does not provide a `pmm-sim` differential run
or a Studio browser test.

Run all GoonFi unit and live checks serially:

```bash
SURFPOOL_TEST_RPC_URL=<rpc-url> cargo test -p surfpool-core --features integration-tests \
  goonfi -- --test-threads=1 --nocapture
```

The RPC variable is optional and defaults to the public mainnet endpoint. A private endpoint
can avoid public RPC rate limits. Re-run after a program upgrade or account-layout change.

## Known boundaries

The staleness window's on-chain source and exact decay formula remain unidentified.
Observed windows vary by market and time; historical slot ages are not fixed protocol
limits. The global account and other market fields are forked without assigned override
semantics. No enable/disable field is exposed. Direct top-level swaps are not covered by
the CPI replay, and the exact tolerance of the reference-band guard is not established
by these tests.
