# PancakeSwap CLMM

PancakeSwap's concentrated-liquidity AMM on Solana, a fork of Raydium CLMM with the same account
layouts. Two templates: a pool, and the fee tier a pool points to. Units and rules match Raydium
CLMM; only the program, pools and fee tiers differ.

# Template index

**PancakeSwap CLMM** &middot; `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq`

| Template | Overrides |
|---|---|
| `pancakeswap-clmm-pool-state` | a pool's price, active liquidity and status, selected by the pool address |
| `pancakeswap-clmm-amm-config` | the fee tier a pool points to, shared by every pool on it |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `sqrt_price_x64` | `sqrt(price_raw) x 2^64`, as a decimal **string** | `"5833372668713515884"` = 100 USDC per SOL |
| `liquidity` | raw `u128`, as a decimal **string** | no fixed scale; read it from the pool |
| `status` | bitmask of disabled instructions | `16` = swaps off, `0` = everything on |
| `trade_fee_rate` | parts per million of the input, below `1000000` | `100` = 0.01% |

## Picking a pool

Find the pools of a pair with `getProgramAccounts` on the program, filtered by size `1544` and the
two mints at offsets `73` and `105`. A pair has a pool per fee tier; the one whose vaults hold the
most is usually the one you want. For SOL/USDC that is
`DJNtGuBGEQiUCWE8F981M2C3ZghZt2XLD8f2sQdZ6rsZ` (0.01% fee, tick spacing 10). Pools are also listed
at `https://solana.pancakeswap.finance/liquidity-pools/`.

**Check the owner.** The layouts match Raydium's, so a PancakeSwap template pointed at a Raydium
pool writes it without complaint.

Set `fetchBeforeUse: true` to pull the pool from upstream first.

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

The shock reads the pool when the scenario is created and stores absolute values, so if the pool's
liquidity changes before Play, create the scenario again.

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

## Change a pool's fee tier

Use **Scenario presets → PancakeSwap state → CLMM fee tier** in Studio with the pool address, or
read the pool's `amm_config` with `getAccountInfo`, `dataSlice {offset 9, length 32}` and encoding
`base58`, and override that account:

```
template: pancakeswap-clmm-amm-config
account:        {"pubkey": "<the pool's amm_config>"}
trade_fee_rate: 10000      # 1%
```

Every pool on that tier shares the account, so the change reaches all of them.

# Troubleshooting

| Symptom | Fix |
|---|---|
| The swap succeeds but the amounts look wrong after a price change | `liquidity` was not moved with the price. Use the Studio price shock |
| The swap fails right after a price change | The new tick has no tick array, or `sqrt_price_x64` and `tick_current` disagree |
| The swap fails on the token transfer | The paying vault holds less than the output. Fund it with `spl-token-account-balance` |
