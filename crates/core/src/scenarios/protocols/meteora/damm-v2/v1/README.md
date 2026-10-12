# Meteora DAMM v2

Meteora's constant-product AMM (`cp_amm`). One template sets a pool's status and its price. For how
scenarios work in general see the [scenarios README](../../../../README.md).

# Template index

**Meteora DAMM v2** &middot; `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`

| Template | Overrides |
|---|---|
| `meteora-damm-v2-pool-state` | a pool's status, price, liquidity and tracked reserves |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `pool_status` | `0` enabled, `1` disabled | |
| `sqrt_price` | `sqrt(price_raw) x 2^64`, as a decimal **string** | |
| `liquidity` | raw `u128`, as a decimal **string** | |
| `token_a_amount`, `token_b_amount` | the reserves the program prices and pays from, raw units | |

`price_raw` is token B per token A in raw units; multiply by `10^(decimals_a - decimals_b)` for the
human price.

## Picking a pool

Find the pools of a pair with `getProgramAccounts` on the program, filtered by size `1112` and the
two mints at offsets `168` and `200`. A pair can have pools in both orders, so query both. The one
whose vaults hold the most is usually the one you want; for SOL/USDC that is
`8Pm2kZpnxD3hoMmt4bjStX2Pw2Z9abpbHzZxMPqxPmie`.

Set `fetchBeforeUse: true` to pull the pool from upstream first.

# Recipes

## Shock the price

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
not count as volatility for the next swap.

**Never move `sqrt_price` alone.** A swap pays out of the tracked reserves, and one that takes more
than a stale reserve holds fails with `MathOverflow`.

## Halt the pool

```
template: meteora-damm-v2-pool-state
pool_status: 1
```

# Troubleshooting

| Symptom | Fix |
|---|---|
| `PoolDisabled` (6003) on a swap | `pool_status` is 1, or the pool is not activated yet |
| `MathOverflow` (6000) on a bigger swap | `sqrt_price` moved without the tracked reserves |
| `PriceRangeViolation` (6015) | The new price is outside the pool's range |
| The swap fails on the token transfer | The paying vault holds less than the output |
