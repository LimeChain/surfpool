# PumpSwap (pump-amm)

The AMM a pump.fun coin trades on after its bonding curve completes and migrates. For the
bonding-curve side and the full lifecycle, see [`../pump/README.md`](../pump/README.md).

Program: `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`. The IDL is copied verbatim from
pump-public-docs (`idl/pump_amm.json`).

## Templates

| Template                  | Account        | Selected by   | Use for                                                                           |
| ------------------------- | -------------- | ------------- | --------------------------------------------------------------------------------- |
| `pump-amm-pool-state`     | `Pool`         | pool address  | any pool, including non-canonical or non-WSOL ones                                |
| `pump-amm-canonical-pool` | `Pool`         | coin mint     | the canonical WSOL pool of a migrated coin, derived so you don't need its address |
| `pump-amm-global-config`  | `GlobalConfig` | — (singleton) | pool fees and disable flags                                                       |

## Field reference

What each overridable field means and what overriding it lets you model.

### `Pool`

| Field                    | Meaning                                                                               | Override it to                                                         |
| ------------------------ | ------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `lp_supply`              | Total LP token supply before user burns and lock-ups                                  | model LP state                                                         |
| `coin_creator`           | Pubkey accruing the coin-creator fee for this pool                                    | point creator fees at a key you control                                |
| `virtual_quote_reserves` | Appended quote reserves added to the quote vault when quoting (0 on every pool today) | shift the effective quote (reprice) without touching any vault balance |

The price-setting reserves live in the pool's token accounts (`pool_base_token_account` /
`pool_quote_token_account`), not the `Pool` account - move those with the spl-token template.

### `GlobalConfig` (singleton, `["global_config"]`)

| Field                                                                               | Meaning                                                            | Override it to                                    |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------ | ------------------------------------------------- |
| `lp_fee_basis_points`, `protocol_fee_basis_points`, `coin_creator_fee_basis_points` | Legacy flat fees; live trades read the fee program's `FeeConfig`   | legacy - won't change what a swap charges         |
| `disable_flags`                                                                     | Bitmask disabling individual instructions (0 = everything enabled) | disable specific instructions to test error paths |

## Pricing

PumpSwap is a constant-product AMM. The reserves that set the price live in the pool's two
token accounts, not in the `Pool` account. Effective quote reserves are the quote vault
balance plus `Pool.virtual_quote_reserves` (which is 0 on every pool today).

Two ways to move the price:

- override `virtual_quote_reserves` on the pool — shifts the effective quote without
  touching any balance;
- override the vault balances with the spl-token template — the vault addresses are in the
  `Pool` account's `pool_base_token_account` / `pool_quote_token_account` fields.

For the canonical WSOL pool of a migrated coin, the Studio preset loads the existing
`pump-amm-canonical-pool` template and stores a standard scenario. AI clients use the same
template through the generic `create_scenario` MCP tool. See the complete example in
[`../pump/README.md`](../pump/README.md).

## Notes

- The canonical template only works for coins that migrated to PumpSwap (roughly March 2025
  onward). Coins that graduated earlier went to Raydium and have no canonical pool — use
  `pump-amm-pool-state` with the pool address for those.
- Fees on a live trade come from the external fee program's `FeeConfig`, not from the
  basis-point fields on `GlobalConfig` (those are legacy). Overriding them here won't change
  what a swap charges.
- Always set `fetchBeforeUse: true` so the fields you don't override keep their live values.
