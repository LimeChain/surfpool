# Raydium AMM v4

Raydium's classic constant-product AMM, behind most "Standard" pools. Three templates: status and
LP supply, fees, and swap counters with the open time.

**The price is not in the pool account.** A swap prices off the two vault token accounts, so move
the price with `spl-token-account-balance` on the vaults named in the pool's `token_coin` and
`token_pc` fields.

# Template index

**Raydium AMM v4** &middot; `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`

| Template | Overrides |
|---|---|
| `raydium-amm-pool-state` | whether the pool trades (`status`), its machine state and LP supply |
| `raydium-amm-fees` | the swap fee, and the trade fee the OpenBook planner uses |
| `raydium-amm-swap-stats` | lifetime swap counters, and when a scheduled pool opens |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `status` | which operations run, `0..=7` | `2` = disabled, `6` = swap only, `7` = waiting for `pool_open_time` |
| `fees.swap_fee_numerator` / `fees.swap_fee_denominator` | the fee on a swap's input | `25` / `10000` = 0.25%, the mainnet default |
| `out_put.pool_open_time` | unix seconds | |
| amounts and `lp_amount` | the mint's smallest unit | |

Swaps run at `status` 1, 6 and 7. At 7 they wait for the cluster clock to pass `pool_open_time`,
then the program sets the status to 6 itself.

## Picking a pool

There is no catalog: pass the pool address. Raydium's API lists pools by liquidity:
`https://api-v3.raydium.io/pools/info/list?poolType=standard&poolSortField=liquidity`. Its
`standard` type also covers CP-Swap, a different program, so keep only entries whose `programId`
is `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`. SOL/USDC:
`58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2`.

Set `fetchBeforeUse: true` so the live pool is forked first.

# Recipes

## Take a pool out of service

```
template: raydium-amm-pool-state
status: 2          # disabled; swaps fail with InvalidStatus
```

**Keep `status` in `0..=7` and `state` in `0..=6`.** The program maps both through an exhaustive
`match`, so a larger number aborts every instruction that reads the pool.

## Make swaps expensive, or free

```
template: raydium-amm-fees
fees.swap_fee_numerator:   1000
fees.swap_fee_denominator: 10000     # 10%
```

Set the numerator to `0` for free swaps. The `trade_fee_*` pair only prices OpenBook orders; swaps
never read it.

## Open a scheduled pool early

```
template: raydium-amm-swap-stats
out_put.pool_open_time: 0
```

Only matters while `status` is 7.

# Troubleshooting

| Symptom | Fix |
|---|---|
| Every instruction on the pool aborts | `status` is above 7 or `state` above 6 |
| `InvalidStatus` on a swap | `status` is not 1, 6 or 7 |
| A pool at status 7 still rejects swaps | The cluster clock has not reached `pool_open_time` |
| A fee change did not move the swap price | You changed `trade_fee_*`. Swaps use `swap_fee_*` |
| An override on the pool did not move the price | The price lives in the vaults. Use `spl-token-account-balance` |
