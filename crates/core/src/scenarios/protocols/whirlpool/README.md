# Whirlpool

Surfpool bundles the IDL and override templates for Orca's Whirlpool concentrated-liquidity AMM,
so a scenario can put a Whirlpool pool (or the protocol's shared fee config) into whatever state
you need before your code runs against it.

This is a how-to. For how scenarios work in general see the [scenarios README](../../README.md).
Every field's own purpose and units are on the template itself, visible in Studio and via
`get_override_templates`.

## Identity (verified 2026-09-21)

| | |
|---|---|
| Program ID | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` |
| Account codec | Anchor/Borsh (not zero-copy) - the standard IDL override path applies |
| `Whirlpool` account | 653 bytes, `reward_infos` is a fixed `[WhirlpoolRewardInfo; 3]` |
| `WhirlpoolsConfig` account | 108 bytes, one deployed singleton (below) |
| `TickArray` account | 9988 bytes, `ticks: [Tick; 88]` |
| Source | [orca-so/whirlpools](https://github.com/orca-so/whirlpools), `programs/whirlpool/src/state/{whirlpool,config,tick,tick_array}.rs` |
| Deployed config singleton | `2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ` - every Whirlpool pool is created under it |

A later upgrade that changes the config singleton, the account layout, or the fee constants below
means this integration must be revisited.

## Templates

| Template | Account | Address | Use for |
|---|---|---|---|
| `whirlpool-custom` | `Whirlpool` | caller-provided `pubkey` | any pool by raw address |
| `whirlpools-config` | `WhirlpoolsConfig` | fixed `pubkey`, the deployed singleton | the default protocol fee new pools are created with |

Both templates set `fetchBeforeUse: true` so non-overridden fields keep their live values; set it
to `false` only for a later override in the same scenario that builds on state an earlier override
already prepared.

### Why five hard-coded pool templates became one

Earlier revisions shipped one hard-coded template per pool (`whirlpool-sol-usdc`,
`-sol-usdt`, `-msol-sol`, `-orca-usdc`, `-popcat-sol`). All five wrote correct bytes, but they were
uncatalogued, had no `llm_context`, and one was mislabeled: `whirlpool-popcat-sol`
(`Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE`) is a SOL/USDC pool with tick spacing 4, not
POPCAT/SOL (verified live: `token_mint_a` = SOL, `token_mint_b` = USDC). They are retired in favor
of `whirlpool-custom`, which takes any pool by address.

## Finding a pool

There is no catalog: supply the pool's address directly. Look it up at
[`https://api.orca.so/v2/solana/pools`](https://api.orca.so/v2/solana/pools) (filter by mint pair,
sort by `tvl`), or confirm a candidate address directly against mainnet - it must be owned by the
Whirlpool program (`whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`), 653 bytes, and carry the
`Whirlpool` discriminator from the bundled IDL. A well-known example: SOL/USDC,
`Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE`.

## Field reference

### `Whirlpool`

| Field | Meaning | Units |
|---|---|---|
| `sqrt_price` | `sqrt(price) * 2^64` (Q64.64), where `price_raw = (sqrt_price/2^64)^2` is token B per token A before decimal adjustment | raw `u128` |
| `liquidity` | Active liquidity at `tick_current_index` | raw `u128` |
| `tick_current_index` | The tick implied by `sqrt_price`: `floor(log(price_raw) / log(1.0001))` | raw `i32` |
| `fee_rate` | Swap fee | hundredths of a basis point; divide by 1,000,000 (`3000` = 0.30%); program max `60000` (6%) |
| `protocol_fee_rate` | Protocol's cut, taken from the collected fee (not the swap) | basis points of the fee; divide by 10,000 (`1300` = 13%, the live default); program max `2500` (25%) |

`sqrt_price` and `tick_current_index` describe the same state and must be set together - see the
coupling rule below. `fee_rate` and `protocol_fee_rate` are independent and either can be set
alone.

### `WhirlpoolsConfig`

| Field | Meaning | Units |
|---|---|---|
| `default_protocol_fee_rate` | Read once, at pool creation, to seed a **new** pool's own `protocol_fee_rate`; has no effect on existing pools | basis points of the fee; divide by 10,000 (`1300` = 13%, the live default); program max `2500` |

`fee_authority`, `collect_protocol_fees_authority` and `reward_emissions_super_authority` are
access-control keys, not economic inputs the swap path reads - overriding them changes who is
*allowed* to change fees, not the fees themselves, so they are intentionally not exposed.

## The `sqrt_price` / `tick_current_index` coupling rule

```
price_raw = (sqrt_price / 2^64)^2
price     = price_raw * 10^(decimals_a - decimals_b)
tick_current_index = floor(log(price_raw) / log(1.0001))
```

A swap walks the `TickArray` whose `start_index` brackets `tick_current_index`
(`start_index = floor(tick_current_index / (tick_spacing * 88)) * tick_spacing * 88`, PDA
`["tick_array", whirlpool, start_index.to_string()]`, 9988 bytes). Setting `tick_current_index`
inconsistently with `sqrt_price`, or to a tick with no initialized `TickArray`, fails the swap
rather than silently mispricing it. A swap's output is also paid from `token_vault_a` /
`token_vault_b` (plain `spl-token` accounts, overridden separately via the `spl-token`
templates) - raising `liquidity` or moving `sqrt_price` without funding the vault the swap pays
out from fails on the token transfer, not on this override.

## Worked example: halve a pool's price, by address

```json
{
  "id": "whirlpool-price-shock",
  "templateId": "whirlpool-custom",
  "label": "halve the SOL/USDC price",
  "enabled": true,
  "scenarioRelativeSlot": 0,
  "fetchBeforeUse": true,
  "account": { "pubkey": "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ" },
  "values": {
    "sqrt_price": "<old_sqrt_price / sqrt(2), as a decimal string>",
    "tick_current_index": "<floor(log((new_sqrt_price/2^64)^2) / log(1.0001))>"
  }
}
```

SOL/USDC (`HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ`) is `token_mint_a` = SOL (9 decimals),
`token_mint_b` = USDC (6 decimals), so `price = price_raw * 10^3`. `sqrt_price` is a `u128`; pass
it as a decimal string once it exceeds `2^53` (JSON number precision), as with any large
`u64`/`u128`/`i128` value.

## Verification

- Wiring and shape: `cargo test -p surfpool-core whirlpool` - the `whirlpools-config` literal
  address, and that every property on both templates has a label and description.
- Registry wiring: `cargo test -p surfpool-core registry` - template count and per-protocol counts.
- MCP surface: `cargo test -p surfpool-mcp` - generic template listing and scenario validation
  exercise both templates without any Whirlpool-specific test code.
- Integration: `cargo test -p surfpool-core --features integration-tests whirlpool -- --test-threads=1`
  needs a network connection (`SURFPOOL_TEST_RPC_URL` overrides the public mainnet endpoint). It
  round-trips a live SOL/USDC `Whirlpool` account through the bundled IDL (catching layout drift),
  proves the `fee_rate` and `sqrt_price` overrides touch only their own bytes, confirms the
  `whirlpools-config` singleton against a live pool's own field, and confirms the `TickArray` for
  the current tick exists on two pools.
