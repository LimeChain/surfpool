//! GoonFi publishes no IDL, so every template write is replayed against the deployed program.

use std::{collections::HashMap, sync::Arc};

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
const PROGRAM: &str = "goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE";
const PROGRAMDATA: &str = "124gUYwjVnJQ4sJsFug9gHPzPLEtwCbAQC5LkbaDgx9s";
const DEPLOYED_SLOT: u64 = 451334772;
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

#[derive(Clone, Copy)]
struct MarketDef {
    pair: &'static str,
    market: &'static str,
    oracle: &'static str,
    base_vault: &'static str,
    quote_vault: &'static str,
}

/// The markets Studio offers as shortcuts, in its order. The behavioural tests replay the first
/// ones whose classic SPL-token vaults are funded.
const FEATURED_MARKETS: [MarketDef; 4] = [
    MarketDef {
        pair: "SOL / USDC",
        market: "GMCJvYGf5Ex2ARiMquaBDqU6iKM8uiEQkB8jCnoNfHpC",
        oracle: "7yecFG22heommABQ5svcbQLK1Ua4ZrJsHPiktZ17jfm3",
        base_vault: "8ncU5YW1CQwvr4gs7buH57bW58e86TDau4STrCJBuz8z",
        quote_vault: "EunHLeqeJKvxnCPQSytnBP63HJVk2fbHceiKKpngyAo8",
    },
    MarketDef {
        pair: "HYPE / USDC",
        market: "8TDBxPXyGvxcaoHZoY5D4X2vePuhMYekKpTEhEhaQX5b",
        oracle: "GHjEJbxWcT55xWCucjSRUJAxxVXD8wXsLUAToBWTUPBa",
        base_vault: "FDNiPEBtf5gTzX91wc3H2riudcFdXzz1k8kJJaZP4MyJ",
        quote_vault: "G4zxFHdZ8nqeHeX9bvhRLfFYGEeZLSVQhRdK5BTGaEwh",
    },
    MarketDef {
        pair: "JUP / USDC",
        market: "AkxuRa1soguFpVRjnDrRsXAcfTkZjRv1XyNXySzVivmC",
        oracle: "APkXGniPLaXoUeRYrdBwgZE7ky9n1xkiMyrYQH9xsMzS",
        base_vault: "G39Mthg4bixLGYRzHqjfRdpyAWSJGH5CiUC4grTMpwyd",
        quote_vault: "GFcj7sRkYqoNCiEkXWxapY7D28UCmAqBjiqGNQGydha4",
    },
    MarketDef {
        pair: "PUMP / USDC",
        market: "FkGgvNwKBMkDAWStXx7C9CbYiwTCHq2pFrkWHkvS9eJg",
        oracle: "14kCeUADn4r4sJKFX78b9qibo9JfgZQdWGED3tqJqe4t",
        base_vault: "BUiRksN6oyCNeDqjPHvtWedVKWFQeiBo5Ft5HeucmTgw",
        quote_vault: "8iR3pQ7MSZidfu2mP2NtYmSq9prdz3u6mhbRmRbJcx6h",
    },
];

/// Every market account the program owns, live and stopped, as of the deployment at slot
/// 451334772. Each template must write only its own bytes on every one of them.
const ALL_MARKETS: [&str; 36] = [
    "2GwiLfAEH1LCNPZtF5JzZUS2KvZ9dEhAyoQ8WLxeYNDY",
    "2U5S52n2L9Rr8vjDiEFRmHo5iQjJQg9zAgJ5nY1aWXK1",
    "2hv45fLgAjhBw4w3zmjMyca8AA1HMfxBPDUXFZtPXVVA",
    "41D2v9b7XJYremti4iGZ36U9fR3ZhmeJpvjarLP62XqG",
    "4rJggoVMajEUtipev1XhSMjESYk8Zibz6CDHPtUe1mem",
    "4sewpY37mKe5b2SRbhYNehQvme1uow2pNnSQqBqSjqX2",
    "5Jw5Lkb3RVaes1h2CTnwvLWQBDQF4qYKwGUHvpuPzciF",
    "5mHTXRU1bgBDo9vUoHKkCidbFRc2mLHSgTTdQyZBjMfg",
    "75nkLnRC6oAqwDjLEwyBRabz6gWkE8EBd5ohNuTMpWyS",
    "8TDBxPXyGvxcaoHZoY5D4X2vePuhMYekKpTEhEhaQX5b",
    "8TxrtAxqA5PA2Y1d2pxzCz9SoDhjrBcYqNAQKVv6p443",
    "8hotzuT21Lj9ekjHV7GBBxp8NaaCDk5MVx7d7GmKdc17",
    "9Nk161kPxkZb1VLwKYXrWbZgr35JN7mHy3EgxxgaXuVH",
    "9yE49sMheNg9Jia9eokPiEgG7suBgD1crLJoN4tn2Y8u",
    "A2fTLPdDC3UJcNPC6TFQnELEFXgJ6FadXT3DRiq8VNbG",
    "AeanNmmxpMEcSv3a3rKaRcrPjXDwpEiG37syPRyu3VJ2",
    "AkxuRa1soguFpVRjnDrRsXAcfTkZjRv1XyNXySzVivmC",
    "Ba4nvPmb4KDAYanxaR5uABigxnUqRTcFmEZaeds4ntVv",
    "CmUBg6HtQDP67Zkt2rhi3oT8rz5cvtysLpw92yxpPMdX",
    "Cvwhi9ryMNjUUKgjjrzzocVFtDLZCBX9NpAgVCXjiqiM",
    "D7sZfY6mrfjdauaXhaRzhL7QNzWVjXtkLScQNMXJqc2p",
    "Daopjqyt5qZZ111x7MDbWbxtf72cdNEUnCECzZ7hVpbp",
    "DnYTE1Yin8uErtQEyjcdvPoF8sW3cM4brnQEsP1uT6Cg",
    "DvNVHqG3FanuNLG8N3P1hw4BdFFgvwDN1n1TPe9rr31f",
    "EEUNhHsRoUVgJUFpkupmdF4v7uLUw1zhYLp7u9s8zFqG",
    "En8TnhTxJ525KawkZxC2rweSPrYmE1VUB3Ny4p9TMikF",
    "F6mM8qrRizECPHLUDK7M3rcLQ6AyCJyENRhFdQ9MWbko",
    "FkGgvNwKBMkDAWStXx7C9CbYiwTCHq2pFrkWHkvS9eJg",
    "FmiBEriWps99eg63dC66UZfS5Mypa8W5s9J5SFpNGQMX",
    "FrQuDkgAc1WQ9Vswxq3LWjB5BgnFxaKareWK4YGh1ZMM",
    "GMCJvYGf5Ex2ARiMquaBDqU6iKM8uiEQkB8jCnoNfHpC",
    "HBDaV4ndLuVe6qK1vGCXReon4B1DJKa9UrbqP8cVqywx",
    "HdQjyoXdhXjWT6ut4WUy2d6767rG9hMSZdBvsbqUeKJV",
    "HzDvPKffZCzRXRQQW4MNaHjrrctiZeqfkyD927JUYv8q",
    "TcMgpxh6SLph4DZNchtSp63KXK115g5Xo9vQ76kwbAZ",
    "kGfQmgrNaU6xq3jVr19oNBP5DmNpivohDf6k77CocpM",
];

/// How many featured markets the behavioural tests replay.
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

            let mut out = Vec::new();
            for def in FEATURED_MARKETS {
                if out.len() == FIXTURES {
                    break;
                }
                let a = fetch(&[def.market, def.oracle, def.base_vault, def.quote_vault]).await;
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
                        Pubkey::from_str_const(def.market),
                        "{} vault authority",
                        def.pair
                    );
                }
                let mints = fetch(&[&base_mint.to_string(), &quote_mint.to_string()]).await;
                let decimals = |mint: &Account| u32::from(mint.data[44]);
                // About 100 quote tokens each way, so no live price or balance is pinned.
                let quote_trade = 100 * 10u64.pow(decimals(&mints[1]));
                let base_trade = (100u128 * 10u128.pow(decimals(&mints[0])) * 1_000_000
                    / u128::from(bid)) as u64;
                if u64_at(&a[2].data, 64) < base_trade.saturating_mul(VAULT_COVER)
                    || u64_at(&a[3].data, 64) < quote_trade.saturating_mul(VAULT_COVER)
                {
                    continue;
                }
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
            let first = out
                .first()
                .expect("no featured GoonFi market has funded classic vaults");
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

fn diff_indices(left: &[u8], right: &[u8]) -> Vec<usize> {
    left.iter()
        .zip(right)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(index, _)| index)
        .collect()
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

    let market = Pubkey::from_str_const(fork.def.market);
    let oracle = Pubkey::from_str_const(fork.def.oracle);
    let base_vault = Pubkey::from_str_const(fork.def.base_vault);
    let quote_vault = Pubkey::from_str_const(fork.def.quote_vault);
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

/// Every template writes only its own bytes on every market the program owns, live or stopped.
#[tokio::test]
async fn goonfi_templates_write_only_proven_bytes_on_every_market() {
    let mut markets = Vec::new();
    for chunk in ALL_MARKETS.chunks(20) {
        markets.extend(chunk.iter().copied().zip(fetch(chunk).await));
    }
    let linked: Vec<String> = markets
        .iter()
        .flat_map(|(_, market)| {
            [208, 144, 176].map(|offset| pubkey_at(&market.data, offset).to_string())
        })
        .collect();
    let mut accounts = HashMap::new();
    for chunk in linked.chunks(20) {
        let keys: Vec<&str> = chunk.iter().map(String::as_str).collect();
        accounts.extend(chunk.iter().cloned().zip(fetch(&keys).await));
    }
    for (address, market) in &markets {
        assert_eq!(market.owner, Pubkey::from_str_const(PROGRAM), "{address}");
        assert_eq!(market.data[..8], MARKET_TAG, "{address}");
        let linked = |offset| {
            accounts[&pubkey_at(&market.data, offset).to_string()]
                .data
                .clone()
        };
        let live = State {
            market: market.data.clone(),
            oracle: linked(208),
            base_vault: linked(144),
            quote_vault: linked(176),
        };
        let slot = u64::from(u32_at(&live.oracle, 16));
        for (id, data) in [
            ("goonfi-price", &live.oracle),
            ("goonfi-freshness", &live.oracle),
            ("goonfi-reference-band", &live.market),
            ("goonfi-vault-balance", &live.base_vault),
            ("goonfi-vault-balance", &live.quote_vault),
        ] {
            assert_eq!(
                &apply_raw(id, data, &[], slot),
                data,
                "{id} must round-trip {address}"
            );
        }

        let price = apply_raw(
            "goonfi-price",
            &live.oracle,
            &[
                ("bid_price_x1e6", serde_json::json!(99_740_000u64)),
                ("ask_price_x1e6", serde_json::json!(99_750_000u64)),
            ],
            slot,
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
            slot + 100,
        );
        assert_writes_within(&live.oracle, &fresh, 16..20, "goonfi-freshness");
        assert_eq!(u64::from(u32_at(&fresh, 16)), slot + 93);

        let band = apply_raw(
            "goonfi-reference-band",
            &live.market,
            &[
                ("reference_price_a_x1e6", serde_json::json!(1u64)),
                ("reference_price_b_x1e6", serde_json::json!(2u64)),
            ],
            slot,
        );
        assert_writes_within(&live.market, &band, 1712..1728, "goonfi-reference-band");
        assert_eq!((u64_at(&band, 1712), u64_at(&band, 1720)), (1, 2));

        let vault = apply_raw(
            "goonfi-vault-balance",
            &live.quote_vault,
            &[("amount", serde_json::json!(123u64))],
            slot,
        );
        assert_writes_within(&live.quote_vault, &vault, 64..72, "goonfi-vault-balance");
        assert_eq!(u64_at(&vault, 64), 123);
    }
}

/// The featured addresses Studio and the README name are the market's own fields, so a replaced
/// market fails here instead of silently targeting the wrong account.
#[tokio::test]
async fn goonfi_featured_markets_match_their_market_accounts() {
    for def in FEATURED_MARKETS {
        let a = fetch(&[def.market, def.oracle, def.base_vault, def.quote_vault]).await;
        let (market, oracle) = (&a[0], &a[1]);
        assert_eq!(
            market.owner,
            Pubkey::from_str_const(PROGRAM),
            "{}",
            def.pair
        );
        assert_eq!(market.data[..8], MARKET_TAG, "{}", def.pair);
        for (offset, expected, what) in [
            (208, def.oracle, "oracle"),
            (144, def.base_vault, "base vault"),
            (176, def.quote_vault, "quote vault"),
        ] {
            assert_eq!(
                pubkey_at(&market.data, offset).to_string(),
                expected,
                "{} {what}",
                def.pair
            );
        }
        assert_eq!(
            oracle.owner,
            Pubkey::from_str_const(ORACLE_PROGRAM),
            "{} oracle owner",
            def.pair
        );
        for (vault, mint_offset) in [(&a[2], 80), (&a[3], 112)] {
            assert_eq!(
                pubkey_at(&vault.data, 0),
                pubkey_at(&market.data, mint_offset),
                "{} vault mint",
                def.pair
            );
        }
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
                AccountAddress::Pubkey(target_account.to_string()),
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
        market: materialized(fork.def.market),
        oracle: materialized(fork.def.oracle),
        base_vault: live.base_vault.clone(),
        quote_vault: live.quote_vault.clone(),
    };
    let limited_quote_vault = materialized(fork.def.quote_vault);
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
