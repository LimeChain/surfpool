# HumidiFi

A proprietary market maker (PMM), not an AMM. Three templates control the fair value a market
quotes around, whether that quote is fresh enough to fill, and the vault inventory it pays from.

HumidiFi publishes no IDL. Every market word it reads is stored XORed with a fixed per-offset key,
so the templates write through raw offsets with `u64_xor` and `slot_xor` encodings; the values you
pass stay plaintext. Program `9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp`, schema-8 market accounts
(1728 bytes, version word 8 at offset 1720).

Deployment: program data last deployed at slot 449592669 (verified 2026-09-25).

## Template index

| Template                 | Overrides                                                 |
| ------------------------ | --------------------------------------------------------- |
| `humidifi-price`         | the fair value HumidiFi quotes around                     |
| `humidifi-freshness`     | whether the quote is current, and how old it may get      |
| `humidifi-vault-balance` | how many tokens HumidiFi has available to pay out a swap  |

## Number formats

| You'll see            | It means                                         | Example                                     |
| --------------------- | ------------------------------------------------ | ------------------------------------------- |
| `fair_value`          | quote atoms per base atom × 2^48, decimal string | WSOL/USDC at 208 → `"58546795155816"`       |
| `last_update_slot`    | offset from the materialization slot             | `0` = quoted now, `-3` = three slots old    |
| `max_staleness_slots` | oldest quote age in slots that still fills       | `2` on WSOL/USDC                            |
| vault `amount`        | that mint's smallest unit                        | `1000000` = 1 USDC                          |

Price conversion is:

```text
fair_value = floor(price × 2^48 / 10^(base_decimals - quote_decimals))
```

where `price` is quote tokens per base token. WSOL/USDC has 9/6 decimals, so 208 USDC per SOL is
`floor(208 × 2^48 / 10^3) = 58546795155816`.

### Layout

| Offset | Encoding                             | Field               | Unit                             | Template                 |
| ------ | ------------------------------------ | ------------------- | -------------------------------- | ------------------------ |
| 8      | stored bytes                         | layout tag          | `[44,90,19,124,56,111,47,150]`   | read-only                |
| 384    | pubkey, four masked words            | quote mint          | n/a                              | read-only                |
| 416    | pubkey, four masked words            | base mint           | n/a                              | read-only                |
| 448    | pubkey, four masked words            | quote vault         | n/a                              | read-only                |
| 480    | pubkey, four masked words            | base vault          | n/a                              | read-only                |
| 576    | `u64_xor` `0xb957ed15dc877426`       | fair value          | quote atoms per base atom × 2^48 | `humidifi-price`         |
| 608    | `u64_xor` `0x6e9de2b30b19f1ea`       | max staleness       | slots                            | `humidifi-freshness`     |
| 616    | `slot_xor` `0x6e9de2b30b19f1ea`      | last update slot    | slot, written from an offset     | `humidifi-freshness`     |
| 1720   | `u64`, plaintext                     | schema version      | version                          | read-only                |
| 64     | `u64` (SPL token account)            | vault amount        | mint's smallest unit             | `humidifi-vault-balance` |

The four pubkey words use the keys `fb5ce87aae443c38`, `04a2178451bac3c7`, `04a1178751b9c3c6` and
`04a0178651b8c3c5`, which is why the market picker unmasks each market's mints and vaults for you.

## Picking a market

Every template starts with a market picker. Its options are not a hardcoded list: when templates are
served to Studio or MCP, Surfpool reads the program's schema-8 markets through the surfnet RPC
(`getProgramAccounts` on 1728-byte accounts carrying the layout tag at offset 8 and version word 8
at offset 1720), unmasks each market's mints, vaults, staleness limit and last quote slot with the
keys above, and keeps only markets that quoted within the last 5000 slots. Options are labelled by
pair, sorted by label with SOL / USDC first, and cached for 60 seconds. A template with no account
chosen targets the first option. If the read fails the picker is empty.

Each market option's value is the market account. Its metadata carries `pair`, `base_mint`,
`quote_mint`, `base_decimals`, `quote_decimals`, `base_vault`, `quote_vault`,
`max_staleness_slots` and `last_update_slot`, so the price formula and a stale offset can be
computed without reading the account. `humidifi-vault-balance` lists two options per market,
`<pair> base vault` and `<pair> quote vault`, whose value is the vault, with the market in
`account` and `side` set to `base` or `quote`.

A market that quoted within 5000 slots is not necessarily fillable now: the staleness limit is 2 to
15 slots, so apply `humidifi-freshness` as described below.

Use `fetchBeforeUse: true` so the selected account is forked before its bytes are changed.

## Two rules that prevent misleading scenarios

**1. Keep the quote fresh while testing price.** HumidiFi rejects a quote older than the market's
staleness limit before pricing, and the limit is only 2 to 15 slots. Nothing on a fork republishes
the quote, so apply `humidifi-freshness` with `last_update_slot: 0` in the same slot as the price,
and schedule it again in each later slot where a swap must fill.

**2. Do not repeatedly reset transaction-owned inventory.** Price and freshness are configuration
inputs. A vault balance is state that swaps modify. Reapplying a vault override after every swap can
undo the swap and manufacture or erase inventory.

## Scenario ideas

### SOL price shock

1. Choose **SOL / USDC** in `humidifi-price`.
2. Set `fair_value` from the formula, for example `"58546795155816"` for 208 USDC per SOL.
3. Add `humidifi-freshness` on the same market with `last_update_slot: 0`.
4. Compare a swap before and after. Keep a run with the original price as a control.

### Maker stops quoting

1. Choose a market in `humidifi-freshness`.
2. Set `last_update_slot` to `-(max_staleness_slots + 1)` from the market metadata, `-3` on
   SOL/USDC.
3. Every swap on that market reverts with `Custom(0xfaded)`. Confirm a router falls back to another
   venue.

### Maker cannot pay one side

1. In `humidifi-vault-balance`, choose the base vault to block users buying the base token, or the
   quote vault to block users selling it.
2. Lower the balance to `0`, or to a thin amount to watch the quote shrink first.
3. Apply it once, then confirm the affected direction fails and the opposite direction still fills.

# Recipes

## Set the fair value

```text
template: humidifi-price
fair_value: "58546795155816"      # WSOL/USDC at 208 with 9/6 decimals
```

Raising `fair_value` makes the base token more expensive, so a quote-to-base swap returns
proportionally less base. At 100 and 208 USDC per SOL the deployed program fills within 1% of the
price-implied output on both WSOL/USDC fixtures. Pass the value as a string; it can exceed what a
JSON number holds exactly.

## Keep the quote current

```text
template: humidifi-freshness
last_update_slot: 0
```

The offset is relative even though the account stores an XOR-masked absolute slot. One application
covers `max_staleness_slots` slots; schedule it again for a longer scenario. Leave
`max_staleness_slots` unset unless the scenario models a more tolerant or stricter venue.

## Make the quote stale

```text
template: humidifi-freshness
last_update_slot: -3              # WSOL/USDC, limit 2
```

HumidiFi fills while `clock_slot - last_update_slot <= max_staleness_slots` and rejects with
`Custom(0xfaded)` (1027565) one slot later. The boundary holds at the live limit and at a rewritten
limit.

## Drain inventory

```text
template: humidifi-vault-balance
account: <vault option>
amount:  0
```

The base vault pays quote-to-base swaps, the quote vault pays base-to-quote swaps. Lowering the
input-side vault leaves that direction unchanged. A thin payout vault still settles but quotes much
less, because HumidiFi prices against its inventory. This is not an AMM reserve formula; use
`humidifi-price` to move the mid.

# Troubleshooting

| Symptom                                                   | Fix                                                                                  |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Price override writes correctly but the swap rejects      | Apply `humidifi-freshness` with `last_update_slot: 0` in the swap's window          |
| `Custom(1027565)` (`0xfaded`)                             | The quote is older than `max_staleness_slots`; refresh it or schedule freshness again |
| `Custom(49)` after lowering a vault                       | The payout vault is empty; HumidiFi refuses the swap itself                          |
| Token program `Custom(1)` (InsufficientFunds)             | The payout vault is empty on a market that leaves the refusal to the token transfer  |
| Output collapses after lowering a vault                   | The payout vault is thin; HumidiFi prices against inventory                          |
| Price is off by a power of ten                            | Use `base_decimals - quote_decimals` from the market metadata in the formula        |
| A vault balance returns after a swap                      | Remove the later vault reset; it is undoing transaction-owned state                  |
