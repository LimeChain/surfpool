# Raydium

Surfpool bundles IDLs and override templates for **three Raydium programs**:

- **AMM v4** (`v4/`): Raydium's classic constant-product AMM, behind most "Standard" pools. Three
  templates: status and LP supply, fees, and swap counters with the open time.
- **CLMM** (`clmm/v1/`): Raydium's concentrated-liquidity AMM (`amm_v3`). Three templates: a pool
  found by its mints, a pool found by its address, and a fee tier.
- **CPMM** (`cpmm/v1/`): Raydium's constant-product AMM without an orderbook (`raydium_cp_swap`,
  listed by Raydium as CP-Swap). Two templates: a pool's status and open time, and a fee tier.

For how scenarios work in general see the [scenarios README](../../README.md).

## Where the price lives

**On AMM v4 and CPMM the price is not in the pool account.** A swap prices off the two vault token
accounts, so move the price with `spl-token-account-balance` on the vaults the pool names: the
`token_coin` and `token_pc` fields on AMM v4, the `token_0_vault` and `token_1_vault` fields on
CPMM.

A CLMM pool keeps its price in the pool account. See [CLMM: Shock the price](#clmm-shock-the-price).

## Number formats

**AMM v4**

| You'll see | It means | Example |
|---|---|---|
| `status` | which operations run, `0..=7` | `2` = disabled, `6` = swap only, `7` = waiting for `pool_open_time` |
| `fees.swap_fee_numerator` / `fees.swap_fee_denominator` | the fee on a swap's input | `25` / `10000` = 0.25%, the mainnet default |
| `out_put.pool_open_time` | unix seconds | |
| amounts and `lp_amount` | the mint's smallest unit | |

Swaps run at `status` 1, 6 and 7. At 7 they wait for the cluster clock to pass `pool_open_time`,
then the program sets the status to 6 itself.

**CLMM**

| You'll see | It means | Example |
|---|---|---|
| `sqrt_price_x64` | `sqrt(price_raw) x 2^64`, as a decimal **string** | |
| `liquidity` | raw `u128`, as a decimal **string** | |
| `status` | bitmask of disabled instructions | `16` = swaps off, `0` = everything on |
| `trade_fee_rate` | parts per million of the input, below `1000000` | `2500` = 0.25% |
| `protocol_fee_rate`, `fund_fee_rate` | parts per million of the collected fee, together at most `1000000` | `120000` = 12% of the fee |

`price_raw` is token_1 per token_0 in raw units; multiply by `10^(decimals_0 - decimals_1)` for the
human price. `status` bits: 0 open or increase liquidity, 1 decrease liquidity, 2 collect fees,
3 collect rewards, 4 swap, 5 limit orders.

**CPMM**

| You'll see | It means | Example |
|---|---|---|
| `status` | bitmask of disabled operations | `4` = swaps off, `0` = everything on |
| `open_time` | unix seconds | |
| `trade_fee_rate`, `creator_fee_rate` | parts per million of the input | `2500` = 0.25% |
| `protocol_fee_rate`, `fund_fee_rate` | parts per million of the collected fee, together at most `1000000` | `120000` = 12% of the fee |

`status` bits: 0 deposit, 1 withdraw, 2 swap.

## Picking a pool

Find the pools of a pair with `getProgramAccounts` on the program, filtered by the pool's size and
the two mints at their offsets. The pool whose vaults hold the most is usually the one you want.

| Program | Size | Mint offsets | SOL/USDC pool |
|---|---|---|---|
| AMM v4 | `752` | `400` (coin) and `432` (pc) | `58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2` |
| CLMM | `1544` | `73` and `105` | `3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv`, on fee tier 8 |
| CPMM | `637` | `168` and `200` | |

- **AMM v4:** there is no catalog, so pass the pool address. A pair can have pools in both orders,
  so query both.
- **CLMM:** a pair has a pool per fee tier.
- **CPMM:** a pool stores the mint whose raw 32 bytes are smaller first, so only one of the two
  orders returns pools. A pair can have a pool per fee tier.

**Order the mints for `raydium-clmm-custom`.** `token_mint_0` is the mint whose raw 32 bytes are
numerically smaller, not the smaller base58 string. A reversed pair derives an empty address and
nothing checks it. SOL sorts before USDC.

Raydium's API also lists pools by liquidity:

- CLMM: `https://api-v3.raydium.io/pools/info/list?poolType=concentrated&poolSortField=liquidity`.
- AMM v4 and CPMM:
  `https://api-v3.raydium.io/pools/info/list?poolType=standard&poolSortField=liquidity`. The
  `standard` type covers both programs, so keep only entries whose `programId` is
  `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` (AMM v4) or
  `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` (CPMM).

Set `fetchBeforeUse: true` to pull the pool from upstream first.

---

# Recipes

## AMM v4: Take a pool out of service

```
template: raydium-amm-pool-state
status: 2          # disabled; swaps fail with InvalidStatus
```

**Keep `status` in `0..=7` and `state` in `0..=6`.** The program maps both through an exhaustive
`match`, so a larger number aborts every instruction that reads the pool.

## AMM v4: Make swaps expensive, or free

```
template: raydium-amm-fees
fees.swap_fee_numerator:   1000
fees.swap_fee_denominator: 10000     # 10%
```

Set the numerator to `0` for free swaps. The `trade_fee_*` pair only prices OpenBook orders; swaps
never read it.

## AMM v4: Open a scheduled pool early

```
template: raydium-amm-swap-stats
out_put.pool_open_time: 0
```

Only matters while `status` is 7.

## CLMM: Shock the price

Use **Scenario presets → Raydium state → CLMM price shock** in Studio. It reads the pool and the
tick arrays the move crosses, and writes all three coupled fields:

```
template: raydium-clmm-pool-state
sqrt_price_x64: "<round(sqrt_price_x64 x sqrt(factor))>"
tick_current:   <floor(2 x ln(new_sqrt_price_x64 / 2^64) / ln(1.0001))>
liquidity:      "<live liquidity +/- liquidity_net of every initialized tick crossed>"
```

The shock reads the pool when the scenario is created and stores absolute values, so if the pool's
liquidity changes before Play, create the scenario again.

**Never move the price without `liquidity`.** The pool keeps the old range's depth at the new
price, and swaps succeed with wrong amounts: a +2% move on the SOL/USDC pool left it about 1.4x too
deep. Going up, a tick `t` is crossed when `old < t <= new` and its `liquidity_net` is added; going
down, when `new < t <= old` and it is subtracted.

**The destination tick array must exist.** A swap resumes from the array covering the new tick,
PDA `["tick_array", pool, start_index as i32 big-endian]` with 60 ticks of `tick_spacing` each.
Studio refuses a move onto a missing array and names the largest (or smallest) factor that stays on
the pool's current one.

## CLMM: Freeze swaps

```
template: raydium-clmm-pool-state
status: 16      # bit 4: swaps off, liquidity still moves
```

## CLMM: Change a fee tier

A pool's tier is the AmmConfig account at its `amm_config` field; read it with `getAccountInfo`,
dataSlice `{offset 9, length 32}` and encoding base58, and override that account by pubkey:

```
template: raydium-clmm-amm-config     # on the pool's amm_config
trade_fee_rate: 10000                 # 1%
```

Every pool on that tier shares the account, so the change reaches all of them. A pool created with a
dynamic fee (its `dynamic_fee_info` is not all zeros) adds a volatility-based fee on top of
`trade_fee_rate`, so on such a pool the tier sets only the base fee.

## CPMM: Shock the price

A swap reads each reserve as the vault balance minus the fees the pool still owes out of it:

```
reserve_N = vault_N amount - protocol_fees_token_N - fund_fees_token_N - creator_fees_token_N
price     = reserve_1 / reserve_0 x 10^(mint_0_decimals - mint_1_decimals)
```

To multiply token 0's price by `f`, write both vaults so that their product stays the same:

```
template: spl-token-account-balance     # on token_0_vault
amount: <reserve_0 / sqrt(f) + owed fees of token 0>

template: spl-token-account-balance     # on token_1_vault
amount: <reserve_1 x sqrt(f) + owed fees of token 1>
```

Writing one vault alone also moves the price, but changes how deep the pool is.

**Scenario presets → Raydium state → CPMM price shock** in Studio reads the pool when the scenario
is created and stores absolute values, so if the pool's liquidity changes before Play, create the
scenario again.

## CPMM: Halt swaps

```
template: raydium-cpmm-pool-state
status: 4      # bit 2: swaps off, deposits and withdrawals still open
```

## CPMM: Change a fee tier

```
template: raydium-cpmm-amm-config       # the pool's amm_config
trade_fee_rate: 10000                   # 1%
```

Every pool created on that tier shares the account, so the change reaches all of them.

---

# Troubleshooting

| Program | Symptom | Fix |
|---|---|---|
| AMM v4 | Every instruction on the pool aborts | `status` is above 7 or `state` above 6 |
| AMM v4 | `InvalidStatus` on a swap | `status` is not 1, 6 or 7 |
| AMM v4 | A pool at status 7 still rejects swaps | The cluster clock has not reached `pool_open_time` |
| AMM v4 | A fee change did not move the swap price | You changed `trade_fee_*`. Swaps use `swap_fee_*` |
| AMM v4, CPMM | An override on the pool did not move the price | The price lives in the vaults. Use `spl-token-account-balance` |
| CLMM | The swap succeeds but the amounts look wrong after a price change | `liquidity` was not moved with the price. Use the Studio price shock |
| CLMM | The swap fails right after a price change | The new tick has no tick array, or `sqrt_price_x64` and `tick_current` disagree |
| CLMM | `raydium-clmm-custom` changed nothing | The mints are in the wrong order, so the derived address is empty |
| CLMM | The swap fails on the token transfer | The paying vault holds less than the output. Fund it with `spl-token-account-balance` |
| CPMM | `NotApproved` (6000) on a swap | `status` has bit 2 set, or the cluster clock is before `open_time` |
| CPMM | `InsufficientVault` (6012) on a swap | A vault was set below the fees the pool owes out of it |

**Known limitation.** `spl-token-account-balance` writes a token account's `amount` and keeps its
lamports. Raising a wrapped-SOL vault therefore leaves part of the new balance without lamports
behind it, and a swap that pays out more SOL than the vault held before fails in the token program.

---

# Template index

**Raydium AMM v4** &middot; `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`

| Template | Overrides |
|---|---|
| `raydium-amm-pool-state` | whether the pool trades (`status`), its machine state and LP supply |
| `raydium-amm-fees` | the swap fee, and the trade fee the OpenBook planner uses |
| `raydium-amm-swap-stats` | lifetime swap counters, and when a scheduled pool opens |

**Raydium CLMM** &middot; `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`

| Template | Overrides |
|---|---|
| `raydium-clmm-custom` | a pool's price, active liquidity and status, selected by fee tier and mints |
| `raydium-clmm-pool-state` | the same fields, selected by the pool address |
| `raydium-clmm-amm-config` | the fee tier a pool points to, shared by every pool on it |

**Raydium CPMM** &middot; `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`

| Template | Overrides |
|---|---|
| `raydium-cpmm-pool-state` | which operations the pool allows (`status`) and when it opens (`open_time`) |
| `raydium-cpmm-amm-config` | a fee tier, shared by every pool created on it |

---

# IDLs

`clmm/v1/idl.json` is the IDL Raydium publishes on chain in its Program Metadata account
`4wQuz7ZDMMxihw8qwm6zt7fNzRNdEvHMAT1eyPHNhWri`, unchanged (sha256
`862adbd60703fac5a5b397de8130778bd127d944c909b883f831f5cbbf911c8f`), as written in slot 459532766.

`cpmm/v1/idl.json` is the IDL Raydium publishes on chain in its Program Metadata account
`FHuJXL7cEttfHk4sXdTAXVaRymBdKfVs7ZH6cMgA1ihk`, unchanged (sha256
`e970d765469559c62089c0cf0bdceb6524c36eb9fd1ae7244d1ef85dd3afcaac`), as written in slot 462074068.
