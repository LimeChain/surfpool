# Meteora DLMM state preparation

Declarative state-preparation templates for Meteora's DLMM (dynamic liquidity market
maker). A DLMM pair holds liquidity in discrete price bins; the `LbPair` account stores
which bin is active, the fee parameters and the pointers to the vaults, while the
liquidity itself lives in `BinArray` accounts of 70 bins each.

`LbPair` is a zero-copy account (`bytemuck`, `repr(C)`), not Borsh. Its declared fields
happen to need no padding, so the Anchor IDL path reproduces the layout byte for byte,
which the live tests assert rather than assume.

## Program identity (verified on mainnet 2026-09-20)

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
| `meteora-dlmm-custom` | `LbPair` | caller-provided pubkey | any DLMM pool |

Set `fetchBeforeUse: true` so the live account is forked first; `false` only for a later
override that builds on state an earlier override in the same scenario prepared.

### Finding a pool

DLMM pools are keypairs, not PDAs, so there is no seed recipe to derive one. Find a pool
at `https://dlmm.datapi.meteora.ag/pools` (filter by mint or bin step), then confirm the
address before using it: it must be owned by `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`,
904 bytes, discriminator `21 0b 31 62 b5 65 b1 0d`. A well-known example is the SOL/USDC
pair at `BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y` (bin step 10).

The bin array covering a bin is
`["bin_array", lb_pair, floor(active_id / 70) as i64 LE]`. The division floors, so
`active_id` -2222 lives in array -32, not -31.

## Field reference

All values are raw on-chain units; nothing is scaled for you.

| Property | Type | Offset | Meaning |
| --- | --- | --- | --- |
| `active_id` | `i32` | 76 | the bin the pair currently trades in |
| `status` | `u8` | 82 | `PairStatus`: 0 enabled, 1 disabled |
| `parameters.base_factor` | `u16` | 8 | base fee factor |
| `v_parameters.volatility_accumulator` | `u32` | 40 | drives the variable fee |

Offsets include the 8-byte discriminator and are computed from the IDL in the live tests,
not hard-coded into the templates.

**Price.** `price = (1 + bin_step / 10000) ^ active_id`, in token Y per token X before
decimals; multiply by `10 ^ (decimals_x - decimals_y)` for the human price. To move the
price by a factor `f`, add `round(ln(f) / ln(1 + bin_step / 10000))` bins to the live
`active_id`.

**Base fee.** The IDL documents
`base_fee_rate = base_factor * bin_step * 10 * 10 ^ base_fee_power_factor`, and
`FEE_PRECISION` is `1e9`, so with `base_fee_power_factor = 0` the fee fraction is
`base_factor * bin_step * 1e-8`. On 2026-09-20 this reproduced the fee Meteora's own API
reports for the 49 pools captured that day, across base factors 125 to 15000 and bin steps
1 to 100: base factor 10000 at bin step 10 is 0.1%, 1875 at bin step 16 is 0.03%, 15000 at
bin step 100 is 1.5%.

**Variable fee.** `v_parameters.volatility_accumulator` counts bins crossed times 10000:
on both audited pools the stored value equals
`volatility_reference + |active_id - index_reference| * 10000`. The program recomputes it
on every swap, so an override of it survives only until the next trade —
`parameters.base_factor` is the durable fee lever.

**`bin_step` is not exposed.** It is the base of the bin price math and a seed of the
pool's own address, so changing it would leave a pool that no longer derives its address
and whose every bin price silently moved. Pick a different pool instead.

## Worked example: make SOL 10% more expensive in the SOL/USDC pair

1. Read the live pool. On 2026-09-20 `BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y` had
   `active_id` -2222 and `bin_step` 10, so
   `price = 1.001 ^ -2222 * 10 ^ (9 - 6) = 108.55` USDC per SOL — Meteora's API reported
   108.51 for the same pool.
2. Size the move. `ln(1.1) / ln(1.001) = 95.3`, so the target is `active_id` -2222 + 95 =
   -2127, about 119.3 USDC per SOL.
3. Check the destination bin array exists and holds liquidity:
   `floor(-2127 / 70) = -31`, so derive `["bin_array", pool, (-31) as i64 LE]` and read
   it. An empty or missing array means the swap will fail or return nothing, whatever the
   pool account says.
4. Apply one override on `meteora-dlmm-custom` at slot 1 with `fetchBeforeUse: true`,
   `account = "BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y"`,
   `values = { "active_id": -2127 }`.

To make the pair expensive to trade instead, raise `parameters.base_factor`: 10000 to
50000 takes the base fee from 0.1% to 0.5%. To take the pair out of a router's graph, set
`status` to 1.

## Verification

Live tests live in `crates/core/src/tests/meteora_dlmm/mod.rs` behind the
`integration-tests` feature and read mainnet accounts rather than embedded snapshots:

```text
cargo test -p surfpool-core --features integration-tests meteora_dlmm -- --test-threads=1
```

| Test | What it pins |
| --- | --- |
| `live_lb_pair_matches_the_bundled_layout` | the IDL's declared `LbPair` size equals the live account length (904), the discriminator matches, and a no-op re-encode changes no byte |
| `overrides_land_only_on_their_own_offsets` | `active_id` and `parameters.base_factor` are at the offsets the IDL computes (76 and 8), and an override of each touches only those bytes |
| `active_bin_array_exists_and_holds_liquidity` | the bin array recipe, on both audited pools, including that the active bin's `liquidity_supply` is non-zero |

Template shape is checked without a network in `scenarios::registry::tests::meteora_*`.

Not covered: no behavioural swap test yet. Nothing here proves a swap reprices after an
`active_id` override; that needs a hand-rolled DLMM swap builder with the surrounding bin
arrays as remaining accounts.
