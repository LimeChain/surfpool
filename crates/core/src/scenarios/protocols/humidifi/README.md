# HumidiFi

HumidiFi is a proprietary market maker without a published IDL. Surfpool prepares its market
state through the raw layout in `v1/overrides.yaml`; it does not construct or submit a swap.

## Deployment

- Program: `9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp`
- ProgramData: `G9S64i58RRWJA28vZiNhnP56Ux4Ef7hfMgHNREnZZSom`
- Deploy slot: `446544344` (2026-09-12 22:35:04 UTC)
- ProgramData length: 339485 bytes, including the 45-byte loader header
- ELF length: 339440 bytes
- ELF SHA-256: `4c2b4c29bce4ee4d2a0dfde28f6d511e60627e86ac3cd417e6734ff999ea4550`

A later redeploy voids the layout evidence; the live suite pins ProgramData and fails when it moves.

## The account is obfuscated

A market is 1728 bytes. Its economic fields and public keys use per-offset XOR keys; the schema
version at offset 1720 is plaintext. The templates keep values plaintext and declare `xor_mask`
for each masked field. The shared raw-layout writer encodes a value, XORs the eight-byte word,
then writes it. Other encodings cannot carry a mask.

The fair-value key is `b957ed15dc877426`. Freshness and the staleness limit use
`6e9de2b30b19f1ea`. The base mint at offset 416 and quote mint at 384 each occupy four words,
decoded with `fb5ce87aae443c38`, `04a2178451bac3c7`, `04a1178751b9c3c6`, and
`04a0178651b8c3c5`. These public-key keys are read-only in Rust.

## The guard, and what it does not cover

The raw-layout guard checks size 1728 and the masked tag `[44,90,19,124,56,111,47,150]` at
offset 8. This tag is shared by several schema versions. `validate_humidifi_market_layout`
also checks the program owner and requires plaintext schema version 8 at offset 1720.
Discovery applies the same size, tag, and version filters, then validates the referenced mints.

The YAML magic guard supports one contiguous range, so owner and schema checks belong in Rust.
The fair-value tool uses them before reading mints or building a scenario. Direct raw-template
composition does not perform these additional checks; the raw scenario API is unvalidated by
contract. Schema versions other than 8 are unsupported.

## Templates

| Template | Prepared state |
| --- | --- |
| `humidifi-fair-value` | Quote-per-base atomic ratio at offset 576 |
| `humidifi-freshness` | Materialization slot at offset 616, default lead 0 |
| `humidifi-stale-quote` | Aged slot at offset 616, default lead -3 |

Each template's `llm_context` in `v1/overrides.yaml` documents its value: the fair-value
conversion, the slot lead, and the staleness boundary.

## Live market discovery

`list_humidifi_markets` uses the target Surfnet RPC's `getProgramAccounts`, then fetches the
referenced mints in batches of at most 100. Addresses identify markets; labels use verified-token
symbols with full mint addresses as a fallback. Discovery sorts by label and address. The templates
contain no static market list or default address; every override must target an explicitly selected market.

Market-sized accounts on mainnet span several schema versions, and schema membership is not proof
of current trading or liquidity. Discovery validates compatible accounts and mint metadata; it does
not promise that every market is quoting. The live test checks returned metadata without pinning a
market count.

To inspect all market-sized accounts, including unsupported schemas:

```bash
curl -s -X POST "$RPC_URL" -H 'Content-Type: application/json' -d '{
  "jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp",
    {"encoding":"base64","commitment":"confirmed","filters":[{"dataSize":1728}]}]}'
```

Decode the little-endian u64 at offset 1720 without an XOR mask. Normal discovery additionally
filters the tag at offset 8 and version 8 at offset 1720.

## Builders and tools

`build_humidifi_fair_value_scenario` is a pure conversion over validated market metadata.
`create_humidifi_fair_value_scenario` reads the market and both mints through the selected
Surfnet RPC, where local accounts take precedence and missing accounts fall back to the
datasource. It stages the result through the shared scenario path.

Both builders leave every override at `fetchBeforeUse: false`: the creation reads already cached
the accounts in Surfnet, and Play applies the values to that local state. Refetching would replace
earlier local edits and the state the amounts were calculated from. Freshness passes a `null`
value, so the encoder uses its zero lead and writes the preparation slot itself; it is applied once,
and a scenario that runs past the market's staleness window refreshes it again at a later slot. When composing
templates directly, set `fetchBeforeUse: true` on the first override for each account that has not
already been prepared.

`build_humidifi_liquidity_scenario` scales the market's vault balances through the generic
`spl-token-account-balance` template, one override per side that changes, from 0 to 10000 remaining
basis points with integer floor, and pairs them with a freshness override. The vault addresses come
from the market's masked words at offsets 448 (quote) and 480 (base); each vault must be a token
account for the market's mint on that side, owned by that mint's token program, initialized and
controlled by the market. `create_humidifi_liquidity_scenario`
reads the market, both mints and both vaults through the Surfnet RPC and stages the result.

`list_humidifi_markets` returns addresses, labels, both mint identities and decimals, and
`maxStalenessSlots`. Both creation tools require a non-empty `market` address from this list.
All tools accept an optional `surfnetPort`, defaulting to 8899. Studio's PMM
fair-value preset uses the market list and fair-value tools; the stale-quote and liquidity chips
request editable state scenarios.

## Behavioral evidence

The live suite checks byte-limited template writes on two markets, guarded layout rejection,
discovered metadata, and scenario materialization that queues nothing past its slot. The swap replay's
setup is described in `crates/core/src/tests/humidifi/mod.rs`.

The fair-value replay checks unchanged output after re-encoding the current ratio, increased
base output after halving the price, and decreased output or rejection after doubling it.
An independent absolute-price test builds scenarios at 100 and 208 USDC per SOL, registers and
materializes them, then compares actual swap output with the human price and mint decimals within
1%. This expectation does not use the encoded fair-value word or the `2^48` conversion.
The staleness replay uses an explicit clock: with the tested SOL/USDC market's limit of 2, age 3 fails with
`Custom(1027565)` (`0xfaded`) and age 2 fills. Changing offset 608 to 10 moves those boundaries
to ages 11 and 10. The limit is inclusive.

The liquidity replay materializes the builder's scenarios through the production path and swaps
against the prepared accounts. A drained base vault fails the transfer with the token program's
insufficient-funds error. On the tested SOL/USDC market, a base vault cut
to 0.5% of its live balance still filled at less than a hundredth of baseline output. Draining
the quote vault left quote-to-base fills unchanged. These observations are tested against live
state; the tool scales balances without assuming a fixed inventory threshold or output multiplier.

The run command for the live suite is in [`../../README.md`](../../README.md#humidifi-integration-tests).

## Known boundaries

Depth and curve fields are not exposed; vault balances are, through the generic token template.
Inventory response depends on current market state and trade direction; no universal price or
depth formula is inferred from the vault-balance control. The behavioral replay covers USDC into
WSOL on the replay's SOL/USDC market: the quote side's
own depletion is proven only to leave that direction unchanged, and the opposite direction and other
pairs are not replayed. Other market schema versions remain unsupported.
