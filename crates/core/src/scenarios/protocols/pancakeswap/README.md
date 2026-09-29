# PancakeSwap CLMM (Solana)

Declarative state-preparation templates for PancakeSwap's concentrated liquidity AMM on
Solana, a fork of Raydium CLMM (`amm_v3`) with identical `PoolState`, `AmmConfig` and
`TickArrayState` layouts and discriminators. The templates use the standard IDL override
path. For how scenarios work in general see the [scenarios README](../../README.md).

## Program identity (verified 2026-09-21)

|                      |                                                                                         |
| -------------------- | --------------------------------------------------------------------------------------- |
| Program ID           | `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq`                                          |
| On-chain IDL account | `CAD7TZySgk4RkqyAkGu3bJFvf8itWsfTtpViRK8TJrKf`                                          |
| Fork of              | Raydium CLMM (`CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`)                           |
| Bundled IDL          | `v1/idl.json`, decompressed from the on-chain IDL account 2026-09-21; `amm_v3` `0.1.0`  |

The bundled IDL keeps every account and type from the on-chain IDL and drops
`instructions`, `errors` and `events`. A later on-chain IDL with a different
`types`/`accounts` shape means the program was upgraded and this integration must be
revisited.

## Templates

| Template                      | Account     | Address                                                                   | Use for                                                 |
| ----------------------------- | ----------- | ------------------------------------------------------------------------- | ------------------------------------------------------- |
| `pancakeswap-clmm-custom`     | `PoolState` | PDA `["pool", PDA(["amm_config", index_be]), token_mint_0, token_mint_1]` | any pool, selected by fee tier index and the two mints  |
| `pancakeswap-clmm-pool-state` | `PoolState` | caller-provided pubkey                                                    | any pool by raw address                                 |
| `pancakeswap-clmm-amm-config` | `AmmConfig` | PDA `["amm_config", index_be]`                                            | a fee tier shared by every pool created with that index |

`token_mint_0` must be the mint whose raw 32-byte pubkey is numerically smaller (compare
decoded bytes, not base58). Nothing checks the order: a reversed pair derives an empty
address. `amm_config_index` lists all 22 live fee tiers, each with its tick spacing, fee
and derived address.

## Finding a pool

- Browse pools at `https://solana.pancakeswap.finance/liquidity-pools/`.
- A pool account is owned by `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq`, is 1544
  bytes, and starts with `PoolState`'s discriminator from `v1/idl.json`.
- Example: the deepest SOL/USDC pool (fee tier index 0, tick spacing 10) at
  `DJNtGuBGEQiUCWE8F981M2C3ZghZt2XLD8f2sQdZ6rsZ`.

## Field reference

Every field, unit and rule is the same as Raydium CLMM's; see the
[Raydium CLMM field reference](../raydium/v3/README.md#field-reference). In short:
`sqrt_price_x64` and `tick_current` must satisfy
`tick_current = floor(log((sqrt_price_x64 / 2^64)^2) / log(1.0001))`, the `status` bitmask
disables instructions (bit4 = swap), and the three `AmmConfig` rates are parts per
million.

## Worked example: halve the SOL/USDC pool's price and disable swaps

Starting from `sqrt_price_x64` 6213368270497578833 (tick -21765), halving the price
multiplies `sqrt_price_x64` by `sqrt(0.5)`:

```json
{
  "templateId": "pancakeswap-clmm-pool-state",
  "address": "DJNtGuBGEQiUCWE8F981M2C3ZghZt2XLD8f2sQdZ6rsZ",
  "values": {
    "sqrt_price_x64": "4393514838078169088",
    "tick_current": -28697,
    "status": 16
  },
  "fetchBeforeUse": true
}
```

`(4393514838078169088 / 2^64)^2 = 0.056726` and `floor(log(0.056726) / log(1.0001)) =
-28697`, so the pair satisfies the coupling rule. `status: 16` sets bit4 (disable swap)
only. The pool's price moves between reads, so recompute both fields from its current
`sqrt_price_x64`.

## Price shock

Multiplying the price by `f` means `sqrt_price_x64' = round(sqrt_price_x64 * sqrt(f))`,
bounded to `[4295048016, 79226673521066979257578248091)`, and
`tick_current' = floor(2 * ln(sqrt_price_x64' / 2^64) / ln(1.0001))`, bounded to
`[-443636, 443636]`. The active liquidity must follow the price:
`liquidity' = liquidity + sum(liquidity_net of initialized ticks crossed)` when it rises,
`liquidity - sum(...)` when it falls. Set all three fields in one
`pancakeswap-clmm-pool-state` override with `fetchBeforeUse: true`.

A tick `t` is crossed when `old < t <= new` going up and `new < t <= old` going down. Without
the liquidity step the pool keeps the old range's depth at the new price, so swaps succeed with
wrong amounts: on the SOL/USDC pool a +10% move left the pool about 2.5x deeper than a real swap to the
same price.

A swap resumes from the tick array covering `tick_current'`: PDA
`["tick_array", pool, start_index as i32 big-endian]` under the program, 60 ticks per
array, `start_index = floor(tick / (tick_spacing * 60)) * tick_spacing * 60`. The destination
array must exist (a null `getAccountInfo` on its PDA means the swap will fail). The Studio
card computes all of this for you.

## Verification

- Static: the `PoolState` and `TickArrayState` type definitions (fields, types, order) and
  the `PoolState` discriminator are identical in `pancakeswap/v1/idl.json` and
  `raydium/v3/idl.json`.
- Unit: `cargo test -p surfpool-core --lib pancakeswap` covers the pool PDA for a live fee
  tier and mint pair and every `amm_config_index` option's derived address.
- Live fork (`cargo test -p surfpool-core --features integration-tests pancakeswap --
  --test-threads=1`, needs network; `SURFPOOL_TEST_RPC_URL` overrides the endpoint):
  round-trips the live SOL/USDC `PoolState` byte-for-byte, checks every
  `amm_config_index` option's live size, discriminator, `tick_spacing` and
  `trade_fee_rate`, runs `status`, `liquidity` and `tick_current` through the
  materializer and asserts only their bytes changed.

## Known limitations

- `token_mint_0`/`token_mint_1` are plain `property_ref` seeds, so the caller orders them.
  Raydium CLMM has the same gap.
- No swap simulation compares baseline and shocked prices, so the evidence for a price
  override is byte-level, not trade-level.
- The layouts are identical and the engine picks the IDL by account owner, so a PancakeSwap
  template pointed at a Raydium CLMM pool writes the same bytes. Check the pool's owner.
