# Pump / PumpSwap state preparation

Declarative state-preparation templates for the pump.fun ecosystem. A pump.fun coin's
life spans two on-chain programs, so the integration covers both:

1. **Pump** (bonding curve launchpad) — new coins trade on a constant-product curve over
   synthetic (virtual) reserves until the curve is bought out (`complete = true`).
2. **PumpSwap** (`pump-amm`, pump.fun's AMM) — completed curves migrate here; reserves
   live in the pool's token accounts, not in the Pool account itself.

Both programs publish Anchor IDLs, so the templates use the standard IDL override path
(no raw-offset layout needed).

## Program identity (verified 2026-08-07)

|                    | Pump                                                                                                                                                | PumpSwap                                                                                                                                   |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Program ID         | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`                                                                                                       | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`                                                                                              |
| ProgramData        | `B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu`                                                                                                      | `6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc`                                                                                             |
| Last deployed slot | 433095571                                                                                                                                           | 433112355                                                                                                                                  |
| Bundled IDL        | byte-identical to [`idl/pump.json`](https://github.com/pump-fun/pump-public-docs/blob/main/idl/pump.json) at pump-public-docs commit `3c6721a67c0b` | byte-identical to [`idl/pump_amm.json`](https://github.com/pump-fun/pump-public-docs/blob/main/idl/pump_amm.json) at commit `2c22246b6708` |

A later deployment slot than the one above means the program was upgraded and this
integration must be revisited (layouts, formulas, fee wiring).

Fees: every buy/sell passes the fee program's `FeeConfig` (market-cap fee tiers) as a
required account under `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ`; the flat
basis-point fields on `Global` / `GlobalConfig` are legacy.

## Templates

| Template                    | Account        | Address                                                                   | Use for                                                                |
| --------------------------- | -------------- | ------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `pump-bonding-curve-custom` | `BondingCurve` | PDA `["bonding-curve", mint]`                                             | any coin's curve, selected by mint                                     |
| `pump-global`               | `Global`       | PDA `["global"]` → `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf`         | fee/init parameters for curves created after the override              |
| `pump-amm-pool-state`       | `Pool`         | caller-provided pubkey                                                    | any pool by raw address (only path for non-canonical / non-WSOL pools) |
| `pump-amm-canonical-pool`   | `Pool`         | PDA `["pool", u16le(0), PDA(pump, ["pool-authority", mint]), mint, WSOL]` | the canonical WSOL-quoted pool of a migrated coin, selected by mint    |
| `pump-amm-global-config`    | `GlobalConfig` | PDA `["global_config"]` → `ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw`  | pool fee/disable flags                                                 |

Notes:

- The mint catalogs offer only verified tokens whose address ends in `pump`
  (976 mints in the bundled catalog; `address_suffix: pump` in both `overrides.yaml`
  files). Coins outside the verified catalog — including freshly launched ones — can
  only be targeted through the raw REST payload, which performs no catalog validation.
- `pump-amm-canonical-pool` covers WSOL-quoted canonical migrations only; any other
  pool goes through `pump-amm-pool-state` with its address.
- Always set `fetchBeforeUse: true` so non-overridden fields keep their live values.

## Field reference

What each overridable field means and what overriding it lets you model. Keep the
curve-lifetime invariants when you touch reserves (`virtual − real` = 279.9T tokens and
30 SOL of quote with today's mainnet `Global` defaults).

### `BondingCurve`

| Field                    | Meaning                                                                       | Override it to                                                            |
| ------------------------ | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `virtual_token_reserves` | Synthetic token reserves in the price formula (raw, 6 decimals)               | reprice the curve - spot = `virtual_quote / virtual_token`                |
| `virtual_quote_reserves` | Synthetic quote reserves (lamports for SOL-quoted coins)                      | reprice the curve                                                         |
| `real_token_reserves`    | Tokens the curve still holds; completion is when this reaches 0               | set how close to graduation the curve sits (a small value = one buy away) |
| `real_quote_reserves`    | Quote the curve actually holds                                                | model accumulated quote                                                   |
| `complete`               | True once bought out; a completed curve rejects buy/sell and can only migrate | flip `true` to model a graduated curve, `false` to reopen trading         |
| `creator`                | Coin creator that accrues creator fees via the creator vault                  | point creator fees at a key you control                                   |
| `token_mint`             | Selects which coin's curve (a PDA seed, not a stored field)                   | choose the target coin                                                    |

### `Global` (singleton, `["global"]`)

| Field                                                                                                                                                   | Meaning                                                          | Override it to                                                                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `fee_basis_points`, `creator_fee_basis_points`                                                                                                          | Legacy flat fees; live trades read the fee program's `FeeConfig` | rarely useful (legacy)                                                                          |
| `initial_virtual_token_reserves`, `initial_virtual_sol_reserves`, `initial_virtual_quote_reserves`, `initial_real_token_reserves`, `token_total_supply` | Seed values a **new** curve is created with                      | change the launch parameters of curves created after the override (existing curves keep theirs) |
| `enable_migrate`                                                                                                                                        | Gates the `migrate` instruction                                  | set `true` to let a completed curve migrate to PumpSwap                                         |
| `pool_migration_fee`                                                                                                                                    | Fee charged when a curve migrates                                | model migration cost                                                                            |
| `withdraw_authority`                                                                                                                                    | Authority the `migrate` / withdraw path checks                   | set to a key you control to drive a real `migrate` transaction on a fork                        |

## Worked example: reset a curve to a fresh state

Create it in the Studio editor (Pump tile → _Override Bonding Curve (Custom)_), which
fills the envelope automatically, or POST the full REST `Scenario` shape below to
`/v1/scenarios` — every envelope field is required by the endpoint, and the `account`
recipe is copied verbatim from the template. Then press Play:

```json
{
  "id": "d2f8a1c4-7b3e-4e9a-8c5d-0f6b2a9e4d71",
  "name": "fresh pump curve",
  "description": "reset a live coin's curve to launch state",
  "tags": ["pump"],
  "overrides": [
    {
      "id": "curve-reset-0",
      "templateId": "pump-bonding-curve-custom",
      "label": "reset bonding curve to launch state",
      "enabled": true,
      "scenarioRelativeSlot": 0,
      "fetchBeforeUse": true,
      "account": {
        "pda": {
          "programId": "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
          "seeds": [
            { "string": "bonding-curve" },
            { "propertyRef": "token_mint" }
          ]
        }
      },
      "values": {
        "token_mint": "<a pump.fun mint from the catalog>",
        "virtual_token_reserves": 1073000000000000,
        "virtual_quote_reserves": 30000000000,
        "real_token_reserves": 793100000000000,
        "real_quote_reserves": 0,
        "complete": false
      }
    }
  ]
}
```

Expected result: the coin's `BondingCurve` PDA holds exactly these values while every
other byte (token_total_supply, creator, the extend_account tail) stays live. Verify by
re-opening the override in the Studio field editor, or via `getAccountInfo`: u64 LE at
offsets 8 (virtual tokens), 16 (virtual quote), 24 (real tokens), 32 (real quote), bool
at 48 (complete). The surfpool log must contain no `skipping override` line.

Variant — a semantically valid _completed_ curve (rejects buys/sells with
`BondingCurveComplete`): `complete: true` requires `real_token_reserves: 0`; keep the
curve-lifetime invariants (`virtual − real` = 279.9T tokens / 30 SOL of quote, verified
against live mainnet data).

## Verification

- Address identity: `cargo test -p surfpool-core --lib pump` — template PDAs pinned to
  externally documented addresses (pump-public-docs), plus byte-exact apply-path tests
  over real 151/300-byte mainnet snapshots.
- MCP surface: `cargo test -p surfpool-mcp --lib` — compact template listing, catalog
  scoping, pre-HTTP validation errors.
- Behavioral differential (planned, `#[ignore = "requires-network"]`): execute real
  `buy` instructions against prepared state on a mainnet fork and assert exact
  fee-inclusive costs and error codes (`TooMuchSolRequired`, `BondingCurveComplete`,
  `ExceededSlippage`).
