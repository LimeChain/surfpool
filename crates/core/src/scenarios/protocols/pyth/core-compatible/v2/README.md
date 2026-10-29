# Pyth Core Compatible

Pyth runs two price receivers on Solana, both deployments of the `pyth_solana_receiver` program, as
listed on [Pyth's Solana contract addresses](https://docs.pyth.network/price-feeds/contract-addresses/solana):

| Receiver | Program | Template |
|---|---|---|
| Pyth Core | `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` | `pyth-price-feed-v2` |
| Pyth Core Compatible | `rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` | `pyth-core-compatible-price-feed-v2` |

They hold separate price accounts for the same feeds, and a consumer reads the accounts of one of
them. Take the price account from the consumer's own account that names it.

# IDL

`idl.json` is the IDL the program publishes in its Anchor IDL account
`raeUSn8gguFWVFs4iw3YathYM3mdb95W5Bfiu3tqE7F` (`pyth_solana_receiver` 0.2.1, legacy format, sha256
`9ec4718f66a71fc524ecdd533770ca5a8a3984f8e1fb603f01f5e31ff0ab0caa`), converted with
`anchor idl convert --program-id rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` (anchor-cli 0.31.1,
output sha256 `29ab79ef705a3429b3ced2b1222e1b5efdcd4b40fed547177d31c16ebd8473fc`).

One change is made before the conversion: the account names `priceUpdateV2` and `twapUpdate` are
written `PriceUpdateV2` and `TwapUpdate`, as the program's Rust types are named. Anchor derives an
account's discriminator from its name, and only the capitalized names give the discriminator the
live price accounts carry.
