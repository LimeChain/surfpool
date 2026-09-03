# Surfpool Scenarios
This module contains pre-defined scenarios that can be executed within the Surfpool environment. 
These scenarios allow you to create a time-sequence of account states.
As the scenario is executed, each step, which is associated with a subsequent slot in the surfnet, will override the surfnet accounts db with the account states defined in that step.
This powerful tool allows for testing and simulation of various account states over time, enabling developers to observe how different scenarios will impact their protocol.


## Scenario Components
There are a few components that allow a scenario to function.

### Program IDL
For Surfpool to understand how to serialize and deserialize account data, it needs to have access to the program's IDL. 
Protocols that are natively supported by Surfpool will have their IDLs included by default.

**Currently supported protocols:**
- **Pyth v2** - Price oracle with 4 price feed templates (SOL/USD, BTC/USD, ETH/BTC, ETH/USD)
- **Jupiter v6** - DEX aggregator with TokenLedger manipulation template
- **Switchboard On-Demand** - On-demand oracle with QuoteAccount override template
- **Kamino** – Lending (v1.23.0), Scope oracle, Farms, Swap/LIMO, Earn vaults and Liquidity, across six programs. See [protocols/kamino/README.md](./protocols/kamino/README.md)
- **Drift v2** - Perp and spot markets, user state, and global state
- **BisonFi v3** – Proprietary market maker (no published IDL, not Anchor), with price, depth, spread and freshness templates. See [protocols/bisonfi/README.md](./protocols/bisonfi/README.md)
- **Pump v1** - Bonding curve launchpad with curve reserve and global config override templates
- **PumpSwap v1** - Constant-product AMM with pool state and global config override templates, including canonical pool derivation for migrated pump.fun coins

For custom protocols, an IDL can be registered at runtime using the [`surfnet_registerIdl`](https://docs.surfpool.run/rpc/cheatcodes#surfnet-registeridl) RPC cheatcode.

### Scenario Registration
Scenarios can be registered at runtime using the [`surfnet_registerScenario`](https://docs.surfpool.run/rpc/cheatcodes#surfnet-registerscenario) RPC cheatcode. 
This cheatcode takes in a scenario definition in JSON format, which includes the scenario name, description, and a list of overrides to apply to accounts.
Each override contains a map of the field in the account to override (as indexed in the IDL), and the value to apply for that key.

Field keys use dot notation. Segments address struct fields by name and array elements by
zero-based index, so `liquidity.total_available_amount`, `deposits.0.deposited_amount` and
`config.borrow_rate_curve.points.3.borrow_rate_bps` are all valid. Supplying a composite value (a
whole struct or array) also works, but it must be **complete** - every field of every element,
padding included - because the account is re-encoded with Borsh. An out-of-range index or a
non-numeric segment on an array is a hard error, never a silent write elsewhere.

By default an override applies to exactly one slot. Set `"persist": true` and it is re-applied on
every following slot, which is needed when something else writes the account in between - a
transaction, or another override fetching it fresh. Persist inputs nothing in the scenario writes
(an oracle price, a disabled switch, a risk parameter), never state the transactions under test
mutate: re-applying reverts their writes at the start of the next slot, so a pool would refill
itself after every swap. Only one entry is queued per override, so it is never applied twice to
one slot, and `fetchBeforeUse` applies to the first slot only - once the account is forked, later
slots re-pin the fields without re-fetching it.

### On-chain integration tests

Byte-level coverage that forks real mainnet state lives in two modules,
`crates/core/src/tests/kamino/` and `crates/core/src/tests/bisonfi/`. Both fetch real accounts, so
they need a network connection and are compiled only behind a feature:

```
# both suites
cargo test -p surfpool-core --features integration-tests

# one at a time
cargo test -p surfpool-core --features integration-tests kamino
cargo test -p surfpool-core --features integration-tests bisonfi
```

Note the per-suite filters are substring matches on the full test path, so `kamino` covers only the
Kamino module. It used to sweep up the BisonFi tests as well, back when they lived inside
`tests/kamino/` and were named `tests::kamino::bisonfi_*` - if you are following an older note that
says the `kamino` filter is enough, it no longer is.

Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint instead of the public one. The default test
run needs no network.

### Programs with no IDL

Some programs publish no IDL and are not Anchor at all, so there is no discriminator to resolve an
account type. Those ship a byte layout in their `overrides.yaml` instead:

```yaml
raw_layout:
  account_size: 2048
  magic: { offset: 0, bytes: [80, 79, 79, 76, 83, 84, 65, 84] }   # optional

templates:
  - id: bisonfi-fair-value
    properties:
      - path: fair_value
        offset: 832
        encoding: u128        # u8/u16/u32/u64/u128/i64/i128/bytes32/slot
```

When a template carries a `raw_layout` the engine writes bytes at each property's offset instead of
decoding through the IDL. `account_size` and `magic` replace the discriminator as the check that
this is the right account - without them a raw write would silently corrupt an unrelated one.

Make that guard as narrow as the layout actually is. Size and a magic prefix are often not enough:
BisonFi has eighteen accounts that are all 2048 bytes with the same `POOLSTAT` prefix, but one of
them is an older layout version, so the magic is extended to cover the version word that follows it.
Any field the program itself validates before trusting the account is a candidate for the guard.
Values are written little-endian and integer-exact; anything above `u64::MAX` must be passed as a
decimal string, since a JSON number that large has already lost digits.

### Override Templates
Directly using the `surfnet_registerScenario` endpoint requires building out a map of account keys that are specific to the schema of the account that is being written to.
This is a cumbersome process in most cases.
Override templates that are registered in the Surfpool repo will automatically be available in Surfpool Studio's drag-and-drop UI for creating scenarios.
This UI will automatically create the `surfnet_registerScenario` payload for you, eliminating the complexity from the process.

## Creating Native Scenario Support for a Protocol
Having a protocol being natively supported by Surfpool Scenarios will allow users to easily create scenarios to override account states for that protocol.
All accounts in a surfnet's account db that are owned by the protocol will include automatic IDL-parsing, and the protocol's account overrides will be available in Surfpool Studio's drag-and-drop UI.

The following steps can be followed to natively support a protocol:
 1. Create a folder for the protocol in the `crates/core/src/scenarios/protocols` folder. For example, `crates/core/src/scenarios/protocols/pyth/v2`.
 2. Add a file called `idl.json` containing the protocol's anchor IDL to the folder. For example, see [Pyth's IDL](./protocols/pyth/v2/idl.json) or [Jupiter's IDL](./protocols/jupiter/v6/idl.json)
 3. Add a file called `overrides.yaml` to the folder. For example, see [Pyth's Override File](./protocols/pyth/v2/overrides.yaml) or [Jupiter's Override File](./protocols/jupiter/v6/overrides.yaml).
    1. This file is what will populate Surfpool Studio's UI with the protocol details. The Pyth override file linked above produces the following in the UI:
   ![Pyth Overrides](../../../..//doc/assets/pyth-overrides.png)
 4. Update the [registry.rs](./registry.rs) file. A small amount of code has to be written to wire together the template registry, the `overrides.yaml`, and the `idl.json`. See the `load_pyth_overrides` and `load_jupiter_overrides` functions for examples.

If any part of these instructions are beyond your skill level or availability, but you'd like to see a specific protocol supported, feel free to [Open an Issue to Support a new Protocol](https://github.com/solana-foundation/surfpool/issues/new?template=native-scenario-support-for-protocol.md)!
Opening an issue to signal interest is a big help.
If you're able to find an IDL for the protocol, even better!
