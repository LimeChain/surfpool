//! Tessera V raw-layout and deployed-program tests.
//!
//! These drive the shipped templates through `materialize_raw_layout` before replaying the current
//! deployed program. Tessera publishes no IDL, and visually plausible offsets are not evidence that
//! a field reaches pricing.

use std::{collections::HashMap, sync::Arc};

use litesvm::LiteSVM;
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::{TemplateRegistry, resolve_live_constants},
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    tests::helpers::diff_indices,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const fn pubkey(address: &str) -> Pubkey {
    Pubkey::from_str_const(address)
}

const PROGRAM: Pubkey = pubkey("TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH");
const PROGRAMDATA: Pubkey = pubkey("BzSXM6KLDpHQQChzr7Fdgbzwp8r8zRYWFFrHK2uZmDYV");
const DEPLOYED_SLOT: u64 = 446053401;
const GLOBAL_STATE: Pubkey = pubkey("8ekCy2jHHUbW2yeNGFWYJT9Hm9FW7SvZcZK66dSZCDiF");
const V11_SENTINEL: Pubkey = pubkey("8xeaWCsJYxRoudEZGJWURdfrtFhLYZz9b4iHJnW5tb3d");
const V11_CONFIG: Pubkey = pubkey("BAT1Ndpu5gbLTp2AZkSXP79LJBZfCH4B3zGhi6LtvdhK");
const V11_MARKET_RECORD: Pubkey = pubkey("4cG31VNF9TzFinNc7BmnjhFvGjxkY3sCETVMtMgbrhPs");
const USDC_VAULT: Pubkey = pubkey("9t4P5wMwfFkyn92Z7hf463qYKEZf8ERVZsGBEPNp8uJx");
const WSOL_MINT: Pubkey = pubkey("So11111111111111111111111111111111111111112");
const USDC_MINT: Pubkey = pubkey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const DFLOW_PROGRAM: Pubkey = pubkey("DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH");
const JUPITER_PROGRAM: Pubkey = pubkey("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

const MARKET_SIZE: usize = 1264;
const MARKET_TAG: [u8; 8] = [5, 0, 0, 0, 0, 0, 0, 0];
const LEVELS: usize = 20;
const LEVEL_SIZE: usize = 24;
const STALE: &str = "Custom(65535)";

const SOL_USDC: MarketSpec = MarketSpec {
    address: pubkey("FLckHLGMJy5gEoXWwcE68Nprde1D4araK4TGLw4pQq2n"),
    base_vault: pubkey("5pVN5XZB8cYBjNLFrsBCPWkCQBan5K5Mq2dWGzwPgGJV"),
    quote_vault: USDC_VAULT,
    base_mint: WSOL_MINT,
    quote_mint: USDC_MINT,
};
const CBB_USDC: MarketSpec = MarketSpec {
    address: pubkey("9NkuAWB4LgCVFV77omEkJEjXqgV5PGupwMTu3B3pBRhc"),
    base_vault: pubkey("37hggNyT4Ec8GEcxMLrWrZyrMSSFMSiFT6VBayRYceZH"),
    quote_vault: USDC_VAULT,
    base_mint: pubkey("cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij"),
    quote_mint: USDC_MINT,
};
const SOL_SELL: u64 = 238_781_608;
const SOL_BUY: u64 = 22_000_000;
const CBB_SELL: u64 = 125_853;
const CBB_BUY: u64 = 4_000_000;
const SOL_SELL_UNIT: u64 = 1_000_000_000;
const SOL_BUY_UNIT: u64 = 100_000_000;
const CBB_SELL_UNIT: u64 = 1_000_000;
const CBB_BUY_UNIT: u64 = 1_000_000_000;

#[derive(Clone, Copy)]
struct MarketSpec {
    address: Pubkey,
    base_vault: Pubkey,
    quote_vault: Pubkey,
    base_mint: Pubkey,
    quote_mint: Pubkey,
}

#[derive(Clone)]
struct MarketFork {
    spec: MarketSpec,
    market: Account,
    base_vault: Account,
    quote_vault: Account,
    base_mint: Account,
    quote_mint: Account,
}

#[derive(Clone)]
struct TesseraFork {
    elf: Vec<u8>,
    global_state: Account,
    sentinel: Account,
    config: Account,
    market_record: Account,
    sol: MarketFork,
    cbb: MarketFork,
}

#[derive(Clone, Copy)]
enum Route {
    DFlow,
    Jupiter,
}

// Tessera's swap entrypoints are reached through a router CPI, so a builtin forwards the call.
declare_process_instruction!(RouterCpi, 1, |invoke_context| {
    let instruction = {
        let context = invoke_context
            .transaction_context
            .get_current_instruction_context()?;
        let accounts = (1..context.get_number_of_instruction_accounts())
            .map(|index| {
                Ok(AccountMeta {
                    pubkey: *context.get_key_of_instruction_account(index)?,
                    is_signer: context.is_instruction_account_signer(index)?,
                    is_writable: context.is_instruction_account_writable(index)?,
                })
            })
            .collect::<Result<Vec<_>, solana_instruction::error::InstructionError>>()?;
        Instruction {
            program_id: PROGRAM,
            accounts,
            data: context.get_instruction_data().to_vec(),
        }
    };
    invoke_context.native_invoke_signed(instruction, &[])
});

async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let mut attempt = 0;
    let results = loop {
        match client
            .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
            .await
        {
            Ok(v) => break v,
            Err(e) => {
                attempt += 1;
                assert!(attempt < 5, "fetch {addresses:?}: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(750 * attempt)).await;
            }
        }
    };
    results
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| match result {
            GetAccountResult::FoundAccount(_, account, _)
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
            GetAccountResult::None(_) => panic!("{address} no longer exists"),
        })
        .collect()
}

async fn fork() -> TesseraFork {
    static CACHE: tokio::sync::OnceCell<Arc<TesseraFork>> = tokio::sync::OnceCell::const_new();
    let cached = CACHE
        .get_or_init(|| async {
            let markets = [SOL_USDC, CBB_USDC];
            let mut addresses = vec![
                PROGRAMDATA,
                GLOBAL_STATE,
                V11_SENTINEL,
                V11_CONFIG,
                V11_MARKET_RECORD,
            ];
            for spec in &markets {
                addresses.extend([
                    spec.address,
                    spec.base_vault,
                    spec.quote_vault,
                    spec.base_mint,
                    spec.quote_mint,
                ]);
            }
            // The markets share USDC accounts, and the remote client answers a repeated key with None.
            addresses.sort_unstable();
            addresses.dedup();
            let accounts: HashMap<Pubkey, Account> = addresses
                .iter()
                .copied()
                .zip(fetch(&addresses).await)
                .collect();
            let get = |key: Pubkey| accounts[&key].clone();
            let programdata = get(PROGRAMDATA);
            assert!(
                programdata.data.len() > 400_000,
                "programdata is unexpectedly short"
            );
            let deployed_slot = u64::from_le_bytes(programdata.data[4..12].try_into().unwrap());
            assert_eq!(
                deployed_slot, DEPLOYED_SLOT,
                "program redeployed at slot {deployed_slot}; revalidate the raw offsets"
            );
            let [sol, cbb] = markets.map(|spec| {
                let market = get(spec.address);
                assert_eq!(market.owner, PROGRAM);
                assert_eq!(market.data.len(), MARKET_SIZE);
                MarketFork {
                    spec,
                    market,
                    base_vault: get(spec.base_vault),
                    quote_vault: get(spec.quote_vault),
                    base_mint: get(spec.base_mint),
                    quote_mint: get(spec.quote_mint),
                }
            });
            Arc::new(TesseraFork {
                elf: programdata.data[45..].to_vec(),
                global_state: get(GLOBAL_STATE),
                sentinel: get(V11_SENTINEL),
                config: get(V11_CONFIG),
                market_record: get(V11_MARKET_RECORD),
                sol,
                cbb,
            })
        })
        .await;
    (**cached).clone()
}

fn user_token_account(
    mint: &Pubkey,
    owner: &Pubkey,
    token_program: Pubkey,
    amount: u64,
) -> Account {
    let is_native = *mint == WSOL_MINT;
    let mut data = vec![0u8; 165];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    if is_native {
        data[109..113].copy_from_slice(&1u32.to_le_bytes());
        data[113..121].copy_from_slice(&2_039_280u64.to_le_bytes());
    }
    Account {
        lamports: if is_native {
            amount.saturating_add(2_039_280)
        } else {
            10_000_000
        },
        data,
        owner: token_program,
        executable: false,
        rent_epoch: 0,
    }
}

/// Direction 1 sells base for quote, direction 0 buys base with quote.
fn swap(
    fork: &TesseraFork,
    market: &MarketFork,
    amount_in: u64,
    direction: u8,
    route: Route,
    market_data: Vec<u8>,
) -> Result<u64, String> {
    let spec = market.spec;
    let mut market_account = market.market.clone();
    market_account.data = market_data;

    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(PROGRAM, &fork.elf)
        .map_err(|e| format!("add_program: {e:?}"))?;
    let (router, unix_timestamp) = match route {
        Route::DFlow => (DFLOW_PROGRAM, 1_787_551_143),
        Route::Jupiter => (JUPITER_PROGRAM, 1_787_662_925),
    };
    svm.add_builtin(router, RouterCpi::register);
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = clock_slot(market);
    clock.unix_timestamp = unix_timestamp;
    svm.set_sysvar(&clock);

    let taker = Keypair::new();
    svm.airdrop(&taker.pubkey(), 10_000_000_000)
        .map_err(|e| format!("airdrop: {e:?}"))?;
    let (base_program, quote_program) = (market.base_mint.owner, market.quote_mint.owner);
    let user_base = Pubkey::new_unique();
    let user_quote = Pubkey::new_unique();
    let (base_amount, quote_amount, destination) = if direction == 1 {
        (amount_in, 0, user_quote)
    } else {
        (0, amount_in, user_base)
    };
    let last_restart_slot = Account {
        lamports: 1_000_000,
        data: 246_464_040u64.to_le_bytes().to_vec(),
        owner: pubkey("Sysvar1111111111111111111111111111111111111"),
        executable: false,
        rent_epoch: 0,
    };
    for (key, account) in [
        (
            pubkey("SysvarLastRestartS1ot1111111111111111111111"),
            last_restart_slot,
        ),
        (GLOBAL_STATE, fork.global_state.clone()),
        (V11_SENTINEL, fork.sentinel.clone()),
        (V11_CONFIG, fork.config.clone()),
        (V11_MARKET_RECORD, fork.market_record.clone()),
        (spec.address, market_account),
        (spec.base_vault, market.base_vault.clone()),
        (spec.quote_vault, market.quote_vault.clone()),
        (spec.base_mint, market.base_mint.clone()),
        (spec.quote_mint, market.quote_mint.clone()),
        (
            user_base,
            user_token_account(&spec.base_mint, &taker.pubkey(), base_program, base_amount),
        ),
        (
            user_quote,
            user_token_account(
                &spec.quote_mint,
                &taker.pubkey(),
                quote_program,
                quote_amount,
            ),
        ),
    ] {
        svm.set_account(key, account)
            .map_err(|e| format!("set {key}: {e:?}"))?;
    }

    let (tag, route_account) = match route {
        Route::DFlow => (0x11, AccountMeta::new_readonly(V11_SENTINEL, true)),
        Route::Jupiter => (
            0x10,
            AccountMeta::new_readonly(pubkey("Sysvar1nstructions1111111111111111111111111"), false),
        ),
    };
    let mut data = vec![tag, direction];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    let mut instructions = Vec::new();
    if let Route::DFlow = route {
        data.push(0);
        let mut budget = vec![2u8];
        budget.extend_from_slice(&1_400_000u32.to_le_bytes());
        instructions.push(Instruction {
            program_id: pubkey("ComputeBudget111111111111111111111111111111"),
            accounts: vec![],
            data: budget,
        });
    }
    instructions.push(Instruction {
        program_id: router,
        accounts: vec![
            AccountMeta::new_readonly(PROGRAM, false),
            AccountMeta::new_readonly(GLOBAL_STATE, false),
            AccountMeta::new(spec.address, false),
            AccountMeta::new(taker.pubkey(), true),
            AccountMeta::new(spec.base_vault, false),
            AccountMeta::new(spec.quote_vault, false),
            AccountMeta::new(user_base, false),
            AccountMeta::new(user_quote, false),
            AccountMeta::new_readonly(spec.base_mint, false),
            AccountMeta::new_readonly(spec.quote_mint, false),
            AccountMeta::new_readonly(base_program, false),
            AccountMeta::new_readonly(quote_program, false),
            route_account,
            AccountMeta::new_readonly(V11_CONFIG, false),
            AccountMeta::new_readonly(V11_MARKET_RECORD, false),
        ],
        data,
    });

    let mut message = solana_message::Message::new(&instructions, Some(&taker.pubkey()));
    message.recent_blockhash = svm.latest_blockhash();
    let signature_count = message.header.num_required_signatures as usize;
    let mut transaction = solana_transaction::Transaction::new_unsigned(message);
    transaction.signatures = vec![solana_signature::Signature::default(); signature_count];
    transaction.signatures[0] = taker.sign_message(&transaction.message.serialize());
    svm.send_transaction(transaction)
        .map_err(|e| format!("{:?}", e.err))?;
    Ok(read_u64(
        &svm.get_account(&destination).expect("destination").data,
        64,
    ))
}

fn clock_slot(market: &MarketFork) -> u64 {
    read_u64(&market.market.data, 120) + 1
}

fn write_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn apply_raw(id: &str, data: &[u8], values: &[(String, serde_json::Value)], slot: u64) -> Vec<u8> {
    let registry = TemplateRegistry::new();
    let t = registry.get(id).unwrap_or_else(|| panic!("missing {id}"));
    assert!(t.raw_layout, "{id} must use raw-layout writes");
    let map = values.iter().cloned().collect::<HashMap<_, _>>();
    t.materialize_raw_layout(data, &map, slot)
        .unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn values<const N: usize>(
    pairs: [(&str, serde_json::Value); N],
) -> Vec<(String, serde_json::Value)> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn first_level_output(market: &[u8], amount_in: u64, direction: u8) -> u64 {
    let (price_offset, factor_offset) = if direction == 1 {
        (128, 168)
    } else {
        (144, 648)
    };
    let output = u128::from(amount_in)
        * u128::from(read_u64(market, price_offset))
        * u128::from(read_u64(market, factor_offset))
        / 1_000_000_000_000_000_000_000_u128;
    u64::try_from(output).unwrap()
}

// The maker pulls and restores whole ladders live, so behaviour is proven on a fixed local ladder.
fn pin_ladder(data: &mut [u8], sell_unit: u64, buy_unit: u64) {
    for (start, unit) in [(160, sell_unit), (640, buy_unit)] {
        data[start..start + LEVELS * LEVEL_SIZE].fill(0);
        for (level, (scale, factor)) in [(1, 999_900), (2, 999_500), (4, 999_000)]
            .into_iter()
            .enumerate()
        {
            let record = start + level * LEVEL_SIZE;
            write_u64(data, record, unit * scale);
            write_u64(data, record + 8, factor);
            data[record + 16] = 1;
        }
    }
}

// The program picks one of five quote-start configs by market-record age, so all are neutralized.
fn set_quote_start(data: &mut [u8], skipped_levels: u8) {
    write_u64(data, 0, 0);
    write_u64(data, 8, 0);
    for selector in 0..5 {
        let config = 1136 + 12 * selector;
        data[config..config + 4].copy_from_slice(&0u32.to_le_bytes());
        data[config + 4..config + 8].copy_from_slice(&1_000_000u32.to_le_bytes());
        data[config + 8] = skipped_levels;
    }
}

fn scaled_ladder(
    market: &[u8],
    field: &str,
    sell_bps: u64,
    buy_bps: u64,
) -> Vec<(String, serde_json::Value)> {
    let field_offset = if field == "amount" { 0 } else { 8 };
    let mut out = Vec::new();
    for (side, start, bps) in [("sell", 160, sell_bps), ("buy", 640, buy_bps)] {
        for level in 0..LEVELS {
            let record = start + level * LEVEL_SIZE;
            if field == "amount" && market[record + 16] == 0 {
                continue;
            }
            let live = read_u64(market, record + field_offset);
            let scaled = (u128::from(live) * u128::from(bps) / 10_000) as u64;
            assert!(
                live == 0 || scaled > 0,
                "{side} level {level} rounds to zero"
            );
            out.push((
                format!("{side}_level_{level}_{field}"),
                serde_json::json!(scaled.to_string()),
            ));
        }
    }
    out
}

fn price_values(quote_per_base: u64) -> Vec<(String, serde_json::Value)> {
    let base_per_quote = 10u128.pow(30) / u128::from(quote_per_base);
    values([
        (
            "quote_atoms_per_base_atom_x1e15",
            serde_json::json!(quote_per_base.to_string()),
        ),
        (
            "base_atoms_per_quote_atom_x1e15",
            serde_json::json!(u64::try_from(base_per_quote).unwrap().to_string()),
        ),
    ])
}

#[tokio::test]
async fn tessera_resolver_returns_every_live_market() {
    let registry = TemplateRegistry::new();
    let all_templates = registry.by_protocol("Tessera");
    assert_eq!(all_templates.len(), 5);
    let rpc_url = std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());
    let served = resolve_live_constants(
        &rpc_url,
        all_templates.iter().map(|t| (*t).clone()).collect(),
    )
    .await;
    let catalog = served[0].constants["market"].options.clone();
    assert!(
        !catalog.is_empty(),
        "no live markets resolved; a 429 or timeout from {rpc_url} is unverified, not a failure"
    );
    assert_eq!(
        catalog[0].value,
        SOL_USDC.address.to_string(),
        "SOL / USDC is the default market"
    );
    for template in &served {
        assert_eq!(
            template.constants["market"].options, catalog,
            "{}",
            template.id
        );
        assert_eq!(
            template.address,
            AccountAddress::Pubkey(catalog[0].value.clone()),
            "{} defaults to the first market",
            template.id
        );
    }
    let addresses = catalog
        .iter()
        .map(|option| pubkey(&option.value))
        .collect::<Vec<_>>();
    let markets = fetch(&addresses).await;
    let meta = |option: &surfpool_types::ConstantOption, key: &str| option.metadata[key].clone();

    let mut mints = catalog
        .iter()
        .flat_map(|option| {
            ["base_mint", "quote_mint"].map(|key| pubkey(meta(option, key).as_str().unwrap()))
        })
        .collect::<Vec<_>>();
    mints.sort_unstable();
    mints.dedup();
    let mint_accounts: HashMap<Pubkey, Account> =
        mints.iter().copied().zip(fetch(&mints).await).collect();

    let mut checked = 0;
    for (option, account) in catalog.iter().zip(&markets) {
        let data = &account.data;
        let id = &option.id;
        assert_eq!(meta(option, "account"), option.value.as_str(), "{id}");
        assert_eq!(account.owner, PROGRAM, "{id}");
        assert_eq!(data.len(), MARKET_SIZE, "{id}");
        assert_eq!(
            &data[96..104],
            &MARKET_TAG,
            "{id} lost the observed market tag"
        );
        for (key, at, decimals_key) in [
            ("base_mint", 24, "base_decimals"),
            ("quote_mint", 56, "quote_decimals"),
        ] {
            let mint = pubkey(meta(option, key).as_str().unwrap());
            assert_eq!(&data[at..at + 32], mint.as_ref(), "{id} {key}");
            assert_eq!(
                u64::from(mint_accounts[&mint].data[44]),
                meta(option, decimals_key).as_u64().unwrap(),
                "{id} {decimals_key}"
            );
        }
        assert_eq!(
            read_u64(data, 88),
            meta(option, "freshness_limit_slots").as_u64().unwrap(),
            "{id} freshness limit"
        );

        for template in &all_templates {
            assert_eq!(
                apply_raw(&template.id, data, &[], 0),
                data.to_vec(),
                "{} must round-trip {id}",
                template.id
            );
        }
        let priced = apply_raw(
            "tessera-price",
            data,
            &price_values(1_000_000_000_000_000),
            0,
        );
        assert!(
            diff_indices(data, &priced)
                .iter()
                .all(|i| (128..136).contains(i) || (144..152).contains(i)),
            "{id} escaped the price write set"
        );
        let halted = apply_raw(
            "tessera-halt",
            data,
            &values([
                ("sell_levels_enabled", serde_json::json!(0)),
                ("buy_levels_enabled", serde_json::json!(0)),
            ]),
            0,
        );
        for i in diff_indices(data, &halted) {
            let in_ladder = (160..1120).contains(&i) && (i - 160) % LEVEL_SIZE == 16;
            assert!(in_ladder, "{id} halt wrote byte {i}");
        }
        checked += 1;
    }
    assert_eq!(
        checked,
        catalog.len(),
        "every live market must be exercised"
    );
}

#[tokio::test]
async fn tessera_scenario_materializes_once_through_surfnet_svm() {
    const BASE_SLOT: u64 = 1_000_000;
    let fork = fork().await;
    let market_key = SOL_USDC.address;
    let original = fork.sol.market.data.clone();
    let target = AccountAddress::Pubkey(market_key.to_string());

    let price = price_values(100_000_000_000_000);
    let mut scenario = Scenario::new("Tessera SOL at 100".into(), "price and freshness".into());
    scenario.add_override(
        OverrideInstance::new("tessera-price".into(), 0, target.clone())
            .with_values(price.iter().cloned().collect()),
    );
    scenario.add_override(
        OverrideInstance::new("tessera-freshness".into(), 0, target).with_values(HashMap::from([
            ("last_update_slot".to_string(), serde_json::json!(0)),
        ])),
    );

    let (mut svm, _events, _geyser) = SurfnetSvm::default();
    svm.inner
        .set_account(market_key, fork.sol.market.clone())
        .unwrap();
    svm.register_scenario(scenario, Some(BASE_SLOT)).unwrap();
    svm.materialize_overrides_for_slot(&None, BASE_SLOT)
        .await
        .expect("materialize Tessera scenario");
    let read = |svm: &SurfnetSvm| svm.inner.get_account(&market_key).unwrap().unwrap().data;
    let materialized = read(&svm);
    assert_eq!(read_u64(&materialized, 120), BASE_SLOT);
    assert_eq!(read_u64(&materialized, 128), 100_000_000_000_000);
    assert_eq!(read_u64(&materialized, 144), 10_000_000_000_000_000);
    assert!(
        diff_indices(&original, &materialized)
            .iter()
            .all(|i| (120..136).contains(i) || (144..152).contains(i)),
        "materialization escaped the price and freshness write set"
    );

    svm.materialize_overrides_for_slot(&None, BASE_SLOT + 1)
        .await
        .unwrap();
    assert_eq!(read(&svm), materialized, "overrides must apply once");
}

#[tokio::test]
async fn tessera_price_formula_reprices_both_directions() {
    let mut fork = fork().await;
    set_quote_start(&mut fork.sol.market.data, 0);
    pin_ladder(&mut fork.sol.market.data, SOL_SELL_UNIT, SOL_BUY_UNIT);
    let sol = &fork.sol;
    let live = sol.market.data.clone();
    let sell = |data: Vec<u8>| swap(&fork, sol, SOL_SELL, 1, Route::DFlow, data);
    let buy = |data: Vec<u8>| swap(&fork, sol, SOL_BUY, 0, Route::DFlow, data);

    let baseline_sell = sell(live.clone()).expect("baseline sell");
    let baseline_buy = buy(live.clone()).expect("baseline buy");
    assert_eq!(baseline_sell, first_level_output(&live, SOL_SELL, 1));
    assert_eq!(baseline_buy, first_level_output(&live, SOL_BUY, 0));

    let doubled = apply_raw(
        "tessera-price",
        &live,
        &values([
            (
                "quote_atoms_per_base_atom_x1e15",
                serde_json::json!(read_u64(&live, 128) * 2),
            ),
            (
                "base_atoms_per_quote_atom_x1e15",
                serde_json::json!(read_u64(&live, 144) / 2),
            ),
        ]),
        0,
    );
    let doubled_sell = sell(doubled.clone()).expect("doubled sell");
    let doubled_buy = buy(doubled).expect("doubled buy");
    assert!(doubled_sell > baseline_sell * 199 / 100 && doubled_sell < baseline_sell * 201 / 100);
    assert!(doubled_buy > baseline_buy * 49 / 100 && doubled_buy < baseline_buy * 51 / 100);

    let sell_side_only = apply_raw(
        "tessera-price",
        &live,
        &values([(
            "quote_atoms_per_base_atom_x1e15",
            serde_json::json!(read_u64(&live, 128) * 2),
        )]),
        0,
    );
    assert_eq!(
        buy(sell_side_only).unwrap(),
        baseline_buy,
        "offset 128 must not move buys"
    );
    let buy_side_only = apply_raw(
        "tessera-price",
        &live,
        &values([(
            "base_atoms_per_quote_atom_x1e15",
            serde_json::json!(read_u64(&live, 144) / 2),
        )]),
        0,
    );
    assert_eq!(
        sell(buy_side_only).unwrap(),
        baseline_sell,
        "offset 144 must not move sells"
    );

    // "SOL is worth $100" with 9/6 decimals: 100 * 10^(6-9) * 10^15.
    let at_100 = apply_raw(
        "tessera-price",
        &live,
        &price_values(100_000_000_000_000),
        0,
    );
    let factor = u128::from(read_u64(&live, 168));
    let expected = u128::from(SOL_SELL) * 100_000_000_000_000 * factor / 10u128.pow(21);
    assert_eq!(u128::from(sell(at_100.clone()).unwrap()), expected);
    let buy_factor = u128::from(read_u64(&live, 648));
    let expected_buy = u128::from(SOL_BUY) * 10_000_000_000_000_000 * buy_factor / 10u128.pow(21);
    assert_eq!(u128::from(buy(at_100).unwrap()), expected_buy);
}

#[tokio::test]
async fn tessera_freshness_boundary_follows_each_market_limit() {
    let mut fork = fork().await;
    set_quote_start(&mut fork.sol.market.data, 0);
    pin_ladder(&mut fork.sol.market.data, SOL_SELL_UNIT, SOL_BUY_UNIT);
    let sol = &fork.sol;
    let live_limit = read_u64(&sol.market.data, 88);
    let slot = clock_slot(sol);
    let aged = |data: &[u8], lead: i64| {
        apply_raw(
            "tessera-freshness",
            data,
            &values([("last_update_slot", serde_json::json!(lead))]),
            slot,
        )
    };

    for limit in [live_limit, 25, 55] {
        let mut configured = sol.market.data.clone();
        write_u64(&mut configured, 88, limit);
        let fresh = aged(&configured, 0);
        assert_eq!(read_u64(&fresh, 120), slot);
        assert!(
            diff_indices(&configured, &fresh)
                .iter()
                .all(|i| (120..128).contains(i))
        );
        let lead = -(limit as i64);
        let just_fresh = aged(&configured, lead + 1);
        let stale = aged(&configured, lead);
        assert_eq!(read_u64(&stale, 120), slot - limit);
        for (direction, amount) in [(1, SOL_SELL), (0, SOL_BUY)] {
            let run = |data: Vec<u8>| swap(&fork, sol, amount, direction, Route::DFlow, data);
            assert!(run(fresh.clone()).expect("a fresh quote fills") > 0);
            assert!(run(just_fresh.clone()).expect("one slot inside the limit fills") > 0);
            let err = run(stale.clone()).expect_err("a quote at its limit must reject");
            assert!(
                err.contains(STALE),
                "limit {limit} direction {direction}: {err}"
            );
        }
    }
}

#[tokio::test]
async fn tessera_depth_thins_only_large_fills() {
    let mut fork = fork().await;
    set_quote_start(&mut fork.cbb.market.data, 0);
    pin_ladder(&mut fork.cbb.market.data, CBB_SELL_UNIT, CBB_BUY_UNIT);
    let cbb = &fork.cbb;
    let live = cbb.market.data.clone();
    for (sell_bps, buy_bps) in [(1_000, 10_000), (10_000, 1_000)] {
        let thinned = apply_raw(
            "tessera-depth",
            &live,
            &scaled_ladder(&live, "amount", sell_bps, buy_bps),
            0,
        );
        for i in diff_indices(&live, &thinned) {
            assert!(
                (160..1120).contains(&i) && (i - 160) % LEVEL_SIZE < 8,
                "byte {i}"
            );
        }
        for (direction, first_record, bps) in [(1, 160, sell_bps), (0, 640, buy_bps)] {
            let first_capacity = read_u64(&live, first_record);
            let small = first_capacity / 20;
            let large = first_capacity * 2;
            assert!(small > 0);
            let run = |amount, data: &[u8]| {
                swap(&fork, cbb, amount, direction, Route::Jupiter, data.to_vec()).unwrap()
            };
            let small_expected = first_level_output(&live, small, direction);
            assert_eq!(run(small, &live), small_expected);
            assert_eq!(run(small, &thinned), small_expected);
            let (before, after) = (run(large, &live), run(large, &thinned));
            if bps == 10_000 {
                assert_eq!(after, before, "direction {direction} must be untouched");
            } else {
                assert!(
                    after > 0 && after < before,
                    "direction {direction}: thinner depth must worsen a large fill: {before} -> {after}"
                );
            }
        }
    }
}

#[tokio::test]
async fn tessera_curve_scales_output_and_rejects_unordered_factors() {
    let mut fork = fork().await;
    set_quote_start(&mut fork.cbb.market.data, 0);
    pin_ladder(&mut fork.cbb.market.data, CBB_SELL_UNIT, CBB_BUY_UNIT);
    let cbb = &fork.cbb;
    let live = cbb.market.data.clone();
    let amount = CBB_SELL.min(read_u64(&live, 160) / 2);
    assert!(amount > 0);
    let run = |data: Vec<u8>| swap(&fork, cbb, amount, 1, Route::Jupiter, data);

    let baseline = run(live.clone()).expect("baseline sell");
    assert_eq!(baseline, first_level_output(&live, amount, 1));
    let halved = apply_raw(
        "tessera-curve",
        &live,
        &scaled_ladder(&live, "factor", 5_000, 10_000),
        0,
    );
    for i in diff_indices(&live, &halved) {
        assert!(
            (160..1120).contains(&i) && (i - 160) % LEVEL_SIZE >= 8,
            "byte {i}"
        );
    }
    let halved_out = run(halved).expect("half-factor sell");
    assert!(halved_out > baseline * 49 / 100 && halved_out < baseline * 51 / 100);
    let buy_side = apply_raw(
        "tessera-curve",
        &live,
        &scaled_ladder(&live, "factor", 10_000, 5_000),
        0,
    );
    assert_eq!(
        run(buy_side).unwrap(),
        baseline,
        "buy factors must not move sells"
    );

    let unordered = apply_raw(
        "tessera-curve",
        &live,
        &values([("sell_level_0_factor", serde_json::json!(500_000))]),
        0,
    );
    let err = run(unordered).expect_err("a rising factor must reject");
    assert!(err.contains("Custom(8)"), "{err}");

    let with_second_factor = |factor: u64| {
        apply_raw(
            "tessera-curve",
            &live,
            &values([("sell_level_1_factor", serde_json::json!(factor.to_string()))]),
            0,
        )
    };
    let first_factor = read_u64(&live, 168);
    let err = run(with_second_factor(first_factor)).expect_err("an equal factor must reject");
    assert!(err.contains("Custom(8)"), "{err}");
    assert_eq!(run(with_second_factor(first_factor - 1)).unwrap(), baseline);
}

#[tokio::test]
async fn tessera_halt_rejects_only_halted_directions() {
    let mut fork = fork().await;
    for skipped_levels in [None, Some(1)] {
        set_quote_start(&mut fork.cbb.market.data, skipped_levels.unwrap_or(0));
        pin_ladder(&mut fork.cbb.market.data, CBB_SELL_UNIT, CBB_BUY_UNIT);
        let cbb = &fork.cbb;
        let live = cbb.market.data.clone();
        let halted = apply_raw(
            "tessera-halt",
            &live,
            &values([
                ("sell_levels_enabled", serde_json::json!(0)),
                ("buy_levels_enabled", serde_json::json!(0)),
            ]),
            0,
        );
        let mut expected = live.clone();
        for start in [176, 656] {
            for level in 0..LEVELS {
                expected[start + level * LEVEL_SIZE] = 0;
            }
        }
        assert_eq!(halted, expected);
        for (direction, amount) in [(1, CBB_SELL), (0, CBB_BUY)] {
            let run = |data: Vec<u8>| swap(&fork, cbb, amount, direction, Route::Jupiter, data);
            let baseline = run(live.clone()).unwrap();
            assert!(baseline > 0);
            if skipped_levels.is_some() {
                let mut first_only = live.clone();
                first_only[176] = 0;
                first_only[656] = 0;
                assert_eq!(
                    run(first_only).unwrap(),
                    baseline,
                    "clearing a skipped first level cannot halt the quote"
                );
            }
            let err = run(halted.clone()).expect_err("a halted market must reject");
            assert!(err.contains(STALE), "direction {direction}: {err}");
        }
        let sell_halted = apply_raw(
            "tessera-halt",
            &live,
            &values([("sell_levels_enabled", serde_json::json!(0))]),
            0,
        );
        let run_side = |direction, amount, data: Vec<u8>| {
            swap(&fork, cbb, amount, direction, Route::Jupiter, data)
        };
        let err =
            run_side(1, CBB_SELL, sell_halted.clone()).expect_err("a halted sell side must reject");
        assert!(err.contains(STALE), "{err}");
        assert_eq!(
            run_side(0, CBB_BUY, sell_halted).expect("the buy side must still fill"),
            run_side(0, CBB_BUY, live.clone()).unwrap()
        );
    }
}
