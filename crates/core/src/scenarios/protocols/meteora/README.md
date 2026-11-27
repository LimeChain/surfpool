# Meteora

Surfpool bundles IDLs and override templates for two Meteora programs:

- **DLMM** (`lb_clmm`) is a bin-based AMM. Liquidity sits in discrete price bins, 70 to a
  `BinArray`, and the pair account records which bin is active. One template sets that active bin,
  the pair's status and its fees.
- **DAMM v2** is Meteora's constant-product AMM (`cp_amm`). One template sets a pool's status and
  its price, the other its base fee.

For how scenarios work in general see the [scenarios README](../../README.md).

# Template index

**Meteora DLMM** &middot; `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`

| Template | Overrides |
|---|---|
| `meteora-dlmm-pool-state` | a pair's active bin, status and fees |

**Meteora DAMM v2** &middot; `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`

| Template | Overrides |
|---|---|
| `meteora-damm-v2-pool-state` | a pool's status, price, liquidity and tracked reserves |
| `meteora-damm-v2-pool-fees` | a pool's base fee, held flat |

## Picking a pair or pool

Find the pools of two mints with `getProgramAccounts` on the program, filtered by the account size
and the two mints at the offsets below. Two mints can have pools in both orders, so query both.

| Program | Account size | Mint offsets |
|---|---|---|
| DLMM | `904` | `88` and `120` |
| DAMM v2 | `1112` | `168` and `200` |

The one whose vaults hold the most is usually the one you want; for DAMM v2 SOL/USDC that is
`8Pm2kZpnxD3hoMmt4bjStX2Pw2Z9abpbHzZxMPqxPmie`.

DLMM pairs are keypairs, so there is no seed recipe. Pairs are also listed at
`https://dlmm.datapi.meteora.ag/pools`.

Set `fetchBeforeUse: true` to pull the pair or pool from upstream first.

## Number formats

### DLMM

| You'll see | It means | Example |
|---|---|---|
| `active_id` | the bin the pair trades in, signed | `-2127` = 119.32 USDC per SOL at bin step 10 |
| `parameters.base_factor` | base fee, `base_factor x bin_step x 1e-8` while `parameters.base_fee_power_factor` is 0, as on SOL/USDC | `10000` = 0.1% at bin step 10, `50000` = 0.5% |
| `status` | `0` enabled, `1` disabled | |

Price is `(1 + bin_step / 10000) ^ active_id` token Y per token X, times
`10 ^ (decimals_x - decimals_y)`. `bin_step` is the pair's own and is not exposed: it is a seed of
the pair's address, so pick another pair to test another bin step.

### DAMM v2

| You'll see | It means | Example |
|---|---|---|
| `pool_status` | `0` enabled, `1` disabled | |
| `sqrt_price` | `sqrt(price_raw) x 2^64`, as a decimal **string** | |
| `liquidity` | raw `u128`, as a decimal **string** | |
| `token_a_amount`, `token_b_amount` | the reserves the program prices and pays from, raw units | |
| `base_fee_info.data` | the 32 base-fee bytes, as an array of 32 numbers `0`-`255`; see [Base fee bytes](#base-fee-bytes) | |

`price_raw` is token B per token A in raw units; multiply by `10^(decimals_a - decimals_b)` for the
human price.

## Base fee bytes

This section is for DAMM v2 only.

The IDL types the base fee as one opaque `[u8; 32]`, `pool_fees.base_fee.base_fee_info.data`, at
pool bytes `8..40`. Byte 8 of the array, `base_fee_mode`, picks how the program reads the rest.
Every integer is little-endian.

| Bytes | Modes 0, 1 (time) | Mode 2 (rate limiter, deprecated) | Modes 3, 4 (market cap) |
|---|---|---|---|
| `0..8` | `cliff_fee_numerator` u64 | `cliff_fee_numerator` u64 | `cliff_fee_numerator` u64 |
| `8` | `base_fee_mode` u8 | `base_fee_mode` u8 | `base_fee_mode` u8 |
| `9..14` | padding, zero | padding, zero | padding, zero |
| `14..16` | `number_of_period` u16 | `fee_increment_bps` u16 | `number_of_period` u16 |
| `16..20` | `period_frequency` u64, bytes `16..24` | `max_limiter_duration` u32 | `sqrt_price_step_bps` u32 |
| `20..24` | | `max_fee_bps` u32 | `scheduler_expiration_duration` u32 |
| `24..32` | `reduction_factor` u64 | `reference_amount` u64 | `reduction_factor` u64 |

- `base_fee_mode`: `0` time linear, `1` time exponential, `2` rate limiter, `3` market-cap linear,
  `4` market-cap exponential.
- `cliff_fee_numerator` is the base fee out of `1,000,000,000`; `10000000` is 1%.
- `reduction_factor` lowers the fee once per period: linear modes subtract it from the numerator,
  exponential modes multiply the fee by `1 - reduction_factor / 10000`. `0` keeps the fee flat.
- `period_frequency` and `scheduler_expiration_duration` count in the pool's activation points,
  slots or seconds.

An override on `data` replaces all 32 bytes. Read the pool's current bytes, change only the
fields you mean, and send all 32 back.

# Recipes

## DLMM

### Shock the price

Use **Scenario presets → Meteora state → Price shock** in Studio. It reads the pair and the bin
array it lands on:

```
template: meteora-dlmm-pool-state
active_id: <live active_id + round(ln(factor) / ln(1 + bin_step / 10000))>
v_parameters.index_reference: <the same new active_id>
```

`v_parameters.index_reference` follows the new `active_id`, so the shock itself does not count as
volatility for the next swap. Studio reads the pair when the scenario is created and stores
absolute values, so if the pair's price or liquidity changes before Play, create the scenario again.

**Only one side sees the shock.** Bins above the active bin hold token X and bins below hold token
Y, and the override does not move them. After a rise, buys of X fill at the new price while sells
skip the X-only bins and fill at the old one; after a drop it is the reverse. The first trade on
the unshocked side moves the active bin back. Shock in the direction the test trades.

**The destination bin must hold what the shocked side takes.** Its array is
`["bin_array", pair, floor(active_id / 70) as i64 little-endian]`, and the division floors, so
`active_id` -2222 lives in array -32. Studio refuses a missing array, naming the largest (or
smallest) factor that stays on the current one, and a bin with none of the token the shocked side
takes.

### Make trading expensive

```
template: meteora-dlmm-pool-state
parameters.base_factor: 50000      # 0.5% at bin step 10, up from 0.1%
```

`v_parameters.volatility_accumulator` also raises the fee, but the program recomputes it on every
swap, so `base_factor` is the lever that stays.

### Halt the pair

```
template: meteora-dlmm-pool-state
status: 1
```

Useful to prove a router drops a dead venue.

## DAMM v2

### Shock the price

Use **Scenario presets → Meteora state → DAMM v2 price shock** in Studio. The pool's
`collect_fee_mode` (byte 484) decides what it writes:

- **Modes 0 and 1, one concentrated range.** `sqrt_price` moves by `sqrt(factor)` and must stay
  inside `[sqrt_min_price, sqrt_max_price]`. `liquidity` stays, as in a real swap, and the tracked
  reserves follow from it:

  ```
  token_a_amount = ceil(liquidity x (sqrt_max_price - sqrt_price) / (sqrt_price x sqrt_max_price))
  token_b_amount = ceil(liquidity x (sqrt_price - sqrt_min_price) / 2^128)
  ```
- **Mode 2, compounding.** `token_a_amount / sqrt(factor)` and `token_b_amount x sqrt(factor)` keep
  their product, and `sqrt_price = isqrt((token_b_amount << 128) / token_a_amount)`.

`pool_fees.dynamic_fee.sqrt_price_reference` follows the new `sqrt_price`, so the shock itself does
not count as volatility for the next swap. Studio reads the pool when the scenario is created and
stores absolute values, so if the pool's price or liquidity changes before Play, create the
scenario again.

The shock moves the tracked reserves but not the vaults, so a raised reserve can hold more than its
vault. A large swap that pays out of that side then fails on the token transfer; keep it below the
paying vault's balance.

**Never move `sqrt_price` alone.** A swap pays out of the tracked reserves, and one that takes more
than a stale reserve holds fails with `MathOverflow`.

### Halt the pool

```
template: meteora-damm-v2-pool-state
pool_status: 1
```

### Spike the fee

Write `cliff_fee_numerator` (bytes `0..8`) and a `reduction_factor` of 0 (bytes `24..32`) into the
pool's current 32 bytes. For a time-exponential pool whose bytes read
`[128,51,2,59,0,0,0,0, 1,0,0,0,0,0,180,0, 1,0,0,0,0,0,0,0, 214,0,0,0,0,0,0,0]` (99%, 180 periods,
reduction 214), a 5% fee (`50000000`) is:

```
template: meteora-damm-v2-pool-fees
pool_fees.base_fee.base_fee_info.data: [128,240,250,2,0,0,0,0, 1,0,0,0,0,0,180,0, 1,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0]
```

A reduction factor of 0 keeps the pool's time or market-cap fee schedule at the base fee. The
variable fee still adds on top, and the program caps the total at 50% (`fee_version` 0) or 99%
(`fee_version` 1). Pools on the deprecated rate limiter (`base_fee_mode` 2) keep
`reference_amount` in bytes `24..32`; leave them alone.

# Troubleshooting

## DLMM

| Symptom | Fix |
|---|---|
| After a rise, sells still fill at the old price | Expected: only buys see a rise. Shock downwards to test sells |
| The shock disappeared after one trade | A trade on the unshocked side moved the active bin back. Re-Play the scenario |
| The swap fails after a price change | The destination bin array does not exist |
| A fee override lasted one swap | You set `volatility_accumulator`. Set `parameters.base_factor` |

## DAMM v2

| Symptom | Fix |
|---|---|
| `PoolDisabled` (6003) on a swap | `pool_status` is 1, or the pool is not activated yet |
| `MathOverflow` (6000) on a bigger swap | `sqrt_price` moved without the tracked reserves |
| `PriceRangeViolation` (6015) | The new price is outside the pool's range |
| The swap fails on the token transfer | The paying vault holds less than the output |

# IDL

## DLMM

`dlmm/v1/idl.json` is the IDL Meteora publishes on chain in its Anchor IDL account
`7UZRobkzaKVm1RbCH5WdFaYCGzCRjnu3prziHAsYiSyr` (`lb_clmm` 0.12.0), indented with two spaces. Only
whitespace changed: `jq -c .` prints the same bytes for both.

| IDL | sha256 |
|---|---|
| Published, as `jq -c .` prints it | `01ec576162331b296e7b696d94d257f805381d79aa4dc611678da5f621a200e9` |
| Bundled `dlmm/v1/idl.json` | `f1a7e513ca463b6977d0b210695cadfd0bd70242a87c07f339d33d93607cf105` |

## DAMM v2

`damm-v2/v1/idl.json` is the IDL of the deployed `cp_amm` 0.2.5, byte for byte as Meteora
publishes it in its SDK:
[`src/idl/cp_amm.json`](https://github.com/MeteoraAg/damm-v2-sdk/blob/a8fadfac18f63d1e3a5af7170d4b9bdc781bdc15/src/idl/cp_amm.json)
at `a8fadfa`. The program's Anchor IDL account still holds 0.2.0, which lacks instructions and
fields of the deployed program, so it is not the source.

| IDL | sha256 |
|---|---|
| Bundled `damm-v2/v1/idl.json` | `7f5e5f5e9cab57cd208575dc53359676550e41ee0c19fc822560fe1062fd3ac5` |
