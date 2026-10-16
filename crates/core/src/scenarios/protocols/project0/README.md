# Project 0

Surfpool bundles the IDL and override templates for **Project 0**, the lending program formerly
known as marginfi v2 (`MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`, v0.1.12), so a scenario can
put a bank or a user account into whatever state you need before your code runs against it.

The bundled IDL is the program's published 0.1.12 IDL with one change: `WrappedI80F48.value` is
typed `i128` instead of `[u8; 16]`. The bytes are the same, read as the signed fixed-point integer
(value x 2^48), so weights and shares take decimal strings instead of 16-byte arrays.

| IDL | Source | sha256 |
|---|---|---|
| Published | Program Metadata account `B66dYmjB9pJ3unjp3QMiEpySb8i44Zhev3qaYj5SfLPG` | `8bb1b2e4331b2f28e9d6d4a3d2c8e5996e371b5acfb102cfcf52175423e5eaf1` |
| Bundled | `v1/idl.json` | `bcab696279320fc5b0aa18a5eb3f711e655b6ac2fc3c3d32d06fe8adf1d5e05c` |

This is a how-to. For how scenarios work in general see the [scenarios README](../../README.md).
Every field's own purpose and units are on the template itself, visible in Studio and via
`get_override_templates`.

## Override inputs, not results

Project 0 recomputes an account's health on every borrow, withdraw and liquidation, from the
account's shares, the banks' share values and weights, and the live oracle price. The account's
`health_cache` and a bank's `cache` are outputs written along the way; nothing reads them back.

| Want to change | Override this | Not this |
|---|---|---|
| A price | the bank's Pyth feed with `pyth-pro-price-feed-v2` (see [Move a price](#move-a-price)) | `cache.last_oracle_price` |
| Position health | `project0-bank-risk-weights` → `config.asset_weight_maint.value` | `health_cache` |
| A position | `project0-account-balance` | |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `.value` of a weight, share or share value | value x 2^48, as a decimal string | 1.0 → `"281474976710656"` |
| Shares | tokens / share value | 10 SOL at share value 1.0 → `"2814749767106560000000000"` |
| Interest rates | out of 1000% as u32 | 100% APR → `429496729` |
| Utilization | out of 100% as u32 | 50% → `2147483647` |
| Token amounts, limits | the mint's smallest unit | 1 USDC → `1000000` |
| `operational_state` | a variant name | `"ReduceOnly"` |

## Picking a bank

Every template takes the account it writes; callers are responsible for passing a Bank or a
MarginfiAccount. Symbols repeat: the main group (`4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8`)
holds native banks and Kamino-, Drift- or JupLend-wrapped banks of the same mint, so key
everything by address.

A token's native bank and the price account it reads come from one `getProgramAccounts` call on
the program: filter on the Bank discriminator at offset 0 (`QnTef4UXSzF` in base58), the group at
offset 41, the mint at offset 8 and an oracle setup byte of 3 at offset 609 (a Pyth push oracle),
with a `dataSlice` of 32 bytes at offset 610 (`config.oracle_keys.0`). The templates'
`llm_context` spells the call out. A wallet's accounts are the program's MarginfiAccounts
(discriminator `CKkRR4La3xu` at offset 0) with the group at offset 8 and the authority at offset 40.
Filter on the discriminator rather than the account size, which program upgrades change.

For example, the native SOL bank is `CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh` (asset tag 1)
and reads `7AviUf9nL62mcxNbQGKm4nKDQnPjswo6c5MX4D57HmyE`; the native USDC bank is
`2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB` (asset tag 0) and reads
`6HAuqASbHEh4w4REJEUUUCginTLfj1kwCh215ZLtMkrT`.

---

# Recipes

## Make a position liquidatable

```
project0-bank-risk-weights   on the SOL bank
  config.asset_weight_maint.value: "84442493013197"   # 0.3
```

**Why:** health is `sum(deposits x asset_weight_maint) - sum(debts x liability_weight_maint)`,
recomputed from the bank on every check. Tested against the deployed program: a SOL-backed USDC
borrow goes from healthy to liquidatable.

## Move a price

Project 0 banks read Pyth `PriceUpdateV2` accounts of the Pyth Pro-compatible receiver,
`rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp`. Use `pyth-pro-price-feed-v2` on the account the
bank names at `config.oracle_keys.0`:

```
pyth-pro-price-feed-v2   on the bank's config.oracle_keys.0, fetchBeforeUse: true
  price_message.price: 2000000000        # $20, exponent -8
  price_message.ema_price: 2000000000
```

**Why:** the price must be fresh: `clock - publish_time <= config.oracle_max_age`. With
`fetchBeforeUse: true` the account carries upstream's current `publish_time` when the scenario
plays. Borrow and withdraw checks use the EMA price, liquidation the spot price, so move both.
Tested against the deployed program: SOL at $20 turns a healthy SOL-backed USDC borrow
liquidatable.

**Stablecoins** such as USDC and USDT have a price circuit breaker. Once the price, net of the
oracle's confidence, is `config.cb_deviation_bps_tiers[0]` basis points (50 for USDC) away from
the bank's `cb_reference_price`, borrow, withdraw and liquidation fail with
`CircuitBreakerPriceJump` (6604). Move the reference with the price:

```
project0-bank-oracle     on the USDC bank, fetchBeforeUse: true
  cb_reference_price.value: "267401227875123"          # $0.95 x 2^48
  cb_window_reference_price.value: "267401227875123"
pyth-pro-price-feed-v2   on the bank's config.oracle_keys.0, fetchBeforeUse: true
  price_message.price: 95000000                         # $0.95, exponent -8
  price_message.ema_price: 95000000
```

The breaker then treats $0.95 as normal and still guards further moves.

## Build a position from scratch

```
project0-account-balance
  lending_account.balances.0.active: 1
  lending_account.balances.0.bank_pk: <the SOL bank>
  lending_account.balances.0.bank_asset_tag: 1
  lending_account.balances.0.asset_shares.value: "2814749767106560000000000"
project0-account-balance
  lending_account.balances.1.active: 1
  lending_account.balances.1.bank_pk: <the USDC bank>
  lending_account.balances.1.bank_asset_tag: 0
  lending_account.balances.1.liability_shares.value: "140737488355328000000000"
```

**Why:** slots stay sorted by `bank_pk`, highest first, so SOL's bank goes before USDC's, and
`bank_asset_tag` must match the bank's `config.asset_tag`. The template lists the other rules.

## Take over a real account

```
project0-account-authority   authority: <your wallet>
```

## Age every loan in a bank

```
project0-bank-shares   liability_share_value.value: <raise it>
```

**Why:** a debt is `liability_shares x liability_share_value`. Raise `asset_share_value` too if
depositors should earn, and keep the vault balance in line (see Troubleshooting).

## Block an action to test the rejection

```
project0-bank-status   config.operational_state: "ReduceOnly"            # no new deposits or borrows
project0-bank-limits   config.borrow_limit: 1                            # bank lent out
project0-bank-limits   config.deposit_limit: 1                           # bank full
```

---

# Troubleshooting

| Rejection | Fix |
|---|---|
| Stale oracle | Refetch the feed with `fetchBeforeUse: true` |
| `CircuitBreakerPriceJump` (6604) | A stablecoin price moved past the bank's circuit breaker. Move the reference prices with it, see [Move a price](#move-a-price) |
| `BankCircuitBreakerHalted` (6600) | `lending_pool_pulse_bank_price_cache` ran during a breach and halted the bank. Refetch the bank with `fetchBeforeUse: true` and move its reference prices |
| The program panics in `accrue_interest` | The bank was loaded from upstream after the Surfnet clock stopped, so its `last_update` is ahead of the clock. Move the clock to the current time with `surfnet_timeTravel`: `[{"absoluteTimestamp": <now in milliseconds>}]` |
| Your price override did nothing | Write the bank's `config.oracle_keys.0` with `pyth-pro-price-feed-v2`. The default receiver's account for the same feed (`pyth-price-feed-v2`) is a different address, which the bank never reads. See [Move a price](#move-a-price) |
| Wrong oracle account | Pass exactly the bank's `config.oracle_keys.0` |
| Withdraw fails on the token transfer | Shares imply more tokens than the bank's `liquidity_vault` holds. Raise the vault with `spl-token-account-balance` |
| `exceeds what a JSON number can hold exactly` | Pass `.value` fields as decimal strings |
| A Kamino-, Drift- or JupLend-wrapped bank rejects its price | Those need the venue account refreshed in the same transaction, which these templates do not cover |

---

# Template index

**Project 0** &middot; `MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`

| Template | Overrides |
|---|---|
| `project0-bank-risk-weights` | the collateral and debt weights of a bank |
| `project0-bank-limits` | the deposit, borrow and collateral value caps of a bank |
| `project0-bank-status` | a bank's operational state (pause, reduce-only) |
| `project0-bank-oracle` | the oracle staleness, confidence and circuit breaker guards of a bank |
| `project0-bank-interest-rate` | the bank borrow-rate curve |
| `project0-bank-shares` | the total shares and share values of a bank |
| `project0-account-balance` | one balance slot of a MarginfiAccount |
| `project0-account-authority` | the authority of a MarginfiAccount |
