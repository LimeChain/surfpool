//! HumidiFi live integration tests.
//!
//! cargo test -p surfpool-core --features integration-tests humidifi -- --test-threads=1 --nocapture
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! HumidiFi market fields are XOR-obfuscated: an 8-byte word is stored as `plaintext XOR key`. These
//! tests fetch live markets and prove the shipped templates write exactly their target fields, that
//! the masked write round-trips to the plaintext the caller asked for, that the guard rejects a
//! tampered account, and that discovery matches the chain. They pin the deployed ProgramData
//! so a redeploy that could move the keys or offsets fails loudly rather than writing garbage.
//!
//! Swap replays execute the deployed HumidiFi program through a native DFlow shim to prove the
//! fair-value effect and the configured staleness boundary.

use std::collections::HashMap;

use litesvm::LiteSVM;
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_message::Message;
use solana_program_pack::Pack;
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;

use super::live::{self, diff_indices, fetch};
use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::humidifi::v1::{
            HumidiFiMarket, build_humidifi_fair_value_scenario, build_humidifi_liquidity_scenario,
            discover_humidifi_markets, humidifi_vault_addresses,
        },
    },
    surfnet::svm::SurfnetSvm,
};

const HUMIDIFI_PROGRAM: Pubkey =
    Pubkey::from_str_const("9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp");
const HUMIDIFI_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("G9S64i58RRWJA28vZiNhnP56Ux4Ef7hfMgHNREnZZSom");
/// Pinned deployment. A change here means HumidiFi was upgraded and the keys and offsets below must
/// be revalidated before the templates are trusted again.
const DEPLOY_SLOT: u64 = 446_544_344;
const PROGRAMDATA_SIZE: usize = 339_485;
const ELF_SHA256: &str = "4c2b4c29bce4ee4d2a0dfde28f6d511e60627e86ac3cd417e6734ff999ea4550";

const SOL_USDC_MARKET: Pubkey =
    Pubkey::from_str_const("FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH");
/// A second, differently-scaled market, so a claim of generic support is tested on two assets.
const SECOND_MARKET: Pubkey = Pubkey::from_str_const("hKgG7iEDRFNsJSwLYqz8ETHuZwzh6qMMLow8VXa8pLm");

const FAIR_VALUE_OFFSET: usize = 576;
const LAST_UPDATE_SLOT_OFFSET: usize = 616;
const MAX_STALENESS_OFFSET: usize = 608;
const MAGIC_OFFSET: usize = 8;
const BASE_MINT_OFFSET: usize = 416;
const QUOTE_MINT_OFFSET: usize = 384;
const SCHEMA_VERSION_OFFSET: usize = 1720;

const FAIR_VALUE_KEY: u64 = 0xb957_ed15_dc87_7426;
const STATE_KEY: u64 = 0x6e9d_e2b3_0b19_f1ea;
const PUBKEY_XOR_KEYS: [u64; 4] = [
    0xfb5c_e87a_ae44_3c38,
    0x04a2_1784_51ba_c3c7,
    0x04a1_1787_51b9_c3c6,
    0x04a0_1786_51b8_c3c5,
];

const MARKET_SIZE: usize = 1728;

// ---- decode helpers ----

fn decode_u64(data: &[u8], offset: usize, key: u64) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap()) ^ key
}

fn encode_masked_u64(data: &mut [u8], offset: usize, key: u64, value: u64) {
    data[offset..offset + 8].copy_from_slice(&(value ^ key).to_le_bytes());
}

fn unmask_pubkey(data: &[u8], offset: usize) -> Pubkey {
    let mut bytes = [0u8; 32];
    for (i, key) in PUBKEY_XOR_KEYS.iter().enumerate() {
        let word = u64::from_le_bytes(data[offset + i * 8..offset + i * 8 + 8].try_into().unwrap());
        bytes[i * 8..i * 8 + 8].copy_from_slice(&(word ^ key).to_le_bytes());
    }
    Pubkey::new_from_array(bytes)
}

/// Asserts every changed byte lies inside one of `ranges`. A masked write can leave a byte equal to
/// the original, so the touched set is a subset of the field, not always the whole field; what must
/// hold is that nothing OUTSIDE the field moved.
fn assert_only_within(diffs: &[usize], ranges: &[std::ops::Range<usize>], context: &str) {
    for index in diffs {
        assert!(
            ranges.iter().any(|range| range.contains(index)),
            "{context}: byte {index} changed outside the target field(s) {ranges:?}"
        );
    }
}

fn template_raw_apply(
    template_id: &str,
    values: HashMap<String, serde_json::Value>,
    slot: u64,
    data: &[u8],
) -> Vec<u8> {
    let registry = TemplateRegistry::new();
    let template = registry.get(template_id).expect("HumidiFi template");
    let materialized = template
        .raw_layout
        .as_ref()
        .expect("HumidiFi raw layout")
        .materialize(data, &template.properties, &values, slot)
        .expect("materialize");
    assert_eq!(materialized.len(), data.len(), "account length changed");
    materialized
}

// ---- tests ----

/// The upgrade canary: pin the deployed program so a redeploy that could move the keys or offsets
/// fails loudly, exactly where the layout evidence would otherwise silently rot.
#[tokio::test]
async fn humidifi_programdata_identity_is_pinned() {
    let data = fetch(&[HUMIDIFI_PROGRAMDATA]).await.remove(0).data;
    assert_eq!(data.len(), PROGRAMDATA_SIZE, "ProgramData size changed");
    let deploy_slot = u64::from_le_bytes(data[4..12].try_into().unwrap());
    assert_eq!(
        deploy_slot, DEPLOY_SLOT,
        "HumidiFi was redeployed; revalidate the layout and XOR keys before trusting the templates"
    );
    let sha = hex::encode(Sha256::digest(&data[45..]));
    assert_eq!(
        sha, ELF_SHA256,
        "HumidiFi ELF changed; revalidate the layout and XOR keys before trusting the templates"
    );
}

/// Each template writes only its target field on live data, and the masked write round-trips to the
/// plaintext the caller asked for. Proven on two markets so generic support is not assumed.
#[tokio::test]
async fn humidifi_templates_write_only_their_fields_and_round_trip() {
    for address in [SOL_USDC_MARKET, SECOND_MARKET] {
        let market = fetch(&[address]).await.remove(0);
        assert_eq!(
            market.data.len(),
            MARKET_SIZE,
            "{address} is not a HumidiFi market layout"
        );
        let data = market.data;

        let chosen: u64 = 12_345_678_901_234;
        let slot = 500_000_000u64;
        // The stale template's default lead of -3 ages the quote past the tested SOL/USDC market's
        // inclusive limit.
        for (template, property, value, offset, key, expected) in [
            (
                "humidifi-fair-value",
                "fair_value",
                serde_json::json!(chosen.to_string()),
                FAIR_VALUE_OFFSET,
                FAIR_VALUE_KEY,
                chosen,
            ),
            (
                "humidifi-freshness",
                "last_update_slot",
                serde_json::Value::Null,
                LAST_UPDATE_SLOT_OFFSET,
                STATE_KEY,
                slot,
            ),
            (
                "humidifi-stale-quote",
                "last_update_slot",
                serde_json::Value::Null,
                LAST_UPDATE_SLOT_OFFSET,
                STATE_KEY,
                slot - 3,
            ),
        ] {
            let out = template_raw_apply(
                template,
                HashMap::from([(property.to_string(), value)]),
                slot,
                &data,
            );
            assert_eq!(
                decode_u64(&out, offset, key),
                expected,
                "{address} {template}: the masked word must unmask to the requested plaintext"
            );
            assert_only_within(
                &diff_indices(&out, &data),
                std::slice::from_ref(&(offset..offset + 8)),
                &format!("{address} {template}"),
            );
        }
    }
}

/// The raw guard rejects a market of the wrong size or with a tampered magic word.
#[tokio::test]
async fn humidifi_guard_rejects_wrong_size_and_magic() {
    let market = fetch(&[SOL_USDC_MARKET]).await.remove(0);
    let registry = TemplateRegistry::new();
    let layout = registry
        .get("humidifi-fair-value")
        .and_then(|t| t.raw_layout.clone())
        .expect("HumidiFi raw layout");

    // Wrong size.
    let mut short = market.data.clone();
    short.truncate(MARKET_SIZE - 8);
    let err = layout.guard(&short).unwrap_err();
    assert!(err.contains("bytes"), "unexpected error: {err}");

    // Right size, wrong magic.
    let mut tampered = market.data.clone();
    tampered[MAGIC_OFFSET] ^= 0xff;
    let err = layout.guard(&tampered).unwrap_err();
    assert!(err.contains("magic"), "unexpected error: {err}");
}

#[tokio::test]
async fn humidifi_discovers_live_markets() {
    let markets = discover_humidifi_markets(&live::client())
        .await
        .expect("discover live markets");
    assert!(!markets.is_empty(), "HumidiFi must expose market accounts");
    let addresses = markets
        .iter()
        .map(|market| market.address)
        .collect::<Vec<_>>();
    let unique = addresses.iter().collect::<std::collections::HashSet<_>>();
    assert_eq!(unique.len(), markets.len());
    let mut accounts = Vec::new();
    for batch in addresses.chunks(100) {
        accounts.extend(fetch(batch).await);
    }
    let mut mints = markets
        .iter()
        .flat_map(|market| [market.base_mint, market.quote_mint])
        .collect::<Vec<_>>();
    mints.sort_unstable();
    mints.dedup();
    let mut mint_accounts = Vec::new();
    for batch in mints.chunks(100) {
        mint_accounts.extend(fetch(batch).await);
    }
    for (market, account) in markets.iter().zip(&accounts) {
        assert_eq!(account.owner, HUMIDIFI_PROGRAM);
        let (base, quote) =
            HumidiFiMarket::mint_addresses(account).expect("valid discovered market");
        let index = |mint| mints.binary_search(mint).expect("fetched mint");
        let expected = HumidiFiMarket::validate(
            market.address,
            account,
            &mint_accounts[index(&base)],
            &mint_accounts[index(&quote)],
        )
        .unwrap();
        assert_eq!(*market, expected);
        assert_eq!(
            unmask_pubkey(&account.data, BASE_MINT_OFFSET),
            market.base_mint
        );
        assert_eq!(
            unmask_pubkey(&account.data, QUOTE_MINT_OFFSET),
            market.quote_mint
        );
        assert_eq!(
            market.max_staleness_slots,
            decode_u64(&account.data, MAX_STALENESS_OFFSET, STATE_KEY)
        );
        assert_eq!(decode_u64(&account.data, SCHEMA_VERSION_OFFSET, 0), 8);
        assert!(!market.label().is_empty());
    }
}

/// The builder's materialization path: prepare the live market locally, build the scenario, then
/// register and materialize it through the real materializer. The fair value lands from the human
/// price and the persisted freshness re-stamps itself on the next slot without remote replacement.
#[tokio::test]
async fn humidifi_builder_scenario_preserves_local_market_and_keeps_quote_fresh() {
    let market_account = fetch(&[SOL_USDC_MARKET]).await.remove(0);
    let base_slot = decode_u64(&market_account.data, LAST_UPDATE_SLOT_OFFSET, STATE_KEY) + 100;
    let (base_mint, quote_mint) = HumidiFiMarket::mint_addresses(&market_account).expect("mints");
    let mints = fetch(&[base_mint, quote_mint]).await;
    let market_key = SOL_USDC_MARKET;
    let market = HumidiFiMarket::validate(market_key, &market_account, &mints[0], &mints[1])
        .expect("valid market");

    let preparation = build_humidifi_fair_value_scenario(&market, "175.5").expect("build scenario");
    let expected_fair_value = preparation.fair_value;

    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let mut local_account = market_account.clone();
    local_account.lamports = local_account.lamports.checked_add(1).unwrap();
    svm.inner
        .set_account(market_key, local_account.clone())
        .expect("seed prepared market");
    let remote = Some((live::client(), CommitmentConfig::confirmed()));
    svm.register_scenario(preparation.scenario, Some(base_slot))
        .expect("register scenario");

    // Play: both overrides apply at the base slot.
    svm.materialize_overrides_for_slot(&remote, base_slot)
        .await
        .expect("materialize");
    let applied_account = svm.inner.get_account(&market_key).unwrap().unwrap();
    assert_eq!(applied_account.owner, local_account.owner);
    assert_eq!(applied_account.lamports, local_account.lamports);
    assert_eq!(applied_account.data.len(), local_account.data.len());
    let applied = &applied_account.data;
    assert_eq!(
        decode_u64(applied, FAIR_VALUE_OFFSET, FAIR_VALUE_KEY),
        expected_fair_value,
        "the built fair value must land, masked"
    );
    assert_eq!(
        decode_u64(applied, LAST_UPDATE_SLOT_OFFSET, STATE_KEY),
        base_slot,
        "freshness must publish the base slot"
    );
    assert_only_within(
        &diff_indices(applied, &local_account.data),
        &[
            FAIR_VALUE_OFFSET..FAIR_VALUE_OFFSET + 8,
            LAST_UPDATE_SLOT_OFFSET..LAST_UPDATE_SLOT_OFFSET + 8,
        ],
        "prepared local market",
    );
    // Next slot: the persisted freshness re-stamps offset 616 to the new slot, and nothing else.
    let scheduled = svm
        .scheduled_overrides
        .get(&(base_slot + 1))
        .expect("read scheduled freshness")
        .expect("persisted freshness");
    assert_eq!(scheduled.len(), 1);
    assert!(scheduled[0].persist);
    assert!(!scheduled[0].fetch_before_use);
    svm.materialize_overrides_for_slot(&remote, base_slot + 1)
        .await
        .expect("materialize next slot");
    let next_account = svm.inner.get_account(&market_key).unwrap().unwrap();
    assert_eq!(next_account.lamports, applied_account.lamports);
    assert_eq!(next_account.owner, applied_account.owner);
    let next = next_account.data;
    assert_eq!(next.len(), applied.len());
    assert_eq!(
        decode_u64(&next, FAIR_VALUE_OFFSET, FAIR_VALUE_KEY),
        expected_fair_value,
        "persisted freshness must preserve the prepared price"
    );
    assert_eq!(
        decode_u64(&next, LAST_UPDATE_SLOT_OFFSET, STATE_KEY),
        base_slot + 1,
        "the persisted freshness must track the slot"
    );
    assert_only_within(
        &diff_indices(&next, applied),
        std::slice::from_ref(&(LAST_UPDATE_SLOT_OFFSET..LAST_UPDATE_SLOT_OFFSET + 8)),
        "persisted freshness next slot",
    );
}

// Swap-replay: the controlled experiment that proves the program reacts to the override.
//
// HumidiFi is only reachable through a router (DFlow), so this registers a native builtin under the
// router's id that re-emits a captured live HumidiFi CPI and runs the deployed program against the
// prepared market. It then re-encodes, doubles, and halves the fair value and asserts the real swap
// output moves exactly the predicted way. The one non-obvious setup detail: DFlow signs for a
// system-owned authority account (SYS0) as a read-only PDA signer, which the deployed program
// requires; sigverify is off, so the replay marks it a read-only signer.
//
// Source: transaction 3zevqwAa8u136UGE1bdBzP1o2dpFuc7X333dC1ut3T1uCgY6iidihfNJNoJ7tuyj3tHNin1i7HTFCUSFWqK7g1Si,
// slot 444225745, DFlow outer instruction 2 and its HumidiFi CPI. Only instruction metadata is
// captured: market, vaults, mints and executable bytes are fetched live on every run.

const DFLOW: Pubkey = Pubkey::from_str_const("DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH");

// The eighteen accounts of the captured HumidiFi CPI, in order.
const SIGNER0: Pubkey = Pubkey::from_str_const("4HJaX8K9mH9fMLGn4Xc5DjGdjDWXNSnnX5kkXFuzk2ET");
const BASE_VAULT: Pubkey = Pubkey::from_str_const("C3FzbX9n1YD2dow2dCmEv5uNyyf22Gb3TLAEqGBhw5fY");
const QUOTE_VAULT: Pubkey = Pubkey::from_str_const("3RWFAQBRkNGq7CMGcTLK3kXDgFTe9jgMeFYqk8nHwcWh");
const DEST_WSOL: Pubkey = Pubkey::from_str_const("CsqNfUnwbVbDQRQFUGCRCik8auK1ExfvwTYvM6Zc1uPe");
const USER_USDC: Pubkey = Pubkey::from_str_const("FMc3ZxJSYyT9JuJ21iDzQs5P2wRpS6JSwvT6ZRkYv6J9");
const CLOCK: Pubkey = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");
const TOKEN: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const SYS0: Pubkey = Pubkey::from_str_const("8xeaWCsJYxRoudEZGJWURdfrtFhLYZz9b4iHJnW5tb3d");
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const USDC_MINT: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const AUX12: Pubkey = Pubkey::from_str_const("8vqruQc1wB3YQpaP4fr1woJGULBQG3c7uj8A8nnSWo9");
const VOTE: Pubkey = Pubkey::from_str_const("J1to1yufRnoWn81KYg1XkTWzmKjnYSnmE2VY8DGUJ9Qv");
const ROUTE_STATE: Pubkey = Pubkey::from_str_const("EXNBiVYTJErnLRaz9hae8P4nePswG21qZJZX9wJLUDnY");
const AUX15: Pubkey = Pubkey::from_str_const("7Qca6CS6sExGXKh3UmJ5fpapc3YFwScVpjfhmi4ScWUM");
const AUX16: Pubkey = Pubkey::from_str_const("6iL7bcqz6tmLo821xcvgDDSvrPh7knjoMQpyZEuSxML3");
const AUX17: Pubkey = Pubkey::from_str_const("1kUMdzAeH1uEdNch7ZJrtfvSo351p3b2DJ6qf5TUWhC");
const LIVE_KEYS: [Pubkey; 7] = [
    SOL_USDC_MARKET,
    BASE_VAULT,
    QUOTE_VAULT,
    WSOL_MINT,
    USDC_MINT,
    VOTE,
    ROUTE_STATE,
];

const INPUT_QUOTE_AMOUNT: u64 = 2_427_890;
/// The captured swap instruction data (obfuscated; it encodes a fixed input of 2,427,890 USDC atoms).
const SWAP_DATA: [u8; 25] = [
    0x6f, 0x86, 0xa3, 0x50, 0xa2, 0x1a, 0xc2, 0xb9, 0xc9, 0xf4, 0x0b, 0xff, 0xe3, 0xba, 0xea, 0xc3,
    0x39, 0xff, 0x2d, 0xff, 0xe0, 0xba, 0xe9, 0xc3, 0x09,
];

/// The captured DFlow route data, so the Instructions sysvar shows a faithful router instruction.
const DFLOW_DATA_HEX: &str = "f8c69e91e17587c80600000025af1df3ba698aa144c79fb835e10649c7698eed21f706d1d24eb9a14f7dd0ce4c3fcd6752db35e4c5ec7f9c670b824cf28236bd4ab7af472aa63da2a26212570664597a1a000024bc9a000000323c31020000006a9991000000000006835faa1a00000000091f020000000381878ec3c80af75c0355798caf40a0297af20b25000000000001118e5964010000000000feb4a000000000003c020000";

fn dflow_data() -> Vec<u8> {
    (0..DFLOW_DATA_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&DFLOW_DATA_HEX[i..i + 2], 16).unwrap())
        .collect()
}

/// (pubkey, writable, signer) for each of the eighteen CPI accounts. DFlow signs for two system-owned
/// authority accounts (SIGNER0 and SYS0) as PDAs; sigverify is off, so the replay marks them signers.
fn cpi_layout() -> [(Pubkey, bool, bool); 18] {
    [
        (SIGNER0, true, true),
        (SOL_USDC_MARKET, true, false),
        (BASE_VAULT, true, false),
        (QUOTE_VAULT, true, false),
        (DEST_WSOL, true, false),
        (USER_USDC, true, false),
        (CLOCK, false, false),
        (TOKEN, false, false),
        (TOKEN, false, false),
        (SYS0, false, true),
        (WSOL_MINT, false, false),
        (USDC_MINT, false, false),
        (AUX12, false, false),
        (VOTE, false, false),
        (ROUTE_STATE, false, false),
        (AUX15, false, false),
        (AUX16, false, false),
        (AUX17, false, false),
    ]
}

fn humidifi_metas() -> Vec<AccountMeta> {
    cpi_layout()
        .into_iter()
        .map(|(key, writable, signer)| {
            if writable {
                AccountMeta::new(key, signer)
            } else {
                AccountMeta::new_readonly(key, signer)
            }
        })
        .collect()
}

// The builtin stands in for DFlow: it re-emits the captured HumidiFi CPI verbatim. Registered under
// the router id so HumidiFi's Instructions-sysvar caller check sees a router instruction.
declare_process_instruction!(HumidiFiRouteShim, 1, |invoke_context| {
    invoke_context.native_invoke_signed(
        Instruction {
            program_id: HUMIDIFI_PROGRAM,
            accounts: humidifi_metas(),
            data: SWAP_DATA.to_vec(),
        },
        &[],
    )?;
    Ok(())
});

fn token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let mut data = vec![0u8; spl_token_interface::state::Account::LEN];
    spl_token_interface::state::Account {
        mint,
        owner,
        amount,
        state: spl_token_interface::state::AccountState::Initialized,
        ..Default::default()
    }
    .pack_into_slice(&mut data);
    Account {
        lamports: 2_039_280,
        data,
        owner: TOKEN,
        ..Account::default()
    }
}

fn system_account(lamports: u64) -> Account {
    Account {
        lamports,
        owner: solana_pubkey::Pubkey::from_str_const("11111111111111111111111111111111"),
        ..Account::default()
    }
}

/// Runs the captured swap against a market whose data has been `mutate`d, returning the WSOL
/// the user received (the increase of the destination account).
fn run_swap(
    programdata: &Account,
    live: &[(Pubkey, Account)],
    clock_slot: Option<u64>,
    mutate: impl FnOnce(&mut Vec<u8>),
) -> Result<u64, String> {
    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(HUMIDIFI_PROGRAM, &programdata.data[45..])
        .map_err(|e| format!("load HumidiFi ELF: {e:?}"))?;
    svm.add_builtin(DFLOW, HumidiFiRouteShim::register);

    let mut market = live
        .iter()
        .find(|(key, _)| *key == SOL_USDC_MARKET)
        .map(|(_, account)| account.clone())
        .ok_or("market not in the live set")?;
    mutate(&mut market.data);
    for (key, account) in live {
        let account = if *key == SOL_USDC_MARKET {
            market.clone()
        } else {
            account.clone()
        };
        svm.set_account(*key, account)
            .map_err(|e| format!("seed {key}: {e:?}"))?;
    }

    // An implicit clock follows the mutated quote; staleness proofs must supply a fixed clock.
    let market_slot = decode_u64(&market.data, LAST_UPDATE_SLOT_OFFSET, STATE_KEY);
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = clock_slot.unwrap_or(market_slot);
    clock.unix_timestamp = 1_788_000_000;
    svm.set_sysvar(&clock);

    // The user pays USDC and receives WSOL; synthesize both token accounts plus the plain signers.
    svm.set_account(SIGNER0, system_account(1_000_000_000))
        .unwrap();
    svm.set_account(SYS0, system_account(1_244_010)).unwrap();
    for aux in [AUX12, AUX15, AUX16, AUX17] {
        svm.set_account(aux, system_account(1_000_000)).unwrap();
    }
    svm.set_account(USER_USDC, token_account(USDC_MINT, SIGNER0, 1_000_000_000))
        .unwrap();
    svm.set_account(DEST_WSOL, token_account(WSOL_MINT, SIGNER0, 0))
        .unwrap();

    let mut accounts = vec![AccountMeta::new_readonly(HUMIDIFI_PROGRAM, false)];
    accounts.extend(humidifi_metas());
    let shim_ix = Instruction {
        program_id: DFLOW,
        accounts,
        data: dflow_data(),
    };
    let compute = |tag: u8, value: &[u8]| Instruction {
        program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
        accounts: vec![],
        data: [vec![tag], value.to_vec()].concat(),
    };
    let message = Message::new_with_blockhash(
        &[compute(2, &1_400_000u32.to_le_bytes()), shim_ix],
        Some(&SIGNER0),
        &svm.latest_blockhash(),
    );
    let tx = Transaction {
        signatures: vec![Signature::default(); message.header.num_required_signatures as usize],
        message,
    };
    svm.send_transaction(tx)
        .map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n")))?;

    let source = svm.get_account(&USER_USDC).unwrap();
    assert_eq!(
        1_000_000_000 - token_amount(&source),
        INPUT_QUOTE_AMOUNT,
        "the captured instruction's quote input changed"
    );
    let dest = svm.get_account(&DEST_WSOL).unwrap();
    let amount = u64::from_le_bytes(dest.data[64..72].try_into().unwrap());
    Ok(amount)
}

#[tokio::test]
async fn humidifi_fair_value_moves_the_swap_output() {
    let fetched = live::fetch(&LIVE_KEYS).await;
    let programdata = live::fetch(&[HUMIDIFI_PROGRAMDATA]).await.remove(0);
    let live: Vec<(Pubkey, Account)> = LIVE_KEYS.into_iter().zip(fetched).collect();

    let market = &live.iter().find(|(k, _)| *k == SOL_USDC_MARKET).unwrap().1;
    let fair_value = decode_u64(&market.data, FAIR_VALUE_OFFSET, FAIR_VALUE_KEY);
    assert!(fair_value > 0, "the live market must carry a fair value");

    let set_fair_value = |value: u64| {
        move |data: &mut Vec<u8>| {
            *data = template_raw_apply(
                "humidifi-fair-value",
                std::collections::HashMap::from([(
                    "fair_value".to_string(),
                    serde_json::json!(value.to_string()),
                )]),
                0,
                data,
            );
        }
    };

    let baseline = run_swap(&programdata, &live, None, |_| {})
        .unwrap_or_else(|e| panic!("baseline swap did not execute:\n{e}"));
    let no_op =
        run_swap(&programdata, &live, None, set_fair_value(fair_value)).expect("re-encode current");
    let cheaper = run_swap(&programdata, &live, None, set_fair_value(fair_value / 2))
        .expect("halve fair value");
    let dearer = run_swap(&programdata, &live, None, set_fair_value(fair_value * 2));

    eprintln!(
        "HumidiFi swap output WSOL: baseline={baseline} no_op={no_op} half_price={cheaper} double_price={dearer:?}"
    );
    assert!(baseline > 0, "the captured swap must return WSOL");
    assert_eq!(
        no_op, baseline,
        "re-encoding the same value must not change the output"
    );
    assert!(
        cheaper > baseline,
        "halving the price (base cheaper) must return MORE base for the same quote input"
    );
    // Doubling the price makes the base twice as dear; the fixed quote input buys less, and the swap
    // either returns less or trips its own minimum-output guard. Either way the fair value gated it.
    match dearer {
        Ok(dearer) => assert!(
            dearer < baseline,
            "doubling the price must return less base"
        ),
        Err(error) => assert!(
            error.contains("Custom") || error.contains("insufficient") || error.contains("0x"),
            "doubling the price should reduce output or revert on min-out, got: {error}"
        ),
    }
}

#[tokio::test]
async fn humidifi_builder_price_matches_the_executed_exchange_rate() {
    let fetched = live::fetch(&LIVE_KEYS).await;
    let programdata = live::fetch(&[HUMIDIFI_PROGRAMDATA]).await.remove(0);
    let live: Vec<(Pubkey, Account)> = LIVE_KEYS.into_iter().zip(fetched).collect();
    let account = |address| &live.iter().find(|(key, _)| *key == address).unwrap().1;
    let market = HumidiFiMarket::validate(
        SOL_USDC_MARKET,
        account(SOL_USDC_MARKET),
        account(WSOL_MINT),
        account(USDC_MINT),
    )
    .expect("valid live market");
    let slot = decode_u64(
        &account(SOL_USDC_MARKET).data,
        LAST_UPDATE_SLOT_OFFSET,
        STATE_KEY,
    ) + 100;
    let remote = Some((live::client(), CommitmentConfig::confirmed()));

    for price in [100u64, 208] {
        let preparation = build_humidifi_fair_value_scenario(&market, &price.to_string())
            .expect("build human-price scenario");
        let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        svm.inner
            .set_account(SOL_USDC_MARKET, account(SOL_USDC_MARKET).clone())
            .expect("seed prepared market");
        svm.register_scenario(preparation.scenario, Some(slot))
            .expect("register price scenario");
        svm.materialize_overrides_for_slot(&remote, slot)
            .await
            .expect("materialize price scenario");
        let mut prepared = live.clone();
        prepared
            .iter_mut()
            .find(|(key, _)| *key == SOL_USDC_MARKET)
            .unwrap()
            .1 = svm.inner.get_account(&SOL_USDC_MARKET).unwrap().unwrap();
        let output = run_swap(&programdata, &prepared, Some(slot), |_| {})
            .expect("builder-priced swap must fill");
        let expected = u64::try_from(
            u128::from(INPUT_QUOTE_AMOUNT) * 10u128.pow(u32::from(market.base_decimals))
                / (u128::from(price) * 10u128.pow(u32::from(market.quote_decimals))),
        )
        .unwrap();
        assert!(
            output.abs_diff(expected) <= expected / 100,
            "price {price}: output {output} differs from independently priced output {expected} by more than 1%"
        );
        eprintln!("HumidiFi builder price={price}: WSOL output={output}, expected={expected}");
    }
}

#[tokio::test]
async fn humidifi_stale_quote_lands_the_rejection_boundary() {
    let fetched = live::fetch(&LIVE_KEYS).await;
    let programdata = live::fetch(&[HUMIDIFI_PROGRAMDATA]).await.remove(0);
    let live: Vec<(Pubkey, Account)> = LIVE_KEYS.into_iter().zip(fetched).collect();
    let account = |address| &live.iter().find(|(key, _)| *key == address).unwrap().1;
    let market_account = account(SOL_USDC_MARKET);
    let market = HumidiFiMarket::validate(
        SOL_USDC_MARKET,
        market_account,
        account(WSOL_MINT),
        account(USDC_MINT),
    )
    .expect("valid live market");
    let limit = decode_u64(&market_account.data, MAX_STALENESS_OFFSET, STATE_KEY);
    assert_eq!(limit, market.max_staleness_slots);
    assert_eq!(
        limit, 2,
        "the tested SOL/USDC market's staleness limit changed"
    );
    let clock = decode_u64(&market_account.data, LAST_UPDATE_SLOT_OFFSET, STATE_KEY) + 100;

    let fresh = template_raw_apply(
        "humidifi-freshness",
        std::collections::HashMap::from([(
            "last_update_slot".to_string(),
            serde_json::Value::Null,
        )]),
        clock,
        &market_account.data,
    );
    assert_eq!(
        decode_u64(&fresh, LAST_UPDATE_SLOT_OFFSET, STATE_KEY),
        clock
    );
    let control = run_swap(&programdata, &live, Some(clock), |data| *data = fresh)
        .expect("fresh quote must fill at the fixed clock");
    assert!(control > 0);

    for configured_limit in [limit, 10] {
        let mut configured = market_account.data.clone();
        encode_masked_u64(
            &mut configured,
            MAX_STALENESS_OFFSET,
            STATE_KEY,
            configured_limit,
        );
        for age in [configured_limit + 1, configured_limit] {
            let staged = template_raw_apply(
                "humidifi-stale-quote",
                std::collections::HashMap::from([(
                    "last_update_slot".to_string(),
                    serde_json::json!(-i64::try_from(age).unwrap()),
                )]),
                clock,
                &configured,
            );
            assert_eq!(
                decode_u64(&staged, LAST_UPDATE_SLOT_OFFSET, STATE_KEY),
                clock - age,
            );
            assert_only_within(
                &live::diff_indices(&market_account.data, &staged),
                &[
                    MAX_STALENESS_OFFSET..MAX_STALENESS_OFFSET + 8,
                    LAST_UPDATE_SLOT_OFFSET..LAST_UPDATE_SLOT_OFFSET + 8,
                ],
                "staleness boundary",
            );
            let result = run_swap(&programdata, &live, Some(clock), |data| *data = staged);
            if age > configured_limit {
                let error =
                    result.expect_err("a quote older than the configured limit must reject");
                assert!(
                    error.contains("Custom(1027565)"),
                    "unexpected error: {error}"
                );
                eprintln!("HumidiFi staleness limit={configured_limit} age={age}: {error}");
            } else {
                let output = result.expect("a quote at the configured maximum age must fill");
                assert!(output > 0);
                eprintln!(
                    "HumidiFi staleness limit={configured_limit} age={age}: WSOL output={output}"
                );
            }
        }
    }
}

/// Builds a liquidity scenario against the live accounts, materializes it through the production
/// path in a fresh Surfnet, and returns the live set with the prepared market and vaults swapped in.
async fn prepared_liquidity(
    live: &[(Pubkey, Account)],
    market: &HumidiFiMarket,
    base_remaining_bps: u16,
    quote_remaining_bps: u16,
) -> Vec<(Pubkey, Account)> {
    let account = |address| &live.iter().find(|(key, _)| *key == address).unwrap().1;
    let base_slot = decode_u64(
        &account(SOL_USDC_MARKET).data,
        LAST_UPDATE_SLOT_OFFSET,
        STATE_KEY,
    ) + 100;
    let scenario = build_humidifi_liquidity_scenario(
        market,
        account(SOL_USDC_MARKET),
        account(BASE_VAULT),
        account(QUOTE_VAULT),
        base_remaining_bps,
        quote_remaining_bps,
    )
    .expect("build liquidity scenario");

    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    for key in [
        SOL_USDC_MARKET,
        BASE_VAULT,
        QUOTE_VAULT,
        WSOL_MINT,
        USDC_MINT,
    ] {
        svm.inner
            .set_account(key, account(key).clone())
            .expect("seed account");
    }
    svm.register_scenario(scenario, Some(base_slot))
        .expect("register liquidity scenario");
    svm.materialize_overrides_for_slot(&None, base_slot)
        .await
        .expect("materialize liquidity scenario");

    live.iter()
        .map(|(key, original)| {
            let prepared = if [SOL_USDC_MARKET, BASE_VAULT, QUOTE_VAULT].contains(key) {
                svm.inner.get_account(key).unwrap().unwrap()
            } else {
                original.clone()
            };
            assert_eq!(
                prepared.data.len(),
                original.data.len(),
                "{key}: account length changed"
            );
            assert_eq!(
                prepared.owner, original.owner,
                "{key}: account owner changed"
            );
            assert_eq!(
                prepared.lamports, original.lamports,
                "{key}: lamports changed"
            );
            (*key, prepared)
        })
        .collect()
}

fn token_amount(account: &Account) -> u64 {
    u64::from_le_bytes(account.data[64..72].try_into().unwrap())
}

#[tokio::test]
async fn humidifi_liquidity_builder_exhausts_only_the_selected_side() {
    let fetched = live::fetch(&LIVE_KEYS).await;
    let programdata = live::fetch(&[HUMIDIFI_PROGRAMDATA]).await.remove(0);
    let live: Vec<(Pubkey, Account)> = LIVE_KEYS.into_iter().zip(fetched).collect();
    let account = |set: &[(Pubkey, Account)], address| {
        set.iter()
            .find(|(key, _)| *key == address)
            .unwrap()
            .1
            .clone()
    };
    let market = HumidiFiMarket::validate(
        SOL_USDC_MARKET,
        &account(&live, SOL_USDC_MARKET),
        &account(&live, WSOL_MINT),
        &account(&live, USDC_MINT),
    )
    .expect("valid live market");
    assert_eq!(
        humidifi_vault_addresses(&account(&live, SOL_USDC_MARKET)).unwrap(),
        [BASE_VAULT, QUOTE_VAULT]
    );
    let live_base = token_amount(&account(&live, BASE_VAULT));
    let baseline = run_swap(&programdata, &live, None, |_| {}).expect("baseline swap");
    assert!(baseline > 0);

    // Draining the base vault leaves the swap nothing to pay out: the token transfer fails.
    let drained = prepared_liquidity(&live, &market, 0, 10_000).await;
    let base_vault = account(&drained, BASE_VAULT);
    assert_eq!(token_amount(&base_vault), 0);
    assert_only_within(
        &live::diff_indices(&account(&live, BASE_VAULT).data, &base_vault.data),
        std::slice::from_ref(&(64..72)),
        "drained base vault",
    );
    assert_eq!(base_vault.lamports, account(&live, BASE_VAULT).lamports);
    assert_eq!(account(&drained, QUOTE_VAULT), account(&live, QUOTE_VAULT));
    assert_only_within(
        &live::diff_indices(
            &account(&live, SOL_USDC_MARKET).data,
            &account(&drained, SOL_USDC_MARKET).data,
        ),
        std::slice::from_ref(&(LAST_UPDATE_SLOT_OFFSET..LAST_UPDATE_SLOT_OFFSET + 8)),
        "liquidity scenario market",
    );
    let error = run_swap(&programdata, &drained, None, |_| {})
        .expect_err("a drained base vault must fail the swap");
    assert!(
        error.contains("insufficient funds"),
        "unexpected error: {error}"
    );

    // A sliver of base inventory pushes the maker into its skew regime: it still fills, but at a
    // fraction of the fair-value output.
    let thin = prepared_liquidity(&live, &market, 50, 10_000).await;
    assert_eq!(
        token_amount(&account(&thin, BASE_VAULT)),
        live_base / 200,
        "0.50% of the live base balance, floored"
    );
    assert_eq!(account(&thin, QUOTE_VAULT), account(&live, QUOTE_VAULT));
    let thin_output = run_swap(&programdata, &thin, None, |_| {}).expect("thin base vault fills");
    eprintln!("HumidiFi liquidity: baseline={baseline} base at 0.50%={thin_output}");
    assert!(
        thin_output * 100 < baseline,
        "a thin base vault must collapse the output, got {thin_output} against {baseline}"
    );

    // The quote vault only receives this direction's input, so draining it changes nothing.
    let quote_drained = prepared_liquidity(&live, &market, 10_000, 0).await;
    assert_eq!(token_amount(&account(&quote_drained, QUOTE_VAULT)), 0);
    assert_eq!(
        account(&quote_drained, BASE_VAULT),
        account(&live, BASE_VAULT)
    );
    let unchanged =
        run_swap(&programdata, &quote_drained, None, |_| {}).expect("drained quote vault fills");
    assert_eq!(unchanged, baseline);
}
