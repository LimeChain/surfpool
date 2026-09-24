# Whirlpool state preparation

Declarative state-preparation templates for Orca's Whirlpool concentrated-liquidity AMM: put a
pool, or the protocol's shared fee config, into the state you need before your code runs
against it. Whirlpool publishes an Anchor IDL, so the templates use the standard IDL override
path (no raw-offset layout needed).

## Program identity (verified 2026-09-21)

|                            |                                                                                                                                      |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Program ID                 | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`                                                                                        |
| ProgramData                | `CtXfPzz36dH5Ws4UYKZvrQ1Xqzn42ecDW6y8NKuiN8nD`                                                                                       |
| Last deployed slot         | 440170207                                                                                                                            |
| `Whirlpool` account        | 653 bytes, `reward_infos` is a fixed `[WhirlpoolRewardInfo; 3]`                                                                      |
| `WhirlpoolsConfig` account | 108 bytes, one deployed singleton `2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ`                                                     |
| `TickArray` account        | 9988 bytes, `ticks: [Tick; 88]`                                                                                                      |
| Source                     | [orca-so/whirlpools](https://github.com/orca-so/whirlpools), `programs/whirlpool/src/state/{whirlpool,config,tick,tick_array}.rs` |

A later deployment slot than the one above means the program was upgraded and this integration
must be revisited (layouts, fee constants, the config singleton).

## Templates

| Template               | Account            | Address                                             | Use for                                    |
| ---------------------- | ------------------ | --------------------------------------------------- | ------------------------------------------ |
| `whirlpool-pool-state` | `Whirlpool`        | caller-provided pubkey                              | any pool by raw address                    |
| `whirlpools-config`    | `WhirlpoolsConfig` | `2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ`      | the default protocol fee new pools copy    |

Always set `fetchBeforeUse: true` so non-overridden fields keep their live values; `false` only
for a later override in the same scenario that builds on state an earlier override prepared.

## Finding a pool

There is no catalog: supply the pool's address. Look it up at
[`https://api.orca.so/v2/solana/pools`](https://api.orca.so/v2/solana/pools) (filter by mint
pair, sort by `tvl`). A valid address is owned by the Whirlpool program, is 653 bytes and carries
the `Whirlpool` discriminator from the bundled IDL. Examples: SOL/USDC
`HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ`, and SOL/USDC with tick spacing 4
`Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE`.

## Field reference

### `Whirlpool`

| Field                | Meaning                                                                | Units                                                                    |
| -------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `sqrt_price`         | `sqrt(price_raw) * 2^64`, `price_raw` = token B per token A, raw units | raw `u128` (Q64.64)                                                      |
| `tick_current_index` | Tick implied by `sqrt_price`                                           | raw `i32`                                                                |
| `liquidity`          | Liquidity active at `tick_current_index`                               | raw `u128`                                                               |
| `fee_rate`           | Swap fee on the input                                                  | hundredths of a bp, divide by 1,000,000 (`3000` = 0.30%), max `60000`    |
| `protocol_fee_rate`  | Protocol's cut of the collected fee, not of the swap                   | bp of the fee, divide by 10,000 (`1300` = 13%), max `2500`               |

`sqrt_price` and `tick_current_index` describe the same state and must be set together:

```
price_raw          = (sqrt_price / 2^64)^2
price              = price_raw * 10^(decimals_a - decimals_b)
tick_current_index = floor(log(price_raw) / log(1.0001))
```

A swap resumes from the `TickArray` at PDA `["tick_array", whirlpool, start_index.to_string()]`,
with `start_index = floor(tick_current_index / (tick_spacing * 88)) * tick_spacing * 88`. A tick
inconsistent with `sqrt_price`, or one with no initialized `TickArray`, fails the swap. Output is
paid from `token_vault_a` / `token_vault_b` (plain SPL token accounts, overridden with the
`spl-token` templates), so raising `liquidity` without funding the paying vault fails on the
transfer. `fee_rate` and `protocol_fee_rate` are independent.

### `WhirlpoolsConfig`

| Field                       | Meaning                                                                   | Units                                                      |
| --------------------------- | ------------------------------------------------------------------------- | ---------------------------------------------------------- |
| `default_protocol_fee_rate` | Copied into a **new** pool's `protocol_fee_rate`; existing pools keep theirs | bp of the fee, divide by 10,000 (`1300` = 13%), max `2500` |

The authority keys on this account decide who may change fees, not the fees, so they are not
exposed.

## Worked example: halve a pool's price

SOL/USDC has `token_mint_a` = SOL (9 decimals) and `token_mint_b` = USDC (6), so
`price = price_raw * 10^3`. A pool at 150 USDC per SOL has `price_raw = 0.15`,
`sqrt_price = 7144393258922745856` and `tick_current_index = -18973`. Halving it to 75 USDC:
`sqrt_price = 7144393258922745856 * sqrt(0.5) = 5051848920847731712`, and
`floor(log(0.075) / log(1.0001)) = -25904`.

```json
{
  "id": "whirlpool-price-shock",
  "templateId": "whirlpool-pool-state",
  "label": "halve the SOL/USDC price",
  "enabled": true,
  "scenarioRelativeSlot": 1,
  "fetchBeforeUse": true,
  "account": { "pubkey": "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ" },
  "values": {
    "sqrt_price": "5051848920847731712",
    "tick_current_index": -25904
  }
}
```

Pass `sqrt_price` as a decimal string: it exceeds `2^53`, the JSON number precision. On a real
pool, start from its live `sqrt_price`, and check that the `TickArray` for the new tick exists.

## Prepare a price shock through MCP

`create_whirlpool_price_shock_scenario` does the arithmetic above from the pool as the running
surfnet sees it and stages a one-override scenario you can edit.

| Param         | Type             | Meaning                                                                          |
| ------------- | ---------------- | -------------------------------------------------------------------------------- |
| `pool`        | string           | Base58 address of the `Whirlpool` account                                        |
| `priceFactor` | string           | Multiplier on the pool's price: `0.5` halves it, `4` quadruples it; finite, above zero, not 1 |
| `surfnetPort` | number, optional | Port of the target surfnet; omit for 8899                                        |

```json
{ "pool": "Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE", "priceFactor": "0.999" }
```

Before staging, the tool checks the pool's owner, size and discriminator, then reads the
`TickArray` covering the new tick. If that array does not exist, it names the array and reports
the largest (or smallest) factor that stays on the array the pool sits on now. On success it
returns `{ "error": null, "url": "<Studio editor URL>" }`; the staged override is
`whirlpool-pool-state` at slot 1 with `fetchBeforeUse: true`, setting only `sqrt_price` and
`tick_current_index`.

## Verification

- Unit: `cargo test -p surfpool-core --lib whirlpool` - price-shock arithmetic against known
  vectors, tick-array start index for negative ticks, pool decode through the IDL, rejection of
  bad factors, foreign accounts and a missing tick array; the `whirlpools-config` address and
  label/description completeness.
- MCP surface: `cargo test -p surfpool-mcp whirlpool` - camelCase params on the wire, and invalid
  input rejected before any RPC call.
- Live-fork byte diff and remote: `cargo test -p surfpool-core --features integration-tests
  whirlpool -- --test-threads=1` (needs network; `SURFPOOL_TEST_RPC_URL` overrides the endpoint).
  A live pool round-trips through the bundled IDL byte for byte; a live pool's config field equals
  the `whirlpools-config` address; `fee_rate` and `sqrt_price` overrides through the materializer
  change only their own bytes; the price-shock builder decodes two live pools and accepts the real
  `TickArray` its PDA recipe derives.

## Known limitations

- Whirlpool rejects swaps with `InvalidTimestamp` (6022) while the surfnet clock is behind the
  pool's `reward_last_updated_timestamp`. Studio's Play pauses the clock and `fetchBeforeUse`
  pulls a fresh timestamp, so resume the clock (Studio's clock control or `surfnet_resumeClock`)
  or move it past that timestamp with `surfnet_timeTravel` before swapping. Its
  `absoluteTimestamp` is in milliseconds while the pool's timestamp is in seconds, so multiply
  the seconds value by 1000.
- There is no behavioral swap test (baseline versus shocked simulate); the evidence for a price
  override is byte-level, not trade-level.
