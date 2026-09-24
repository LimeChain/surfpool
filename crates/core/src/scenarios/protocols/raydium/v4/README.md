# Raydium AMM v4 state preparation

Declarative state-preparation templates for Raydium's classic constant-product AMM, the
program behind most of Raydium's "Standard" pools. A pool is one 752-byte `AmmInfo`
account plus two SPL token vaults; the price lives in the vaults, not in `AmmInfo`.

## Program identity (verified 2026-09-20)

|                    | Raydium AMM v4                                                                                                        |
| ------------------ | --------------------------------------------------------------------------------------------------------------------- |
| Program ID         | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`                                                                        |
| ProgramData        | `A7ZG7ByDi8DpzT9Ab7CiXhvgYTJQmaDPJkMDoPitaCQV`                                                                        |
| Last deployed slot | 445763204                                                                                                             |
| Layout source      | [`raydium-io/raydium-amm`](https://github.com/raydium-io/raydium-amm) `program/src/state.rs` at commit `d26944bf`     |
| Account types      | `AmmInfo` 752 bytes, `TargetOrders` 2208 bytes (both asserted by the program's own tests: `[0u8; 752]`, `[0u8; 2208]`) |

A later deployment slot than the one above means the program was upgraded and this
integration must be revisited (layout, status semantics, fee wiring).

Discriminators: the program stores none, so the hand-written `idl.json` declares
`"discriminator": []`. The engine matches an empty-discriminator type only when the
account decodes to it with zero bytes left over; two such matches are refused as ambiguous.

## Templates

Every template takes the pool's own address (`address: {type: pubkey}`): canonical AMM v4
pools are keypair accounts, not PDAs.

| Template                  | Account   | Address       | Use for                                                       |
| ------------------------- | --------- | ------------- | -------------------------------------------------------------- |
| `raydium-amm-pool-state`  | `AmmInfo` | pool address  | opening or closing a pool to swaps (`status`), LP supply        |
| `raydium-amm-fees`        | `AmmInfo` | pool address  | making swaps cost more, less, or nothing                       |
| `raydium-amm-swap-stats`  | `AmmInfo` | pool address  | lifetime swap counters, and the open time of a scheduled pool   |

Always set `fetchBeforeUse: true` so the live pool is forked before the override applies.

## Finding a pool

There is no bundled catalog; find a pool's address yourself and paste it in:

- Raydium's public API lists pools by liquidity:
  `https://api-v3.raydium.io/pools/info/list?poolType=standard&poolSortField=liquidity`.
  Its `standard` pool type covers both AMM v4 and CP-Swap, which are different programs,
  so keep only entries whose `programId` is `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`.
- To confirm an address by hand: fetch the account and check it is 752 bytes, owned by
  `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`, and that byte 0 (the `status` field, a
  `u64`) holds a value in `0..=7` - the program has no discriminator, so size and owner
  are the only checks.
- A well-known example: the SOL/USDC AMM v4 pool at
  `58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2`.

## Field reference

Offsets are absolute byte offsets into the account, from `state.rs` at `d26944bf`.

### `AmmInfo`

| Field                          | Offset | Type | Meaning                                                                | Override it to                                              |
| ------------------------------ | ------ | ---- | ---------------------------------------------------------------------- | ----------------------------------------------------------- |
| `status`                       | 0      | u64  | which operations the program allows                                    | close a pool to swaps, or reopen one                        |
| `state`                        | 48     | u64  | OpenBook order-planning state machine                                  | rarely useful; not read by swaps                            |
| `fees.swap_fee_numerator`      | 176    | u64  | numerator of the fee a swap charges on its input                       | make swaps free, or expensive                               |
| `fees.swap_fee_denominator`    | 184    | u64  | denominator of the same fee; mainnet default 25/10000 = 0.25%          | change the scale of the fee                                 |
| `out_put.need_take_pnl_coin`   | 192    | u64  | coin owed to the protocol; subtracted from the vault before pricing    | shrink the effective coin reserve without moving the vault  |
| `out_put.need_take_pnl_pc`     | 200    | u64  | pc owed to the protocol; subtracted from the pc vault before pricing   | shrink the effective pc reserve                             |
| `out_put.pool_open_time`       | 224    | u64  | unix seconds from which a `status = 7` pool accepts swaps              | open a scheduled pool early                                 |
| `token_coin` (the coin vault)  | 336    | key  | SPL token account holding the base side                                | read-only: the address to point `spl-token-account-balance` |
| `token_pc` (the pc vault)      | 368    | key  | SPL token account holding the quote side                               | read-only: the same, for the quote side                     |
| `coin_mint`, `pc_mint`         | 400/432| key  | the vaults' mints                                                      | read-only                                                   |
| `market`                       | 528    | key  | the pool's OpenBook market                                             | read-only                                                    |
| `target_orders`                | 592    | key  | the pool's `TargetOrders` account                                      | read-only                                                   |
| `lp_amount`                    | 720    | u64  | LP tokens minted, raw units of the LP mint                             | model an empty or a large LP supply                         |

`status` values, from `AmmStatus` in `state.rs`: 0 uninitialized, 1 initialized, 2
disabled, 3 withdraw only, 4 liquidity only, 5 orderbook only, 6 swap only, 7 waiting
trade. `swap_base_in` and `swap_base_out` run only at 1, 6 and 7; 7 additionally requires
the cluster clock to have passed `pool_open_time`, after which the program rewrites the
status to 6 itself. Keep the value in 0..=7 and `state` in 0..=6: both are mapped through
an exhaustive `match` with an `unreachable!()` arm, so a larger number aborts every
instruction that reads the pool.

### Where the price is

A swap does not read a price from `AmmInfo`. It reads the two vault token accounts and
subtracts the pending PnL:

```
reserve_coin = coin_vault.amount - out_put.need_take_pnl_coin
reserve_pc   = pc_vault.amount   - out_put.need_take_pnl_pc
fee          = ceil(amount_in * fees.swap_fee_numerator / fees.swap_fee_denominator)
out          = constant_product(amount_in - fee, reserve_pc, reserve_coin, direction)
```

So the price lever is the vault balances, through the `spl-token-account-balance`
template on the addresses stored at offsets 336 and 368. `fees.trade_fee_*` is read by
the OpenBook order-planning path (`get_max_buy_size_at_price` /
`get_max_sell_size_at_price`), not by either swap instruction.

## Worked example: disable a live pool

POST the REST `Scenario` below to `/v1/scenarios`, or build it in the Studio editor
(Raydium tile → _Override AMM Pool State_), then press Play.

```json
{
  "id": "7c1f2a63-5d84-4a0e-9b21-3f8e6c0d5a19",
  "name": "halt SOL/USDC on Raydium v4",
  "description": "take the deepest AMM v4 pool out of service",
  "tags": ["raydium"],
  "overrides": [
    {
      "id": "raydium-v4-halt-0",
      "templateId": "raydium-amm-pool-state",
      "label": "disable the pool",
      "enabled": true,
      "scenarioRelativeSlot": 0,
      "fetchBeforeUse": true,
      "account": { "pubkey": "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2" },
      "values": { "status": 2 }
    }
  ]
}
```

Expected result: `getAccountInfo` on the pool returns 752 bytes whose first `u64` LE is 2
and whose remaining 744 bytes are byte-identical to the live account. A `swap_base_in`
against it then fails with `InvalidStatus`. The surfpool log must contain no
`skipping override` line.

To raise the swap fee instead, use `raydium-amm-fees` with
`{"fees.swap_fee_numerator": 1000, "fees.swap_fee_denominator": 10000}` (10%); only
offsets 176..192 change.

## Verification

- Unit: `cargo test -p surfpool-core --lib raydium` covers every property's label and
  description and the bundle-wide rule that only this IDL declares empty discriminators.
- Engine: `cargo test -p surfpool-core --lib forge` covers undiscriminated types told
  apart by exact fill (trailing bytes refused), same-size types refused as ambiguous, an
  8-byte discriminator winning over an empty one, and a synthetic 752-byte `AmmInfo`
  round-trip where a fee override and a `status = 0` pool each move only their own bytes.
- Live fork (`cargo test -p surfpool-core --features integration-tests raydium_amm --
  --test-threads=1`, needs network; `SURFPOOL_TEST_RPC_URL` overrides the endpoint): three
  live pools round-trip byte-identically, a live `TargetOrders` resolves to its own type,
  and a `status` plus `fees.swap_fee_numerator` override through the materializer changes
  only those bytes.
- Not verified: a real `swap_base_in` against an overridden pool (needs an instruction
  builder with the OpenBook market accounts).
