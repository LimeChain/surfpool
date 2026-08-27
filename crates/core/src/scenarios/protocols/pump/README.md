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
derivation is copied verbatim from the template. Then press Play:

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

## Prepare a graduation

The `create_pump_graduation_scenario` MCP tool builds the whole preparation from Surfnet
state, with no field math required from the caller. Accounts already present locally are
authoritative; Surfnet fetches only missing accounts from mainnet. The same tool powers the
_Pump Graduation_ preset card in Studio.

```json
{
  "tokenMint": "<SOL-quoted pump.fun mint still on its bonding curve>"
}
```

The coin must have a **Token-2022 mint**, a SOL-quoted incomplete curve, and no canonical
PumpSwap pool yet. Eligibility failures are returned in the MCP tool result. The preset
does not cover coins with a classic SPL-Token mint or a non-SOL quote mint — their curve
can still be overridden field by field with `pump-bonding-curve-custom`, but the vault
template and this graduation flow are Token-2022 and SOL-quote only. A successful tool call
returns the Studio editor URL, where the stored scenario contains three overrides:

1. the curve one buy away from completion (`real_token_reserves` = the finishing buy,
   sized so the buy also clears the migration fee),
2. the curve vault topped up to `migration reserve + finishing buy` — the reserve is
   what `migrate_v2` moves into the pool, so draining the vault to match
   `real_token_reserves` would make migration fail with `ZeroBaseAmount`,
3. `Global.enable_migrate = true`.

Press Play, then drive it like a user would: a real `buy_v2` of the curve override's
`real_token_reserves` completes the curve, and a real `migrate_v2` creates the canonical
WSOL pool with the reserve as its base liquidity.

## Shock a migrated pool's price

Use the existing `pump-amm-canonical-pool` template through the standard scenario API or
the generic `create_scenario` MCP tool. The Studio preset follows the same path: it loads
the registered template, supplies the mint and reserve value, and stores a normal scenario.

```json
{
  "templateId": "pump-amm-canonical-pool",
  "values": {
    "base_mint": "<WSOL-paired migrated pump.fun mint>",
    "virtual_quote_reserves": 15000000000000
  },
  "scenarioRelativeSlot": 1,
  "fetchBeforeUse": true
}
```

`virtual_quote_reserves` is appended to the quote vault balance
when the AMM quotes, so raising it makes the same sell return more quote without
touching any token balance. The template derives the canonical WSOL pool from `base_mint`.
If the derived account is absent or invalid, Play reports the materialization failure.
After a successful Play, the identical sell transaction simulates with a higher quote-token
output than before.

## Verification

- Address identity: `cargo test -p surfpool-core --lib pump` — template PDAs pinned to
  externally documented addresses (pump-public-docs).
- MCP surface: `cargo test -p surfpool-mcp surfpool::tests` — compact template listing,
  catalog scoping, and generic scenario validation.
- Integration: `cargo test -p surfpool-core --features integration-tests pump` needs a
  network connection (`SURFPOOL_TEST_RPC_URL` overrides the public mainnet endpoint).
  It round-trips live mainnet curve, pool, and config accounts through the bundled
  IDLs to catch layout drift after a program upgrade, proves overrides touch only
  their target bytes on real account data, exercises the graduation builder's
  validation against live state, and runs the full lifecycle: the frozen Token-2022
  fixture is prepared through the production graduation scenario builder and
  materializer, then real `buy_v2`, `migrate_v2`, and PumpSwap `sell` instructions
  execute against live mainnet programs, including a baseline vs. price-shocked sell
  simulation whose quote-token output must differ.

### The test snapshot and how to retake it

`crates/core/src/tests/assets/pump_token2022_graduation.snapshot.json` freezes the
HRTz coin (`HRTzNRJNnY78xe8e4a9DuMotw6qA97GwSQLzpVw9pump`, Token-2022, incomplete
SOL-quoted curve) plus everything a buy/migrate/sell reads: pump `Global`, both
programs' fee-program `FeeConfig`s, the PumpSwap `GlobalConfig` (mayhem mode left on,
exactly as on mainnet), the fee recipients and their WSOL ATAs, and the other accounts
those instructions read along the way. Entries owned by the system program with 0
lamports (the canonical pool and its accounts) deliberately pin those addresses
**absent** — the surfnet then neither finds them locally nor fetches them remotely, so
`migrate_v2` gets to create them. The two programs are *not* frozen on purpose: they load
live from the mainnet fork, so an on-chain upgrade against the frozen config fails the
test instead of passing silently.

The file is the same account-map shape `surfpool start --snapshot` accepts. To retake
it, fetch every pubkey already present in the file in one `getMultipleAccounts` request
with base64 encoding, record the response context slot, and replace the frozen entries
wholesale while keeping the zero-lamport pinned-absent entries as they are. Pick a coin
state matching the checks above (Token-2022 mint, incomplete SOL-quoted curve, no
canonical pool) if HRTz has since graduated.
