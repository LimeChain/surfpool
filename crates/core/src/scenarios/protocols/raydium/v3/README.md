# Raydium CLMM (v3)

Declarative state-preparation templates for Raydium's concentrated liquidity AMM
(`amm_v3`, the program most people mean by "Raydium CLMM"). The program publishes an
Anchor IDL, so the templates use the standard IDL override path. For how scenarios work
in general see the [scenarios README](../../../README.md).

## Program identity (verified 2026-09-20)

|                     |                                                                                 |
| ------------------- | ------------------------------------------------------------------------------- |
| Program ID          | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`                                  |
| ProgramData         | `HzD2cCXXT3UQNjMMY6kDv9w6gZ9qquSdfoGXrLL3LXx`                                   |
| Last deployed slot  | 439846317                                                                       |
| Source              | [raydium-io/raydium-clmm](https://github.com/raydium-io/raydium-clmm), `amm_v3` |
| Bundled IDL         | `idl.json`, `amm_v3` spec/version `0.1.0`                                       |

A later deployment slot than the one above means the program was upgraded and this
integration must be revisited (layouts, formulas, fee wiring).

## Templates

| Template                  | Account     | Address                                                                   | Use for                                                 |
| ------------------------- | ----------- | ------------------------------------------------------------------------- | ------------------------------------------------------- |
| `raydium-clmm-custom`     | `PoolState` | PDA `["pool", PDA(["amm_config", index_be]), token_mint_0, token_mint_1]` | any pool, selected by fee tier index and the two mints  |
| `raydium-clmm-pool-state` | `PoolState` | caller-provided pubkey                                                    | any pool by raw address                                 |
| `raydium-clmm-amm-config` | `AmmConfig` | PDA `["amm_config", index_be]`                                            | a fee tier shared by every pool created with that index |

`token_mint_0` must be the mint whose raw 32-byte pubkey is numerically smaller (compare
decoded bytes, not base58). Nothing checks the order: a reversed pair derives an empty
address. SOL (`So1111...`) sorts before USDC (`EPjFWdd...`).

## Finding a pool

- Raydium's public API lists CLMM pools by liquidity:
  `https://api-v3.raydium.io/pools/info/list?poolType=concentrated&poolSortField=liquidity`.
- A pool account is owned by `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`, is 1544
  bytes, and starts with `PoolState`'s discriminator from `idl.json`.
- Example: the SOL/USDC pool (fee tier index 8, tick spacing 1) at
  `3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv`.

## Field reference

### `PoolState`

| Field            | Meaning                                                                                                   |
| ---------------- | --------------------------------------------------------------------------------------------------------- |
| `liquidity`      | In-range liquidity (raw `u128`, no decimals)                                                              |
| `sqrt_price_x64` | `sqrt(token_1/token_0)` in Q64.64 fixed point (raw `u128`); must move with `tick_current`                 |
| `tick_current`   | Tick index of the current price (`i32`)                                                                   |
| `status`         | Disable bitmask (`u8`): bit0 open/increase liquidity, bit1 decrease liquidity, bit2 collect fee, bit3 collect reward, bit4 swap; `0` = all enabled |

Price and tick must agree, or no swap can trade against the pool:

- raw price = `(sqrt_price_x64 / 2^64)^2` (token_1 per token_0, raw units; multiply by
  `10^(mint_decimals_0 - mint_decimals_1)` for a human price)
- `tick_current = floor(log(raw price) / log(1.0001))`
- a swap loads the `TickArrayState` covering `tick_current`; it must already exist.

The price fields do not move the vault balances, so a swap at depth still needs funded
vaults.

### `AmmConfig`

All three rates are parts per million (`FEE_RATE_DENOMINATOR_VALUE = 1,000,000`).

| Field               | Meaning                                                                                      |
| ------------------- | -------------------------------------------------------------------------------------------- |
| `trade_fee_rate`    | `fee = amount_in * trade_fee_rate / 1_000_000`; must stay `< 1_000_000` (`2500` = 0.25%)     |
| `protocol_fee_rate` | Share of the collected trade fee, not of volume, routed to the protocol treasury             |
| `fund_fee_rate`     | Share of the collected trade fee routed to the fund owner; `protocol + fund <= 1_000_000`    |

Every pool created with the same index shares this account, so an override changes all
of them at once.

## Worked example: move the SOL/USDC pool's price and cut its protocol fee

```json
{
  "templateId": "raydium-clmm-pool-state",
  "address": "3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv",
  "values": {
    "sqrt_price_x64": "6200000000000000000",
    "tick_current": -21808
  },
  "fetchBeforeUse": true
}
```

`(6200000000000000000 / 2^64)^2 = 0.11297` and `floor(log(0.11297) / log(1.0001)) = -21808`,
so the pair satisfies the coupling rule.

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

## Prepare a price shock through MCP

`create_raydium_clmm_price_shock_scenario` reads the pool through the running surfnet,
multiplies its price by a factor, recomputes the matching `tick_current`, and stages a
one-override scenario (`raydium-clmm-pool-state`, slot 1, `fetchBeforeUse: true`).

| Param         | Type             | Meaning                                                         |
| ------------- | ---------------- | --------------------------------------------------------------- |
| `pool`        | string           | Base58 address of the `PoolState` account                       |
| `priceFactor` | string           | `0.5` halves the price, `4` quadruples it; finite, > 0, not 1   |
| `surfnetPort` | number, optional | Port of the target surfnet; omit for 8899                       |

```json
{ "pool": "3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv", "priceFactor": "0.999" }
```

It checks the pool's owner, size and discriminator, and that the tick array
`["tick_array", pool, start_index as i32 big-endian]` covering the new tick exists. A
missing array is rejected with the factor closest to the request that stays on the pool's
current array. The result carries `error` and the Studio editor `url`.

## Verification

- Unit: `cargo test -p surfpool-core --lib raydium` covers PDA derivation for the pool
  and every `amm_config_index` option, template properties against the bundled IDL, and
  the price-shock arithmetic, tick-array start index, and missing-array rejection.
- MCP: `cargo test -p surfpool-mcp raydium` covers the camelCase param schema and input
  rejection before any RPC.
- Live fork (`cargo test -p surfpool-core --features integration-tests raydium_clmm --
  --test-threads=1`, needs network; `SURFPOOL_TEST_RPC_URL` overrides the endpoint):
  round-trips the live SOL/USDC `PoolState` byte-for-byte, checks every
  `amm_config_index` option's live size, discriminator, `tick_spacing` and
  `trade_fee_rate`, runs `status`, `liquidity` and `tick_current` through the
  materializer and asserts only their bytes changed, and runs the price-shock builder on
  the live pool and checks the derived tick array's owner, `pool_id` and start index.
- Not verified: a swap simulation comparing baseline and shocked prices.
