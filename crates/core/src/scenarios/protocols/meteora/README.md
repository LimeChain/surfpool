# Meteora DLMM

Meteora's bin-based AMM. Liquidity sits in discrete price bins, 70 to a `BinArray`, and the pair
account records which bin is active. One template sets that active bin, the pair's status and its
fees.

# Template index

**Meteora DLMM** &middot; `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`

| Template | Overrides |
|---|---|
| `meteora-dlmm-pool-state` | a pair's active bin, status and fees |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `active_id` | the bin the pair trades in, signed | `-2127` = 119.32 USDC per SOL at bin step 10 |
| `parameters.base_factor` | base fee, `base_factor x bin_step x 1e-8` | `10000` = 0.1% at bin step 10, `50000` = 0.5% |
| `status` | `0` enabled, `1` disabled | |

Price is `(1 + bin_step / 10000) ^ active_id` token Y per token X, times
`10 ^ (decimals_x - decimals_y)`. `bin_step` is the pair's own and is not exposed: it is a seed of
the pair's address, so pick another pair to test another bin step.

## Picking a pair

DLMM pairs are keypairs, so there is no seed recipe. Find one at
`https://dlmm.datapi.meteora.ag/pools` (filter by mint or bin step). SOL/USDC at bin step 10:
`BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y`.

Set `fetchBeforeUse: true` so the live pair is forked first.

# Recipes

## Shock the price

Use **Scenario presets → Meteora state → Price shock** in Studio. It reads the pair and the bin
array it lands on:

```
template: meteora-dlmm-pool-state
active_id: <live active_id + round(ln(factor) / ln(1 + bin_step / 10000))>
```

**Only one side sees the shock.** Bins above the active bin hold token X and bins below hold token
Y, and the override does not move them. After a rise, buys of X fill at the new price while sells
skip the X-only bins and fill at the old one; after a drop it is the reverse. The first trade on
the unshocked side moves the active bin back. Shock in the direction the test trades.

**The destination bin must hold what the shocked side takes.** Its array is
`["bin_array", pair, floor(active_id / 70) as i64 little-endian]`, and the division floors, so
`active_id` -2222 lives in array -32. Studio refuses a missing array, naming the largest (or
smallest) factor that stays on the current one, and a bin with none of the token the shocked side
takes.

## Make trading expensive

```
template: meteora-dlmm-pool-state
parameters.base_factor: 50000      # 0.5% at bin step 10, up from 0.1%
```

`v_parameters.volatility_accumulator` also raises the fee, but the program recomputes it on every
swap, so `base_factor` is the lever that stays.

## Halt the pair

```
template: meteora-dlmm-pool-state
status: 1
```

Useful to prove a router drops a dead venue.

# Troubleshooting

| Symptom | Fix |
|---|---|
| After a rise, sells still fill at the old price | Expected: only buys see a rise. Shock downwards to test sells |
| The shock disappeared after one trade | A trade on the unshocked side moved the active bin back. Re-Play the scenario |
| The swap fails after a price change | The destination bin array does not exist |
| A fee override lasted one swap | You set `volatility_accumulator`. Set `parameters.base_factor` |
