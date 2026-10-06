# GoonFi V2

A proprietary market maker (PMM), not an AMM. Four templates control its external quote, quote
freshness, the reference band that guards the quote, and vault inventory.

GoonFi does not derive its price from vault ratios. Each market reads a 32-byte oracle owned by a
companion publisher program, checks that quote against a reference band stored in the market, and
settles from its two token vaults.

Deployment: market program `goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE`, oracle publisher
`dijkbkCAKfFTCxQg3u1pg82gVU1jJGHBBRcteD11mBu`; layout verified against the deployment at slot
451334772.

## Template index

| Template                | Overrides                                          |
| ----------------------- | -------------------------------------------------- |
| `goonfi-price`          | the bid and ask GoonFi quotes                      |
| `goonfi-freshness`      | whether GoonFi's quote is current or stale         |
| `goonfi-reference-band` | the band that must contain the quote for a fill    |
| `goonfi-vault-balance`  | how many tokens GoonFi has available for swaps     |

## Number formats

| You'll see                   | It means                                        | Example                          |
| ---------------------------- | ----------------------------------------------- | -------------------------------- |
| bid, ask, reference prices   | quote per base × 10^6, whatever the mint decimals | SOL at 99.74 USDC → `99740000` |
| `last_update_slot`           | offset from the materialization slot            | `0` = this slot, `-2000` = stale |
| vault `amount`               | that mint's smallest unit                       | `1000000` = 1 USDC               |

Price conversion is:

```text
price_x1e6 = human_price × 10^6
```

### Layout

| Account     | Owner            | Size | Offset | Field                    | Encoding | Written by              |
| ----------- | ---------------- | ---- | ------ | ------------------------ | -------- | ----------------------- |
| MarketState | market program   | 2048 | 0      | tag `30bc2f353458329a`   | bytes    | read only               |
| MarketState | market program   | 2048 | 40     | activity slot            | u64      | read only               |
| MarketState | market program   | 2048 | 80     | base mint                | pubkey   | read only               |
| MarketState | market program   | 2048 | 112    | quote mint               | pubkey   | read only               |
| MarketState | market program   | 2048 | 144    | base vault               | pubkey   | read only               |
| MarketState | market program   | 2048 | 176    | quote vault              | pubkey   | read only               |
| MarketState | market program   | 2048 | 208    | oracle                   | pubkey   | read only               |
| MarketState | market program   | 2048 | 1712   | `reference_price_a_x1e6` | u64      | `goonfi-reference-band` |
| MarketState | market program   | 2048 | 1720   | `reference_price_b_x1e6` | u64      | `goonfi-reference-band` |
| PriceOracle | oracle publisher | 32   | 0      | `bid_price_x1e6`         | u64      | `goonfi-price`          |
| PriceOracle | oracle publisher | 32   | 8      | `ask_price_x1e6`         | u64      | `goonfi-price`          |
| PriceOracle | oracle publisher | 32   | 16     | `last_update_slot`       | slot32   | `goonfi-freshness`      |
| PriceOracle | oracle publisher | 32   | 20     | decay multiplier         | u32      | read only               |
| PriceOracle | oracle publisher | 32   | 24     | publish timestamp (ms)   | u64      | read only, not consulted |
| TokenAccount | token program   | 165  | 64     | `amount`                 | u64      | `goonfi-vault-balance`  |

## Picking a market

The templates have no default account, and the backend embeds no market catalog. Studio offers a
short list of useful markets and also accepts a market, oracle or vault address directly; API and
MCP callers put the account in the override. The band template targets the market account, the
price and freshness templates target the oracle the market points at (bytes 208..240), and the
vault template targets one of the market's SPL-token vaults (base at 144..176, quote at 176..208).

Studio highlights four markets:

| Choice      | Best suited for                                                       |
| ----------- | --------------------------------------------------------------------- |
| SOL / USDC  | SOL price shocks, band moves and liquidity tests                      |
| HYPE / USDC | Altcoin price and inventory stress tests                              |
| JUP / USDC  | A second altcoin market with deep classic SPL-token vaults            |
| PUMP / USDC | Volatile-token price tests; its base vault is a Token-2022 account    |

For any other pair, each template's LLM guidance finds the market on the program: two calls filter
its accounts by the two mints and return each market with its oracle and base vault. A pair can
have several markets; the guidance then keeps the one whose base vault holds the most tokens. One
that stopped trading usually has near-empty vaults, a zero reference band and an oracle millions of
slots old.

The deployed-program tests replay the first three featured markets whose classic SPL-token vaults
each hold at least ten times the tests' 100-quote-token trade, with SOL / USDC first, and check
every template's writes on every market the program owns.

Do not reuse an oracle or vault merely because the token pair looks similar. Use
`fetchBeforeUse: true` so the selected account is pulled from upstream before its bytes are changed.

## Two rules that prevent misleading scenarios

**1. Move the band with the price.** A price can be encoded correctly and still never fill. GoonFi
rejects a sell whose bid sits above the reference band, and a buy whose ask sits below it, with
error `0x24`. Every `goonfi-price` move needs `goonfi-reference-band` on the paired market, scaled
by the same factor.

**2. Do not repeatedly reset transaction-owned inventory.** Price, band and freshness are
configuration inputs. A vault balance is state that swaps modify. Reapplying a vault override
after every swap can undo the swap and manufacture or erase inventory.

## Scenario ideas

### SOL reprices by 50%

This is the scenario the deployed-program tests replay through the production materializer.

1. On **SOL / USDC**, use `goonfi-price` with bid and ask at 1.5 times the live bid.
2. Use `goonfi-reference-band` with both anchors at the same value.
3. Use `goonfi-freshness` with `last_update_slot: 0` so the new quote is undecayed.
4. Use `goonfi-vault-balance` on the **USDC vault** with half the expected sell output. Apply this
   transaction-owned balance once before the swaps being tested.

A sale of about 100 USDC worth of SOL fills at the new price with the original inventory, the same
sale fails with error `0x1` against the capped USDC vault, and a buy still fills from the SOL vault.

### Stablecoin depeg

1. Choose **USDT / USDC** in `goonfi-price` and `goonfi-reference-band`.
2. Set bid, ask and both anchors to `950000` for $0.95 or `1050000` for $1.05.
3. Keep the quote current with `goonfi-freshness`.
4. Compare both swap directions against an unchanged control run.

### The maker refuses one direction

1. Choose a market in `goonfi-price` and leave the band untouched.
2. Raise the bid above the band to block sells, or lower the ask below it to block buys.
3. Confirm the blocked side fails with `0x24` while the other side still fills.

### The maker's quote goes stale

1. Choose a market in `goonfi-freshness`.
2. Set `last_update_slot` to `-2000`. Both swap directions reject with `0x15`.
3. To recover, schedule `goonfi-freshness` with `0` at a later slot.

### GoonFi cannot fill one side of a swap

1. In `goonfi-vault-balance`, choose the quote vault to block sells, or the base vault to block
   buys.
2. Lower the balance below the requested output.
3. Confirm the affected swap fails with `0x1` and the opposite direction still works.

# Recipes

## Set the PMM price

```text
template: goonfi-price
bid_price_x1e6: 150000000      # SOL at $150
ask_price_x1e6: 150000000

template: goonfi-reference-band
reference_price_a_x1e6: 150000000
reference_price_b_x1e6: 150000000
```

Set all four fields. On each market the tests replay, a coupled 2x move doubles a sell's output
and a coupled halving doubles a buy's output, within 1%. Keep ask at or above bid.

## Keep the quote current

```text
template: goonfi-freshness
last_update_slot: 0
```

The stored value is an absolute 4-byte slot; the input is an offset from the materialization slot.
The stamp is written once. A quote decays with age inside a per-market window and then rejects
with `0x15`; volatile pairs reject after roughly 16 to 21 slots, stablecoin pairs after hundreds.
For a longer scenario, schedule this template again in each later slot where a quote is needed.

## Make the quote stale

```text
template: goonfi-freshness
last_update_slot: -2000
```

`-2000` is past the window of every market the tests replay. A smaller negative offset prepares a
decayed but still fillable quote.

## Reduce inventory or make a direction unfillable

```text
template: goonfi-vault-balance
account: <actual market vault>
amount:  <smaller raw token amount>
```

The base vault pays buys, the quote vault pays sells. A quote vault holding exactly a sell's output
still fills; one unit less rejects with `0x1`. Buys also price base inventory, so lowering the
base vault moves a buy's output slightly before it blocks it.

# Troubleshooting

| Symptom                                                   | Fix                                                                  |
| --------------------------------------------------------- | -------------------------------------------------------------------- |
| `Custom(36)` (`0x24`) after a price change                 | Move `goonfi-reference-band` by the same factor as the price         |
| `Custom(21)` (`0x15`)                                      | The quote is past its window; apply `goonfi-freshness` with `0`      |
| `Custom(38)` (`0x26`) on an ageing quote                   | Observed when a decayed fill falls about 5% below fresh; refresh it  |
| `Custom(1)` (`0x1`) after lowering a vault                 | The payout vault cannot settle the requested output                  |
| `Custom(15)` (`0xf`)                                       | The swap's minimum output is above the fill                          |
| A price override writes correctly but the fill is unchanged | The override targets another market's oracle; read it from 208..240 |
| A vault balance returns after a swap                       | Remove the later vault reset; it is undoing transaction-owned state  |
