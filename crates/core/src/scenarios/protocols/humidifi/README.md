# HumidiFi

A proprietary market maker (PMM), not an AMM. Three templates control the fair value a market
quotes around, whether that quote is fresh enough to fill, and the vault inventory it pays from.

HumidiFi publishes no IDL. Every market word it reads is stored XORed with a fixed per-offset key,
so the templates write through raw offsets with `u64_xor` and `slot_xor` encodings; the values you
pass stay plaintext. Program `9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp`, schema-8 market accounts
(1728 bytes, version word 8 at offset 1720).

## Template index

| Template                 | Overrides                                                 |
| ------------------------ | --------------------------------------------------------- |
| `humidifi-price`         | the fair value HumidiFi quotes around                     |
| `humidifi-freshness`     | whether the quote is current, and how old it may get      |
| `humidifi-vault-balance` | how many tokens HumidiFi has available to pay out a swap  |

## Number formats

| You'll see            | It means                                         | Example                                     |
| --------------------- | ------------------------------------------------ | ------------------------------------------- |
| `fair_value`          | quote atoms per base atom × 2^48, decimal string | SOL/USDC at 208 → `"58546795155816"`        |
| `last_update_slot`    | offset from the materialization slot             | `0` = quoted now, `-1000` = stale           |
| `max_staleness_slots` | oldest quote age in slots that still fills       | `2` on SOL/USDC                             |
| vault `amount`        | that mint's smallest unit                        | `1000000` = 1 USDC                          |

Price conversion is:

```text
fair_value = floor(price × 2^48 / 10^(base_decimals - quote_decimals))
```

where `price` is quote tokens per base token. SOL/USDC has 9/6 decimals, so 208 USDC per SOL is
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
`04a0178651b8c3c5`. Surfpool does not unmask them for callers; see below.

## Picking a market

The templates have no default account, and the backend embeds no market catalog. Studio offers a
short list of featured markets and also accepts a market or vault address directly. API callers must
provide the market account in the override; `humidifi-vault-balance` takes one of that market's
vaults instead. The price and freshness templates target the 1728-byte market account, and the
vault template targets one of its SPL-token vaults.

Studio includes four featured markets:

| Choice      | Best suited for                                                       |
| ----------- | --------------------------------------------------------------------- |
| SOL / USDC  | SOL price shocks, stale quotes and liquidity tests                    |
| HYPE / USDC | Altcoin price and inventory stress tests                              |
| JUP / USDC  | A second altcoin market with classic SPL-token vaults                 |
| PUMP / USDC | Volatile-token price tests; its base vault is a Token-2022 account    |

HumidiFi stores each market's mints and vaults masked, and a search by mint needs those masked
bytes, so start from a market address. When only a pair is named, each template's LLM guidance uses
the market Studio lists for it or asks for an address, and it asks for the pair when only an address
is given. Every vault is a token account owned by its market, so `getTokenAccountsByOwner` with the
market and the paying side's mint returns it without unmasking. Some markets also own a dust account
for their quote mint; the guidance keeps the account holding the most tokens and asks for the vault
when every one is nearly empty.

Before it prepares a price or freshness override, the guidance reads the market's token accounts
for both of the pair's mints and says the market looks stopped when even the largest is nearly
empty.

Use `fetchBeforeUse: true` so the selected account is loaded into the local VM from the upstream
datasource before its bytes are changed. A later override that builds on an earlier one in the same
scenario uses `false`, or the re-fetch undoes the earlier write.

## Two rules that prevent misleading scenarios

**1. Keep the quote fresh while testing price.** HumidiFi rejects a quote older than the market's
staleness limit before pricing, and the limits stored in market accounts are only a few slots.
Nothing in the local VM republishes the quote, so apply `humidifi-freshness` with `last_update_slot: 0` and
`max_staleness_slots: 200` in the same slot as the price, which keeps the quote current for 200
slots.

**2. Do not repeatedly reset transaction-owned inventory.** Price and freshness are configuration
inputs. A vault balance is state that swaps modify. Reapplying a vault override after every swap can
undo the swap and manufacture or erase inventory.

## Scenario ideas

### SOL price shock

1. Choose **SOL / USDC** in `humidifi-price`.
2. Set `fair_value` from the formula, for example `"58546795155816"` for 208 USDC per SOL.
3. Add `humidifi-freshness` on the same market with `last_update_slot: 0` and
   `max_staleness_slots: 200`.
4. Compare a swap before and after. Keep a run with the original price as a control.

### Maker stops quoting

1. Choose a market in `humidifi-freshness`.
2. Set `last_update_slot` to `-1000`, older than every market's limit.
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
fair_value: "58546795155816"      # SOL/USDC at 208 with 9/6 decimals
```

Raising `fair_value` makes the base token more expensive, so a quote-to-base swap returns
proportionally less base. A buy fills within 1% of the price-implied output at the stored and at
a doubled fair value. Pass the value as a string; it can
exceed what a JSON number holds exactly.

## Keep the quote current

```text
template: humidifi-freshness
last_update_slot: 0
max_staleness_slots: 200
```

The offset is relative even though the account stores an XOR-masked absolute slot. The limit of
200 emulates a current maker for 200 slots; without it the quote expires within the stored limit of a
few slots. For a longer scenario, schedule it again with `fetchBeforeUse: false`.

## Make the quote stale

```text
template: humidifi-freshness
last_update_slot: -1000
```

HumidiFi fills while `clock_slot - last_update_slot <= max_staleness_slots` and rejects with
`Custom(0xfaded)` (1027565) one slot later. To test that exact boundary for a swap in the slot the
override materializes, set `max_staleness_slots` in the same override and `last_update_slot` to
`-(max_staleness_slots + 1)`; one slot younger still fills. The boundary holds at the stored limit and
at a rewritten limit.

## Drain inventory

```text
template: humidifi-vault-balance
account: <vault from getTokenAccountsByOwner>
amount:  0
```

The base vault pays quote-to-base swaps, the quote vault pays base-to-quote swaps. Lowering the
input-side vault leaves that direction unchanged. A thin payout vault still settles but quotes much
less, because HumidiFi prices against its inventory: at 0.5% of the stored base inventory a buy
returns roughly a thousandth of its output. This is not an AMM reserve formula; use
`humidifi-price` to move the mid.

# Troubleshooting

| Symptom                                                   | Fix                                                                                  |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Price override writes correctly but the swap rejects      | Apply `humidifi-freshness` with `last_update_slot: 0` in the swap's window          |
| `Custom(1027565)` (`0xfaded`)                             | The quote is older than `max_staleness_slots`; refresh it or schedule freshness again |
| `Custom(49)` after lowering a vault                       | The payout vault is empty; HumidiFi refuses the swap itself                          |
| Token program `Custom(1)` (InsufficientFunds)             | The payout vault is empty on a market that leaves the refusal to the token transfer  |
| Output collapses after lowering a vault                   | The payout vault is thin; HumidiFi prices against inventory                          |
| Price is off by a power of ten                            | Use both tokens' decimals from `get_token_address` in the formula                    |
| A vault override has no effect                            | It targets a dust account; use the market-owned account holding the most tokens      |
| A vault balance returns after a swap                      | Remove the later vault reset; it is undoing transaction-owned state                  |
