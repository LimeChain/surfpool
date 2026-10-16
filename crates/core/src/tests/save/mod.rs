//! On-chain tests for Save (formerly Solend).
//!
//! Like the Kamino suite these fetch real mainnet accounts rather than embedding captured copies,
//! so they need a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests save
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! Save publishes no IDL and its accounts carry no discriminator, so the byte tests pin every
//! template offset against the layout of the deployed build, and the behavioural tests run that
//! deployed program over live mainnet state to show the levers move a position's health.

use std::collections::HashMap;

use litesvm::LiteSVM;
use solana_account::Account;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use spl_token_interface::state::{Account as TokenAccount, AccountState};
use surfpool_types::{AccountAddress, OverrideInstance, RawEncoding, Scenario};

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    tests::helpers::diff_indices,
    types::RemoteRpcResult,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const SAVE_PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const SAVE_PROGRAMDATA: &str = "DMCvGv1fS5rMcAvEDPDDBawPqbDRSzJh2Bo6qXCmgJkR";
const MAIN_MARKET: &str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
const SOL_RESERVE: &str = "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36";
const USDC_RESERVE: &str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
const SOL_ORACLE: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
const USDC_ORACLE: &str = "Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX";
const PYTH_RECEIVER: &str = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";
const SOL_USD_FEED: &str = "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const USDC_USD_FEED: &str = "0xeaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
/// One deposit and one borrow, SOL against USDC.
const OBLIGATION_1D1B: &str = "142ajEdeDtNPKokqySkqqr5eNdFEcoJPNGn2ddEJYrbP";
const OBLIGATION_1D1B_OWNER: &str = "6cPZda7Qt7RK8HfYtz2rjiAEeh3ztR6fQ5o2G2Frb36z";
/// Three deposits and two borrows, with non-zero bytes past its last entry. The fixed-shape
/// template must work over an account like this, not only over a tidy one.
const OBLIGATION_3D2B: &str = "8XBi1bNjZScafhk5XrunTEW7BQCMjfx4q13xP5Lks7Qo";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Offsets and widths from the deployed build's `pack_into_slice` (SPL@d04ce00b), written down
/// independently of the YAML so a typo in either shows up here.
const LAYOUT: &[(&str, &str, usize, usize)] = &[
    ("Reserve", "liquidity.available_amount", 171, 8),
    ("Reserve", "config.optimal_utilization_rate", 299, 1),
    ("Reserve", "config.loan_to_value_ratio", 300, 1),
    ("Reserve", "config.liquidation_bonus", 301, 1),
    ("Reserve", "config.liquidation_threshold", 302, 1),
    ("Reserve", "config.min_borrow_rate", 303, 1),
    ("Reserve", "config.optimal_borrow_rate", 304, 1),
    ("Reserve", "config.max_borrow_rate", 305, 1),
    ("Reserve", "config.deposit_limit", 323, 8),
    ("Reserve", "config.borrow_limit", 331, 8),
    ("Reserve", "config.added_borrow_weight_bps", 445, 8),
    ("Reserve", "config.max_utilization_rate", 470, 1),
    ("Reserve", "config.super_max_borrow_rate", 471, 8),
    ("Reserve", "config.max_liquidation_bonus", 479, 1),
    ("Reserve", "config.max_liquidation_threshold", 480, 1),
    ("Obligation", "lending_market", 10, 32),
    ("Obligation", "owner", 42, 32),
    ("Obligation", "deposits_len", 202, 1),
    ("Obligation", "borrows_len", 203, 1),
    ("Obligation", "deposits.0.deposit_reserve", 204, 32),
    ("Obligation", "deposits.0.deposited_amount", 236, 8),
    ("Obligation", "borrows.0.borrow_reserve", 292, 32),
    (
        "Obligation",
        "borrows.0.cumulative_borrow_rate_wads",
        324,
        16,
    ),
    ("Obligation", "borrows.0.borrowed_amount_wads", 340, 16),
];

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[&str]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let pubkeys: Vec<Pubkey> = addresses
        .iter()
        .map(|a| Pubkey::from_str_const(a))
        .collect();

    // The public endpoint throttles and intermittently 503s, which has nothing to do with what these
    // tests assert.
    let mut errors = Vec::new();
    let results = loop {
        match client
            .get_multiple_accounts(&pubkeys, CommitmentConfig::confirmed())
            .await
        {
            Ok(r) => break r,
            Err(e) => {
                errors.push(e.to_string());
                if errors.len() >= 5 {
                    panic!("failed to fetch {addresses:?} from mainnet: {errors:#?}");
                }
                tokio::time::sleep(std::time::Duration::from_millis(750 * errors.len() as u64))
                    .await;
            }
        }
    };
    results
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

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

fn i64_at(data: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

fn u128_at(data: &[u8], at: usize) -> u128 {
    u128::from_le_bytes(data[at..at + 16].try_into().unwrap())
}

fn pubkey_at(data: &[u8], at: usize) -> Pubkey {
    Pubkey::new_from_array(data[at..at + 32].try_into().unwrap())
}

/// A value that differs from the one stored, so the write is visible in a diff.
fn changed_value(data: &[u8], offset: usize, encoding: &RawEncoding) -> serde_json::Value {
    match encoding {
        RawEncoding::U8 => serde_json::json!(data[offset] ^ 1),
        RawEncoding::U64 => serde_json::json!(u64_at(data, offset) ^ 1),
        RawEncoding::U128 => serde_json::json!((u128_at(data, offset) ^ 1).to_string()),
        RawEncoding::Bytes32 => serde_json::json!(Pubkey::new_unique().to_string()),
        other => panic!("Save templates do not use {other:?}"),
    }
}

/// The live account each template is exercised against.
fn live_account_for(account_type: &str) -> &'static str {
    match account_type {
        "Reserve" => SOL_RESERVE,
        "Obligation" => OBLIGATION_3D2B,
        other => panic!("no live account for {other:?}"),
    }
}

/// Live accounts match the deployed layout the templates are written against.
#[tokio::test]
async fn save_live_accounts_match_the_layout() {
    let accounts = fetch(&[
        MAIN_MARKET,
        SOL_RESERVE,
        USDC_RESERVE,
        OBLIGATION_1D1B,
        OBLIGATION_3D2B,
    ])
    .await;
    let program = Pubkey::from_str_const(SAVE_PROGRAM);
    let market = Pubkey::from_str_const(MAIN_MARKET);
    for (account, size) in accounts.iter().zip([290, 619, 619, 1300, 1300]) {
        assert_eq!(account.owner, program);
        assert_eq!(account.data.len(), size);
        assert_eq!(account.data[0], 1, "version byte");
    }

    let market_data = &accounts[0].data;
    let (authority, bump) = Pubkey::find_program_address(&[market.as_ref()], &program);
    assert_eq!(market_data[1], bump, "LendingMarket bump_seed at offset 1");
    assert_eq!(
        authority,
        Pubkey::from_str_const("DdZR6zRFiUt4S5mg7AV1uKB2z1f1WzcNYCaTEEWPAuby")
    );
    assert_eq!(
        pubkey_at(market_data, 66),
        Pubkey::from_str_const(TOKEN_PROGRAM)
    );

    for (reserve, mint, oracle) in [
        (
            &accounts[1].data,
            "So11111111111111111111111111111111111111112",
            SOL_ORACLE,
        ),
        (
            &accounts[2].data,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            USDC_ORACLE,
        ),
    ] {
        assert_eq!(pubkey_at(reserve, 10), market);
        assert_eq!(pubkey_at(reserve, 42), Pubkey::from_str_const(mint));
        assert_eq!(pubkey_at(reserve, 107), Pubkey::from_str_const(oracle));
    }

    for obligation in [&accounts[3].data, &accounts[4].data] {
        assert_eq!(pubkey_at(obligation, 10), market);
    }
    let tidy = &accounts[3].data;
    assert_eq!((tidy[202], tidy[203]), (1, 1));
    assert_eq!(pubkey_at(tidy, 204), Pubkey::from_str_const(SOL_RESERVE));
    assert_eq!(pubkey_at(tidy, 292), Pubkey::from_str_const(USDC_RESERVE));

    // A shifted offset would land on unrelated bytes, which would rarely keep these relations.
    for reserve in [&accounts[1].data, &accounts[2].data] {
        let (ltv, threshold, max_threshold) = (reserve[300], reserve[302], reserve[480]);
        assert!(
            ltv < threshold && threshold <= max_threshold && max_threshold <= 100,
            "loan to value {ltv}, threshold {threshold}, max threshold {max_threshold}"
        );
        assert!(
            reserve[299] <= 100 && reserve[470] <= 100,
            "utilisation rates"
        );
    }
}

/// Every template property sits exactly where the deployed layout puts it, and writing it changes
/// that field's bytes and nothing else.
#[tokio::test]
async fn save_templates_write_only_their_offsets() {
    let accounts = fetch(&[SOL_RESERVE, OBLIGATION_3D2B]).await;
    let by_address: HashMap<&str, &Vec<u8>> = [
        (SOL_RESERVE, &accounts[0].data),
        (OBLIGATION_3D2B, &accounts[1].data),
    ]
    .into_iter()
    .collect();

    let registry = TemplateRegistry::new();
    let mut checked = 0;
    for template in registry
        .all()
        .into_iter()
        .filter(|t| t.id.starts_with("save-"))
    {
        let data = by_address[live_account_for(&template.account_type)];
        let mut allowed = Vec::new();
        let mut values = HashMap::new();
        for property in &template.properties {
            let offset = property.offset.unwrap();
            let encoding = property.encoding.as_ref().unwrap();
            let expected = LAYOUT
                .iter()
                .find(|(account, path, _, _)| {
                    *account == template.account_type && *path == property.path
                })
                .unwrap_or_else(|| panic!("{}.{} is not in LAYOUT", template.id, property.path));
            assert_eq!(
                (offset, encoding.width()),
                (expected.2, expected.3),
                "{}.{} disagrees with the deployed layout",
                template.id,
                property.path
            );
            allowed.push(offset..offset + encoding.width());
            values.insert(property.path.clone(), changed_value(data, offset, encoding));
            checked += 1;
        }

        let written = template
            .materialize_raw_layout(data, &values, 0)
            .unwrap_or_else(|e| panic!("{}: {e}", template.id));
        assert_eq!(written.len(), data.len());
        let diffs = diff_indices(&written, data);
        assert!(
            diffs.iter().all(|i| allowed.iter().any(|r| r.contains(i))),
            "{} wrote outside its fields: {diffs:?}",
            template.id
        );
        for range in &allowed {
            assert!(
                diffs.iter().any(|i| range.contains(i)),
                "{} did not write {range:?}",
                template.id
            );
        }
    }
    assert!(checked > 0);
}

/// A scenario registered on the SVM goes through the raw-layout materializer once: two templates on
/// the same reserve in one slot both land, nothing else changes, and a later slot does not reapply.
#[tokio::test]
async fn save_scenario_materializes_once_through_surfnet_svm() {
    const BASE_SLOT: u64 = 1_000_000;
    let reserve_key = Pubkey::from_str_const(SOL_RESERVE);
    let reserve = fetch(&[SOL_RESERVE]).await.remove(0);
    let target = AccountAddress::Pubkey(SOL_RESERVE.to_string());

    let mut scenario = Scenario::new("Save SOL reserve".into(), "threshold and limit".into());
    scenario.add_override(
        OverrideInstance::new("save-reserve-risk".into(), 0, target.clone()).with_values(
            HashMap::from([(
                "config.liquidation_threshold".to_string(),
                serde_json::json!(50),
            )]),
        ),
    );
    scenario.add_override(
        OverrideInstance::new("save-reserve-limits".into(), 0, target).with_values(HashMap::from(
            [("config.borrow_limit".to_string(), serde_json::json!(0))],
        )),
    );

    let (mut svm, _events, _geyser) = SurfnetSvm::default();
    svm.inner.set_account(reserve_key, reserve.clone()).unwrap();
    svm.register_scenario(scenario, Some(BASE_SLOT)).unwrap();
    svm.materialize_overrides_for_slot(&None, BASE_SLOT)
        .await
        .expect("materialize Save scenario");
    let read = |svm: &SurfnetSvm| svm.inner.get_account(&reserve_key).unwrap().unwrap().data;
    let materialized = read(&svm);
    assert_eq!(materialized[302], 50);
    assert_eq!(u64_at(&materialized, 331), 0);
    assert!(
        diff_indices(&reserve.data, &materialized)
            .iter()
            .all(|i| *i == 302 || (331..339).contains(i)),
        "materialization escaped the threshold and borrow limit"
    );

    svm.materialize_overrides_for_slot(&None, BASE_SLOT + 1)
        .await
        .unwrap();
    assert_eq!(read(&svm), materialized, "overrides must apply once");
}

/// The address rule the templates document, checked against a live obligation and its owner.
#[tokio::test]
async fn save_obligation_address_is_created_with_seed() {
    let owner = Pubkey::from_str_const(OBLIGATION_1D1B_OWNER);
    let derived = Pubkey::create_with_seed(
        &owner,
        &MAIN_MARKET[..32],
        &Pubkey::from_str_const(SAVE_PROGRAM),
    )
    .unwrap();
    assert_eq!(derived, Pubkey::from_str_const(OBLIGATION_1D1B));

    let obligation = fetch(&[OBLIGATION_1D1B]).await.remove(0);
    assert_eq!(pubkey_at(&obligation.data, 42), owner);
}

/// The main pool's SOL and USDC reserves read Pyth PriceUpdateV2 accounts that the existing
/// pyth-price-feed-v2 template addresses, so the price lever needs no Save template.
#[tokio::test]
async fn save_reserve_oracles_are_pyth_v2_template_accounts() {
    let registry = TemplateRegistry::new();
    let pyth = registry.get("pyth-price-feed-v2").unwrap();
    let accounts = fetch(&[SOL_RESERVE, USDC_RESERVE, SOL_ORACLE, USDC_ORACLE]).await;

    for (reserve, oracle, feed) in [
        (&accounts[0], &accounts[2], SOL_USD_FEED),
        (&accounts[1], &accounts[3], USDC_USD_FEED),
    ] {
        let values = HashMap::from([("feed_id".to_string(), serde_json::json!(feed))]);
        let address = pyth.address.resolve(Some(&values)).unwrap();
        assert_eq!(address, pubkey_at(&reserve.data, 107));
        assert_eq!(oracle.owner, Pubkey::from_str_const(PYTH_RECEIVER));
        assert_eq!(oracle.data[40], 1, "Full verification");
    }
}

/// The llm_context lookups, run against mainnet as written. Save accounts carry no discriminator,
/// so the reserve and obligation lookups filter on size; a program upgrade that changes either
/// size empties them without an error, and this test is what notices.
#[tokio::test]
async fn llm_context_lookups_find_the_main_reserves_and_an_owners_obligation() {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let lookup = async |size: u64, memcmps: &[(usize, &str)], slice: UiDataSliceConfig| {
        let mut filters = vec![RpcFilterType::DataSize(size)];
        filters.extend(memcmps.iter().map(|(offset, bytes)| {
            RpcFilterType::Memcmp(Memcmp::new(
                *offset,
                MemcmpEncodedBytes::Base58(bytes.to_string()),
            ))
        }));
        let config = RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(slice),
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        };
        match client
            .get_program_accounts(&Pubkey::from_str_const(SAVE_PROGRAM), config, Some(filters))
            .await
            .unwrap_or_else(|e| panic!("getProgramAccounts failed: {e}"))
        {
            RemoteRpcResult::Ok(found) => found,
            RemoteRpcResult::MethodNotSupported => {
                panic!("the RPC does not serve getProgramAccounts")
            }
        }
    };

    for (mint, reserve, oracle) in [
        (
            "So11111111111111111111111111111111111111112",
            SOL_RESERVE,
            SOL_ORACLE,
        ),
        (
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            USDC_RESERVE,
            USDC_ORACLE,
        ),
    ] {
        let slice = UiDataSliceConfig {
            offset: 107,
            length: 32,
        };
        let found = lookup(619, &[(10, MAIN_MARKET), (42, mint)], slice).await;
        assert_eq!(found.len(), 1, "expected one main-pool reserve for {mint}");
        let (pubkey, account) = &found[0];
        assert_eq!(pubkey.to_string(), reserve);
        let price_account = account.data.decode().expect("base64 data slice");
        assert_eq!(pubkey_at(&price_account, 0).to_string(), oracle);
    }

    let slice = UiDataSliceConfig {
        offset: 0,
        length: 0,
    };
    let found = lookup(1300, &[(42, OBLIGATION_1D1B_OWNER)], slice).await;
    assert!(
        found
            .iter()
            .any(|(pubkey, _)| pubkey.to_string() == OBLIGATION_1D1B),
        "the owner's obligation is missing from {found:?}"
    );
}

// ---------------------------------------------------------------- behavioural (LiteSVM)

/// Offsets inside a Pyth PriceUpdateV2 with Full verification.
const PYTH_PRICE: usize = 73;
const PYTH_EXPO: usize = 89;
const PYTH_PUBLISH_TIME: usize = 93;
const PYTH_EMA: usize = 109;

/// The obligation's deposit, in cSOL base units.
const DEPOSIT_CTOKENS: u64 = 100_000_000_000;
/// Debt as a share of the collateral's value: healthy against the SOL threshold of 72%.
const BORROW_SHARE: f64 = 0.6;
const LIQUIDATOR_USDC: u64 = 1_000_000_000;
const REPAY_USDC: u64 = 100_000_000;
const TOKEN_RENT: u64 = 2_039_280;

struct SaveFixture {
    elf: Vec<u8>,
    accounts: HashMap<Pubkey, Account>,
}

impl SaveFixture {
    fn data(&self, address: &str) -> &[u8] {
        &self.accounts[&Pubkey::from_str_const(address)].data
    }
}

/// The deployed program and every account a refresh and a liquidation touch, fetched fresh.
async fn fixture() -> SaveFixture {
    let elf = fetch(&[SAVE_PROGRAMDATA]).await.remove(0).data[45..].to_vec();

    let base = [
        MAIN_MARKET,
        SOL_RESERVE,
        USDC_RESERVE,
        SOL_ORACLE,
        USDC_ORACLE,
        OBLIGATION_3D2B,
    ];
    let mut accounts: HashMap<Pubkey, Account> = base
        .iter()
        .map(|a| Pubkey::from_str_const(a))
        .zip(fetch(&base).await)
        .collect();

    // Vaults, the cToken mint and the fee receiver, read from the reserves at their offsets.
    let sol = &accounts[&Pubkey::from_str_const(SOL_RESERVE)].data;
    let usdc = &accounts[&Pubkey::from_str_const(USDC_RESERVE)].data;
    let extra: Vec<Pubkey> = vec![
        pubkey_at(sol, 75),
        pubkey_at(sol, 227),
        pubkey_at(sol, 267),
        pubkey_at(sol, 339),
        pubkey_at(usdc, 75),
    ];
    let names: Vec<String> = extra.iter().map(|p| p.to_string()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    accounts.extend(extra.into_iter().zip(fetch(&names).await));

    SaveFixture { elf, accounts }
}

fn token_account(mint: &Pubkey, owner: &Pubkey, amount: u64, native: bool) -> Account {
    let mut data = vec![0u8; TokenAccount::LEN];
    TokenAccount {
        mint: *mint,
        owner: *owner,
        amount,
        state: AccountState::Initialized,
        is_native: if native {
            COption::Some(TOKEN_RENT)
        } else {
            COption::None
        },
        ..Default::default()
    }
    .pack_into_slice(&mut data);
    Account {
        lamports: TOKEN_RENT + if native { amount } else { 0 },
        data,
        owner: Pubkey::from_str_const(TOKEN_PROGRAM),
        executable: false,
        rent_epoch: 0,
    }
}

fn apply(fixture: &mut SaveFixture, template_id: &str, address: &str, values: serde_json::Value) {
    let registry = TemplateRegistry::new();
    let template = registry.get(template_id).unwrap();
    let values: HashMap<String, serde_json::Value> = serde_json::from_value(values).unwrap();
    let account = fixture
        .accounts
        .get_mut(&Pubkey::from_str_const(address))
        .unwrap();
    account.data = template
        .materialize_raw_layout(&account.data, &values, 0)
        .unwrap_or_else(|e| panic!("{template_id}: {e}"));
}

fn apply_pyth(fixture: &mut SaveFixture, oracle: &str, values: serde_json::Value) {
    let registry = TemplateRegistry::new();
    let pyth = registry.get("pyth-price-feed-v2").unwrap();
    let (svm, _events, _geyser) = SurfnetSvm::default();
    let values: HashMap<String, serde_json::Value> = serde_json::from_value(values).unwrap();
    let address = Pubkey::from_str_const(oracle);
    let account = fixture.accounts.get_mut(&address).unwrap();
    account.data = svm
        .get_forged_account_data(&address, &account.data, pyth.idl(), &values)
        .unwrap();
}

/// Rebuilds the live 3+2 obligation as SOL collateral against USDC debt worth [`BORROW_SHARE`] of
/// it, and makes both oracles fresh. Returns the publish time the clock must be set to.
fn prepare_position(fixture: &mut SaveFixture) -> i64 {
    let sol = fixture.data(SOL_RESERVE);
    let total_liquidity =
        u64_at(sol, 171) as f64 + u128_at(sol, 179) as f64 / 1e18 - u128_at(sol, 373) as f64 / 1e18;
    let lamports = DEPOSIT_CTOKENS as f64 * total_liquidity / u64_at(sol, 259) as f64;
    let oracle = fixture.data(SOL_ORACLE);
    let expo = i32::from_le_bytes(oracle[PYTH_EXPO..PYTH_EXPO + 4].try_into().unwrap());
    let sol_price = i64_at(oracle, PYTH_PRICE) as f64 * 10f64.powi(expo);
    let debt_micro_usdc = (lamports / 1e9 * sol_price * BORROW_SHARE * 1e6) as u128;
    let publish_time = i64_at(oracle, PYTH_PUBLISH_TIME);
    let usdc_rate = u128_at(fixture.data(USDC_RESERVE), 195);

    apply(
        fixture,
        "save-obligation-position",
        OBLIGATION_3D2B,
        serde_json::json!({
            "owner": Pubkey::new_unique().to_string(),
            "lending_market": MAIN_MARKET,
            "deposits_len": 1,
            "borrows_len": 1,
            "deposits.0.deposit_reserve": SOL_RESERVE,
            "deposits.0.deposited_amount": DEPOSIT_CTOKENS,
            "borrows.0.borrow_reserve": USDC_RESERVE,
            "borrows.0.cumulative_borrow_rate_wads": usdc_rate.to_string(),
            "borrows.0.borrowed_amount_wads": (debt_micro_usdc * 10u128.pow(18)).to_string(),
        }),
    );
    for oracle in [SOL_ORACLE, USDC_ORACLE] {
        apply_pyth(
            fixture,
            oracle,
            serde_json::json!({ "price_message.publish_time": publish_time }),
        );
    }
    publish_time
}

struct Health {
    borrowed_value: u128,
    unhealthy_borrow_value: u128,
    sol_market_price: u128,
    liquidation: Result<(u64, u128), String>,
}

/// Loads the fixture into LiteSVM, runs refresh_reserve x2 and refresh_obligation, reads the
/// obligation's health, then tries a real liquidate_obligation_and_redeem_reserve_collateral.
/// A successful liquidation reports the SOL the liquidator received and the debt left.
fn run(fixture: &SaveFixture, publish_time: i64) -> Health {
    let program = Pubkey::from_str_const(SAVE_PROGRAM);
    let market = Pubkey::from_str_const(MAIN_MARKET);
    let sol = Pubkey::from_str_const(SOL_RESERVE);
    let usdc = Pubkey::from_str_const(USDC_RESERVE);
    let obligation = Pubkey::from_str_const(OBLIGATION_3D2B);
    let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);
    let (sol_data, usdc_data) = (fixture.data(SOL_RESERVE), fixture.data(USDC_RESERVE));

    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(program, &fixture.elf).unwrap();
    for (address, account) in &fixture.accounts {
        svm.set_account(*address, account.clone()).unwrap();
    }
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = u64_at(sol_data, 1).max(u64_at(usdc_data, 1)) + 1;
    clock.unix_timestamp = publish_time;
    svm.set_sysvar(&clock);

    let liquidator = Keypair::new();
    svm.airdrop(&liquidator.pubkey(), 10_000_000_000).unwrap();
    let (source_usdc, dest_csol, dest_sol) = (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );
    let owner = liquidator.pubkey();
    for (address, account) in [
        (
            source_usdc,
            token_account(&pubkey_at(usdc_data, 42), &owner, LIQUIDATOR_USDC, false),
        ),
        (
            dest_csol,
            token_account(&pubkey_at(sol_data, 227), &owner, 0, false),
        ),
        (
            dest_sol,
            token_account(&pubkey_at(sol_data, 42), &owner, 0, true),
        ),
    ] {
        svm.set_account(address, account).unwrap();
    }

    let refresh_reserve = |reserve: Pubkey, data: &[u8]| Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(reserve, false),
            AccountMeta::new_readonly(pubkey_at(data, 107), false),
            AccountMeta::new_readonly(pubkey_at(data, 139), false),
        ],
        data: vec![3],
    };
    let refresh = vec![
        refresh_reserve(sol, sol_data),
        refresh_reserve(usdc, usdc_data),
        Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta::new(obligation, false),
                AccountMeta::new(sol, false),
                AccountMeta::new(usdc, false),
            ],
            data: vec![7],
        },
    ];
    let mut liquidate_data = vec![17];
    liquidate_data.extend_from_slice(&REPAY_USDC.to_le_bytes());
    let (authority, _) = Pubkey::find_program_address(&[market.as_ref()], &program);
    let liquidate = Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(source_usdc, false),
            AccountMeta::new(dest_csol, false),
            AccountMeta::new(dest_sol, false),
            AccountMeta::new(usdc, false),
            AccountMeta::new(pubkey_at(usdc_data, 75), false),
            AccountMeta::new(sol, false),
            AccountMeta::new(pubkey_at(sol_data, 227), false),
            AccountMeta::new(pubkey_at(sol_data, 267), false),
            AccountMeta::new(pubkey_at(sol_data, 75), false),
            AccountMeta::new(pubkey_at(sol_data, 339), false),
            AccountMeta::new(obligation, false),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new_readonly(token_program, false),
        ],
        data: liquidate_data,
    };

    send(&mut svm, &liquidator, refresh.clone()).expect("refresh_reserve x2 + refresh_obligation");
    let refreshed = svm.get_account(&obligation).unwrap().data;
    let borrowed_value = u128_at(&refreshed, 90);
    let unhealthy_borrow_value = u128_at(&refreshed, 122);
    let sol_market_price = u128_at(&svm.get_account(&sol).unwrap().data, 211);

    let mut instructions = refresh;
    instructions.push(liquidate);
    let liquidation = send(&mut svm, &liquidator, instructions).map(|_| {
        let received = u64_at(&svm.get_account(&dest_sol).unwrap().data, 64);
        let debt_left = u128_at(&svm.get_account(&obligation).unwrap().data, 340);
        (received, debt_left)
    });

    Health {
        borrowed_value,
        unhealthy_borrow_value,
        sol_market_price,
        liquidation,
    }
}

fn send(svm: &mut LiteSVM, payer: &Keypair, instructions: Vec<Instruction>) -> Result<(), String> {
    let mut all = vec![ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)];
    all.extend(instructions);
    let tx = Transaction::new_signed_with_payer(
        &all,
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n")))
}

fn assert_liquidated(health: &Health, debt_before: u128) {
    assert!(
        health.borrowed_value >= health.unhealthy_borrow_value,
        "still healthy: borrowed {} vs unhealthy {}",
        health.borrowed_value,
        health.unhealthy_borrow_value
    );
    let (received, debt_left) = health
        .liquidation
        .as_ref()
        .unwrap_or_else(|e| panic!("liquidation failed: {e}"));
    assert!(*received > 0, "liquidator received no SOL");
    assert!(debt_left < &debt_before, "no debt repaid");
}

/// The position built by the template is healthy after a real refresh, priced from the Pyth
/// account the reserve names, and a liquidation is refused.
#[tokio::test]
async fn save_position_template_builds_a_healthy_position() {
    let mut fixture = fixture().await;
    let publish_time = prepare_position(&mut fixture);
    let oracle = fixture.data(SOL_ORACLE);
    let expo = i32::from_le_bytes(oracle[PYTH_EXPO..PYTH_EXPO + 4].try_into().unwrap());
    let pyth_price = i64_at(oracle, PYTH_PRICE) as u128 * 10u128.pow((18 + expo) as u32);

    let health = run(&fixture, publish_time);

    assert_eq!(health.sol_market_price, pyth_price, "SOL priced from Pyth");
    let ratio = health.borrowed_value as f64 / health.unhealthy_borrow_value as f64;
    assert!((0.8..0.87).contains(&ratio), "borrowed/unhealthy {ratio}");
    let error = health
        .liquidation
        .expect_err("a healthy position must not liquidate");
    assert!(
        error.contains("Obligation must be unhealthy"),
        "unexpected rejection: {error}"
    );
}

/// Lowering the collateral reserve's liquidation threshold below the loan's share makes the same
/// position liquidatable, and a real liquidation goes through.
#[tokio::test]
async fn save_lower_threshold_makes_the_position_liquidatable() {
    let mut fixture = fixture().await;
    let publish_time = prepare_position(&mut fixture);
    let debt_before = u128_at(fixture.data(OBLIGATION_3D2B), 340);
    apply(
        &mut fixture,
        "save-reserve-risk",
        SOL_RESERVE,
        serde_json::json!({ "config.liquidation_threshold": 50 }),
    );

    assert_liquidated(&run(&fixture, publish_time), debt_before);
}

/// Halving SOL through pyth-price-feed-v2 makes the same position liquidatable, and a real
/// liquidation goes through.
#[tokio::test]
async fn save_sol_price_crash_makes_the_position_liquidatable() {
    let mut fixture = fixture().await;
    let publish_time = prepare_position(&mut fixture);
    let debt_before = u128_at(fixture.data(OBLIGATION_3D2B), 340);
    let oracle = fixture.data(SOL_ORACLE);
    let (price, ema) = (i64_at(oracle, PYTH_PRICE), i64_at(oracle, PYTH_EMA));
    apply_pyth(
        &mut fixture,
        SOL_ORACLE,
        serde_json::json!({
            "price_message.price": price / 2,
            "price_message.ema_price": ema / 2,
        }),
    );

    assert_liquidated(&run(&fixture, publish_time), debt_before);
}
