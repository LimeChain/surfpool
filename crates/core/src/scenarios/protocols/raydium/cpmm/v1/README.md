# Raydium CPMM

Raydium's constant-product AMM without an orderbook (`raydium_cp_swap`, listed by Raydium as
CP-Swap). Two templates: a pool's status and open time, and a fee tier. For how scenarios work in
general see the [scenarios README](../../../../README.md).

**The price is not in the pool account.** A swap prices off the two vault token accounts, so move
the price with `spl-token-account-balance` on the vaults named in the pool's `token_0_vault` and
`token_1_vault` fields.

# Template index

**Raydium CPMM** &middot; `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`

| Template | Overrides |
|---|---|
| `raydium-cpmm-pool-state` | which operations the pool allows (`status`) and when it opens (`open_time`) |
| `raydium-cpmm-amm-config` | a fee tier, shared by every pool created on it |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `status` | bitmask of disabled operations | `4` = swaps off, `0` = everything on |
| `open_time` | unix seconds | |
| `trade_fee_rate`, `creator_fee_rate` | parts per million of the input | `2500` = 0.25% |
| `protocol_fee_rate`, `fund_fee_rate` | parts per million of the collected fee, together at most `1000000` | `120000` = 12% of the fee |

`status` bits: 0 deposit, 1 withdraw, 2 swap.

## Picking a pool

Find the pools of a pair with `getProgramAccounts` on the program, filtered by size `637` and the
two mints at offsets `168` and `200`. A pool stores the mint whose raw 32 bytes are smaller first,
so only one of the two orders returns pools. A pair can have a pool per fee tier; the one whose
vaults hold the most is usually the one you want. Raydium's API also lists pools by liquidity:
`https://api-v3.raydium.io/pools/info/list?poolType=standard&poolSortField=liquidity`. Its
`standard` type also covers AMM v4, so keep only entries whose `programId` is
`CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`.

Set `fetchBeforeUse: true` to pull the pool from upstream first.

# Recipes

## Shock the price

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

## Halt swaps

```
template: raydium-cpmm-pool-state
status: 4      # bit 2: swaps off, deposits and withdrawals still open
```

## Change a fee tier

```
template: raydium-cpmm-amm-config       # the pool's amm_config
trade_fee_rate: 10000                   # 1%
```

Every pool created on that tier shares the account, so the change reaches all of them.

# Troubleshooting

| Symptom | Fix |
|---|---|
| `NotApproved` (6000) on a swap | `status` has bit 2 set, or the cluster clock is before `open_time` |
| `InsufficientVault` (6012) on a swap | A vault was set below the fees the pool owes out of it |
| An override on the pool did not move the price | The price lives in the vaults. Use `spl-token-account-balance` |

**Known limitation.** `spl-token-account-balance` writes a token account's `amount` and keeps its
lamports. Raising a wrapped-SOL vault therefore leaves part of the new balance without lamports
behind it, and a swap that pays out more SOL than the vault held before fails in the token program.
