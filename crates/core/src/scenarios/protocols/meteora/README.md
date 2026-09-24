# Meteora DLMM state preparation

Declarative state-preparation templates for Meteora's DLMM (dynamic liquidity market
maker). A DLMM pair holds liquidity in discrete price bins: the `LbPair` account stores
which bin is active, the fee parameters and the vault pointers, while the liquidity itself
lives in `BinArray` accounts of 70 bins each.

`LbPair` is a zero-copy account (`bytemuck`, `repr(C)`) whose fields need no padding, so
the standard Anchor IDL override path reproduces it byte for byte.

## Program identity (verified 2026-09-20)

| | |
| --- | --- |
| Program ID | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` |
| ProgramData | `HZcJwcJ2njPDxZtpPoKnF8v2w9QAx2rS7TdJPSRkbEhu` |
| Last deployed slot | 423977638 |
| Upgrade authority | `JADaUV8kvDpDbJr55wxXJHVaBS3VCj8thZZHjfeuCVLd` |
| Bundled IDL | `lb_clmm` 0.10.1, Anchor spec 0.1.0 (`dlmm/v1/idl.json`; instructions stripped) |
| `LbPair` | 904 bytes, discriminator `21 0b 31 62 b5 65 b1 0d` |
| `BinArray` | 10136 bytes, discriminator `5c 8e 5c dc 05 94 46 b5`, 70 bins |

A later deployment slot than the one above means the program was upgraded and this
integration must be revisited.

## Templates

| Template | Account | Address | Use for |
| --- | --- | --- | --- |
| `meteora-dlmm-pool-state` | `LbPair` | caller-provided pubkey | any DLMM pool: price bin, status, fees |

Set `fetchBeforeUse: true` so the live account is forked first; `false` only for a later
override that builds on state an earlier override in the same scenario prepared.

### Finding a pool

DLMM pools are keypairs, not PDAs, so there is no seed recipe. Find a pool at
`https://dlmm.datapi.meteora.ag/pools` (filter by mint or bin step), then confirm the
address: it must be owned by `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`, 904 bytes,
discriminator `21 0b 31 62 b5 65 b1 0d`. Example: SOL/USDC at
`BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y` (bin step 10).

The bin array covering a bin is `["bin_array", lb_pair, floor(active_id / 70) as i64 LE]`.
The division floors, so `active_id` -2222 lives in array -32, not -31.

## Field reference

All values are raw on-chain units; nothing is scaled for you. Offsets include the 8-byte
discriminator.

| Property | Type | Offset | Meaning |
| --- | --- | --- | --- |
| `active_id` | `i32` | 76 | the bin the pair currently trades in |
| `status` | `u8` | 82 | `PairStatus`: 0 enabled, 1 disabled |
| `parameters.base_factor` | `u16` | 8 | base fee factor |
| `v_parameters.volatility_accumulator` | `u32` | 40 | drives the variable fee, 10000 per bin crossed |

**Price.** `price = (1 + bin_step / 10000) ^ active_id` in token Y per token X before
decimals; multiply by `10 ^ (decimals_x - decimals_y)` for the human price. To move the
price by a factor `f`, add `round(ln(f) / ln(1 + bin_step / 10000))` to the live
`active_id`.

**Base fee.** `base_fee_rate = base_factor * bin_step * 10 * 10 ^ base_fee_power_factor`
with `FEE_PRECISION = 1e9`, so with `base_fee_power_factor = 0` the fee fraction is
`base_factor * bin_step * 1e-8`: 10000 at bin step 10 is 0.1%, 15000 at bin step 100 is
1.5%.

**Variable fee.** The program recomputes `v_parameters.volatility_accumulator` on every
swap, so an override of it lasts only until the next trade; `parameters.base_factor` is
the durable fee lever.

**`bin_step` is not exposed.** It is the base of the bin price math and a seed of the
pool's own address. To test a different bin step, pick a different pool.

## Worked example: make SOL 10% more expensive in the SOL/USDC pair

1. Read the live pool. Say `BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y` has `active_id`
   -2222 and `bin_step` 10: `price = 1.001 ^ -2222 * 10 ^ (9 - 6) = 108.51` USDC per SOL.
2. Size the move: `ln(1.1) / ln(1.001) = 95.36`, which rounds to 95, so the target is
   `active_id` -2222 + 95 = -2127, `1.001 ^ -2127 * 10 ^ 3 = 119.32` USDC per SOL.
3. Check the destination bin array exists and holds liquidity: `floor(-2127 / 70) = -31`,
   so read `["bin_array", pool, (-31) as i64 LE]`. A missing or empty array means the swap
   fails or returns nothing, whatever the pool account says.
4. Apply one override on `meteora-dlmm-pool-state` at slot 1 with `fetchBeforeUse: true`,
   `account = "BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y"`,
   `values = { "active_id": -2127 }`.

To make the pair expensive to trade instead, raise `parameters.base_factor` from 10000 to
50000 (0.1% to 0.5%). To take the pair out of a router's graph, set `status` to 1.

## Prepare a price shock

The `create_meteora_price_shock_scenario` MCP tool reads the pool from the surfnet, moves
`active_id` by `round(ln(priceFactor) / ln(1 + bin_step / 10000))`, reads the bin array
covering the new bin, and stages the scenario. It returns the Studio editor URL.

| Param | Type | Notes |
| --- | --- | --- |
| `pool` | string | Base58 `LbPair` address |
| `priceFactor` | string | Multiplier on the current price; finite, > 0, not 1 |
| `surfnetPort` | number, optional | Defaults to 8899 |

```json
{
  "pool": "BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y",
  "priceFactor": "1.1"
}
```

If the destination bin array does not exist, the tool rejects the request, naming the
missing account and the largest (or smallest) factor that stays on the pool's current bin
array.

## Verification

- Unit: `cargo test -p surfpool-core --lib meteora` covers the bin delta, the floored
  bin array index, the IDL decode and the rejections of the price-shock builder, plus the
  template shape in `scenarios::registry::tests`.
- MCP surface: `cargo test -p surfpool-mcp meteora` covers the camelCase schema and input
  rejection before any RPC.
- Live fork: `cargo test -p surfpool-core --features integration-tests meteora_dlmm --
  --test-threads=1` needs a network connection (`SURFPOOL_TEST_RPC_URL` overrides the
  public mainnet endpoint). It checks that the IDL's `LbPair` size equals the live account
  length and that a no-op re-encode changes no byte on two pools, that one materialized
  override of `active_id`, `parameters.base_factor` and `status` touches only offsets
  76-79, 8-9 and 82, and that the builder decodes both live pools and finds the bin
  array one bin away at the index it derived.

## Known limitations

No behavioural swap test yet: nothing here proves a swap reprices after an `active_id`
override. That needs a DLMM swap builder with the surrounding bin arrays as remaining
accounts.
