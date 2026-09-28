//! GoonFi publishes no IDL, so every template write is replayed against the deployed program.

use std::{collections::HashMap, sync::Arc};

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::{
    scenarios::{TemplateRegistry, resolve_live_constants},
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient},
    tests::helpers::diff_indices,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
const PROGRAM: &str = "goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE";
const PROGRAMDATA: &str = "124gUYwjVnJQ4sJsFug9gHPzPLEtwCbAQC5LkbaDgx9s";
const DEPLOYED_SLOT: u64 = 448984043;
const ORACLE_PROGRAM: &str = "dijkbkCAKfFTCxQg3u1pg82gVU1jJGHBBRcteD11mBu";
const GLOBAL: &str = "BNrK9LpEn65QA4TyBLVSMdngW3XHj3xLfFPwGdCBv8wV";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const NATIVE_MINT: &str = "So11111111111111111111111111111111111111112";
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const MARKET_TAG: [u8; 8] = [48, 188, 47, 53, 52, 88, 50, 154];

const PRICE_OUT_OF_BAND: &str = "Custom(36)";
const INSUFFICIENT_LIQUIDITY: &str = "Custom(1)";
const STALE_ORACLE: &str = "Custom(21)";

const SELL: u8 = 0;
const BUY: u8 = 1;

async fn fetch(addresses: &[&str]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let keys: Vec<Pubkey> = addresses
        .iter()
        .map(|a| Pubkey::from_str_const(a))
        .collect();
    let mut attempt = 0;
    let results = loop {
        match client
            .get_multiple_accounts(&keys, CommitmentConfig::confirmed())
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

#[derive(Clone)]
struct MarketDef {
    pair: String,
    market: String,
    oracle: String,
    base_vault: String,
    quote_vault: String,
}

/// How many resolved markets the behavioural tests replay.
const FIXTURES: usize = 3;
/// A fixture's vaults each hold at least this many times the test trade.
const VAULT_COVER: u64 = 10;

#[derive(Clone)]
struct State {
    market: Vec<u8>,
    oracle: Vec<u8>,
    base_vault: Vec<u8>,
    quote_vault: Vec<u8>,
}

#[derive(Clone)]
struct GoonfiFork {
    def: MarketDef,
    elf: Arc<Vec<u8>>,
    global: Account,
    market: Account,
    oracle: Account,
    base_vault: Account,
    quote_vault: Account,
    base_mint: (Pubkey, Account),
    quote_mint: (Pubkey, Account),
    base_trade: u64,
    quote_trade: u64,
    slot: u64,
    unix_timestamp: i64,
}

impl GoonfiFork {
    fn live(&self) -> State {
        State {
            market: self.market.data.clone(),
            oracle: self.oracle.data.clone(),
            base_vault: self.base_vault.data.clone(),
            quote_vault: self.quote_vault.data.clone(),
        }
    }

    fn trade(&self, side: u8) -> u64 {
        if side == SELL {
            self.base_trade
        } else {
            self.quote_trade
        }
    }
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

async fn forks() -> Arc<Vec<GoonfiFork>> {
    static CACHE: tokio::sync::OnceCell<Arc<Vec<GoonfiFork>>> = tokio::sync::OnceCell::const_new();
    CACHE
        .get_or_init(|| async {
            // The public endpoint refuses ProgramData and the global account batched with a market.
            let programdata = fetch(&[PROGRAMDATA]).await.remove(0);
            assert!(
                programdata.data.len() > 240_000,
                "programdata is unexpectedly short"
            );
            let deployed_slot = u64::from_le_bytes(programdata.data[4..12].try_into().unwrap());
            assert_eq!(
                deployed_slot, DEPLOYED_SLOT,
                "program redeployed at slot {deployed_slot}; revalidate the raw offsets"
            );
            let elf = Arc::new(programdata.data[45..].to_vec());
            let global = fetch(&[GLOBAL]).await.remove(0);
            assert_eq!(global.owner, Pubkey::from_str_const(PROGRAM));

            let rpc_url =
                std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());
            let registry = TemplateRegistry::new();
            let band = registry.get("goonfi-reference-band").expect("band template").clone();
            let served = resolve_live_constants(&rpc_url, vec![band]).await;
            let options = served[0].constants["market"].options.clone();
            assert!(
                !options.is_empty(),
                "no live markets resolved; a 429 or timeout from {rpc_url} is unverified, not a failure"
            );

            let mut out = Vec::new();
            for option in &options {
                if out.len() == FIXTURES {
                    break;
                }
                let meta = |key: &str| {
                    option.metadata[key]
                        .as_str()
                        .unwrap_or_else(|| panic!("{} has no {key}", option.id))
                        .to_string()
                };
                let def = MarketDef {
                    pair: meta("pair"),
                    market: option.value.clone(),
                    oracle: meta("oracle"),
                    base_vault: meta("base_vault"),
                    quote_vault: meta("quote_vault"),
                };
                let a = fetch(&[&def.market, &def.oracle, &def.base_vault, &def.quote_vault]).await;
                let (market, oracle) = (&a[0], &a[1]);
                assert_eq!(
                    market.owner,
                    Pubkey::from_str_const(PROGRAM),
                    "{}",
                    def.pair
                );
                assert_eq!(market.data[..8], MARKET_TAG, "{}", def.pair);
                assert_eq!(oracle.owner, Pubkey::from_str_const(ORACLE_PROGRAM));
                assert_eq!(oracle.data.len(), 32, "{}", def.pair);
                let classic_vaults = [&a[2], &a[3]].iter().all(|vault| {
                    vault.owner == Pubkey::from_str_const(TOKEN_PROGRAM) && vault.data.len() == 165
                });
                let bid = u64_at(&oracle.data, 0);
                if !classic_vaults || bid == 0 {
                    continue;
                }
                let base_mint = pubkey_at(&market.data, 80);
                let quote_mint = pubkey_at(&market.data, 112);
                for (vault, mint) in [(&a[2], base_mint), (&a[3], quote_mint)] {
                    assert_eq!(pubkey_at(&vault.data, 0), mint, "{} vault mint", def.pair);
                    assert_eq!(
                        pubkey_at(&vault.data, 32),
                        Pubkey::from_str_const(&def.market),
                        "{} vault authority",
                        def.pair
                    );
                }
                let decimals = |key: &str| {
                    option.metadata[key]
                        .as_u64()
                        .unwrap_or_else(|| panic!("{} has no {key}", option.id))
                        as u32
                };
                // About 100 quote tokens each way, so no live price or balance is pinned.
                let quote_trade = 100 * 10u64.pow(decimals("quote_decimals"));
                let base_trade = (100u128 * 10u128.pow(decimals("base_decimals")) * 1_000_000
                    / u128::from(bid)) as u64;
                if u64_at(&a[2].data, 64) < base_trade.saturating_mul(VAULT_COVER)
                    || u64_at(&a[3].data, 64) < quote_trade.saturating_mul(VAULT_COVER)
                {
                    continue;
                }
                let mints = fetch(&[&base_mint.to_string(), &quote_mint.to_string()]).await;
                out.push(GoonfiFork {
                    slot: u64::from(u32_at(&oracle.data, 16)),
                    unix_timestamp: (u64_at(&oracle.data, 24) / 1_000) as i64,
                    def,
                    elf: elf.clone(),
                    global: global.clone(),
                    market: a[0].clone(),
                    oracle: a[1].clone(),
                    base_vault: a[2].clone(),
                    quote_vault: a[3].clone(),
                    base_mint: (base_mint, mints[0].clone()),
                    quote_mint: (quote_mint, mints[1].clone()),
                    base_trade,
                    quote_trade,
                });
            }
            let first = out.first().expect("no resolved GoonFi market has funded vaults");
            assert_eq!(
                (first.base_mint.0, first.quote_mint.0),
                (
                    Pubkey::from_str_const(NATIVE_MINT),
                    Pubkey::from_str_const(USDC_MINT)
                ),
                "SOL / USDC must be the first funded market; got {}",
                first.def.pair
            );
            Arc::new(out)
        })
        .await
        .clone()
}

fn apply_raw(id: &str, data: &[u8], values: &[(&str, serde_json::Value)], slot: u64) -> Vec<u8> {
    let registry = TemplateRegistry::new();
    let t = registry.get(id).unwrap_or_else(|| panic!("missing {id}"));
    let map = values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect::<HashMap<_, _>>();
    assert!(t.raw_layout, "{id} must use raw-layout writes");
    t.materialize_raw_layout(data, &map, slot)
        .unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn assert_writes_within(before: &[u8], after: &[u8], allowed: std::ops::Range<usize>, what: &str) {
    assert!(
        diff_indices(before, after)
            .iter()
            .all(|i| allowed.contains(i)),
        "{what} escaped bytes {allowed:?}"
    );
}

fn scaled(fork: &GoonfiFork, numerator: u64, denominator: u64, with_band: bool) -> State {
    let scale = |data: &[u8], offset: usize| {
        serde_json::json!(
            (u128::from(u64_at(data, offset)) * u128::from(numerator) / u128::from(denominator))
                as u64
        )
    };
    let mut state = fork.live();
    state.oracle = apply_raw(
        "goonfi-price",
        &state.oracle,
        &[
            ("bid_price_x1e6", scale(&state.oracle, 0)),
            ("ask_price_x1e6", scale(&state.oracle, 8)),
        ],
        fork.slot,
    );
    if with_band {
        state.market = apply_raw(
            "goonfi-reference-band",
            &state.market,
            &[
                ("reference_price_a_x1e6", scale(&state.market, 1712)),
                ("reference_price_b_x1e6", scale(&state.market, 1720)),
            ],
            fork.slot,
        );
    }
    state
}

fn token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Account {
    const RENT: u64 = 2_039_280;
    let native = *mint == Pubkey::from_str_const(NATIVE_MINT);
    let mut data = vec![0u8; 165];
    data[..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    if native {
        data[109] = 1;
        data[113..121].copy_from_slice(&RENT.to_le_bytes());
    }
    Account {
        lamports: if native { RENT + amount } else { RENT },
        data,
        owner: Pubkey::from_str_const(TOKEN_PROGRAM),
        executable: false,
        rent_epoch: 0,
    }
}

/// `clock_slot` is absolute, so a test can age the oracle.
fn run(
    fork: &GoonfiFork,
    state: &State,
    side: u8,
    amount_in: u64,
    clock_slot: u64,
) -> Result<u64, String> {
    use litesvm::LiteSVM;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    let program = Pubkey::from_str_const(PROGRAM);
    let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);
    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(program, &fork.elf)
        .map_err(|e| format!("add_program: {e:?}"))?;
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = clock_slot;
    clock.unix_timestamp = fork.unix_timestamp + 1;
    svm.set_sysvar(&clock);

    let market = Pubkey::from_str_const(&fork.def.market);
    let oracle = Pubkey::from_str_const(&fork.def.oracle);
    let base_vault = Pubkey::from_str_const(&fork.def.base_vault);
    let quote_vault = Pubkey::from_str_const(&fork.def.quote_vault);
    let global = Pubkey::from_str_const(GLOBAL);
    let (base_mint, quote_mint) = (fork.base_mint.0, fork.quote_mint.0);
    for (key, account, data) in [
        (global, &fork.global, &fork.global.data),
        (market, &fork.market, &state.market),
        (oracle, &fork.oracle, &state.oracle),
        (base_vault, &fork.base_vault, &state.base_vault),
        (quote_vault, &fork.quote_vault, &state.quote_vault),
        (base_mint, &fork.base_mint.1, &fork.base_mint.1.data),
        (quote_mint, &fork.quote_mint.1, &fork.quote_mint.1.data),
    ] {
        let mut account = account.clone();
        account.data = data.clone();
        svm.set_account(key, account)
            .map_err(|e| format!("set {key}: {e:?}"))?;
    }

    let taker = Keypair::new();
    svm.airdrop(&taker.pubkey(), 10_000_000_000)
        .map_err(|e| format!("airdrop: {e:?}"))?;
    let user_base = Pubkey::new_unique();
    let user_quote = Pubkey::new_unique();
    let (base_funds, quote_funds) = if side == SELL {
        (amount_in, 0)
    } else {
        (0, amount_in)
    };
    for (key, mint, amount) in [
        (user_base, base_mint, base_funds),
        (user_quote, quote_mint, quote_funds),
    ] {
        svm.set_account(key, token_account(&mint, &taker.pubkey(), amount))
            .map_err(|e| format!("user token account: {e:?}"))?;
    }

    let mut data = vec![1u8, side];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    let mut budget = vec![2u8];
    budget.extend_from_slice(&1_400_000u32.to_le_bytes());
    let instructions = [
        Instruction {
            program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
            accounts: vec![],
            data: budget,
        },
        Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta::new(taker.pubkey(), true),
                AccountMeta::new(market, false),
                AccountMeta::new(user_base, false),
                AccountMeta::new(user_quote, false),
                AccountMeta::new(base_vault, false),
                AccountMeta::new(quote_vault, false),
                AccountMeta::new_readonly(base_mint, false),
                AccountMeta::new_readonly(quote_mint, false),
                AccountMeta::new_readonly(oracle, false),
                AccountMeta::new_readonly(global, false),
                AccountMeta::new_readonly(
                    Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111"),
                    false,
                ),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            data,
        },
    ];
    let mut msg = solana_message::Message::new(&instructions, Some(&taker.pubkey()));
    msg.recent_blockhash = svm.latest_blockhash();
    let mut tx = Transaction::new_unsigned(msg);
    tx.signatures = vec![
        solana_signature::Signature::default();
        tx.message.header.num_required_signatures as usize
    ];
    tx.signatures[0] = taker.sign_message(&tx.message.serialize());
    svm.send_transaction(tx)
        .map_err(|e| format!("{:?} {:?}", e.err, e.meta.logs))?;
    let dst = if side == SELL { user_quote } else { user_base };
    Ok(u64_at(
        &svm.get_account(&dst).expect("destination").data,
        64,
    ))
}

fn assert_rejects(result: Result<u64, String>, code: &str, context: &str) {
    match result {
        Ok(out) => panic!("{context}: expected {code}, got a fill of {out}"),
        Err(e) => assert!(e.contains(code), "{context}: expected {code}, got {e}"),
    }
}

#[tokio::test]
async fn goonfi_templates_write_only_proven_bytes_on_the_fixture_markets() {
    let forks = forks().await;
    for fork in forks.iter() {
        let def = &fork.def;
        let live = fork.live();
        for (id, data) in [
            ("goonfi-price", &live.oracle),
            ("goonfi-freshness", &live.oracle),
            ("goonfi-reference-band", &live.market),
            ("goonfi-vault-balance", &live.base_vault),
            ("goonfi-vault-balance", &live.quote_vault),
        ] {
            assert_eq!(
                &apply_raw(id, data, &[], fork.slot),
                data,
                "{id} must round-trip {}",
                def.pair
            );
        }

        let price = apply_raw(
            "goonfi-price",
            &live.oracle,
            &[
                ("bid_price_x1e6", serde_json::json!(99_740_000u64)),
                ("ask_price_x1e6", serde_json::json!(99_750_000u64)),
            ],
            fork.slot,
        );
        assert_writes_within(&live.oracle, &price, 0..16, "goonfi-price");
        assert_eq!(
            (u64_at(&price, 0), u64_at(&price, 8)),
            (99_740_000, 99_750_000)
        );

        let fresh = apply_raw(
            "goonfi-freshness",
            &live.oracle,
            &[("last_update_slot", serde_json::json!(-7))],
            fork.slot + 100,
        );
        assert_writes_within(&live.oracle, &fresh, 16..20, "goonfi-freshness");
        assert_eq!(u64::from(u32_at(&fresh, 16)), fork.slot + 93);

        let band = apply_raw(
            "goonfi-reference-band",
            &live.market,
            &[
                ("reference_price_a_x1e6", serde_json::json!(1u64)),
                ("reference_price_b_x1e6", serde_json::json!(2u64)),
            ],
            fork.slot,
        );
        assert_writes_within(&live.market, &band, 1712..1728, "goonfi-reference-band");
        assert_eq!((u64_at(&band, 1712), u64_at(&band, 1720)), (1, 2));

        let vault = apply_raw(
            "goonfi-vault-balance",
            &live.quote_vault,
            &[("amount", serde_json::json!(123u64))],
            fork.slot,
        );
        assert_writes_within(&live.quote_vault, &vault, 64..72, "goonfi-vault-balance");
        assert_eq!(u64_at(&vault, 64), 123);
    }
}

#[tokio::test]
async fn goonfi_resolver_returns_the_live_markets_with_their_oracles_and_vaults() {
    let registry = TemplateRegistry::new();
    let templates = registry.by_protocol("GoonFi");
    assert_eq!(templates.len(), 4);
    let rpc_url = std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());
    let served =
        resolve_live_constants(&rpc_url, templates.iter().map(|t| (*t).clone()).collect()).await;
    let options = |id: &str| {
        served
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .constants["market"]
            .options
            .clone()
    };
    let markets = options("goonfi-reference-band");
    let oracles = options("goonfi-price");
    let vaults = options("goonfi-vault-balance");
    assert!(
        !markets.is_empty(),
        "no live markets resolved; a 429 or timeout from {rpc_url} is unverified, not a failure"
    );
    assert_eq!(options("goonfi-freshness"), oracles);
    assert_eq!(oracles.len(), markets.len());
    assert_eq!(vaults.len(), 2 * markets.len());
    for template in &served {
        let first = &template.constants["market"].options[0];
        assert_eq!(
            template.address,
            surfpool_types::AccountAddress::Pubkey(first.value.clone()),
            "{} defaults to its first option",
            template.id
        );
        assert_eq!(first.metadata["base_mint"], NATIVE_MINT, "{}", template.id);
        assert_eq!(first.metadata["quote_mint"], USDC_MINT, "{}", template.id);
    }

    let addresses = markets
        .iter()
        .map(|option| option.value.as_str())
        .collect::<Vec<_>>();
    let mut accounts = HashMap::new();
    for chunk in addresses.chunks(20) {
        accounts.extend(chunk.iter().copied().zip(fetch(chunk).await));
    }
    let text = |option: &surfpool_types::ConstantOption, key: &str| {
        option.metadata[key]
            .as_str()
            .unwrap_or_else(|| panic!("{} has no {key}", option.id))
            .to_string()
    };
    let bytes_of = |option: &surfpool_types::ConstantOption| {
        let market = &accounts[text(option, "account").as_str()];
        assert_eq!(
            market.owner,
            Pubkey::from_str_const(PROGRAM),
            "{}",
            option.id
        );
        assert_eq!(market.data[..8], MARKET_TAG, "{}", option.id);
        market.data.clone()
    };
    let at = |data: &[u8], offset: usize| pubkey_at(data, offset).to_string();

    for option in &markets {
        let data = bytes_of(option);
        assert_eq!(text(option, "account"), option.value, "{}", option.id);
        for (key, offset) in [
            ("base_mint", 80),
            ("quote_mint", 112),
            ("base_vault", 144),
            ("quote_vault", 176),
            ("oracle", 208),
        ] {
            assert_eq!(text(option, key), at(&data, offset), "{} {key}", option.id);
        }
    }
    for option in &oracles {
        let data = bytes_of(option);
        assert_eq!(option.value, at(&data, 208), "{} oracle pointer", option.id);
    }
    for option in &vaults {
        let data = bytes_of(option);
        let offset = match option.metadata["side"].as_str() {
            Some("base") => 144,
            Some("quote") => 176,
            other => panic!("{} has side {other:?}", option.id),
        };
        assert_eq!(
            option.value,
            at(&data, offset),
            "{} vault pointer",
            option.id
        );
    }
}

/// One override per shipped collection, through the production materializer, then replayed.
#[tokio::test]
async fn goonfi_scenario_materializes_every_collection_through_surfnet_svm() {
    use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

    use crate::surfnet::svm::SurfnetSvm;

    let fork = &forks().await[0];
    let live = fork.live();
    let live_bid = u64_at(&live.oracle, 0);
    let target = live_bid * 3 / 2;
    let baseline = run(fork, &live, SELL, fork.base_trade, fork.slot + 1).expect("baseline sell");
    let expected = (u128::from(baseline) * u128::from(target) / u128::from(live_bid)) as u64;
    let payout_capacity = expected / 2;

    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    for (address, seeded) in [
        (&fork.def.oracle, &fork.oracle),
        (&fork.def.market, &fork.market),
        (&fork.def.quote_vault, &fork.quote_vault),
    ] {
        svm.inner
            .set_account(Pubkey::from_str_const(address), seeded.clone())
            .unwrap_or_else(|e| panic!("seed {address}: {e:?}"));
    }
    let mut scenario = Scenario::new(
        "GoonFi SOL/USDC repricing".to_string(),
        "Reprice SOL by 1.5x with its band, keep the quote fresh and cap USDC inventory"
            .to_string(),
    );
    for (template_id, target_account, values) in [
        (
            "goonfi-price",
            &fork.def.oracle,
            vec![
                ("bid_price_x1e6", serde_json::json!(target)),
                ("ask_price_x1e6", serde_json::json!(target)),
            ],
        ),
        (
            "goonfi-freshness",
            &fork.def.oracle,
            vec![("last_update_slot", serde_json::json!(0))],
        ),
        (
            "goonfi-reference-band",
            &fork.def.market,
            vec![
                ("reference_price_a_x1e6", serde_json::json!(target)),
                ("reference_price_b_x1e6", serde_json::json!(target)),
            ],
        ),
        (
            "goonfi-vault-balance",
            &fork.def.quote_vault,
            vec![("amount", serde_json::json!(payout_capacity))],
        ),
    ] {
        scenario.add_override(
            OverrideInstance::new(
                template_id.to_string(),
                0,
                AccountAddress::Pubkey(target_account.clone()),
            )
            .with_values(
                values
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            ),
        );
    }
    svm.register_scenario(scenario, Some(fork.slot))
        .expect("register the GoonFi scenario");
    svm.materialize_overrides_for_slot(&None, fork.slot)
        .await
        .expect("materialize every GoonFi override");

    let materialized = |address: &str| {
        svm.inner
            .get_account(&Pubkey::from_str_const(address))
            .expect("read scenario account")
            .unwrap_or_else(|| panic!("missing scenario account {address}"))
            .data
    };
    let prepared = State {
        market: materialized(&fork.def.market),
        oracle: materialized(&fork.def.oracle),
        base_vault: live.base_vault.clone(),
        quote_vault: live.quote_vault.clone(),
    };
    let limited_quote_vault = materialized(&fork.def.quote_vault);
    assert_writes_within(&live.oracle, &prepared.oracle, 0..20, "oracle overrides");
    assert_writes_within(&live.market, &prepared.market, 1712..1728, "band override");
    assert_writes_within(&live.quote_vault, &limited_quote_vault, 64..72, "vault");
    assert_eq!(u64::from(u32_at(&prepared.oracle, 16)), fork.slot);
    assert_eq!(u64_at(&limited_quote_vault, 64), payout_capacity);

    let filled = run(fork, &prepared, SELL, fork.base_trade, fork.slot + 1)
        .expect("sell against the materialized price and band");
    assert!(
        filled.abs_diff(expected) <= expected / 500,
        "the materialized price must set the fill: {filled} vs ~{expected}"
    );

    let capped = State {
        quote_vault: limited_quote_vault,
        ..prepared.clone()
    };
    assert_rejects(
        run(fork, &capped, SELL, fork.base_trade, fork.slot + 1),
        INSUFFICIENT_LIQUIDITY,
        "a sell larger than the capped USDC inventory",
    );
    assert!(
        run(fork, &capped, BUY, fork.quote_trade, fork.slot + 1).expect("the base side still pays")
            > 0
    );
}

#[tokio::test]
async fn goonfi_coupled_price_and_band_scale_the_fill_on_the_fixture_markets() {
    let forks = forks().await;
    for fork in forks.iter() {
        let clock = fork.slot + 1;
        let sell = run(fork, &fork.live(), SELL, fork.base_trade, clock)
            .unwrap_or_else(|e| panic!("{} baseline sell: {e}", fork.def.pair));
        let buy = run(fork, &fork.live(), BUY, fork.quote_trade, clock)
            .unwrap_or_else(|e| panic!("{} baseline buy: {e}", fork.def.pair));

        let doubled = scaled(fork, 2, 1, true);
        let halved = scaled(fork, 1, 2, true);
        let doubled_sell = run(fork, &doubled, SELL, fork.base_trade, clock)
            .unwrap_or_else(|e| panic!("{} doubled sell: {e}", fork.def.pair));
        let halved_sell = run(fork, &halved, SELL, fork.base_trade, clock)
            .unwrap_or_else(|e| panic!("{} halved sell: {e}", fork.def.pair));
        let halved_buy = run(fork, &halved, BUY, fork.quote_trade, clock)
            .unwrap_or_else(|e| panic!("{} halved buy: {e}", fork.def.pair));

        for (what, got, want) in [
            ("doubled sell", doubled_sell, sell * 2),
            ("halved sell", halved_sell * 2, sell),
            ("halved buy", halved_buy, buy * 2),
        ] {
            assert!(
                got.abs_diff(want) <= want / 100,
                "{} {what}: {got} vs {want}",
                fork.def.pair
            );
        }
    }
}

#[tokio::test]
async fn goonfi_band_rejects_the_unfavourable_side_with_0x24_on_the_fixture_markets() {
    let forks = forks().await;
    for fork in forks.iter() {
        let clock = fork.slot + 1;
        let live = fork.live();
        let bid = u64_at(&live.oracle, 0);
        let ask = u64_at(&live.oracle, 8);
        let price = |bid: u64, ask: u64| State {
            oracle: apply_raw(
                "goonfi-price",
                &live.oracle,
                &[
                    ("bid_price_x1e6", serde_json::json!(bid)),
                    ("ask_price_x1e6", serde_json::json!(ask)),
                ],
                fork.slot,
            ),
            ..live.clone()
        };

        let raised_bid = price(bid * 2, ask * 2);
        assert_rejects(
            run(fork, &raised_bid, SELL, fork.base_trade, clock),
            PRICE_OUT_OF_BAND,
            &format!("{} sell above an untouched band", fork.def.pair),
        );
        let lowered_ask = price(bid / 2, ask / 2);
        assert_rejects(
            run(fork, &lowered_ask, BUY, fork.quote_trade, clock),
            PRICE_OUT_OF_BAND,
            &format!("{} buy below an untouched band", fork.def.pair),
        );
        assert!(
            run(fork, &raised_bid, BUY, fork.quote_trade, clock)
                .unwrap_or_else(|e| panic!("{} buy is the favourable side: {e}", fork.def.pair))
                > 0
        );
        assert!(
            run(fork, &lowered_ask, SELL, fork.base_trade, clock)
                .unwrap_or_else(|e| panic!("{} sell is the favourable side: {e}", fork.def.pair))
                > 0
        );
    }
}

#[tokio::test]
async fn goonfi_drained_payout_vault_rejects_only_its_direction_with_0x1() {
    let forks = forks().await;
    for fork in forks.iter() {
        let clock = fork.slot + 1;
        for side in [SELL, BUY] {
            let live = fork.live();
            let control = run(fork, &live, side, fork.trade(side), clock)
                .unwrap_or_else(|e| panic!("{} control: {e}", fork.def.pair));
            let with_payout = |amount: u64| {
                let mut state = fork.live();
                let payout = if side == SELL {
                    &mut state.quote_vault
                } else {
                    &mut state.base_vault
                };
                *payout = apply_raw(
                    "goonfi-vault-balance",
                    payout,
                    &[("amount", serde_json::json!(amount))],
                    fork.slot,
                );
                state
            };
            // A sell's price ignores the quote vault, so its boundary is exact; buys price base inventory.
            if side == SELL {
                assert_eq!(
                    run(fork, &with_payout(control), side, fork.trade(side), clock),
                    Ok(control),
                    "{}: a quote vault holding exactly the output must fill",
                    fork.def.pair
                );
            }
            let starved = if side == SELL {
                control - 1
            } else {
                control / 2
            };
            for amount in [starved, 0] {
                assert_rejects(
                    run(fork, &with_payout(amount), side, fork.trade(side), clock),
                    INSUFFICIENT_LIQUIDITY,
                    &format!("{} side {side} payout vault at {amount}", fork.def.pair),
                );
            }
            let other = 1 - side;
            assert!(
                run(fork, &with_payout(0), other, fork.trade(other), clock)
                    .unwrap_or_else(|e| panic!("{} opposite side: {e}", fork.def.pair))
                    > 0
            );
        }
    }
}

#[tokio::test]
async fn goonfi_freshness_restamps_a_stale_quote_and_a_negative_lead_rejects_with_0x15() {
    let forks = forks().await;
    for fork in forks.iter() {
        let live = fork.live();
        let fresh = run(fork, &live, SELL, fork.base_trade, fork.slot + 1).expect("fresh sell");
        let later = fork.slot + 5_000;
        assert_rejects(
            run(fork, &live, SELL, fork.base_trade, later),
            STALE_ORACLE,
            &format!("{} live oracle 5000 slots later", fork.def.pair),
        );

        let stamp = |lead: i64| State {
            oracle: apply_raw(
                "goonfi-freshness",
                &live.oracle,
                &[("last_update_slot", serde_json::json!(lead))],
                later,
            ),
            ..live.clone()
        };
        for side in [SELL, BUY] {
            let restamped = run(fork, &stamp(0), side, fork.trade(side), later)
                .unwrap_or_else(|e| panic!("{} side {side} restamped: {e}", fork.def.pair));
            assert!(restamped > 0);
            assert_rejects(
                run(fork, &stamp(-2_000), side, fork.trade(side), later),
                STALE_ORACLE,
                &format!("{} side {side} lead -2000", fork.def.pair),
            );
        }
        let restamped = run(fork, &stamp(0), SELL, fork.base_trade, later).unwrap();
        assert!(
            restamped * 100 >= fresh * 99,
            "{}: restamped {restamped} vs fresh {fresh}",
            fork.def.pair
        );
    }
}
