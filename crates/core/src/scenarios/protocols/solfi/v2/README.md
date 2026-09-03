# SolFi V2

A proprietary market maker (PMM), not an AMM. Five templates control its external price, quote
freshness, directional spread, size impact and vault inventory.

SolFi does not derive its mid from vault ratios. Its external oracle supplies the price, market
splines widen around that price, and the vaults provide inventory and settlement capacity. Keeping
those roles separate is the key to building a scenario that means what it says.

This is a how-to. Field offsets and reverse-engineering mechanics deliberately live outside this
file; every field's purpose, unit and safety guidance are also attached to the template and visible
through `get_override_templates` and Studio.

## Template index

| Template | Overrides |
|---|---|
| `solfi-price` | the authoritative external-oracle price |
| `solfi-freshness` | the oracle publication and validity slots |
| `solfi-spread` | a constant directional widening around the mid |
| `solfi-size-impact` | widening at the market's eight live quote-notional breakpoints |
| `solfi-vault-balance` | inventory and hard payout capacity in one token vault |

## Number formats

| You'll see | It means | Example |
|---|---|---|
| `price_coefficient`, `price_exponent` | coefficient × 10^exponent, adjusted for token decimals | WSOL/USDC $50 at exponent -10 → `500000000` |
| directional curve `y` | scaled by the oracle's widening scale | WSOL/USDC `10000` → 1% with a neutral age multiplier |
| `max_widening` | tenths of a ppm | `100000` = 1%, `10000` = 0.1% |
| size-curve `x` | quote-token smallest units | `1000000000` = 1,000 USDC |
| vault `amount` | that mint's smallest unit | `1000000` = 1 USDC |
| freshness values | offsets from the materialization slot | `0` = this slot, `200` = 200 slots ahead |

Price conversion is:

```text
human_price = coefficient × 10^exponent × 10^(base_decimals - quote_decimals)
```

The exponent is live state and changed during validation. Set exponent and coefficient together.

## Picking a market

The price and freshness templates default to the WSOL/USDC oracle
`2ny7eGyZCoeEVTkNLf5HcnJFBKkyA4p4gcrtb3b8y8ou`. The market templates default to WSOL/USDC market
`65ZHSArs5XxPseKQbB1B4r16vDxMWnCxHMzogDAqiDUc`.

For another market, read these addresses from its 1,728-byte account:

| Account | Market bytes |
|---|---:|
| external oracle | 24..56 |
| base mint | 56..88 |
| quote mint | 88..120 |
| base vault | 120..152 |
| quote vault | 152..184 |

Do not reuse the default oracle or vault merely because the token pair looks similar. Use
`fetchBeforeUse: true` so the selected account is forked before its bytes are changed.

### Why the templates have account guards

These templates write a schema-less binary layout. A valid-looking value written at the correct
offset of the wrong account is silent corruption, not a useful override. The guards therefore reject
an account before materialization unless it has the exact supported size and layout marker:

- market templates require 1,728 bytes and the initialized/version word at byte 704;
- oracle templates require 168 bytes and the fixed oracle marker at byte 72;
- vault templates require the canonical 165-byte token-account layout and initialized state byte.

This matters in practice: nine accounts owned by the deployed program have the same 1,728-byte size
as an initialized market but carry a different marker. A size-only guard would admit all nine and
write spline values into unsupported state.

The vault guard is intentionally weaker than protocol identity. SPL token accounts do not contain a
SolFi discriminator, and the raw materializer does not receive account owner/address metadata. The
guard proves the byte layout, not that a vault belongs to SolFi. Always select the vault pubkey
embedded at market bytes 120..152 or 152..184.

## Two rules that prevent misleading scenarios

**1. Keep the oracle current while testing price or widening.** A price can be encoded correctly and
still never reach the quote if the oracle has expired. Apply `solfi-freshness` with
`publication_slot: 0`, `validity_horizon: 200` and `persist: true` when setup spans multiple slots.

**2. Persist configuration, not transaction-owned inventory.** Price, freshness and spline settings
are inputs and may be persisted. A vault balance is state that swaps modify. Persisting it can undo a
swap after every slot and manufacture or erase inventory.

---

# Recipes

## Set the PMM price

```text
template: solfi-price
price_exponent:    -10
price_coefficient: 500000000      # WSOL/USDC at $50 with 9/6 decimals
```

Set both fields. Doubling the coefficient doubles base-to-quote output and halves quote-to-base
output, subject to spread and rounding. The price-looking word in the market account is not the
authoritative input; changing the external oracle is what reprices a fill.

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
Refreshing only the validity horizon keeps the swap executable but lets oracle age continue to widen
the quote. Re-stamp both fields to model a continuously publishing maker.

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
breakpoint. Set all eight values for the side being modeled; use non-decreasing values for ordinary
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

- quote-to-base uses the raw quote input;
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

The base vault pays quote-to-base swaps; the quote vault pays base-to-quote swaps. Reducing the
payout vault far enough makes that direction reject with SolFi error 18. Vaults also enter nonlinear
inventory policy, so changing the input-side vault can move a quote even though it is not paying out.

This is not an AMM reserve-price formula. Use `solfi-price` to change the mid. Do not persist a vault
override unless restoring the same inventory after every transaction is deliberately the scenario.

## Caller-specific quoting

SolFi does inspect the calling transaction and shared global policy. `pmm-sim` can construct calls
from these four aggregator program identities:

| Caller | Program address |
|---|---|
| Jupiter | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` |
| DFlow | `DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH` |
| OKX Labs | `6m2CDdhRgxpH4WjvdzxAYbGxwdGUz5MziiL5jek2kBma` |
| Titan | `T1TANpTeScyeqVzzgNViGDNrkQ6qHz9KrSBS4aNXvGT` |

Every identity was exercised in both directions on both funded markets. The pinned results were:

| Market and direction | Jupiter / OKX / Titan vs direct | DFlow vs direct |
|---|---:|---:|
| USDT → USDC, 1,000 USDT | no change | +300 raw USDC (+0.3 ppm) |
| USDC → USDT, 1,000 USDC | no change | +400 raw USDT (+0.4 ppm) |
| WSOL → USDC, 1 WSOL | +4,346 raw USDC (+42.1 ppm) | -107,719 raw USDC (-1,044 ppm) |
| USDC → WSOL, 100 USDC | +40,783 lamports (+42.1 ppm) | -1,019,003 lamports (-1,052 ppm) |

These are all aggregator identities currently supported by `pmm-sim`; they are not proof that the
opaque SolFi policy tree recognizes no other callers. Quote differences are also policy snapshots,
not stable fee promises.

This proves caller-specific policy exists. It does not identify a safe market-local fee field. The
policy lives in a shared 1 MiB configuration and changing it could affect every market, so there is
intentionally no caller, fee or aggregator template.

---

# Coverage and limits

As enumerated on 2026-09-03, the deployed program owns 22 accounts: 19 market-sized accounts, two
1 MiB shared configurations and one 352-byte account. Ten of the 19 market-sized accounts carry the
initialized `MarketConfig` marker accepted by the raw guard. Byte-level tests fetch all ten, verify
their embedded oracle, exercise all four market/oracle raw layouts, constrain each write to its
proven byte range, exercise every embedded vault with the supported token-account layout, and verify
that all nine same-sized siblings are rejected.

Behavioral assertions currently cover two complete replay fixtures, WSOL/USDC and USDT/USDC. They
use the current deployed ELF and freshly fetched market, oracle, configuration, vault and mint
accounts, but execute locally at the market-state slot. These are also the only two markets with
meaningful vault liquidity at enumeration time. Seven other initialized markets have empty vaults;
the old BONK market holds only 7,959 raw BONK units and 5,802 raw USDC units. Replaying fabricated
liquidity on those accounts would test a state SolFi did not publish, so behavioral claims remain
limited to the two funded markets rather than reporting synthetic pools as coverage.

The shared caller-policy tree and exact nonlinear inventory formula remain intentionally unexposed.

# Troubleshooting

| Symptom | Fix |
|---|---|
| Price or spread override writes correctly but the swap rejects | Apply and persist `solfi-freshness`; expiry is checked before pricing |
| `Custom(23)` | The oracle validity horizon is behind the executing slot |
| `Custom(18)` after lowering a vault | The payout vault cannot settle the requested output |
| Constant spread is smaller than requested | Account for `oracle_scale`, neutralize the age/additional curves, and raise `max_widening` |
| Size impact appears at the wrong base amount | Breakpoints are quote notional; base input is converted at the oracle price first |
| One direction widened instead of the other | Quote-to-base is buying base; base-to-quote is selling base |
| A vault balance returns after a swap | Remove persistence; repeated application is undoing transaction-owned state |
| The guard rejects a market | It is not the initialized 1,728-byte layout supported by these templates |
| Another aggregator gets a tiny different fill | SolFi has shared caller policy; no safe override for it is exposed |
