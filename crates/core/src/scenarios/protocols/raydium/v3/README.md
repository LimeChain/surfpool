# Raydium CLMM (v3)

Declarative state-preparation templates for Raydium's concentrated liquidity AMM
(`amm_v3`, the program most people mean by "Raydium CLMM"). The program publishes an
Anchor IDL, so the templates use the standard IDL override path; `PoolState` is a
packed zero-copy struct rather than ordinary Borsh, but its fields are all fixed-width
primitives in declared order, so it round-trips through the same generic IDL codec.

This is a how-to. For how scenarios work in general see the
[scenarios README](../../../README.md). Every field's own purpose and units are also on
the template itself, visible in Studio and via `get_override_templates`.

## Program identity (verified 2026-09-20)

|                     |                                                                         |
| ------------------- | ----------------------------------------------------------------------- |
| Program ID          | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`                          |
| ProgramData         | `HzD2cCXXT3UQNjMMY6kDv9w6gZ9qquSdfoGXrLL3LXx`                           |
| Last deployed slot  | 439846317                                                               |
| Source              | [raydium-io/raydium-clmm](https://github.com/raydium-io/raydium-clmm), `amm_v3` |
| Bundled IDL         | `idl.json`, `amm_v3` spec/version `0.1.0`                               |

A later deployment slot than the one above means the program was upgraded and this
integration must be revisited (layouts, formulas, fee wiring).

## Templates

| Template                    | Account     | Address                                                          | Use for                                                           |
| ---------------------------- | ----------- | ----------------------------------------------------------------- | ------------------------------------------------------------------ |
| `raydium-clmm-custom`        | `PoolState` | PDA `["pool", PDA(["amm_config", index_be]), token_mint_0, token_mint_1]` | any pool, selected by fee tier index and the two mints             |
| `raydium-clmm-pool-state`    | `PoolState` | caller-provided pubkey                                            | any pool by raw address                                            |
| `raydium-clmm-amm-config`    | `AmmConfig` | PDA `["amm_config", index_be]`                                    | a fee tier shared by every pool created with that index           |

Notes:

- `token_mint_0`/`token_mint_1` on `raydium-clmm-custom` are plain `property_ref` seeds,
  in the order the caller supplies them - the caller must pass the numerically smaller
  raw pubkey first (see Field reference). A sorted-pair seed kind that would enforce
  this automatically is being added to the scenario engine separately; once available,
  it should replace these two seeds.
- The `amm_config_index` catalog's `tick_spacing` metadata for `ultra_tight` (index 0)
  and `wide` (index 3) does not match the live `AmmConfig.tick_spacing` (10 and 120,
  not the documented 1 and 100); `fee_rate_bps` and every `derived_address` are correct.
  Found while writing the live tests below; not fixed here since it's outside this
  package's scope.

### Finding a pool

`raydium-clmm-custom` derives a pool's address from its fee tier and its two mints, so
no lookup is needed when you already know those. To use `raydium-clmm-pool-state`
instead, or to check a derived address by hand:

- Raydium's public API lists CLMM pools by liquidity:
  `https://api-v3.raydium.io/pools/info/list?poolType=concentrated&poolSortField=liquidity`.
- To confirm an address by hand: fetch the account, check it is owned by
  `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`, is 1544 bytes, and its first 8 bytes
  match `PoolState`'s discriminator in `idl.json`.
- A well-known example: the SOL/USDC CLMM pool (fee tier index 8, tick spacing 1) at
  `3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv`.

## Field reference

### `PoolState`

| Field            | Meaning                                                                 | Override it to                                          |
| ---------------- | ------------------------------------------------------------------------ | --------------------------------------------------------- |
| `liquidity`      | In-range liquidity available to the pool (raw `u128`, no decimals)       | model a deep or shallow book around the current price     |
| `sqrt_price_x64` | `sqrt(token_1/token_0)` in Q64.64 fixed point (raw `u128`)                | shock the price - **must move with `tick_current`**       |
| `tick_current`   | Tick index of the last price transition (`i32`)                          | must agree with `sqrt_price_x64` - see the formula below   |
| `status`         | Bitmask (`u8`): bit0 open/increase liquidity, bit1 decrease liquidity, bit2 collect fee, bit3 collect reward, bit4 swap, all "disable" flags | flip bits on to test a rejected instruction; `0` is normal |

**Price / tick coupling.** `sqrt_price_x64` and `tick_current` describe the same price
and a swap will not tolerate them disagreeing:

- raw price = `(sqrt_price_x64 / 2^64)^2`, a `token_1`-per-`token_0` ratio in raw
  (undecimaled) units; multiply by `10^(mint_decimals_0 - mint_decimals_1)` for a
  human price
- `tick_current = floor(log(raw price) / log(1.0001))`
- a swap walks `TickArrayState` accounts starting from `tick_current`; if it disagrees
  with `sqrt_price_x64`, or the tick array covering it was never initialized, the swap
  fails. Overriding one field without recomputing the other from the formula above
  leaves the pool priced in a way no real swap can trade against.

**Mint order.** `token_mint_0` is always the mint whose raw 32-byte pubkey is
numerically smaller than `token_mint_1`'s (compare decoded bytes, not the base58
string) - this is part of the pool's own PDA derivation, not a UI convention. SOL
(`So1111...`) sorts before USDC (`EPjFWdd...`) as raw bytes, matching mainnet's main
SOL/USDC pool.

**Vaults.** `sqrt_price_x64`/`tick_current` change the pool's quoted price only; they
do not move `token_vault_0`/`token_vault_1` balances. A swap that should trade at depth
against the new price still needs the vaults funded to support it.

### `AmmConfig`

All three fee fields share `FEE_RATE_DENOMINATOR_VALUE = 1,000,000`, verified against
`raydium-io/raydium-clmm`'s `create_amm_config` and `swap` instructions:

| Field               | Meaning                                                                                  | Override it to                        |
| -------------------- | ------------------------------------------------------------------------------------------ | ---------------------------------------- |
| `trade_fee_rate`     | Fee on trade **volume**: `fee = amount_in * trade_fee_rate / 1_000_000`. Must stay `< 1_000_000`. | change the swap fee, e.g. `2500` = 0.25% |
| `protocol_fee_rate`  | Share of the fee **already collected** by `trade_fee_rate` (not of volume) routed to the protocol treasury, in parts per million | model protocol revenue changes           |
| `fund_fee_rate`      | Same as `protocol_fee_rate` but routed to the fund owner. The program requires `protocol_fee_rate + fund_fee_rate <= 1_000_000`. | model fund revenue changes               |

`protocol_fee_rate`/`fund_fee_rate` are easy to misread: `120000` does **not** mean 12%
of the trade, it means 12% of the fee `trade_fee_rate` already took. This `AmmConfig`
is shared by every pool created with the same index, so overriding it changes fees for
all of them at once.

## Worked example: shock the SOL/USDC pool's price and fee

Move the main SOL/USDC pool's price up and drop its fee tier's protocol cut, keeping
`sqrt_price_x64`/`tick_current` consistent:

```json
{
  "templateId": "raydium-clmm-pool-state",
  "address": "3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv",
  "values": {
    "sqrt_price_x64": "6200000000000000000",
    "tick_current": -22000
  },
  "fetchBeforeUse": true
}
```

```json
{
  "templateId": "raydium-clmm-amm-config",
  "values": {
    "config_index": "8",
    "protocol_fee_rate": 60000
  },
  "fetchBeforeUse": true
}
```

The pool override alone is enough for a price-reading client; pairing it with the
`AmmConfig` override in the same scenario is what changes what a real swap against
this pool would cost, matching the field reference's "shared by every pool" note.

## Verification

- Static: `PoolState`'s declared IDL fields sum to exactly `1536 + 8` bytes (1544
  total with the discriminator); `AmmConfig`'s sum to `109 + 8` (117 total). Confirmed
  by decoding live accounts, not just counting IDL field widths.
- Unit: `cargo test -p surfpool-core raydium` - PDA derivation for the pool and for
  every `amm_config_index` catalog option (pinned to the documented addresses), mint
  order sensitivity, and every template property resolving against the bundled IDL.
- Live-fork byte diff / remote: `cargo test -p surfpool-core --features
  integration-tests raydium_clmm -- --test-threads=1` (needs a network connection;
  `SURFPOOL_TEST_RPC_URL` overrides the public mainnet endpoint). Round-trips the live
  SOL/USDC `PoolState` through the bundled IDL byte-for-byte, proves `liquidity` and
  `tick_current` overrides touch only their own field's bytes on that live account, and
  fetches every `amm_config_index` option live to check 117 bytes and the `AmmConfig`
  discriminator.
- MCP surface: `cargo test -p surfpool-mcp` - generic template listing and catalog
  scoping cover this protocol like every other one; no protocol-specific MCP test
  exists here (none was needed, unlike Pump's graduation builder).
- Not verified here: a behavioral swap simulate (baseline vs. shocked price/fee) is out
  of this package's scope; see the open questions below.

## Open questions for the orchestrator

- The sorted-pair seed kind mentioned in `AMM-MILESTONE-3.4-PLAN.md`'s WP-B is not yet
  in the engine; `raydium-clmm-custom`'s `token_mint_0`/`token_mint_1` should switch to
  it once available (see Templates notes above).
- Pre-existing `tick_spacing` metadata defect in `amm_config_index` (see Templates
  notes) - found, not fixed, since it predates this package's scope.
- No behavioral swap harness (baseline vs. price-shock simulate) was built; the plan
  document scopes one to WP-B but this package's brief did not ask for it.
