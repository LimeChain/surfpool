# Tessera V

A proprietary market maker (PMM), not an AMM. Five templates control its price, quote freshness,
ladder depth, ladder curve and a full halt.

Tessera does not derive its price from vault ratios. Its maker writes one price per swap direction
into the market account, and two directional ladders of twenty levels decide how much fills at
which output factor. It publishes no IDL, so every template writes the market account through a
raw byte layout.

Deployment: program `TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH`, last deployed at slot
446053401; verified 2026-09-25.

## Template index

| Template            | Overrides                                              |
| ------------------- | ------------------------------------------------------ |
| `tessera-price`     | the price Tessera uses for each swap direction         |
| `tessera-freshness` | whether Tessera's quote is current or stale            |
| `tessera-depth`     | how much each ladder level fills before the next one   |
| `tessera-curve`     | the output factor applied at each ladder level         |
| `tessera-halt`      | whether any level quotes at all                        |

## Number formats

| You'll see                          | It means                                          | Example                                          |
| ----------------------------------- | ------------------------------------------------- | ------------------------------------------------ |
| `quote_atoms_per_base_atom_x1e15`   | quote atoms per base atom × 10^15, prices sells   | SOL/USDC $100 → `100000000000000`               |
| `base_atoms_per_quote_atom_x1e15`   | floor(10^30 / the value above), prices buys       | SOL/USDC $100 → `10000000000000000`             |
| `*_level_N_amount`                  | capacity in atoms of the swap's input token       | sell side of SOL/USDC: `1000000000` = 1 SOL     |
| `*_level_N_factor`                  | output factor, `1000000` is neutral               | `990000` pays 1% less                            |
| `*_levels_enabled`                  | enabled flag of all twenty levels on one side     | `0` halts that side                              |
| `last_update_slot`                  | offset from the materialization slot              | `0` = updated now, `-20` = twenty slots old      |

Price conversion, with both decimals taken from the selected market's metadata:

```text
quote_atoms_per_base_atom_x1e15 = price × 10^(quote_decimals - base_decimals) × 10^15
base_atoms_per_quote_atom_x1e15 = floor(10^30 / quote_atoms_per_base_atom_x1e15)
```

For a fill that starts and ends in the first level:

```text
sell output = floor(base_in  × quote_atoms_per_base_atom_x1e15 × sell_level_0_factor / 10^21)
buy output  = floor(quote_in × base_atoms_per_quote_atom_x1e15 × buy_level_0_factor  / 10^21)
```

### Layout

The market account is 1264 bytes. Offsets not listed are not written by any template.

| Offset    | Field                                  | Encoding          | Unit                     |
| --------- | -------------------------------------- | ----------------- | ------------------------ |
| 24        | base mint                              | pubkey, read-only | -                        |
| 56        | quote mint                             | pubkey, read-only | -                        |
| 88        | freshness limit                        | u64, read-only    | slots                    |
| 120       | last update slot                       | `slot`            | slot offset              |
| 128       | quote atoms per base atom              | u64               | × 10^15                  |
| 144       | base atoms per quote atom              | u64               | × 10^15                  |
| 160 + 24n | sell level n capacity                  | u64               | base atoms               |
| 168 + 24n | sell level n output factor             | u64               | 10^6 = neutral           |
| 176 + 24n | sell level n enabled                   | `u8_strided`      | 0 or 1                   |
| 640 + 24n | buy level n capacity                   | u64               | quote atoms              |
| 648 + 24n | buy level n output factor              | u64               | 10^6 = neutral           |
| 656 + 24n | buy level n enabled                    | `u8_strided`      | 0 or 1                   |

`n` runs from 0 to 19. The sell ladder and offset 128 serve base-to-quote swaps; the buy ladder and
offset 144 serve quote-to-base swaps. Five 12-byte quote-start configs at 1136, which can make a
quote skip leading levels, and two consumed-depth words at 0 and 8 are not exposed.

## Picking a market

Every template starts with a market picker. Its options are read from the program when Studio or
MCP loads the templates: every 1264-byte account with market tag `5` at offset 96 whose quote was
updated within the last 5000 slots, through the surfnet (local state first, then mainnet) and cached
for 60 seconds. Markets the maker stopped quoting drop out. On 2026-09-25 that included:

| Choice                         | Best suited for                                       |
| ------------------------------ | ----------------------------------------------------- |
| SOL / USDC, SOL / USDT         | SOL price shocks and stablecoin-quote comparisons     |
| cbBTC / USDC, ETH / USDC       | majors with 8-decimal base tokens                     |
| JLP / SOL                      | a quote token that is not a stablecoin                |
| JTO, HYPE, RAY, PUMP, USELESS  | long-tail tokens quoted in USDC; PUMP is Token-2022   |

The picker supplies the market account to the template. SOL / USDC is listed first and is the
default when no market is chosen. Each option's metadata carries the pair, both mints, both decimals and the
market's `freshness_limit_slots`, which the price and freshness formulas need. A BONK / USDC market
cannot be priced through `tessera-price`: its base-per-quote price exceeds the u64 field.

Use `fetchBeforeUse: true` so the selected market is forked before its bytes are changed.

## Two rules that prevent misleading scenarios

**1. Keep the quote fresh.** Tessera rejects a quote whose last update is at least the market's
freshness limit old with custom error 65535, so a correct price or depth change can appear to do
nothing. Apply
`tessera-freshness` with `last_update_slot: 0` alongside every price, depth or curve change, and
schedule it again in each later slot where a quote is needed.

**2. Change a ladder as a whole.** The live ladder has a different number of enabled levels on each
market and side, and the maker can pull a side at any moment. Read the live account first, scale
every enabled level on the side you stress by one ratio, and keep factors descending.

## Scenario ideas

### SOL price shock

Use this to test whether a router or strategy reacts when Tessera's SOL price moves.

1. Choose **SOL / USDC** in `tessera-price`.
2. Set both price fields from the formula. $100 per SOL is `100000000000000` and
   `10000000000000000`.
3. Add `tessera-freshness` with `last_update_slot: 0`.
4. Compare both swap directions before and after. Doubling the price doubles a sell's output and
   halves a buy's.

### Maker widens one side

Use this to test a maker that still quotes but pays less in one direction.

1. Choose the market in `tessera-curve`.
2. Multiply all twenty sell factors by one ratio to make selling base worse, or the buy factors to
   make buying base worse. The other side is unaffected.
3. Keep the quote fresh and compare equal-size swaps in both directions.

### Thin liquidity for large orders

Use this to test order splitting when Tessera's depth drops.

1. Choose the market in `tessera-depth`.
2. Multiply every enabled capacity on one side by one ratio, for example 10%.
3. Swap a small and a large amount. The small fill is unchanged; the large one gets worse.

### Stale maker

Use this to test fallback routing when Tessera stops updating.

1. Choose the market in `tessera-freshness`.
2. Set `last_update_slot` to minus the market's `freshness_limit_slots`, for example `-20` on
   SOL / USDC.
3. Confirm both directions fail with custom error 65535 and that one slot younger still fills.

### Maker pulls all liquidity

1. Apply `tessera-halt` with both fields `0`.
2. Confirm both directions fail with custom error 65535 and that an alternative venue still fills.

# Recipes

## Set the price

```text
template: tessera-price
quote_atoms_per_base_atom_x1e15: "100000000000000"     # SOL/USDC at $100, 9/6 decimals
base_atoms_per_quote_atom_x1e15: "10000000000000000"
```

Set both fields. Offset 128 alone moves only sells and offset 144 alone moves only buys, so a
single-field write creates a one-sided price.

## Keep the quote current

```text
template: tessera-freshness
last_update_slot: 0
```

A fresh quote lasts `freshness_limit_slots` slots. Schedule the template again in each later slot
where a quote is needed.

## Make the quote stale

```text
template: tessera-freshness
last_update_slot: -20      # SOL/USDC freshness_limit_slots is 20
```

The boundary is exclusive: `-19` still fills, `-20` rejects. Do not schedule a fresh override after it.

## Reduce depth

```text
template: tessera-depth
sell_level_N_amount: floor(live sell_level_N_amount × 1000 / 10000)   # every enabled sell level
```

Leave disabled levels and the other side unset. A capacity that rounds to zero disables its level.

## Widen the curve

```text
template: tessera-curve
sell_level_N_factor: floor(live sell_level_N_factor × 5000 / 10000)   # all twenty sell levels
```

Scaling every factor by one ratio keeps them descending unless two close neighbours round to the same value, so check the result stays strictly descending. A factor equal to or larger than the level
before it rejects with custom error 8.

## Halt the market

```text
template: tessera-halt
sell_levels_enabled: 0
buy_levels_enabled:  0
```

Only `0` is supported. Clearing only the first level does not halt a quote that starts at a later
level.

# Troubleshooting

| Symptom                                               | Fix                                                                                   |
| ----------------------------------------------------- | ------------------------------------------------------------------------------------- |
| `Custom(65535)` after a price, depth or curve change  | The quote is stale. Add `tessera-freshness` with `last_update_slot: 0` in that slot   |
| `Custom(65535)` on a market you did not halt          | The maker pulled that side live; every level on it is disabled                        |
| `Custom(8)`                                           | A curve factor is not lower than the level before it; scale all factors by one ratio  |
| Only one direction repriced                           | Set both price fields; 128 prices sells and 144 prices buys                           |
| A depth change has no visible effect                  | The fill was smaller than the first level; use a larger swap                          |
| A price change lands on the wrong account          | No owner or size check exists on raw writes; pick the market from the picker, which only offers Tessera market accounts |
| `buy_levels_enabled: 1` re-enabled maker-disabled levels | Only 0 is supported; 1 makes the program quote levels the maker had pulled                                   |
| Price field rejected as out of range                  | The reciprocal exceeds u64; the price is too small for that market's decimals         |
| A stale override stops rejecting                      | A later `tessera-freshness` with a non-negative offset refreshed it; remove it        |
