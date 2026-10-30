# Tessera V

A proprietary market maker (PMM), not an AMM. Four templates control its price, ladder depth,
ladder curve and a full halt. Surfpool keeps the quote current on its own once a scenario overrides a
market.

Tessera does not derive its price from vault ratios. Its maker writes one price per swap direction
into the market account, and two directional ladders of twenty levels decide how much fills at
which output factor. It publishes no IDL, so every template writes the market account through a
raw byte layout.

Program: `TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH`.

## Template index

| Template            | Overrides                                              |
| ------------------- | ------------------------------------------------------ |
| `tessera-price`     | the price Tessera uses for each swap direction         |
| `tessera-depth`     | how much each ladder level fills before the next one   |
| `tessera-curve`     | the output factor applied at each ladder level         |
| `tessera-halt`      | whether any level quotes at all                        |

## Number formats

| You'll see                          | It means                                          | Example                                          |
| ----------------------------------- | ------------------------------------------------- | ------------------------------------------------ |
| `quote_atoms_per_base_atom_x1e15`   | quote atoms per base atom × 10^15, prices sells   | SOL/USDC $100 → `100000000000000`               |
| `base_atoms_per_quote_atom_x1e15`   | floor(10^30 / the value above), prices buys       | SOL/USDC $100 → `10000000000000000`             |
| `*_level_N_amount`                  | capacity in atoms of the swap's input token       | sell side of SOL/USDC: `1000000000` = 1 SOL     |
| `*_level_N_factor`                  | output factor in millionths, below `1000000`      | `990000` pays 1% less than the bare price        |
| `*_levels_enabled`                  | enabled flag of all twenty levels on one side     | `0` halts that side                              |

Price conversion, with both decimals taken from the two mints:

```text
quote_atoms_per_base_atom_x1e15 = price × 10^(quote_decimals - base_decimals) × 10^15
base_atoms_per_quote_atom_x1e15 = floor(10^30 / quote_atoms_per_base_atom_x1e15)
```

For a fill that starts and ends in the first level:

```text
sell output = floor(base_in  × quote_atoms_per_base_atom_x1e15 × sell_level_0_factor / 10^21)
buy output  = floor(quote_in × base_atoms_per_quote_atom_x1e15 × buy_level_0_factor  / 10^21)
```

The formulas hold for a quote younger than its grace period, which keep-alive keeps it (see
[How a quote ages](#how-a-quote-ages)), and with neutral quote-start configs. The five quote-start configs at offset 1136 can lower a fill or make it skip leading levels. Which
one applies depends on how many slots ago the config account
`BAT1Ndpu5gbLTp2AZkSXP79LJBZfCH4B3zGhi6LtvdhK` was updated. A surfnet never updates it, so after
about 300 slots the last config applies. Configs that each hold `0`, `1000000` and `0` change
nothing; read them before relying on the formula.

### Layout

The market account is 1264 bytes. Offsets not listed are not written by any template.

| Offset    | Field                                  | Encoding          | Unit                     |
| --------- | -------------------------------------- | ----------------- | ------------------------ |
| 0         | buy depth used since the publication   | u64, keep-alive   | quote atoms              |
| 8         | sell depth used since the publication  | u64, keep-alive   | base atoms               |
| 24        | base mint                              | pubkey, read-only | -                        |
| 56        | quote mint                             | pubkey, read-only | -                        |
| 88        | freshness limit                        | u64, read-only    | slots                    |
| 96        | grace period                           | u64, read-only    | slots                    |
| 120       | last update slot                       | u64, keep-alive   | slot                     |
| 128       | quote atoms per base atom              | u128              | × 10^15                  |
| 144       | base atoms per quote atom              | u128              | × 10^15                  |
| 160 + 24n | sell level n capacity                  | u64               | base atoms               |
| 168 + 24n | sell level n output factor             | u64               | millionths, below 10^6   |
| 176 + 24n | sell level n enabled                   | `u8_strided`      | 0 or 1                   |
| 640 + 24n | buy level n capacity                   | u64               | quote atoms              |
| 648 + 24n | buy level n output factor              | u64               | millionths, below 10^6   |
| 656 + 24n | buy level n enabled                    | `u8_strided`      | 0 or 1                   |
| 1136 + 12n | quote-start config n (n = 0..4)       | 12 bytes, read-only | -                      |

`n` runs from 0 to 19. The sell ladder and offset 128 serve base-to-quote swaps; the buy ladder and
offset 144 serve quote-to-base swaps.

## Picking a market

The templates have no default account, and the backend embeds no market catalog. Studio offers a
short list of featured markets and also accepts a market address directly; API and MCP callers must
provide the market account in the override. Every template writes the market account itself.

Studio includes four featured markets:

| Choice       | Best suited for                                       |
| ------------ | ----------------------------------------------------- |
| SOL / USDC   | SOL price shocks, halts and depth tests               |
| cbBTC / USDC | a major with an 8-decimal base token                  |
| JLP / SOL    | a quote token that is not a stablecoin                |
| PUMP / USDC  | a long-tail token whose mint is a Token-2022 account  |

For any other pair, each template's LLM guidance finds the market on the program: one
`getProgramAccounts` call filters the program's accounts by the market size, 1264 bytes, and the two
mints at 24 and 56. A market without a price in both directions has every level disabled and
cannot fill, so the guidance says so instead of preparing it.

Use `fetchBeforeUse: true` so the selected market is loaded into the local VM from the upstream
datasource before its bytes are changed. A later override that builds on an earlier one in the same
scenario uses `false`, or the re-fetch undoes the earlier write.

## Keep-alive (Surfpool handles it)

On mainnet the maker republishes each market's quote every slot or two. Each publication writes the
slot (offset 120) and clears the depth swaps have used since the last one (offsets 0 and 8). Nobody
runs the maker against a surfnet, so Surfpool republishes the quote every slot
([keep-alive](../../README.md#keep-alive)): the quote stays 0 slots old, and every slot starts with
the full ladder. Swaps in one slot still use up the ladder as on mainnet; the next slot gives it back.
There is nothing to schedule.

Every template keeps the market it writes quoting. Keep-alive starts in the slot the override plays
and runs until the network is reset.

## A rule that prevents misleading scenarios

**Change a ladder as a whole.** The stored ladder has a different number of enabled levels on
each market and side, and the maker can pull a side at any moment. Read the selected market account
first, scale every enabled level on the side you stress by one ratio, and keep factors descending.

## How a quote ages

Keep-alive keeps a market a scenario overrides 0 slots old. A market no override names is not
republished, and ages like this. A quote younger than the market's grace period (offset 96, 5 slots
on the featured markets) fills at the formula. From then on each slot lowers the fill by a growing
amount, in both directions, until the quote rejects at its freshness limit (offset 88). Measured on
SOL / USDC:

| Slots since the update | Fill        |
| ---------------------- | ----------- |
| 0 to 4                 | the formula |
| 5                      | -50 ppm     |
| 6                      | -200 ppm    |
| 8                      | -800 ppm    |
| 10                     | -0.18%      |
| 12                     | -0.32%      |
| 15 to 19               | -0.5% (cap) |
| 20                     | rejected (`65535`) |

cbBTC / USDC loses half as much per slot.

## Scenario ideas

### SOL price shock

Use this to test whether a router or strategy reacts when Tessera's SOL price moves.

1. Choose **SOL / USDC** in `tessera-price`.
2. Set both price fields from the formula. $100 per SOL is `100000000000000` and
   `10000000000000000`.
3. Compare both swap directions before and after. Doubling the price doubles a sell's output and
   halves a buy's.

### Maker widens one side

Use this to test a maker that still quotes but pays less in one direction.

1. Choose the market in `tessera-curve`.
2. Multiply all twenty sell factors by one ratio to make selling base worse, or the buy factors to
   make buying base worse. The other side is unaffected.
3. Compare equal-size swaps in both directions.

### Thin liquidity for large orders

Use this to test order splitting when Tessera's depth drops.

1. Choose the market in `tessera-depth`.
2. Multiply every enabled capacity on one side by one ratio, for example 10%.
3. Swap a small and a large amount. The small fill is unchanged; the large one gets worse.

### Maker pulls all liquidity

1. Apply `tessera-halt` with both fields `0`.
2. Confirm both directions fail with custom error 65535 and that an alternative venue still fills.

### Price moves between quote and fill

Use this to test a router's slippage limit when Tessera reprices after the router quoted it.

1. Add `tessera-price` at the first slot with the stored price, so keep-alive keeps the market
   quoting.
2. Two slots later, add `tessera-price` 1% lower in both directions.
3. Quote a swap in the first slot and fill it after the move. The fill pays 1% less, and a tight
   slippage limit must reject it.

### Arbitrage against an AMM

Use this to test an arbitrage bot across Tessera and an AMM pool.

1. Read the SOL / USDC price of an AMM pool, for example Orca Whirlpool.
2. Set Tessera's SOL / USDC price 2% above it with `tessera-price`.
3. Buying SOL on the AMM and selling it into Tessera should clear both venues' fees.

# Recipes

## Set the price

```text
template: tessera-price
quote_atoms_per_base_atom_x1e15: "100000000000000"     # SOL/USDC at $100, 9/6 decimals
base_atoms_per_quote_atom_x1e15: "10000000000000000"
```

Set both fields. Offset 128 alone moves only sells and offset 144 alone moves only buys, so a
single-field write creates a one-sided price.

## Reduce depth

```text
template: tessera-depth
sell_level_N_amount: floor(stored sell_level_N_amount × 1000 / 10000)   # every enabled sell level
```

Leave disabled levels and the other side unset. A capacity that rounds to zero disables its level.

## Widen the curve

```text
template: tessera-curve
sell_level_N_factor: floor(stored sell_level_N_factor × 5000 / 10000)   # all twenty sell levels
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

# Checking a scenario with a real swap

Tessera fills swaps only through a router. A direct call must carry the DFlow sentinel
`8xeaWCsJYxRoudEZGJWURdfrtFhLYZz9b4iHJnW5tb3d` as a signer. To send one on a local surfnet:

1. Start surfpool with `--skip-signature-verification`, so the sentinel needs no key.
2. Play the scenario. Keep-alive keeps the quote fresh.
3. Simulate the swap before sending it. On a fresh surfnet the send otherwise fails with
   "Failed to fetch accounts from remote".

The instruction data is tag `0x11`, the direction (`1` sells base), the input amount and the
minimum output as u64, then `0`. The accounts are the global state
`8ekCy2jHHUbW2yeNGFWYJT9Hm9FW7SvZcZK66dSZCDiF`, the market, the taker, the base and quote vaults,
the taker's base and quote token accounts, both mints, both token programs, the sentinel, the config
`BAT1Ndpu5gbLTp2AZkSXP79LJBZfCH4B3zGhi6LtvdhK` and the market tick
`4cG31VNF9TzFinNc7BmnjhFvGjxkY3sCETVMtMgbrhPs`. The vaults are token accounts of the global state,
one per mint, and the USDC vault is shared by every USDC market.

# Troubleshooting

| Symptom                                               | Fix                                                                                   |
| ----------------------------------------------------- | ------------------------------------------------------------------------------------- |
| `Custom(65535)` on a market you did not override      | Expected: nothing republishes a market no override names, so it went stale            |
| `Custom(65535)` on a market you did not halt          | The maker pulled that side upstream; every level on it is disabled                    |
| `Custom(8)`                                           | A curve factor is not lower than the level before it; scale all factors by one ratio  |
| `Custom(7)`                                           | A curve factor is `1000000` or more; factors stay below it                            |
| A second swap in one slot fills worse than the first  | Not a bug: swaps use up the ladder; the next slot's republication gives it back       |
| Only one direction repriced                           | Set both price fields; 128 prices sells and 144 prices buys                           |
| A depth change has no visible effect                  | The fill was smaller than the first level; use a larger swap                          |
| A price change lands on the wrong account          | No owner or size check exists on raw writes; pick a featured market or check that a typed address is a Tessera market account |
| `buy_levels_enabled: 1` re-enabled maker-disabled levels | Only 0 is supported; 1 makes the program quote levels the maker had pulled                                   |
