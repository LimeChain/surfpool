//! Project 0 (marginfi v2) integration tests.
//!
//! Like the Kamino suite these fetch real mainnet accounts, so they need a network connection and
//! are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests project0
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! A correct byte offset does not show that the risk engine reads it, so the last tests run the
//! deployed program over overridden state.

use std::collections::HashMap;

use litesvm::LiteSVM;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    tests::helpers::diff_indices,
    types::RemoteRpcResult,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const PROGRAM: &str = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";
const PROGRAM_DATA: &str = "4Q8u2ny8YYgytJEncwZQfVWbd5axZtfgDxYKHehvPQR7";
const DEPLOYED_SLOT: u64 = 456093551;
const GROUP: &str = "4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8";
const USDC_BANK: &str = "2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB";
const SOL_BANK: &str = "CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh";
const ACCOUNT: &str = "CGnzJGWscJAasjwngK1Jixx4xHfxF5EZaMje6uRtnbMy";
const USDC_BANK_ORACLE: &str = "6HAuqASbHEh4w4REJEUUUCginTLfj1kwCh215ZLtMkrT";
const SOL_BANK_ORACLE: &str = "7AviUf9nL62mcxNbQGKm4nKDQnPjswo6c5MX4D57HmyE";

/// Fixed-point Bank fields and their offsets in program 0.1.12.
const BANK_FIXED_POINT: [(&str, usize); 10] = [
    ("asset_share_value.value", 80),
    ("liability_share_value.value", 96),
    ("total_liability_shares.value", 256),
    ("total_asset_shares.value", 272),
    ("config.asset_weight_init.value", 296),
    ("config.asset_weight_maint.value", 312),
    ("config.liability_weight_init.value", 328),
    ("config.liability_weight_maint.value", 344),
    ("cb_reference_price.value", 1800),
    ("cb_window_reference_price.value", 1816),
];

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[&str]) -> Vec<solana_account::Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let pubkeys: Vec<Pubkey> = addresses
        .iter()
        .map(|a| Pubkey::from_str_const(a))
        .collect();

    client
        .get_multiple_accounts(&pubkeys, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| match result {
            GetAccountResult::FoundAccount(_, account, _)
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
            GetAccountResult::None(_) => {
                panic!("{address} no longer exists on mainnet; the test needs a new address")
            }
        })
        .collect()
}

fn read_i128(data: &[u8], offset: usize) -> i128 {
    i128::from_le_bytes(data[offset..offset + 16].try_into().unwrap())
}

/// Offset of a balance slot's first byte: `lending_account.balances` starts at 72, 104 bytes each.
fn balance_slot(slot: usize) -> usize {
    72 + 104 * slot
}

/// A failure here means the bundled IDL disagrees with the live on-chain layout.
#[tokio::test]
async fn real_mainnet_accounts_round_trip_unchanged() {
    let cases: &[(&str, &str, &str)] = &[
        ("project0-bank-limits", "Bank", USDC_BANK),
        ("project0-bank-limits", "Bank", SOL_BANK),
        ("project0-account-balance", "MarginfiAccount", ACCOUNT),
    ];

    let addresses: Vec<&str> = cases.iter().map(|(_, _, a)| *a).collect();
    let accounts = fetch(&addresses).await;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let pubkey = Pubkey::new_unique();

    for ((template_id, account_name, _), account) in cases.iter().zip(&accounts) {
        let data = &account.data;
        let template = registry
            .get(template_id)
            .unwrap_or_else(|| panic!("template {template_id} should exist"));

        let account_def = template
            .idl()
            .accounts
            .iter()
            .find(|a| a.name == *account_name)
            .unwrap_or_else(|| panic!("{account_name} not in the IDL"));
        assert_eq!(
            &data[..8],
            account_def.discriminator.as_slice(),
            "{account_name} discriminator does not match the IDL"
        );

        let forged = surfnet_svm
            .get_forged_account_data(&pubkey, data, template.idl(), &HashMap::new())
            .unwrap_or_else(|e| panic!("live {account_name} failed to round-trip: {e}"));

        assert_eq!(
            forged.len(),
            data.len(),
            "{account_name} changed size on round-trip"
        );
        let diffs = diff_indices(&forged, data);
        assert!(
            diffs.is_empty(),
            "live {account_name} changed at {} byte(s), first at {:?}",
            diffs.len(),
            diffs.first()
        );
    }
}

/// Runs a getProgramAccounts call on the Project 0 program against mainnet, as the llm_context
/// recipes write it.
async fn program_accounts(
    encoding: UiAccountEncoding,
    data_slice: UiDataSliceConfig,
    memcmps: &[(usize, &str)],
) -> Vec<(Pubkey, solana_account_decoder::UiAccount)> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let filters = memcmps
        .iter()
        .map(|(offset, bytes)| {
            RpcFilterType::Memcmp(Memcmp::new(
                *offset,
                MemcmpEncodedBytes::Base58(bytes.to_string()),
            ))
        })
        .collect();
    let config = RpcAccountInfoConfig {
        encoding: Some(encoding),
        data_slice: Some(data_slice),
        commitment: Some(CommitmentConfig::confirmed()),
        ..Default::default()
    };
    match client
        .get_program_accounts(&Pubkey::from_str_const(PROGRAM), config, Some(filters))
        .await
        .unwrap_or_else(|e| panic!("getProgramAccounts {memcmps:?} failed: {e}"))
    {
        RemoteRpcResult::Ok(found) => found,
        RemoteRpcResult::MethodNotSupported => panic!("the RPC does not serve getProgramAccounts"),
    }
}

const ORACLE_SLICE: UiDataSliceConfig = UiDataSliceConfig {
    offset: 610,
    length: 32,
};

/// The bank lookup the llm_context teaches: Bank discriminator, main group, mint and a Pyth push
/// oracle setup, with the bank's price account sliced from config.oracle_keys.0. It filters on the
/// discriminator because program upgrades change the Bank's size.
#[tokio::test]
async fn llm_context_bank_lookup_finds_the_native_sol_and_usdc_banks() {
    for (mint, bank, oracle) in [
        (
            "So11111111111111111111111111111111111111112",
            SOL_BANK,
            SOL_BANK_ORACLE,
        ),
        (
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            USDC_BANK,
            USDC_BANK_ORACLE,
        ),
    ] {
        let filters = [(0, "QnTef4UXSzF"), (41, GROUP), (8, mint), (609, "4")];
        let found = program_accounts(UiAccountEncoding::Base58, ORACLE_SLICE, &filters).await;

        assert_eq!(found.len(), 1, "expected one native bank for {mint}");
        let (pubkey, account) = &found[0];
        assert_eq!(pubkey.to_string(), bank);
        let price_account = account.data.decode().expect("base58 data slice");
        assert_eq!(
            Pubkey::try_from(price_account.as_slice())
                .unwrap()
                .to_string(),
            oracle
        );
    }
}

/// A token whose banks are not priced straight from Pyth (here jitoSOL, a PythLST bank) has no
/// result with the oracle setup filter; the llm_context's second call, without it, finds the bank.
#[tokio::test]
async fn llm_context_bank_lookup_falls_back_for_a_bank_not_priced_by_pyth_push() {
    const JITOSOL_MINT: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";
    const JITOSOL_BANK: &str = "Bohoc1ikHLD7xKJuzTyiTyCwzaL5N7ggJQu75A8mKYM8";
    let filters = [(0, "QnTef4UXSzF"), (41, GROUP), (8, JITOSOL_MINT)];

    let with_oracle_filter = [filters.as_slice(), &[(609, "4")]].concat();
    let pyth_push =
        program_accounts(UiAccountEncoding::Base58, ORACLE_SLICE, &with_oracle_filter).await;
    assert!(
        pyth_push.is_empty(),
        "jitoSOL now has a Pyth push bank: {pyth_push:?}"
    );

    let found = program_accounts(UiAccountEncoding::Base58, ORACLE_SLICE, &filters).await;
    let banks: Vec<String> = found.iter().map(|(pubkey, _)| pubkey.to_string()).collect();
    assert_eq!(banks, [JITOSOL_BANK], "expected the one jitoSOL bank");
}

/// The wallet lookup the llm_context teaches, sent as written: base58 is refused for accounts this
/// large, so it asks for base64 with a zero-length slice and reads only the pubkeys.
#[tokio::test]
async fn llm_context_wallet_lookup_finds_the_wallets_account() {
    let account = fetch(&[ACCOUNT]).await.remove(0);
    let wallet = Pubkey::new_from_array(account.data[40..72].try_into().unwrap()).to_string();
    let empty = UiDataSliceConfig {
        offset: 0,
        length: 0,
    };
    let filters = [(0, "CKkRR4La3xu"), (8, GROUP), (40, wallet.as_str())];

    let found = program_accounts(UiAccountEncoding::Base64, empty, &filters).await;
    assert!(
        found
            .iter()
            .any(|(pubkey, _)| pubkey.to_string() == ACCOUNT),
        "the wallet's account {ACCOUNT} is missing from {found:?}"
    );
}

/// The vendored IDL retypes `WrappedI80F48.value` from `[u8; 16]` to `i128`. Writing back the
/// little-endian i128 read from the raw bytes must be a no-op, which pins the byte order.
#[tokio::test]
async fn fixed_point_fields_read_as_little_endian_i128() {
    let fetched = fetch(&[SOL_BANK, ACCOUNT]).await;
    let (bank_data, account_data) = (&fetched[0].data, &fetched[1].data);

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let pubkey = Pubkey::new_unique();

    let bank = registry.get("project0-bank-shares").unwrap();
    let overrides: HashMap<String, serde_json::Value> = BANK_FIXED_POINT
        .iter()
        .map(|(path, offset)| {
            let raw = read_i128(bank_data, *offset);
            (path.to_string(), serde_json::json!(raw.to_string()))
        })
        .collect();
    let forged = surfnet_svm
        .get_forged_account_data(&pubkey, bank_data, bank.idl(), &overrides)
        .expect("write-back on the live SOL bank");
    assert!(
        diff_indices(&forged, bank_data).is_empty(),
        "write-back changed the bank"
    );

    let one = 1i128 << 48;
    let weight = read_i128(bank_data, 312);
    assert!(
        weight > 0 && weight <= 2 * one,
        "SOL asset_weight_maint should be between 0 and 2, got {weight}"
    );

    let account = registry.get("project0-account-balance").unwrap();
    let mut overrides = HashMap::new();
    for slot in 0..16 {
        let base = balance_slot(slot);
        overrides.insert(
            format!("lending_account.balances.{slot}.asset_shares.value"),
            serde_json::json!(read_i128(account_data, base + 40).to_string()),
        );
        overrides.insert(
            format!("lending_account.balances.{slot}.liability_shares.value"),
            serde_json::json!(read_i128(account_data, base + 56).to_string()),
        );
    }
    let forged = surfnet_svm
        .get_forged_account_data(&pubkey, account_data, account.idl(), &overrides)
        .expect("write-back on the live account");
    assert!(
        diff_indices(&forged, account_data).is_empty(),
        "write-back changed the account"
    );
}

/// Writes every property of every Project 0 template alone, against real padding and live enums,
/// and checks that only its own bytes change.
#[tokio::test]
async fn each_template_touches_only_its_offsets() {
    let fetched = fetch(&[SOL_BANK, ACCOUNT]).await;
    let (bank_data, account_data) = (&fetched[0].data, &fetched[1].data);

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let pubkey = Pubkey::new_unique();

    let fixed = |v: i128| (serde_json::json!(v.to_string()), v.to_le_bytes().to_vec());
    let int = |v: u64, width: usize| (serde_json::json!(v), v.to_le_bytes()[..width].to_vec());
    let key = |k: Pubkey| (serde_json::json!(k.to_string()), k.to_bytes().to_vec());

    // (path, offset, (value, bytes)). Each value differs from the live one.
    let mut bank_rows: Vec<_> = BANK_FIXED_POINT
        .iter()
        .map(|(path, offset)| (*path, *offset, fixed(123_456_789_012_345)))
        .collect();
    bank_rows.extend([
        ("config.deposit_limit", 360, int(12_345, 8)),
        (
            "config.interest_rate_config.zero_util_rate",
            496,
            int(11, 4),
        ),
        (
            "config.interest_rate_config.hundred_util_rate",
            500,
            int(22, 4),
        ),
        ("config.interest_rate_config.points.0.util", 504, int(33, 4)),
        ("config.interest_rate_config.points.0.rate", 508, int(44, 4)),
        ("config.interest_rate_config.curve_type", 544, int(2, 1)),
        (
            "config.operational_state",
            608,
            (serde_json::json!("ReduceOnly"), vec![2]),
        ),
        ("config.borrow_limit", 776, int(23_456, 8)),
        ("config.total_asset_value_init_limit", 792, int(34_567, 8)),
        ("config.oracle_max_age", 800, int(3_600, 2)),
        ("config.oracle_max_confidence", 804, int(777_777, 4)),
        ("config.cb_window_max_up_bps", 770, int(2_100, 2)),
        ("config.cb_window_max_down_bps", 772, int(5_100, 2)),
        ("config.cb_deviation_bps_tiers.0", 824, int(650, 2)),
        ("config.cb_deviation_bps_tiers.1", 826, int(1_100, 2)),
        ("config.cb_deviation_bps_tiers.2", 828, int(1_900, 2)),
    ]);
    let slot = balance_slot(0);
    let account_rows = vec![
        ("lending_account.balances.0.active", slot, int(7, 1)),
        (
            "lending_account.balances.0.bank_pk",
            slot + 1,
            key(Pubkey::new_unique()),
        ),
        (
            "lending_account.balances.0.bank_asset_tag",
            slot + 33,
            int(9, 1),
        ),
        (
            "lending_account.balances.0.asset_shares.value",
            slot + 40,
            fixed(1 << 60),
        ),
        (
            "lending_account.balances.0.liability_shares.value",
            slot + 56,
            fixed(1 << 50),
        ),
        (
            "lending_account.balances.0.last_update",
            slot + 88,
            int(1_234_567, 8),
        ),
        ("authority", 40, key(Pubkey::new_unique())),
    ];

    let mut used = std::collections::BTreeSet::new();
    for template in registry.by_protocol("Project 0") {
        let (data, rows) = match template.account_type.as_str() {
            "Bank" => (bank_data, &bank_rows),
            "MarginfiAccount" => (account_data, &account_rows),
            other => panic!("{} targets {other}", template.id),
        };
        for path in template.property_paths() {
            let (_, offset, (value, bytes)) = rows
                .iter()
                .find(|(row, ..)| *row == path)
                .unwrap_or_else(|| panic!("{}:{path} has no row", template.id));
            used.insert(path);

            let forged = surfnet_svm
                .get_forged_account_data(
                    &pubkey,
                    data,
                    template.idl(),
                    &HashMap::from([(path.to_string(), value.clone())]),
                )
                .unwrap_or_else(|e| panic!("[{}] {path}: {e}", template.id));

            assert_eq!(forged.len(), data.len(), "[{}] {path}", template.id);
            let target = *offset..offset + bytes.len();
            let diffs = diff_indices(&forged, data);
            assert!(
                !diffs.is_empty() && diffs.iter().all(|i| target.contains(i)),
                "[{}] {path} changed {diffs:?}, expected {target:?}",
                template.id
            );
            assert_eq!(
                &forged[target],
                bytes.as_slice(),
                "[{}] {path}",
                template.id
            );
        }
    }
    for (path, ..) in bank_rows.iter().chain(&account_rows) {
        assert!(used.contains(path), "no template exposes {path}");
    }
}

/// The deployed program. 45 bytes of `UpgradeableLoaderState::ProgramData` precede the ELF: a u32
/// tag, the deploy slot and the upgrade authority.
async fn project0_elf() -> Vec<u8> {
    let program_data = fetch(&[PROGRAM_DATA]).await.remove(0).data;
    let deployed_slot = u64::from_le_bytes(program_data[4..12].try_into().unwrap());
    assert_eq!(
        deployed_slot, DEPLOYED_SLOT,
        "program redeployed at slot {deployed_slot}; re-verify the templates against it"
    );
    program_data[45..].to_vec()
}

/// The live accounts `lending_account_pulse_health` reads, all from one slot.
struct Fixture {
    elf: Vec<u8>,
    accounts: HashMap<&'static str, solana_account::Account>,
}

impl Fixture {
    async fn load() -> Self {
        let addresses = [
            GROUP,
            SOL_BANK,
            USDC_BANK,
            SOL_BANK_ORACLE,
            USDC_BANK_ORACLE,
            ACCOUNT,
        ];
        let fetched = fetch(&addresses).await;
        Self {
            elf: project0_elf().await,
            accounts: addresses.iter().copied().zip(fetched).collect(),
        }
    }

    fn data(&self, address: &str) -> &[u8] {
        &self.accounts[address].data
    }
}

/// `price_message.publish_time` of a fully verified `PriceUpdateV2`.
fn pyth_publish_time(data: &[u8]) -> i64 {
    i64::from_le_bytes(data[93..101].try_into().unwrap())
}

/// `conf` and `ema_conf` of a fully verified `PriceUpdateV2` scaled to `price`, keeping their share
/// of the price: Project 0 refuses a price whose confidence is too wide a share of it.
fn confidence_scaled_to(data: &[u8], price: i64) -> [(String, serde_json::Value); 2] {
    let at =
        |offset: usize| u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap()) as u128;
    let scaled = |conf: u128, from: u128| serde_json::json!((conf * price as u128 / from) as u64);
    [
        ("price_message.conf".to_string(), scaled(at(81), at(73))),
        (
            "price_message.ema_conf".to_string(),
            scaled(at(117), at(109)),
        ),
    ]
}

/// What the risk engine wrote into `health_cache` after a pulse.
#[derive(Debug)]
struct Health {
    flags: u32,
    asset_value_maint: i128,
    liability_value_maint: i128,
}

impl Health {
    const HEALTHY: u32 = 1;
    const ENGINE_OK: u32 = 2;
    const ORACLE_OK: u32 = 4;

    fn healthy(&self) -> bool {
        self.flags & Self::HEALTHY != 0
    }

    fn engine_ran(&self) -> bool {
        self.flags & (Self::ENGINE_OK | Self::ORACLE_OK) == Self::ENGINE_OK | Self::ORACLE_OK
    }
}

/// The deployed program and the fixture's accounts on a LiteSVM, with `account` as the
/// MarginfiAccount and `overrides` replacing fetched account data by address. The clock sits one
/// second after the newest price, so both prices are fresh.
fn loaded_svm(fixture: &Fixture, account: &[u8], overrides: &[(&str, Vec<u8>)]) -> LiteSVM {
    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(Pubkey::from_str_const(PROGRAM), &fixture.elf)
        .expect("load the Project 0 program");

    let mut accounts = fixture.accounts.clone();
    accounts.get_mut(ACCOUNT).unwrap().data = account.to_vec();
    for (address, data) in overrides {
        accounts.get_mut(address).unwrap().data = data.clone();
    }

    let newest_price = [SOL_BANK_ORACLE, USDC_BANK_ORACLE]
        .iter()
        .map(|address| pyth_publish_time(&accounts[address].data))
        .max()
        .unwrap();
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.unix_timestamp = newest_price + 1;
    svm.set_sysvar(&clock);

    for (address, account) in accounts {
        svm.set_account(Pubkey::from_str_const(address), account)
            .unwrap_or_else(|e| panic!("set {address}: {e:?}"));
    }
    svm
}

/// The (bank, oracle) list in balance-slot order, as the risk engine reads it.
fn remaining_metas(remaining: &[(&str, &str)]) -> Vec<AccountMeta> {
    remaining
        .iter()
        .flat_map(|(bank, oracle)| {
            [
                AccountMeta::new_readonly(Pubkey::from_str_const(bank), false),
                AccountMeta::new_readonly(Pubkey::from_str_const(oracle), false),
            ]
        })
        .collect()
}

/// Runs `lending_account_pulse_health` on the deployed program. `overrides` replaces fetched
/// account data by address, and `remaining` is the (bank, oracle) list in balance-slot order.
fn pulse_health(
    fixture: &Fixture,
    account: &[u8],
    overrides: &[(&str, Vec<u8>)],
    remaining: &[(&str, &str)],
) -> Health {
    const PULSE_HEALTH: [u8; 8] = [186, 52, 117, 97, 34, 74, 39, 253];
    const HEALTH_CACHE: usize = 1840;

    let program = Pubkey::from_str_const(PROGRAM);
    let mut svm = loaded_svm(fixture, account, overrides);

    let mut metas = vec![
        AccountMeta::new(Pubkey::from_str_const(ACCOUNT), false),
        AccountMeta::new_readonly(Pubkey::from_str_const(GROUP), false),
    ];
    metas.extend(remaining_metas(remaining));

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: program,
            accounts: metas,
            data: PULSE_HEALTH.to_vec(),
        }],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    if let Err(failed) = svm.send_transaction(tx) {
        panic!(
            "pulse_health failed: {:?}\n{}",
            failed.err,
            failed.meta.logs.join("\n")
        );
    }

    let data = svm
        .get_account(&Pubkey::from_str_const(ACCOUNT))
        .unwrap()
        .data;
    Health {
        flags: u32::from_le_bytes(
            data[HEALTH_CACHE + 104..HEALTH_CACHE + 108]
                .try_into()
                .unwrap(),
        ),
        asset_value_maint: read_i128(&data, HEALTH_CACHE + 32),
        liability_value_maint: read_i128(&data, HEALTH_CACHE + 48),
    }
}

/// Runs an empty flash loan, `lending_account_start_flashloan` then `_end_flashloan`, signed by
/// `authority`. Its end runs the circuit breaker gate over the banks of an account with debt,
/// and moves no tokens.
fn empty_flashloan(
    fixture: &Fixture,
    account: &[u8],
    overrides: &[(&str, Vec<u8>)],
    remaining: &[(&str, &str)],
    authority: &Keypair,
) -> Result<(), String> {
    const START_FLASHLOAN: [u8; 8] = [14, 131, 33, 220, 81, 186, 180, 107];
    const END_FLASHLOAN: [u8; 8] = [105, 124, 201, 106, 153, 2, 8, 156];

    let program = Pubkey::from_str_const(PROGRAM);
    let marginfi_account = Pubkey::from_str_const(ACCOUNT);
    let mut svm = loaded_svm(fixture, account, overrides);

    // end_index: the end instruction is the second one in the transaction.
    let start = Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(marginfi_account, false),
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(
                Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111"),
                false,
            ),
        ],
        data: [START_FLASHLOAN.as_slice(), &1u64.to_le_bytes()].concat(),
    };
    let mut end_metas = vec![
        AccountMeta::new(marginfi_account, false),
        AccountMeta::new_readonly(Pubkey::from_str_const(GROUP), false),
        AccountMeta::new_readonly(authority.pubkey(), true),
    ];
    end_metas.extend(remaining_metas(remaining));
    let end = Instruction {
        program_id: program,
        accounts: end_metas,
        data: END_FLASHLOAN.to_vec(),
    };

    svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[start, end],
        Some(&authority.pubkey()),
        &[authority],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .map(|_| ())
        .map_err(|failed| format!("{:?}\n{}", failed.err, failed.meta.logs.join("\n")))
}

fn forge(template_id: &str, data: &[u8], values: &[(String, serde_json::Value)]) -> Vec<u8> {
    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry.get(template_id).unwrap();
    surfnet_svm
        .get_forged_account_data(
            &Pubkey::new_unique(),
            data,
            template.idl(),
            &values.iter().cloned().collect(),
        )
        .unwrap_or_else(|e| panic!("[{template_id}] {e}"))
}

/// The live account rebuilt with `project0-account-balance` into 10 SOL of collateral against a
/// 500 USDC debt, every other slot empty.
fn sol_backed_usdc_borrow(account: &[u8]) -> Vec<u8> {
    // Balances are sorted by bank_pk, highest first.
    let sol = Pubkey::from_str_const(SOL_BANK);
    let usdc = Pubkey::from_str_const(USDC_BANK);
    assert!(
        sol.to_bytes() > usdc.to_bytes(),
        "SOL must sort before USDC"
    );

    let one = 1i128 << 48;
    let empty = Pubkey::default().to_string();
    let mut values = Vec::new();
    let mut slot = |index: usize, bank: &str, tag: u8, asset: i128, liability: i128| {
        let prefix = format!("lending_account.balances.{index}");
        let active = u8::from(bank != empty);
        values.extend([
            (format!("{prefix}.active"), serde_json::json!(active)),
            (format!("{prefix}.bank_pk"), serde_json::json!(bank)),
            (format!("{prefix}.bank_asset_tag"), serde_json::json!(tag)),
            (
                format!("{prefix}.asset_shares.value"),
                serde_json::json!(asset.to_string()),
            ),
            (
                format!("{prefix}.liability_shares.value"),
                serde_json::json!(liability.to_string()),
            ),
        ]);
    };
    slot(0, SOL_BANK, 1, 10_000_000_000 * one, 0);
    slot(1, USDC_BANK, 0, 0, 500_000_000 * one);
    for index in 2..16 {
        slot(index, &empty, 0, 0, 0);
    }
    forge("project0-account-balance", account, &values)
}

/// Differential check against the deployed program: a SOL-backed USDC borrow, built on the live
/// account with `project0-account-balance`, is healthy on live state and becomes liquidatable
/// after either the weight lever or the price lever.
#[tokio::test]
async fn risk_engine_honours_weight_and_price_overrides() {
    let fixture = Fixture::load().await;
    let registry = TemplateRegistry::new();

    let price_feed = registry.get("pyth-core-compatible-price-feed-v2").unwrap();
    for oracle in [SOL_BANK_ORACLE, USDC_BANK_ORACLE] {
        assert_eq!(
            fixture.accounts[oracle].owner.to_string(),
            price_feed.idl().address,
            "{oracle} owner"
        );
    }

    // The bank names its price account at config.oracle_keys.0; the template writes that account.
    let sol_feed =
        Pubkey::new_from_array(fixture.data(SOL_BANK)[610..642].try_into().unwrap()).to_string();
    assert_eq!(
        sol_feed, SOL_BANK_ORACLE,
        "the SOL bank reads a different price account"
    );

    let one = 1i128 << 48;
    let account = sol_backed_usdc_borrow(fixture.data(ACCOUNT));
    let remaining = [(SOL_BANK, SOL_BANK_ORACLE), (USDC_BANK, USDC_BANK_ORACLE)];

    let baseline = pulse_health(&fixture, &account, &[], &remaining);
    assert!(
        baseline.engine_ran() && baseline.healthy(),
        "baseline should be healthy: {baseline:?}"
    );
    assert!(baseline.asset_value_maint > baseline.liability_value_maint);

    let weakened_bank = forge(
        "project0-bank-risk-weights",
        fixture.data(SOL_BANK),
        &[
            (
                "config.asset_weight_init.value".to_string(),
                serde_json::json!((one * 3 / 10).to_string()),
            ),
            (
                "config.asset_weight_maint.value".to_string(),
                serde_json::json!((one * 3 / 10).to_string()),
            ),
        ],
    );
    let weakened = pulse_health(&fixture, &account, &[(SOL_BANK, weakened_bank)], &remaining);
    assert!(
        weakened.engine_ran() && !weakened.healthy(),
        "30% weight should be liquidatable: {weakened:?}"
    );
    assert!(weakened.asset_value_maint < baseline.asset_value_maint);
    assert_eq!(
        weakened.liability_value_maint, baseline.liability_value_maint,
        "debt moved"
    );

    let fresh = pyth_publish_time(fixture.data(&sol_feed))
        .max(pyth_publish_time(fixture.data(USDC_BANK_ORACLE)));
    let crash_price = 20i64 * 100_000_000;
    let [conf, ema_conf] = confidence_scaled_to(fixture.data(&sol_feed), crash_price);
    let crashed_oracle = forge(
        "pyth-core-compatible-price-feed-v2",
        fixture.data(&sol_feed),
        &[
            (
                "price_message.price".to_string(),
                serde_json::json!(crash_price),
            ),
            (
                "price_message.ema_price".to_string(),
                serde_json::json!(crash_price),
            ),
            conf,
            ema_conf,
            (
                "price_message.publish_time".to_string(),
                serde_json::json!(fresh),
            ),
            (
                "price_message.prev_publish_time".to_string(),
                serde_json::json!(fresh),
            ),
        ],
    );
    let crashed = pulse_health(
        &fixture,
        &account,
        &[(sol_feed.as_str(), crashed_oracle)],
        &remaining,
    );
    assert!(
        crashed.engine_ran() && !crashed.healthy(),
        "SOL at $20 should be liquidatable: {crashed:?}"
    );
    assert!(crashed.asset_value_maint < baseline.asset_value_maint);
    assert_eq!(
        crashed.liability_value_maint, baseline.liability_value_maint,
        "debt moved"
    );
}

/// Stablecoin banks run a price circuit breaker: once USDC moves past its first tier, an account
/// with USDC debt fails every risk-carrying action with `CircuitBreakerPriceJump`, until
/// `project0-bank-oracle` moves the breaker's reference prices to the new price.
#[tokio::test]
async fn usdc_breaker_accepts_a_move_once_its_reference_prices_follow() {
    let fixture = Fixture::load().await;
    let usdc_bank = fixture.data(USDC_BANK);
    let flags = u64::from_le_bytes(usdc_bank[840..848].try_into().unwrap());
    assert_eq!(flags >> 11 & 1, 1, "the USDC bank's circuit breaker is off");

    let authority = Keypair::new();
    let account = forge(
        "project0-account-authority",
        &sol_backed_usdc_borrow(fixture.data(ACCOUNT)),
        &[(
            "authority".to_string(),
            serde_json::json!(authority.pubkey().to_string()),
        )],
    );
    let remaining = [(SOL_BANK, SOL_BANK_ORACLE), (USDC_BANK, USDC_BANK_ORACLE)];

    let usdc_feed = fixture.data(USDC_BANK_ORACLE);
    assert_eq!(usdc_feed[40], 1, "Full verification");
    let price = i64::from_le_bytes(usdc_feed[73..81].try_into().unwrap());
    let exponent = i32::from_le_bytes(usdc_feed[89..93].try_into().unwrap());
    let depeg = price * 95 / 100;
    let fresh = pyth_publish_time(usdc_feed).max(pyth_publish_time(fixture.data(SOL_BANK_ORACLE)));
    let [conf, ema_conf] = confidence_scaled_to(usdc_feed, depeg);
    let moved_feed = forge(
        "pyth-core-compatible-price-feed-v2",
        usdc_feed,
        &[
            ("price_message.price".to_string(), serde_json::json!(depeg)),
            (
                "price_message.ema_price".to_string(),
                serde_json::json!(depeg),
            ),
            conf,
            ema_conf,
            (
                "price_message.publish_time".to_string(),
                serde_json::json!(fresh),
            ),
            (
                "price_message.prev_publish_time".to_string(),
                serde_json::json!(fresh),
            ),
        ],
    );
    let reference = ((depeg as i128) << 48) / 10i128.pow(exponent.unsigned_abs());
    let moved_bank = forge(
        "project0-bank-oracle",
        usdc_bank,
        &[
            (
                "cb_reference_price.value".to_string(),
                serde_json::json!(reference.to_string()),
            ),
            (
                "cb_window_reference_price.value".to_string(),
                serde_json::json!(reference.to_string()),
            ),
        ],
    );

    empty_flashloan(&fixture, &account, &[], &remaining, &authority)
        .unwrap_or_else(|e| panic!("live prices should pass the breaker: {e}"));

    let refused = empty_flashloan(
        &fixture,
        &account,
        &[(USDC_BANK_ORACLE, moved_feed.clone())],
        &remaining,
        &authority,
    )
    .expect_err("USDC 5% down must trip the breaker");
    assert!(refused.contains("CircuitBreakerPriceJump"), "{refused}");

    empty_flashloan(
        &fixture,
        &account,
        &[(USDC_BANK_ORACLE, moved_feed), (USDC_BANK, moved_bank)],
        &remaining,
        &authority,
    )
    .unwrap_or_else(|e| panic!("the breaker should accept a price its references follow: {e}"));
}
