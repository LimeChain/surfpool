# Chainlink

Surfpool bundles the IDL of Chainlink's Solana Store program
(`HEvSKofvBgfaexv23kMabbYqxasxU3mQ4ibBMEmJWHny`) and one template, `chainlink-data-feed`, so a
scenario can move the latest round of any Chainlink data feed: its answer, slot and timestamp.
Programs that read the feed then see that round. The template only prepares state.

Chainlink Data Streams has no counterpart here. Its reports are verified inside the consuming
transaction and stored, if at all, in the consumer's own accounts (Kamino's Scope prices, for
example), so those accounts are overridden through the consumer's templates.

## The bundled IDL

`v1/idl.json` is the program's on-chain IDL account `7FQupg5eV1vLDs7eFcSGDFDgskKUCb9hEGv1JbVA4BPo`
(legacy format, sha256 `bf4dbb4df4245331997cf00c0f4f7fc40ef13a3f4d14dfcc7ab8587f7c6e20af`),
converted with `anchor idl convert --program-id HEvSKofvBgfaexv23kMabbYqxasxU3mQ4ibBMEmJWHny`
(Anchor CLI 0.31.1, output sha256
`e62c5f885b8fa219008a378f7d778acfb95e416fed0dbe7825143ed70b91ec90`), with nothing changed.

A feed account is the `Transmissions` header followed by its rounds, and the published IDL
describes only the header: it ends at byte 160, and the IDL's `HEADER_SIZE` constant pads it to
192 bytes after the discriminator. The round is not in the IDL, so the template writes it by
offset (`raw_layout: true`) and carries no IDL. The offsets come from Chainlink's Solana SDK,
`chainlink_solana` 2.0.8 (`v2::read_feed_v2`), which reads one 48-byte `Transmission` at byte
200 and rejects any feed whose `live_length` is not 1:

| Field       | Offset | Encoding |
| ----------- | ------ | -------- |
| `slot`      | 200    | `u64`    |
| `timestamp` | 208    | `u32`    |
| `answer`    | 216    | `i128`   |

Bytes 212 to 215 and 232 to 247 are padding and are never written. A raw-layout write only checks
that each field fits in the account, so the feed must be a Store program feed with one live round.

## Tests

`registry.rs` pins the round's offsets without a network. The live tests read the SOL / USD feed:
one checks that only the round's bytes change, the other runs the deployed Store program's
`query` and checks that it returns the round the template wrote:

```
cargo test -p surfpool-core --features integration-tests chainlink
```
