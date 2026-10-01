# Raydium CLMM

Raydium's concentrated-liquidity AMM (`amm_v3`). Three templates: a pool found by its mints, a pool
found by its address, and a fee tier. For how scenarios work in general see the
[scenarios README](../../../README.md).

# Template index

**Raydium CLMM** &middot; `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`

| Template | Overrides |
|---|---|
| `raydium-clmm-custom` | a pool's price, active liquidity and status, selected by fee tier and mints |
| `raydium-clmm-pool-state` | the same fields, selected by the pool address |
| `raydium-clmm-amm-config` | a fee tier, shared by every pool created on it |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `sqrt_price_x64` | `sqrt(price_raw) x 2^64`, as a decimal **string** | |
| `liquidity` | raw `u128`, as a decimal **string** | |
| `status` | bitmask of disabled instructions | `16` = swaps off, `0` = everything on |
| `trade_fee_rate` | parts per million of the input, below `1000000` | `2500` = 0.25% |
| `protocol_fee_rate`, `fund_fee_rate` | parts per million of the collected fee, together at most `1000000` | `120000` = 12% of the fee |

`price_raw` is token_1 per token_0 in raw units; multiply by `10^(decimals_0 - decimals_1)` for the
human price. `status` bits: 0 open or increase liquidity, 1 decrease liquidity, 2 collect fees,
3 collect rewards, 4 swap.

## Picking a pool

Raydium's API lists pools by liquidity:
`https://api-v3.raydium.io/pools/info/list?poolType=concentrated&poolSortField=liquidity`. SOL/USDC
on fee tier 8: `3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv`.

**Order the mints for `raydium-clmm-custom`.** `token_mint_0` is the mint whose raw 32 bytes are
numerically smaller, not the smaller base58 string. A reversed pair derives an empty address and
nothing checks it. SOL sorts before USDC.

Set `fetchBeforeUse: true` so the live pool is forked first.

# Recipes

## Shock the price

Use **Scenario presets → Raydium state → CLMM price shock** in Studio. It reads the pool and the
tick arrays the move crosses, and writes all three coupled fields:

```
template: raydium-clmm-pool-state
sqrt_price_x64: "<round(sqrt_price_x64 x sqrt(factor))>"
tick_current:   <floor(2 x ln(new_sqrt_price_x64 / 2^64) / ln(1.0001))>
liquidity:      "<live liquidity +/- liquidity_net of every initialized tick crossed>"
```

**Never move the price without `liquidity`.** The pool keeps the old range's depth at the new
price, and swaps succeed with wrong amounts: a +2% move on the SOL/USDC pool left it about 1.4x too
deep. Going up, a tick `t` is crossed when `old < t <= new` and its `liquidity_net` is added; going
down, when `new < t <= old` and it is subtracted.

**The destination tick array must exist.** A swap resumes from the array covering the new tick,
PDA `["tick_array", pool, start_index as i32 big-endian]` with 60 ticks of `tick_spacing` each.
Studio refuses a move onto a missing array and names the largest (or smallest) factor that stays on
the pool's current one.

## Freeze swaps

```
template: raydium-clmm-pool-state
status: 16      # bit 4: swaps off, liquidity still moves
```

## Change a fee tier

```
template: raydium-clmm-amm-config
config_index:   8          # the SOL/USDC pool's tier
trade_fee_rate: 10000      # 1%
```

Every pool created with that index shares the account, so the change reaches all of them.

# Troubleshooting

| Symptom | Fix |
|---|---|
| The swap succeeds but the amounts look wrong after a price change | `liquidity` was not moved with the price. Use the Studio price shock |
| The swap fails right after a price change | The new tick has no tick array, or `sqrt_price_x64` and `tick_current` disagree |
| `raydium-clmm-custom` changed nothing | The mints are in the wrong order, so the derived address is empty |
| The swap fails on the token transfer | The paying vault holds less than the output. Fund it with `spl-token-account-balance` |
