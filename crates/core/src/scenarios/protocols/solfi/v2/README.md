# SolFi V2

A proprietary market maker (PMM), not an AMM. Five templates control its external price, quote
freshness, directional spread, size impact and vault inventory.

SolFi does not derive its mid from vault ratios. Its external oracle supplies the price, market
splines widen around that price, and the vaults provide inventory and settlement capacity.

## Template index

| Template              | Overrides                                        |
| --------------------- | ------------------------------------------------ |
| `solfi-price`         | the price SolFi uses for swaps                   |
| `solfi-freshness`     | whether SolFi's price is current and usable      |
| `solfi-spread`        | the extra cost SolFi adds when buying or selling |
| `solfi-size-impact`   | how much worse the price gets for larger trades  |
| `solfi-vault-balance` | how many tokens SolFi has available for swaps    |

## Number formats

| You'll see                            | It means                                               | Example                                              |
| ------------------------------------- | ------------------------------------------------------ | ---------------------------------------------------- |
| `price_coefficient`, `price_exponent` | coefficient × 10^exponent, adjusted for token decimals | WSOL/USDC $50 at exponent -10 → `500000000`          |
| directional curve `y`                 | scaled by the oracle's widening scale                  | WSOL/USDC `10000` → 1% with a neutral age multiplier |
| `max_widening`                        | tenths of a ppm                                        | `100000` = 1%, `10000` = 0.1%                        |
| size-curve `x`                        | quote-token smallest units                             | `1000000000` = 1,000 USDC                            |
| vault `amount`                        | that mint's smallest unit                              | `1000000` = 1 USDC                                   |
| freshness values                      | offsets from the materialization slot                  | `0` = this slot, `200` = 200 slots ahead             |

Price conversion is:

```text
human_price = coefficient × 10^exponent × 10^(base_decimals - quote_decimals)
```

The exponent is live state and changed during validation. Set exponent and coefficient together.

## Picking a market

Every template now starts with a market picker. Choose one of the two SolFi markets that currently
has meaningful swap liquidity:

| Choice      | Best suited for                                      |
| ----------- | ---------------------------------------------------- |
| WSOL / USDC | SOL price shocks, spread changes and liquidity tests |
| USDT / USDC | Stablecoin depegs and stablecoin liquidity tests     |

The picker supplies the correct market or oracle account to the template automatically. The vault
template asks for both the market and the token vault because each market has one vault for each
side of a swap. Users do not need to copy account addresses into these templates.

Do not reuse an oracle or vault merely because the token pair looks similar. Use
`fetchBeforeUse: true` so the selected account is forked before its bytes are changed.

## Two rules that prevent misleading scenarios

**1. Keep the oracle current while testing price or widening.** A price can be encoded correctly and
still never reach the quote if the oracle has expired. Apply `solfi-freshness` with
`publication_slot: 0`, `validity_horizon: 200` and `persist: true` when setup spans multiple slots.

**2. Persist configuration, not transaction-owned inventory.** Price, freshness and spline settings
are inputs and may be persisted. A vault balance is state that swaps modify. Persisting it can undo a
swap after every slot and manufacture or erase inventory.

## Scenario ideas

### PMM risk-off during a SOL crash

This is the scenario exposed in Studio's Bento examples. It models a maker that remains available
for small trades but protects itself after SOL falls: it marks SOL at $50, pays 1% less when buying
SOL, and limits its USDC payout inventory to 25 USDC.

1. On **WSOL / USDC**, use `solfi-price` with exponent `-10` and coefficient `500000000`.
2. Use `solfi-freshness` with publication slot `0` and validity horizon `200` so the new quote is
   usable.
3. Use `solfi-spread` with buy-base widening `1000`, sell-base widening `10000`, age multiplier
   `1000`, additional widening `0`, and maximum widening `100000`.
4. Use `solfi-vault-balance` on the WSOL/USDC **USDC vault** with amount `25000000`. Do not persist
   this transaction-owned balance.

The deployed-program integration test confirms the complete four-template scenario: a 0.1 SOL sale
still fills near 4.95 USDC, the opposite direction is not widened, a 1 SOL control sale fills with
the original vault, and the same 1 SOL sale fails with error 18 after the 25 USDC limit is applied.

### SOL price shock

Use this to test whether a router, trading strategy or lending flow reacts correctly when SolFi's SOL
price moves suddenly.

1. Choose **WSOL / USDC** in `solfi-price`.
2. Set both price fields to the new price. For example, exponent `-10` and coefficient `500000000`
   means $50 per SOL.
3. Add `solfi-freshness` for **WSOL / USDC** with publication slot `0` and validity horizon `200`.
4. Persist both overrides if the test runs for more than one slot.
5. Compare a swap before and after the price change. Also keep a run with the original price as a
   control.

### USDT depeg

Use this to model USDT trading below or above one dollar and observe route selection or collateral
valuation.

1. Choose **USDT / USDC** in `solfi-price`.
2. At exponent `-10`, use coefficient `9500000000` for $0.95 or `10500000000` for $1.05.
3. Keep the **USDT / USDC** oracle current with `solfi-freshness`.
4. Compare both swap directions so the test proves the new price is applied reciprocally.

### Maker becomes cautious in one direction

Use this to test what happens when SolFi still trades but strongly discourages users from buying one
asset from it.

1. Choose the market in `solfi-spread`.
2. Increase **Buy-base widening** to make buying WSOL or USDT more expensive, or increase
   **Sell-base widening** to make selling it more expensive.
3. Keep the other direction low, set the age multiplier to `1000`, additional widening to `0`, and
   set the maximum high enough to allow the requested spread.
4. Keep the matching oracle fresh, then compare equal-notional swaps in both directions.

### Large orders receive a worse price

Use this to test order splitting and whether a router moves a large trade to another venue.

1. Choose a market in `solfi-size-impact`.
2. Give the first size knots small values and later knots progressively larger values.
3. Set the age multiplier to `1000` and additional widening to `0` so only trade size is changing the
   result.
4. Run small, medium and large swaps. Require at least one strict deterioration

### SolFi cannot fill one side of a swap

Use this to test fallback routing and transaction failure handling when the maker runs out of the
token it must pay.

1. In `solfi-vault-balance`, choose the WSOL or USDT vault to block users buying the base asset, or
   choose the USDC vault to block users selling the base asset.
2. Lower the balance enough that the requested swap cannot be paid.
3. Do not persist the vault balance unless resetting inventory after every transaction is explicitly
   part of the test.
4. Confirm the affected swap fails with SolFi error 18 and that the opposite direction or an
   alternative venue still works.

# Recipes

## Set the PMM price

```text
template: solfi-price
price_exponent:    -10
price_coefficient: 500000000      # WSOL/USDC at $50 with 9/6 decimals
```

Set both fields. Doubling the coefficient doubles base-to-quote output and halves quote-to-base
output, subject to spread and rounding. The price-looking word in the market account is not the
authoritative input, but changing the external oracle is what reprices a fill.

For a multi-slot scenario, persist both `solfi-price` and `solfi-freshness`. Repricing only SolFi
while leaving another venue unchanged creates a real cross-venue dislocation suitable for router,
arbitrage and liquidation-path testing. Always include an undislocated control leg.

## Keep the quote current

```text
template: solfi-freshness
publication_slot: 0
validity_horizon:  200
persist: true
```

Both inputs are relative offsets even though the account stores XOR-obfuscated absolute slots.
Re-stamp both fields to model a continuously publishing maker.

Unlike BisonFi, an expired SolFi oracle rejects the transaction with error 23. It does not succeed
with zero output.

## Quote a constant directional spread

Default WSOL/USDC example, one percent in both directions:

```text
template: solfi-spread
quote_to_base_curve_y:       10000
base_to_quote_curve_y:       10000
age_multiplier_curve_y:      1000
additional_widening_curve_y: 0
max_widening:                100000
```

`quote_to_base_curve_y` makes buying the base asset more expensive. `base_to_quote_curve_y` makes
selling it more expensive. Set only one directional property for a risk-off scenario, or both for a
symmetric spread.

The directional value is market-specific because the oracle contributes a scale:

```text
final_widening = directional_y × oracle_scale / 1000
output ≈ oracle_mid × (1 - final_widening / 10000000)
```

The verified WSOL/USDC oracle scale is `10000`, so `directional_y: 10000` reaches the 1% clamp. The
verified USDT/USDC scale is `1000`, so its corresponding value is `100000`. Do not copy the same
directional value across markets without reading the oracle scale.

Set the age multiplier to `1000` and additional widening to `0` when you need the configured spread
to be deterministic. Set `max_widening` at or above the intended result or the clamp will flatten it.

## Make large trades progressively worse

Use `solfi-size-impact`. Each direction has eight `y` properties, one for each existing live `x`
breakpoint. Set all eight values for the side being modeled and use non-decreasing values for ordinary
liquidity deterioration.

```text
template: solfi-size-impact
quote_to_base_y_0: 1000
quote_to_base_y_1: 1000
quote_to_base_y_2: 10000
quote_to_base_y_3: 20000
quote_to_base_y_4: 40000
quote_to_base_y_5: 60000
quote_to_base_y_6: 80000
quote_to_base_y_7: 100000
age_multiplier_curve_y:      1000
additional_widening_curve_y: 0
max_widening:                100000
```

The template intentionally preserves the market's `x` positions because the operator can change
them live. The `x` axis is raw quote-token notional:

- quote-to-base uses the raw quote input
- base-to-quote converts the base input to quote notional at the oracle price before lookup.

SolFi linearly interpolates between adjacent points and uses the nearest endpoint outside the
configured range. The same oracle scale and maximum clamp described under `solfi-spread` still apply.

## Put the maker into directional risk-off mode

Use either `solfi-spread` for one constant penalty or `solfi-size-impact` for a penalty that grows
with size:

```text
# Maker does not want to sell more base
quote_to_base_curve_y: <wide>
base_to_quote_curve_y: <tight>
```

Reverse the two values when the maker does not want to buy more base. The deployed-program tests
require the targeted response to dominate any cross-effect rather than assuming bit-identical output
on the other side.

## Reduce inventory or make a direction unfillable

```text
template: solfi-vault-balance
account: <actual market vault>
amount:  <smaller raw token amount>
```

The base vault pays quote-to-base swaps, the quote vault pays base-to-quote swaps. Reducing the
payout vault far enough makes that direction reject with SolFi error 18. Vaults also enter nonlinear
inventory policy, so changing the input-side vault can move a quote even though it is not paying out.

This is not an AMM reserve-price formula. Use `solfi-price` to change the mid. Do not persist a vault
override unless restoring the same inventory after every transaction is deliberately the scenario.

# Troubleshooting

| Symptom                                                        | Fix                                                                                        |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Price or spread override writes correctly but the swap rejects | Apply and persist `solfi-freshness`. Expiry is checked before pricing                      |
| `Custom(23)`                                                   | The oracle validity horizon is behind the executing slot                                   |
| `Custom(18)` after lowering a vault                            | The payout vault cannot settle the requested output                                        |
| Constant spread is smaller than requested                      | Account for `oracle_scale`, neutralize the age/additional curves, and raise `max_widening` |
| Size impact appears at the wrong base amount                   | Breakpoints are quote notional. Base input is converted at the oracle price first          |
| One direction widened instead of the other                     | Quote-to-base is buying base. Base-to-quote is selling base                                |
| A vault balance returns after a swap                           | Remove persistence. Repeated application is undoing transaction-owned state                |
