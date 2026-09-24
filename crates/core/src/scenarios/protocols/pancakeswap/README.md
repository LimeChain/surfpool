# PancakeSwap CLMM (Solana)

Declarative state-preparation templates for PancakeSwap's concentrated liquidity AMM on
Solana, a fork of Raydium CLMM (`amm_v3`) with identical `PoolState`, `AmmConfig` and
`TickArrayState` layouts and discriminators. The templates use the standard IDL override
path, and the price-shock builder is Raydium's, run with the PancakeSwap program id. For
how scenarios work in general see the [scenarios README](../../README.md).

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
`sqrt_price_x64`, or use the MCP tool below.

## Prepare a price shock through MCP

`create_pancakeswap_price_shock_scenario` reads the pool through the running surfnet,
multiplies its price by a factor, recomputes the matching `tick_current`, and stages a
one-override scenario (`pancakeswap-clmm-pool-state`, slot 1, `fetchBeforeUse: true`).

| Param         | Type             | Meaning                                                       |
| ------------- | ---------------- | ------------------------------------------------------------- |
| `pool`        | string           | Base58 address of the `PoolState` account                     |
| `priceFactor` | string           | `0.5` halves the price, `4` quadruples it; finite, > 0, not 1 |
| `surfnetPort` | number, optional | Port of the target surfnet; omit for 8899                     |

```json
{ "pool": "DJNtGuBGEQiUCWE8F981M2C3ZghZt2XLD8f2sQdZ6rsZ", "priceFactor": "0.999" }
```

It checks the pool's owner, size and discriminator, and that the tick array
`["tick_array", pool, start_index as i32 big-endian]` covering the new tick exists and is
owned by the program. A missing array is rejected with the factor closest to the request
that stays on the pool's current array. The result carries `error` and the Studio editor
`url`.

## Verification

- Static: the `PoolState` and `TickArrayState` type definitions (fields, types, order) and
  the `PoolState` discriminator are identical in `pancakeswap/v1/idl.json` and
  `raydium/v3/idl.json`.
- Unit: `cargo test -p surfpool-core --lib pancakeswap` covers the pool PDA for a live fee
  tier and mint pair, every `amm_config_index` option's derived address, and the tick
  array PDA the PancakeSwap program id derives for a live pool. The price-shock
  arithmetic is covered once, by the Raydium CLMM builder tests.
- MCP: `cargo test -p surfpool-mcp pancakeswap` covers the camelCase param schema and
  input rejection before any RPC.
- Live fork (`cargo test -p surfpool-core --features integration-tests pancakeswap --
  --test-threads=1`, needs network; `SURFPOOL_TEST_RPC_URL` overrides the endpoint):
  round-trips the live SOL/USDC `PoolState` byte-for-byte, checks every
  `amm_config_index` option's live size, discriminator, `tick_spacing` and
  `trade_fee_rate`, runs `status`, `liquidity` and `tick_current` through the
  materializer and asserts only their bytes changed, and runs the shared price-shock
  builder on the live pool and checks the derived tick array's owner, `pool_id` and start
  index.

## Known limitations

- `token_mint_0`/`token_mint_1` are plain `property_ref` seeds, so the caller orders them.
  Raydium CLMM has the same gap.
- No swap simulation compares baseline and shocked prices, so the evidence for a price
  override is byte-level, not trade-level.
- The layouts are identical and the engine picks the IDL by account owner, so a PancakeSwap
  template pointed at a Raydium CLMM pool writes the same bytes. Check the pool's owner.
