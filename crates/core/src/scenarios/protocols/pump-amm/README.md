# PumpSwap (pump-amm)

The AMM a pump.fun coin trades on after its bonding curve completes and migrates. For the
bonding-curve side and the full lifecycle, see [`../pump/README.md`](../pump/README.md).

Program: `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`.

## The bundled IDL

`v1/idl.json` is the IDL the program publishes on chain in its Anchor IDL account
`5fLnXNNoZcZt9Qku6HARM3un3Ttm2cGsR7gN9Zp1R7h3`, last written on 2026-05-14 (slot 419683407).
The account holds compact JSON (sha256
`e16ac8008908911575241a25cad33bed8d2da2153f0d726790065fd467b90ff4`). The bundled file is that
JSON as `jq .` prints it, without the final newline, and is byte-identical to
[`idl/pump_amm.json`](https://github.com/pump-fun/pump-public-docs/blob/3c6721a67c0b206b39130b454c8ba22a83ce972e/idl/pump_amm.json)
at pump-public-docs commit `3c6721a67c0b`. Its sha256 is
`cd5c58a5dded23632aedb993b08cc0d2ba91bda2bb084560913464274d3bee50`.

Later pump-public-docs commits describe more state than the on-chain IDL does, such as
`Pool.virtual_quote_reserves`, a signed `i128` in `Pool` bytes 245 to 260. The deployed
program adds it to the quote vault balance when it prices a swap (checked on 2026-11-18).
It is past the end of the published `Pool` type, so the IDL templates cannot write it, and
an IDL write keeps every byte past the type unchanged. `v1/raw_overrides.yaml` writes it by
offset instead, through the `raw_layout` mechanism.

The published `Pool` type is 245 bytes. Most live pools are 300 or 301 bytes. Older pools
are 211, 243, 244 or 245 bytes: an IDL write on the first three fails with "not enough
bytes", and none of them has room for `virtual_quote_reserves`. The published
`GlobalConfig` type is 907 bytes, and the live account is 949 bytes.

## Templates

| Template                               | Account        | Selected by   | Use for                                                                           |
| -------------------------------------- | -------------- | ------------- | --------------------------------------------------------------------------------- |
| `pump-amm-pool-state`                  | `Pool`         | pool address  | any pool, including non-canonical or non-WSOL ones                                |
| `pump-amm-pool-virtual-quote-reserves` | `Pool`         | pool address  | any pool's `virtual_quote_reserves`, written by byte offset                       |
| `pump-amm-canonical-pool`              | `Pool`         | coin mint     | the canonical WSOL pool of a migrated coin, derived so you don't need its address |
| `pump-amm-virtual-quote-reserves`      | `Pool`         | coin mint     | the canonical pool's `virtual_quote_reserves`, written by byte offset             |
| `pump-amm-global-config`               | `GlobalConfig` | — (singleton) | pool fees and disable flags                                                       |

## Field reference

What each overridable field means and what overriding it lets you model.

### `Pool`

| Field                    | Meaning                                                           | Override it to                                                         |
| ------------------------ | ----------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `lp_supply`              | Total LP token supply before user burns and lock-ups              | model LP state                                                         |
| `coin_creator`           | Pubkey accruing the coin-creator fee for this pool                | point creator fees at a key you control                                |
| `virtual_quote_reserves` | Signed quote amount added to the quote vault balance when quoting | shift the effective quote (reprice) without touching any vault balance |

`virtual_quote_reserves` is written only by the raw-layout templates:
`pump-amm-pool-virtual-quote-reserves` by pool address, and `pump-amm-virtual-quote-reserves`
by coin mint. The field is not 0 on every pool: a census of all 1,610,872 `Pool` accounts on
2026-11-18 found 129,698 of the 1,357,424 pools of 261 bytes or more with a non-zero value
(89,035 positive, 40,663 negative), such as -769230 on
`GseMAnNDvntR5uFePZ51yZBXzNSn7GdFPkfHwfr6d77J`. A write replaces the current value. On a pool
shorter than 261 bytes the write is skipped with a warning in the log. Neither template checks
the account's owner or discriminator, so give `pump-amm-pool-virtual-quote-reserves` the
address of a PumpSwap `Pool`.

The other price-setting reserves live in the pool's token accounts (`pool_base_token_account`
/ `pool_quote_token_account`), not the `Pool` account - move those with the spl-token
template.

### `GlobalConfig` (singleton, `["global_config"]`)

| Field                                                                               | Meaning                                                            | Override it to                                    |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------ | ------------------------------------------------- |
| `lp_fee_basis_points`, `protocol_fee_basis_points`, `coin_creator_fee_basis_points` | Legacy flat fees; live trades read the fee program's `FeeConfig`   | legacy - won't change what a swap charges         |
| `disable_flags`                                                                     | Bitmask disabling individual instructions (0 = everything enabled) | disable specific instructions to test error paths |

## Pricing

PumpSwap is a constant-product AMM. The reserves that set the price live in the pool's two
token accounts, not in the `Pool` account. Effective quote reserves are the quote vault
balance plus `Pool.virtual_quote_reserves`.

Two ways to move the price:

- override `virtual_quote_reserves` with `pump-amm-pool-virtual-quote-reserves` (any pool, by
  address) or `pump-amm-virtual-quote-reserves` (canonical pool, by mint) — shifts the
  effective quote without touching any balance;
- override the vault balances with the `spl-token-account-balance` template — the vault
  addresses are in the `Pool` account's `pool_base_token_account` /
  `pool_quote_token_account` fields.

For the canonical WSOL pool of a migrated coin, both IDL and raw-layout templates derive the
pool from `base_mint`, so one scenario can hold a `pump-amm-canonical-pool` override and a
`pump-amm-virtual-quote-reserves` override for the same pool. See the complete example in
[`../pump/README.md`](../pump/README.md).

## Notes

- The canonical template only works for coins that migrated to PumpSwap (roughly March 2025
  onward). Coins that graduated earlier went to Raydium and have no canonical pool — use
  `pump-amm-pool-state` with the pool address for those.
- Fees on a live trade come from the external fee program's `FeeConfig`, not from the
  basis-point fields on `GlobalConfig` (those are legacy). Overriding them here won't change
  what a swap charges.
- Always set `fetchBeforeUse: true` so the fields you don't override keep their live values.
