//! Chainlink integration tests.
//!
//! These read a live data feed and the deployed Jupiter Lend oracle program from mainnet, so they
//! need a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests chainlink
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.

use std::collections::HashMap;

use base64::{Engine, prelude::BASE64_STANDARD};
use solana_account::Account;
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use surfpool_types::{AccountAddress, DEFAULT_MAINNET_RPC_URL, OverrideInstance, Scenario};

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const STORE_PROGRAM: Pubkey =
    Pubkey::from_str_const("HEvSKofvBgfaexv23kMabbYqxasxU3mQ4ibBMEmJWHny");
const SOL_USD_FEED: Pubkey = Pubkey::from_str_const("CH31Xns5z3M1cTAbKW34jcxPPciazARpijcHj9rxtemt");
const JUPITER_LEND_ORACLE_PROGRAM: Pubkey =
    Pubkey::from_str_const("jupnw4B6Eqs7ft6rxpzYLJZYSnrpRgPcr589n5Kv4oc");
const JUPITER_LEND_ORACLE_PROGRAMDATA: Pubkey =
    Pubkey::from_str_const("A5jX8nt82Pj5aQyrtooMPkgrfk5myqNRjuxC6urSJKAT");
/// A Jupiter Lend oracle whose only source is the SOL / USD feed.
const JUPITER_LEND_SOL_USD_ORACLE: Pubkey =
    Pubkey::from_str_const("6uU9EvKL1rSTyw3ZqKz4hMNEB4NBCo3D3rg3n13fymHm");
const GET_EXCHANGE_RATE_OPERATE: [u8; 8] = [174, 166, 126, 10, 122, 153, 94, 203];
const GET_EXCHANGE_RATE_LIQUIDATE: [u8; 8] = [228, 169, 73, 39, 91, 82, 27, 5];

/// The round chainlink_solana::v2::read_feed_v2 reads: slot, timestamp and answer.
const ROUND_FIELDS: [(usize, usize); 3] = [(200, 8), (208, 4), (216, 16)];

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_MAINNET_RPC_URL.to_string()),
    )
    .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
    .await
    .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
    .into_iter()
    .zip(addresses)
    .map(|(result, address)| {
        result.map_account().unwrap_or_else(|_| {
            panic!("{address} no longer exists on mainnet; the test needs a new address")
        })
    })
    .collect()
}

fn live_round(feed: &Account) -> (u64, u32, i128) {
    let data = &feed.data;
    (
        u64::from_le_bytes(data[200..208].try_into().unwrap()),
        u32::from_le_bytes(data[208..212].try_into().unwrap()),
        i128::from_le_bytes(data[216..232].try_into().unwrap()),
    )
}

/// Registers and plays a one-override scenario at `slot` and returns the feed it produced.
async fn play(
    locker: &SurfnetSvmLocker,
    slot: u64,
    values: [(&str, serde_json::Value); 3],
) -> Account {
    let mut scenario = Scenario::new(
        "Chainlink SOL / USD".to_string(),
        "Rewrite the live round".to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            "chainlink-data-feed".to_string(),
            0,
            AccountAddress::Pubkey(SOL_USD_FEED.to_string()),
        )
        .with_values(
            values
                .into_iter()
                .map(|(path, value)| (path.to_string(), value))
                .collect(),
        ),
    );
    locker.register_scenario(scenario, Some(slot)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, slot)
        .await
        .unwrap();
    locker
        .with_svm_reader(|svm| svm.get_account(&SOL_USD_FEED))
        .unwrap()
        .expect("the feed should exist")
}

#[tokio::test(flavor = "multi_thread")]
async fn data_feed_override_writes_only_the_live_round() {
    let live = fetch(&[SOL_USD_FEED]).await.remove(0);
    assert_eq!(live.owner, STORE_PROGRAM);
    assert_eq!(live.data.len(), 248, "a single-round feed is 248 bytes");
    assert_eq!(
        u32::from_le_bytes(live.data[148..152].try_into().unwrap()),
        1,
        "live_length must be 1, the only shape the Chainlink SDK reads"
    );

    let registry = TemplateRegistry::new();
    let template = registry.get("chainlink-data-feed").unwrap();
    // The llm_context finds a feed by the description at byte 106; its example must match.
    assert_eq!(&live.data[106..116], b"SOL / USD\0");
    assert!(
        template
            .llm_context
            .as_deref()
            .unwrap()
            .contains(&BASE64_STANDARD.encode(b"SOL / USD\0"))
    );

    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let identity = svm
        .get_forged_account_data(&SOL_USD_FEED, &live.data, template.idl(), &HashMap::new())
        .unwrap();
    assert_eq!(identity, live.data, "no-op round trip changed bytes");

    let (live_slot, live_timestamp, live_answer) = live_round(&live);
    let parsed = match svm
        .encode_ui_account(
            &SOL_USD_FEED,
            &live,
            UiAccountEncoding::JsonParsed,
            None,
            None,
        )
        .data
    {
        UiAccountData::Json(parsed) => parsed.parsed,
        other => panic!("the feed should decode through the bundled IDL, got {other:?}"),
    };
    let round = &parsed["live"];
    assert_eq!(round["slot"], serde_json::json!(live_slot), "{parsed}");
    assert_eq!(
        round["timestamp"],
        serde_json::json!(live_timestamp),
        "{parsed}"
    );
    assert_eq!(
        round["answer"].to_string().trim_matches('"'),
        live_answer.to_string(),
        "{parsed}"
    );

    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm| svm.set_account(&SOL_USD_FEED, live.clone()).unwrap());
    let (slot, timestamp, answer) = (live_slot + 7, live_timestamp + 3, live_answer / 3);
    let forged = play(
        &locker,
        100,
        [
            ("live.slot", serde_json::json!(slot)),
            ("live.timestamp", serde_json::json!(timestamp)),
            ("live.answer", serde_json::json!(answer.to_string())),
        ],
    )
    .await;

    assert_eq!(forged.owner, live.owner);
    assert_eq!(forged.lamports, live.lamports);
    assert_eq!(forged.data.len(), live.data.len());
    assert_eq!(live_round(&forged), (slot, timestamp, answer));
    let outside_round: Vec<usize> = (0..live.data.len())
        .filter(|&i| forged.data[i] != live.data[i])
        .filter(|i| {
            !ROUND_FIELDS
                .iter()
                .any(|(offset, width)| (*offset..offset + width).contains(i))
        })
        .collect();
    assert!(outside_round.is_empty(), "changed {outside_round:?}");
}

/// Jupiter Lend prices through `chainlink_solana::v2::read_feed_v2`, so its oracle is an
/// authoritative reader of the round the template writes.
#[tokio::test(flavor = "multi_thread")]
async fn jupiter_lend_reads_the_overridden_round() {
    let [feed, oracle, programdata]: [Account; 3] = fetch(&[
        SOL_USD_FEED,
        JUPITER_LEND_SOL_USD_ORACLE,
        JUPITER_LEND_ORACLE_PROGRAMDATA,
    ])
    .await
    .try_into()
    .unwrap();
    // Oracle: discriminator, nonce u16, then a vec of 66-byte sources whose first field is the
    // source account and whose last byte is the source type (4 = Chainlink).
    let nonce = u16::from_le_bytes(oracle.data[8..10].try_into().unwrap());
    assert_eq!(
        u32::from_le_bytes(oracle.data[10..14].try_into().unwrap()),
        1,
        "{JUPITER_LEND_SOL_USD_ORACLE} should have one source; the test needs a new oracle"
    );
    assert_eq!(
        Pubkey::try_from(&oracle.data[14..46]).unwrap(),
        SOL_USD_FEED,
        "{JUPITER_LEND_SOL_USD_ORACLE} should read the SOL / USD feed"
    );
    assert_eq!(oracle.data[79], 4, "the source should be a Chainlink feed");

    let (live_slot, live_timestamp, live_answer) = live_round(&feed);
    let clock = Clock {
        slot: live_slot + 10,
        unix_timestamp: live_timestamp as i64 + 4,
        ..Clock::default()
    };
    let (svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(svm);
    locker.with_svm_writer(|svm| {
        svm.inner.set_sysvar(&clock);
        svm.inner
            .svm
            .add_program(JUPITER_LEND_ORACLE_PROGRAM, &programdata.data[45..])
            .unwrap();
        svm.set_account(&SOL_USD_FEED, feed.clone()).unwrap();
        svm.set_account(&JUPITER_LEND_SOL_USD_ORACLE, oracle.clone())
            .unwrap();
    });
    let rate = |discriminator: [u8; 8]| -> Result<u128, String> {
        let payer = Keypair::new();
        locker.with_svm_writer(|svm| {
            svm.inner.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
            let transaction = Transaction::new_signed_with_payer(
                &[Instruction {
                    program_id: JUPITER_LEND_ORACLE_PROGRAM,
                    accounts: vec![
                        AccountMeta::new_readonly(JUPITER_LEND_SOL_USD_ORACLE, false),
                        AccountMeta::new_readonly(SOL_USD_FEED, false),
                    ],
                    data: [discriminator.as_slice(), &nonce.to_le_bytes()].concat(),
                }],
                Some(&payer.pubkey()),
                &[&payer],
                svm.inner.svm.latest_blockhash(),
            );
            svm.inner
                .send_transaction(transaction)
                .map(|meta| u128::from_le_bytes(meta.return_data.data.try_into().unwrap()))
                .map_err(|failed| format!("{:?}, logs: {:?}", failed.err, failed.meta.logs))
        })
    };

    let before = rate(GET_EXCHANGE_RATE_OPERATE).unwrap();
    assert!(before > 0);

    let crashed = live_answer / 4;
    play(
        &locker,
        100,
        [
            ("live.slot", serde_json::json!(clock.slot)),
            ("live.timestamp", serde_json::json!(clock.unix_timestamp)),
            ("live.answer", serde_json::json!(crashed.to_string())),
        ],
    )
    .await;
    let after = rate(GET_EXCHANGE_RATE_OPERATE).unwrap();
    let expected = before * crashed as u128 / live_answer as u128;
    assert!(
        after.abs_diff(expected) <= 1,
        "rate {before} -> {after}, expected {expected} for answer {live_answer} -> {crashed}"
    );

    // The limits the template's llm_context states. Jupiter Lend ages a round by its slot alone:
    // 2644 slots fail borrows and withdrawals, 28804 fail liquidations, a round ahead of the
    // clock fails both, and the timestamp is not read.
    for (scenario_slot, (slot, timestamp, operate, liquidate)) in (101..).zip([
        (clock.slot - 2_643, clock.unix_timestamp, true, true),
        (clock.slot - 2_644, clock.unix_timestamp, false, true),
        (clock.slot - 28_803, clock.unix_timestamp, false, true),
        (clock.slot - 28_804, clock.unix_timestamp, false, false),
        (clock.slot + 1, clock.unix_timestamp, false, false),
        (clock.slot, 0, true, true),
    ]) {
        play(
            &locker,
            scenario_slot,
            [
                ("live.slot", serde_json::json!(slot)),
                ("live.timestamp", serde_json::json!(timestamp)),
                ("live.answer", serde_json::json!(crashed.to_string())),
            ],
        )
        .await;
        let outcome = (
            rate(GET_EXCHANGE_RATE_OPERATE),
            rate(GET_EXCHANGE_RATE_LIQUIDATE),
        );
        assert_eq!(
            (outcome.0.is_ok(), outcome.1.is_ok()),
            (operate, liquidate),
            "round slot {slot} at clock slot {}: {outcome:?}",
            clock.slot
        );
    }
}
