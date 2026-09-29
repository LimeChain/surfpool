# PancakeSwap CLMM

PancakeSwap's concentrated-liquidity AMM on Solana, a fork of Raydium CLMM with the same account
layouts. Three templates: a pool found by its mints, a pool found by its address, and a fee tier.
Units and rules match [Raydium CLMM](../raydium/v3/README.md); only the program, pools and fee
tiers differ.

# Template index

**PancakeSwap CLMM** &middot; `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq`

| Template | Overrides |
|---|---|
| `pancakeswap-clmm-custom` | a pool's price, active liquidity and status, selected by fee tier and mints |
| `pancakeswap-clmm-pool-state` | the same fields, selected by the pool address |
| `pancakeswap-clmm-amm-config` | a fee tier, shared by every pool created on it |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `sqrt_price_x64` | `sqrt(price_raw) x 2^64`, as a decimal **string** | |
| `liquidity` | raw `u128`, as a decimal **string** | |
| `status` | bitmask of disabled instructions | `16` = swaps off, `0` = everything on |
| `trade_fee_rate` | parts per million of the input, below `1000000` | `100` = 0.01% |

`amm_config_index` lists all 22 live fee tiers with their tick spacing and fee.

## Picking a pool

Browse pools at `https://solana.pancakeswap.finance/liquidity-pools/`. The deepest SOL/USDC pool
is `DJNtGuBGEQiUCWE8F981M2C3ZghZt2XLD8f2sQdZ6rsZ` (fee tier 0, tick spacing 10).

**Order the mints for `pancakeswap-clmm-custom`.** `token_mint_0` is the mint whose raw 32 bytes
are numerically smaller, not the smaller base58 string. A reversed pair derives an empty address
and nothing checks it.

**Check the owner.** The layouts match Raydium's, so a PancakeSwap template pointed at a Raydium
pool writes it without complaint.

Set `fetchBeforeUse: true` so the live pool is forked first.

# Recipes

## Shock the price

Use **Scenario presets → PancakeSwap state → CLMM price shock** in Studio. It reads the pool and
the tick arrays the move crosses, and writes all three coupled fields:

```
template: pancakeswap-clmm-pool-state
sqrt_price_x64: "<round(sqrt_price_x64 x sqrt(factor))>"
tick_current:   <floor(2 x ln(new_sqrt_price_x64 / 2^64) / ln(1.0001))>
liquidity:      "<live liquidity +/- liquidity_net of every initialized tick crossed>"
```

**Never move the price without `liquidity`.** The pool keeps the old range's depth at the new
price, and swaps succeed with wrong amounts: a +10% move on the SOL/USDC pool left it about 2.5x
too deep. Going up, a tick `t` is crossed when `old < t <= new` and its `liquidity_net` is added;
going down, when `new < t <= old` and it is subtracted.

**The destination tick array must exist.** A swap resumes from the array covering the new tick,
PDA `["tick_array", pool, start_index as i32 big-endian]` with 60 ticks of `tick_spacing` each.
Studio refuses a move onto a missing array.

## Freeze swaps

```
template: pancakeswap-clmm-pool-state
status: 16      # bit 4: swaps off, liquidity still moves
```

## Change a fee tier

```
template: pancakeswap-clmm-amm-config
config_index:   0          # the SOL/USDC pool's tier
trade_fee_rate: 10000      # 1%
```

Every pool created with that index shares the account, so the change reaches all of them.

# Troubleshooting

| Symptom | Fix |
|---|---|
| The swap succeeds but the amounts look wrong after a price change | `liquidity` was not moved with the price. Use the Studio price shock |
| The swap fails right after a price change | The new tick has no tick array, or `sqrt_price_x64` and `tick_current` disagree |
| `pancakeswap-clmm-custom` changed nothing | The mints are in the wrong order, so the derived address is empty |
| The swap fails on the token transfer | The paying vault holds less than the output. Fund it with `spl-token-account-balance` |
