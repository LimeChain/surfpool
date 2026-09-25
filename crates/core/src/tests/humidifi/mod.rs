//! HumidiFi v8 raw-layout and deployed-program tests.
//!
//! HumidiFi publishes no IDL and checks its caller through the Instructions sysvar, so the swap
//! proofs replay a captured DFlow route against the live ELF in LiteSVM after driving the shipped
//! templates through `materialize_raw_layout` or the Surfnet materializer.

use std::{collections::HashMap, sync::Arc};

use litesvm::LiteSVM;
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
use solana_message::Message;
use solana_program_pack::Pack;
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use spl_token_interface::error::TokenError;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::{TemplateRegistry, resolve_live_constants},
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
const PROGRAM: Pubkey = Pubkey::from_str_const("9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp");
const PROGRAMDATA: Pubkey = Pubkey::from_str_const("G9S64i58RRWJA28vZiNhnP56Ux4Ef7hfMgHNREnZZSom");
const DEPLOYED_SLOT: u64 = 449592669;

const LAYOUT_TAG: [u8; 8] = [44, 90, 19, 124, 56, 111, 47, 150];
const FAIR_VALUE_MASK: u64 = 0xb957_ed15_dc87_7426;
const SLOT_MASK: u64 = 0x6e9d_e2b3_0b19_f1ea;
const PUBKEY_MASKS: [u64; 4] = [
    0xfb5c_e87a_ae44_3c38,
    0x04a2_1784_51ba_c3c7,
    0x04a1_1787_51b9_c3c6,
    0x04a0_1786_51b8_c3c5,
];
const STALE_QUOTE_ERROR: u32 = 0xfaded;
const FAIR_VALUE: std::ops::Range<usize> = 576..584;
const LAST_UPDATE: std::ops::Range<usize> = 616..624;
const AMOUNT: std::ops::Range<usize> = 64..72;

async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let mut out = Vec::new();
    for batch in addresses.chunks(100) {
        let mut attempt = 0;
        let results = loop {
            match client
                .get_multiple_accounts(batch, CommitmentConfig::confirmed())
                .await
            {
                Ok(v) => break v,
                Err(e) => {
                    attempt += 1;
                    assert!(attempt < 5, "fetch {batch:?}: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(750 * attempt)).await;
                }
            }
        };
        out.extend(
            results
                .into_iter()
                .zip(batch)
                .map(|(result, address)| match result {
                    GetAccountResult::FoundAccount(_, account, _)
                    | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
                    GetAccountResult::None(_) => panic!("{address} no longer exists"),
                }),
        );
    }
    out
}

fn word(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn masked_pubkey(data: &[u8], offset: usize) -> Pubkey {
    let mut bytes = [0u8; 32];
    for (i, mask) in PUBKEY_MASKS.iter().enumerate() {
        bytes[i * 8..(i + 1) * 8]
            .copy_from_slice(&(word(data, offset + i * 8) ^ mask).to_le_bytes());
    }
    Pubkey::new_from_array(bytes)
}

fn amount(data: &[u8]) -> u64 {
    word(data, 64)
}

fn apply_raw(id: &str, data: &[u8], values: &[(&str, serde_json::Value)], slot: u64) -> Vec<u8> {
    let registry = TemplateRegistry::new();
    let t = registry.get(id).unwrap_or_else(|| panic!("missing {id}"));
    assert!(t.raw_layout, "{id} must use raw-layout writes");
    let map = values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect::<HashMap<_, _>>();
    t.materialize_raw_layout(data, &map, slot)
        .unwrap_or_else(|e| panic!("{id}: {e}"))
}

// A masked write can leave a byte equal to the original, so only "nothing outside moved" holds.
fn assert_only_within(
    before: &[u8],
    after: &[u8],
    ranges: &[std::ops::Range<usize>],
    context: &str,
) {
    assert_eq!(before.len(), after.len(), "{context}: length changed");
    for (i, (a, b)) in before.iter().zip(after).enumerate() {
        assert!(
            a == b || ranges.iter().any(|r| r.contains(&i)),
            "{context}: byte {i} changed outside {ranges:?}"
        );
    }
}

fn values(entries: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

#[tokio::test]
async fn humidifi_resolver_returns_every_live_market_and_vault() {
    let registry = TemplateRegistry::new();
    let templates = registry.by_protocol("HumidiFi");
    assert_eq!(templates.len(), 3);
    let rpc_url = std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());
    let served = resolve_live_constants(&rpc_url, templates.into_iter().cloned().collect()).await;
    let options = |id: &str| {
        let template = served.iter().find(|t| t.id == id).unwrap();
        let options = template.constants["market"].options.clone();
        assert!(
            !options.is_empty(),
            "{id} resolved no options; a 429 or timeout from {rpc_url} is unverified, not a failure"
        );
        assert_eq!(
            template.address,
            AccountAddress::Pubkey(options[0].value.clone()),
            "{id} defaults to the first option"
        );
        options
    };
    let markets = options("humidifi-price");
    assert_eq!(options("humidifi-freshness"), markets);
    let vaults = options("humidifi-vault-balance");
    let meta = |option: &surfpool_types::ConstantOption, key: &str| {
        option.metadata[key]
            .as_str()
            .map(Pubkey::from_str_const)
            .unwrap_or_else(|| panic!("{}: {key}", option.id))
    };
    let number = |option: &surfpool_types::ConstantOption, key: &str| {
        option.metadata[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{}: {key}", option.id))
    };
    assert_eq!(
        (
            meta(&markets[0], "base_mint"),
            meta(&markets[0], "quote_mint")
        ),
        (WSOL_MINT, USDC_MINT),
        "SOL / USDC is the default market"
    );

    let market_keys = markets
        .iter()
        .map(|m| Pubkey::from_str_const(&m.value))
        .collect::<Vec<_>>();
    let vault_keys = vaults
        .iter()
        .map(|v| Pubkey::from_str_const(&v.value))
        .collect::<Vec<_>>();
    let mut mint_keys = markets
        .iter()
        .flat_map(|m| [meta(m, "base_mint"), meta(m, "quote_mint")])
        .collect::<Vec<_>>();
    mint_keys.sort_unstable();
    mint_keys.dedup();
    let market_accounts = fetch(&market_keys).await;
    let vault_accounts = fetch(&vault_keys).await;
    let mint_accounts = fetch(&mint_keys).await;
    let mint = |key: Pubkey| &mint_accounts[mint_keys.binary_search(&key).unwrap()];

    for (option, account) in markets.iter().zip(&market_accounts) {
        let data = &account.data;
        let id = &option.id;
        assert_eq!(meta(option, "account").to_string(), option.value, "{id}");
        assert_eq!(account.owner, PROGRAM, "{id}");
        assert_eq!(data.len(), 1728, "{id}");
        assert_eq!(&data[8..16], &LAYOUT_TAG, "{id} lost the layout tag");
        assert_eq!(word(data, 1720), 8, "{id} is no longer schema 8");
        for (offset, key) in [
            (384, "quote_mint"),
            (416, "base_mint"),
            (448, "quote_vault"),
            (480, "base_vault"),
        ] {
            assert_eq!(
                masked_pubkey(data, offset),
                meta(option, key),
                "{id}: {key}"
            );
        }
        for (key, decimals) in [
            ("base_mint", "base_decimals"),
            ("quote_mint", "quote_decimals"),
        ] {
            assert_eq!(
                u64::from(mint(meta(option, key)).data[44]),
                number(option, decimals),
                "{id}: {decimals}"
            );
        }
        assert_eq!(
            word(data, 608) ^ SLOT_MASK,
            number(option, "max_staleness_slots"),
            "{id}"
        );
        assert!(
            word(data, 616) ^ SLOT_MASK >= number(option, "last_update_slot"),
            "{id}: the live quote slot cannot move backwards"
        );

        for template in ["humidifi-price", "humidifi-freshness"] {
            assert_eq!(
                apply_raw(template, data, &[], 0),
                *data,
                "{template} must round-trip {id}"
            );
        }
        let priced = apply_raw(
            "humidifi-price",
            data,
            &[("fair_value", serde_json::json!("123456789"))],
            0,
        );
        assert_only_within(data, &priced, &[FAIR_VALUE], id);
        assert_eq!(word(&priced, 576) ^ FAIR_VALUE_MASK, 123_456_789);
    }

    assert_eq!(vaults.len(), 2 * markets.len(), "one option per vault");
    for (option, account) in vaults.iter().zip(&vault_accounts) {
        let id = &option.id;
        let side = option.metadata["side"].as_str().unwrap();
        let market = markets
            .iter()
            .find(|m| m.value == option.metadata["account"])
            .unwrap_or_else(|| panic!("{id} belongs to no listed market"));
        assert_eq!(
            meta(market, &format!("{side}_vault")).to_string(),
            option.value,
            "{id}"
        );
        let mint_key = meta(market, &format!("{side}_mint"));
        assert_eq!(account.owner, mint(mint_key).owner, "{id} token program");
        assert_eq!(
            Pubkey::try_from(&account.data[..32]).unwrap(),
            mint_key,
            "{id}"
        );
        assert_eq!(account.data[108], 1, "{id} is not initialized");
        assert_eq!(
            apply_raw("humidifi-vault-balance", &account.data, &[], 0),
            account.data
        );
        let changed = apply_raw(
            "humidifi-vault-balance",
            &account.data,
            &[("amount", serde_json::json!(123u64))],
            0,
        );
        assert_only_within(&account.data, &changed, &[AMOUNT], id);
        assert_eq!(amount(&changed), 123);
    }
    println!(
        "resolved {} HumidiFi markets and {} vaults",
        markets.len(),
        vaults.len()
    );
}

// Instruction metadata captured from DFlow outer instruction 2 of tx 3zevqwAa8u136UGE1bdBzP1o2dpFuc7X333dC1ut3T1uCgY6iidihfNJNoJ7tuyj3tHNin1i7HTFCUSFWqK7g1Si (slot 444225745).
const DFLOW: Pubkey = Pubkey::from_str_const("DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH");
const SIGNER0: Pubkey = Pubkey::from_str_const("4HJaX8K9mH9fMLGn4Xc5DjGdjDWXNSnnX5kkXFuzk2ET");
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

const INPUT_QUOTE_AMOUNT: u64 = 2_427_890;
// Obfuscated by HumidiFi; it encodes the fixed input above.
const SWAP_DATA_HEX: &str = "6f86a350a21ac2b9c9f40bffe3baeac339ff2dffe0bae9c309";
// Kept verbatim so the Instructions sysvar shows a faithful router instruction.
const DFLOW_DATA_HEX: &str = "f8c69e91e17587c80600000025af1df3ba698aa144c79fb835e10649c7698eed21f706d1d24eb9a14f7dd0ce4c3fcd6752db35e4c5ec7f9c670b824cf28236bd4ab7af472aa63da2a26212570664597a1a000024bc9a000000323c31020000006a9991000000000006835faa1a00000000091f020000000381878ec3c80af75c0355798caf40a0297af20b25000000000001118e5964010000000000feb4a000000000003c020000";

#[derive(Clone, Copy)]
struct MarketDef {
    market: Pubkey,
    base_vault: Pubkey,
    quote_vault: Pubkey,
    /// How this market refuses a swap its payout vault holds nothing for.
    empty_payout_error: u32,
}

/// The catalog's WSOL/USDC market, and a paused WSOL/USDC market that still holds deep inventory.
const MARKETS: [MarketDef; 2] = [
    MarketDef {
        market: Pubkey::from_str_const("8sKQHfjNhvmAw94PhfvfMcytmqW6jmxvwieYyzXCCPu"),
        base_vault: Pubkey::from_str_const("H292B1VbSvD6GuUmSvUvfQstg1Acfzog796uQ7d1ccCw"),
        quote_vault: Pubkey::from_str_const("A3C9xwv4Hfx92M5HQpxUiibSqCa5pYhD2kTwnU5fEPq"),
        empty_payout_error: 49,
    },
    MarketDef {
        market: Pubkey::from_str_const("FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH"),
        base_vault: Pubkey::from_str_const("C3FzbX9n1YD2dow2dCmEv5uNyyf22Gb3TLAEqGBhw5fY"),
        quote_vault: Pubkey::from_str_const("3RWFAQBRkNGq7CMGcTLK3kXDgFTe9jgMeFYqk8nHwcWh"),
        empty_payout_error: TokenError::InsufficientFunds as u32,
    },
];

thread_local! {
    // The shim is a plain builtin with no arguments, so the replay tells it which market to route to.
    static ROUTE: std::cell::Cell<MarketDef> = const { std::cell::Cell::new(MARKETS[0]) };
}

// DFlow signs for SIGNER0 and SYS0 as PDAs; sigverify is off, so the replay marks them signers.
fn humidifi_metas(def: MarketDef) -> Vec<AccountMeta> {
    [
        (SIGNER0, true, true),
        (def.market, true, false),
        (def.base_vault, true, false),
        (def.quote_vault, true, false),
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

// Registered under DFlow's id so HumidiFi's Instructions-sysvar caller check sees its router.
declare_process_instruction!(HumidiFiRouteShim, 1, |invoke_context| {
    invoke_context.native_invoke_signed(
        Instruction {
            program_id: PROGRAM,
            accounts: humidifi_metas(ROUTE.with(std::cell::Cell::get)),
            data: hex::decode(SWAP_DATA_HEX).unwrap(),
        },
        &[],
    )?;
    Ok(())
});

#[derive(Clone)]
struct HumidifiFork {
    def: MarketDef,
    elf: Vec<u8>,
    market: Account,
    base_vault: Account,
    quote_vault: Account,
    shared: Vec<(Pubkey, Account)>,
    /// A clock the live quote is fresh at, so price proofs are not staleness proofs.
    slot: u64,
    max_staleness: u64,
}

async fn forks() -> Arc<Vec<HumidifiFork>> {
    static CACHE: tokio::sync::OnceCell<Arc<Vec<HumidifiFork>>> =
        tokio::sync::OnceCell::const_new();
    CACHE
        .get_or_init(|| async {
            let programdata = fetch(&[PROGRAMDATA]).await.remove(0);
            assert!(
                programdata.data.len() > 300_000,
                "programdata is unexpectedly short"
            );
            let deployed_slot = u64::from_le_bytes(programdata.data[4..12].try_into().unwrap());
            assert_eq!(
                deployed_slot, DEPLOYED_SLOT,
                "program redeployed at slot {deployed_slot}; revalidate the raw offsets"
            );
            let shared_keys = [WSOL_MINT, USDC_MINT, VOTE, ROUTE_STATE];
            let mut out = Vec::new();
            for def in MARKETS {
                let a = fetch(
                    &[
                        &[def.market, def.base_vault, def.quote_vault][..],
                        &shared_keys,
                    ]
                    .concat(),
                )
                .await;
                assert_eq!(a[0].data.len(), 1728);
                assert_eq!(masked_pubkey(&a[0].data, 480), def.base_vault);
                assert_eq!(masked_pubkey(&a[0].data, 448), def.quote_vault);
                out.push(HumidifiFork {
                    def,
                    elf: programdata.data[45..].to_vec(),
                    slot: word(&a[0].data, 616) ^ SLOT_MASK,
                    max_staleness: word(&a[0].data, 608) ^ SLOT_MASK,
                    market: a[0].clone(),
                    base_vault: a[1].clone(),
                    quote_vault: a[2].clone(),
                    shared: shared_keys
                        .into_iter()
                        .zip(a[3..].iter().cloned())
                        .collect(),
                });
            }
            Arc::new(out)
        })
        .await
        .clone()
}

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
        owner: Pubkey::default(),
        ..Account::default()
    }
}

/// Swaps the captured USDC input for WSOL against the supplied market and vault bytes, returning
/// the WSOL the user received.
fn replay(
    fork: &HumidifiFork,
    clock_slot: u64,
    market: Vec<u8>,
    base_vault: Vec<u8>,
    quote_vault: Vec<u8>,
) -> Result<u64, TransactionError> {
    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(PROGRAM, &fork.elf)
        .expect("load HumidiFi ELF");
    svm.add_builtin(DFLOW, HumidiFiRouteShim::register);
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = clock_slot;
    clock.unix_timestamp = 1_788_000_000;
    svm.set_sysvar(&clock);

    for (key, template, data) in [
        (fork.def.market, &fork.market, market),
        (fork.def.base_vault, &fork.base_vault, base_vault),
        (fork.def.quote_vault, &fork.quote_vault, quote_vault),
    ] {
        svm.set_account(
            key,
            Account {
                data,
                ..template.clone()
            },
        )
        .unwrap();
    }
    for (key, account) in &fork.shared {
        svm.set_account(*key, account.clone()).unwrap();
    }
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

    let mut accounts = vec![AccountMeta::new_readonly(PROGRAM, false)];
    accounts.extend(humidifi_metas(fork.def));
    let route = Instruction {
        program_id: DFLOW,
        accounts,
        data: hex::decode(DFLOW_DATA_HEX).unwrap(),
    };
    let compute_limit = Instruction {
        program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
        accounts: vec![],
        data: [vec![2], 1_400_000u32.to_le_bytes().to_vec()].concat(),
    };
    let message = Message::new_with_blockhash(
        &[compute_limit, route],
        Some(&SIGNER0),
        &svm.latest_blockhash(),
    );
    let tx = Transaction {
        signatures: vec![Signature::default(); message.header.num_required_signatures as usize],
        message,
    };
    ROUTE.with(|route| route.set(fork.def));
    svm.send_transaction(tx).map_err(|failed| failed.err)?;
    assert_eq!(
        1_000_000_000 - amount(&svm.get_account(&USER_USDC).unwrap().data),
        INPUT_QUOTE_AMOUNT,
        "the captured instruction's quote input changed"
    );
    Ok(amount(&svm.get_account(&DEST_WSOL).unwrap().data))
}

fn run(fork: &HumidifiFork, clock_slot: u64, market: Vec<u8>) -> Result<u64, TransactionError> {
    replay(
        fork,
        clock_slot,
        market,
        fork.base_vault.data.clone(),
        fork.quote_vault.data.clone(),
    )
}

/// WSOL lamports that `INPUT_QUOTE_AMOUNT` USDC atoms buy at `price` USDC per SOL, before spread.
fn expected_output(price: u64) -> u64 {
    (u128::from(INPUT_QUOTE_AMOUNT) * 1_000_000_000 / (u128::from(price) * 1_000_000)) as u64
}

fn priced_market(fork: &HumidifiFork, price: u64) -> Vec<u8> {
    let fair_value = u128::from(price) * (1u128 << 48) / 1_000;
    let priced = apply_raw(
        "humidifi-price",
        &fork.market.data,
        &[("fair_value", serde_json::json!(fair_value.to_string()))],
        fork.slot,
    );
    apply_raw(
        "humidifi-freshness",
        &priced,
        &[("last_update_slot", serde_json::json!(0))],
        fork.slot,
    )
}

#[tokio::test]
async fn humidifi_templates_write_only_proven_bytes_on_both_replay_fixtures() {
    for fork in forks().await.iter() {
        let slot = fork.slot + 1_000;
        let context = fork.def.market.to_string();

        let priced = apply_raw(
            "humidifi-price",
            &fork.market.data,
            &[("fair_value", serde_json::json!("58546795155816"))],
            slot,
        );
        assert_only_within(&fork.market.data, &priced, &[FAIR_VALUE], &context);
        assert_eq!(word(&priced, 576) ^ FAIR_VALUE_MASK, 58_546_795_155_816);

        let fresh = apply_raw(
            "humidifi-freshness",
            &fork.market.data,
            &[
                ("last_update_slot", serde_json::Value::Null),
                ("max_staleness_slots", serde_json::json!(40)),
            ],
            slot,
        );
        assert_only_within(
            &fork.market.data,
            &fresh,
            &[608..616, LAST_UPDATE],
            &context,
        );
        assert_eq!(word(&fresh, 616) ^ SLOT_MASK, slot);
        assert_eq!(word(&fresh, 608) ^ SLOT_MASK, 40);

        let aged = apply_raw(
            "humidifi-freshness",
            &fork.market.data,
            &[("last_update_slot", serde_json::json!(-3))],
            slot,
        );
        assert_only_within(&fork.market.data, &aged, &[LAST_UPDATE], &context);
        assert_eq!(word(&aged, 616) ^ SLOT_MASK, slot - 3);

        for vault in [&fork.base_vault.data, &fork.quote_vault.data] {
            let replacement = amount(vault) / 2;
            let changed = apply_raw(
                "humidifi-vault-balance",
                vault,
                &[("amount", serde_json::json!(replacement))],
                slot,
            );
            assert_only_within(vault, &changed, &[AMOUNT], &context);
            assert_eq!(amount(&changed), replacement);
        }
    }
}

#[tokio::test]
async fn humidifi_price_moves_the_executed_fill() {
    for fork in forks().await.iter() {
        let mut outputs = Vec::new();
        for price in [100u64, 208] {
            let output = run(fork, fork.slot, priced_market(fork, price))
                .unwrap_or_else(|e| panic!("{} price {price} must fill: {e:?}", fork.def.market));
            let expected = expected_output(price);
            assert!(
                output.abs_diff(expected) <= expected / 100,
                "{} price {price}: output {output} is more than 1% from {expected}",
                fork.def.market
            );
            outputs.push(output);
        }
        let ratio = outputs[0] as f64 / outputs[1] as f64;
        assert!(
            (ratio - 2.08).abs() < 0.02,
            "{}: a 2.08x cheaper base must buy about 2.08x more, got {ratio}",
            fork.def.market
        );
    }
}

#[tokio::test]
async fn humidifi_price_scenario_materializes_through_surfnet() {
    let fork = &forks().await[0];
    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    svm.inner
        .set_account(fork.def.market, fork.market.clone())
        .expect("seed market");

    let mut scenario = Scenario::new(
        "HumidiFi WSOL/USDC at 208".to_string(),
        "Reprice SOL to 208 USDC and keep the quote fresh".to_string(),
    );
    for (template_id, override_values) in [
        (
            "humidifi-price",
            values(&[("fair_value", serde_json::json!("58546795155816"))]),
        ),
        (
            "humidifi-freshness",
            values(&[("last_update_slot", serde_json::json!(0))]),
        ),
    ] {
        scenario.add_override(
            OverrideInstance::new(
                template_id.to_string(),
                0,
                AccountAddress::Pubkey(fork.def.market.to_string()),
            )
            .with_values(override_values),
        );
    }
    let slot = fork.slot + 500;
    svm.register_scenario(scenario, Some(slot))
        .expect("register the price scenario");
    svm.materialize_overrides_for_slot(&None, slot)
        .await
        .expect("materialize the price scenario");

    let market = svm
        .inner
        .get_account(&fork.def.market)
        .unwrap()
        .unwrap()
        .data;
    assert_only_within(
        &fork.market.data,
        &market,
        &[FAIR_VALUE, LAST_UPDATE],
        "scenario market",
    );
    assert_eq!(word(&market, 576) ^ FAIR_VALUE_MASK, 58_546_795_155_816);
    assert_eq!(word(&market, 616) ^ SLOT_MASK, slot);

    let output = run(fork, slot, market).expect("the materialized quote must fill");
    let expected = expected_output(208);
    assert!(
        output.abs_diff(expected) <= expected / 100,
        "output {output} is more than 1% from {expected}"
    );
}

#[tokio::test]
async fn humidifi_freshness_boundary_is_real_program_behavior_on_both_markets() {
    let mut checked = 0;
    for fork in forks().await.iter() {
        let clock = fork.slot + 100;
        for limit in [fork.max_staleness, fork.max_staleness + 8] {
            for age in [limit, limit + 1] {
                let staged = apply_raw(
                    "humidifi-freshness",
                    &fork.market.data,
                    &[
                        ("last_update_slot", serde_json::json!(-(age as i64))),
                        ("max_staleness_slots", serde_json::json!(limit)),
                    ],
                    clock,
                );
                let result = run(fork, clock, staged);
                let context = format!("{} limit {limit}, age {age}", fork.def.market);
                if age > limit {
                    assert_eq!(
                        result,
                        Err(TransactionError::InstructionError(
                            1,
                            InstructionError::Custom(STALE_QUOTE_ERROR)
                        )),
                        "{context}"
                    );
                } else {
                    assert!(
                        result.unwrap_or_else(|e| panic!("{context}: {e:?}")) > 0,
                        "{context}: a quote at the limit must fill"
                    );
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 8, "both boundaries at two limits on both markets");
}

async fn materialized_vault(slot: u64, address: Pubkey, seeded: Account, balance: u64) -> Vec<u8> {
    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    svm.inner.set_account(address, seeded).expect("seed vault");
    let mut scenario = Scenario::new(
        "HumidiFi vault inventory".to_string(),
        "Lower one HumidiFi vault".to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            "humidifi-vault-balance".to_string(),
            0,
            AccountAddress::Pubkey(address.to_string()),
        )
        .with_values(values(&[("amount", serde_json::json!(balance))])),
    );
    svm.register_scenario(scenario, Some(slot))
        .expect("register the vault scenario");
    svm.materialize_overrides_for_slot(&None, slot)
        .await
        .expect("materialize the vault scenario");
    svm.inner.get_account(&address).unwrap().unwrap().data
}

#[tokio::test]
async fn humidifi_vault_scenario_drains_only_the_selected_side() {
    for fork in forks().await.iter() {
        let context = fork.def.market.to_string();
        let baseline = run(fork, fork.slot, fork.market.data.clone()).expect("baseline swap");
        let base_swap = |base_vault: Vec<u8>| {
            replay(
                fork,
                fork.slot,
                fork.market.data.clone(),
                base_vault,
                fork.quote_vault.data.clone(),
            )
        };

        let drained_base =
            materialized_vault(fork.slot, fork.def.base_vault, fork.base_vault.clone(), 0).await;
        assert_only_within(&fork.base_vault.data, &drained_base, &[AMOUNT], &context);
        assert_eq!(amount(&drained_base), 0);
        assert_eq!(
            base_swap(drained_base),
            Err(TransactionError::InstructionError(
                1,
                InstructionError::Custom(fork.def.empty_payout_error)
            )),
            "{context}"
        );

        let thin_balance = amount(&fork.base_vault.data) / 200;
        let thin_base = materialized_vault(
            fork.slot,
            fork.def.base_vault,
            fork.base_vault.clone(),
            thin_balance,
        )
        .await;
        let thin_output = base_swap(thin_base).expect("a thin base vault still settles");
        assert!(
            thin_output < baseline,
            "{context}: thin inventory {thin_output} must quote below {baseline}"
        );

        // The quote vault only receives this direction's input.
        let drained_quote =
            materialized_vault(fork.slot, fork.def.quote_vault, fork.quote_vault.clone(), 0).await;
        assert_eq!(amount(&drained_quote), 0);
        assert_eq!(
            replay(
                fork,
                fork.slot,
                fork.market.data.clone(),
                fork.base_vault.data.clone(),
                drained_quote
            ),
            Ok(baseline),
            "{context}"
        );
    }
}
