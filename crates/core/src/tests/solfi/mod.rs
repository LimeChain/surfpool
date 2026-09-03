//! SolFi V2 raw-layout and deployed-program tests.
//!
//! These deliberately drive the shipped templates through `RawLayout::materialize` before replaying
//! the current deployed program. SolFi publishes no IDL, and visually plausible offsets are not
//! evidence that a field reaches pricing.

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
const PROGRAM: &str = "SV2EYYJyRz2YhfXwXnhNAevDEui5Q6yrfyo13WtupPF";
const PROGRAMDATA: &str = "H6M3jMJCednoAr7BR9P6versKQmbo5kV3oi8R5JsWNKz";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

const PRICE_MASK: u64 = 0x44dd_2288_77ee_1166;
const EXPONENT_MASK: u64 = 0x990f_f033_cc55_aaff;
const PUBLICATION_MASK: u64 = 0x66cc_3300_ffaa_55bb;
const SCALE_MASK: u64 = 0x4488_dd22_ee11_7799;

#[derive(Clone, Copy)]
struct MarketDef {
    market: &'static str,
    oracle: &'static str,
    cfg: &'static str,
    base_vault: &'static str,
    quote_vault: &'static str,
    base_mint: &'static str,
    quote_mint: &'static str,
    base_trade: u64,
    quote_trade: u64,
}

const MARKETS: [MarketDef; 2] = [
    MarketDef {
        market: "65ZHSArs5XxPseKQbB1B4r16vDxMWnCxHMzogDAqiDUc",
        oracle: "2ny7eGyZCoeEVTkNLf5HcnJFBKkyA4p4gcrtb3b8y8ou",
        cfg: "FmxXDSR9WvpJTCh738D1LEDuhMoA8geCtZgHb3isy7Dp",
        base_vault: "CRo8DBwrmd97DJfAnvCv96tZPL5Mktf2NZy2ZnhDer1A",
        quote_vault: "GhFfLFSprPpfoRaWakPMmJTMJBHuz6C694jYwxy2dAic",
        base_mint: "So11111111111111111111111111111111111111112",
        quote_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        base_trade: 1_000_000_000,
        quote_trade: 100_000_000,
    },
    MarketDef {
        market: "FkEB6uvyzuoaGpgs4yRtFtxC4WJxhejNFbUkj5R6wR32",
        oracle: "CyCUgmaCYUZxbux3J2svDzxSryVFMtZNPrnMKS41nc4G",
        cfg: "QoFvFhDZg9TaZEi4SsasWpH5xXzk3zBqfRyicGexfNQ",
        base_vault: "5bHD9xdEzJdkVuhs54mGPC9BZgUshqgMg4tqmTwhWggc",
        quote_vault: "ARWaajRJyF6PKQryJ4HLzLBfTWM2qmVQUQVtBjk6PgPc",
        base_mint: "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
        quote_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        base_trade: 1_000_000_000,
        quote_trade: 1_000_000_000,
    },
];

// Every deployed 1,728-byte account carrying the initialized MarketConfig marker on 2026-09-03.
// Only the two entries in `MARKETS` are funded and can currently provide meaningful fills; this
// wider list is a byte-level layout canary for the raw templates and their guards.
const INITIALIZED_LAYOUTS: [(&str, &str); 10] = [
    (
        "FkEB6uvyzuoaGpgs4yRtFtxC4WJxhejNFbUkj5R6wR32",
        "CyCUgmaCYUZxbux3J2svDzxSryVFMtZNPrnMKS41nc4G",
    ),
    (
        "65ZHSArs5XxPseKQbB1B4r16vDxMWnCxHMzogDAqiDUc",
        "2ny7eGyZCoeEVTkNLf5HcnJFBKkyA4p4gcrtb3b8y8ou",
    ),
    (
        "BjBHvbqgQCRmvZ6u3VzGrHn3QZ1NfmMRujoqjeaK6fLT",
        "7GELMFc4yK1jBMZHsoPhHYYhKTPgmpK7doFPVx62Kiy1",
    ),
    (
        "394qso4LKsjjHchKu6S8A1Nt9iEryNPyU5cFN79RTVpY",
        "AwppY62pZj9WApQ4r8ezKY69nadWjmEiASwpU6WCY1Ni",
    ),
    (
        "72ekvC4bc94sg8BVvqNYHDugFatPbxbyYFQDaL9EPupi",
        "DNTdSZXtejmGQkR4RuwgkCnrmbUwUeSs4vRKfEfhc9jZ",
    ),
    (
        "HH2HgSHxgFjyzUd2dv9k68D5RQbBZWtpYan1gx8kjUi5",
        "4G3szXUscfmzPJgzrCwZ5ecu9cQNeXjFRmimMmyPANsA",
    ),
    (
        "8BrwYAr1K11sG8GvM8vUFAw45Mm1mLCuTd4ynhNMYjRC",
        "4LSXFMNRw8k3h7c95f7kPM8aphUswqcgvi8pTmfeL2T1",
    ),
    (
        "2sp6rCc4VaXJ5qCbrPukpQVjZVZey42pj7QkynYNDdw3",
        "By9zHEbZJvYrBws27SqPXggfSAH3fjnJcdxKgdogyXUm",
    ),
    (
        "ErP5XNqqLXN99hoa4JnHB199rcetMjMdEoJwnphwv7sn",
        "5PE4Z3LEzeUW6UYqGmpmswPwo7TVu3R7chXbCBSBaCZ5",
    ),
    (
        "5Q6oe47U9WxMhvnEjpi6AnZZPMBcatcWKUTfLkguPEiG",
        "6LRUvVthoRGUSfJMqewZtFRmp2fK96xoUn6AyahqxxBw",
    ),
];

const UNINITIALIZED_MARKETS: [&str; 9] = [
    "2kfQuYG2FVZL2RqqKEttcdadbPWP4c7b6AFQztNcBWyV",
    "2Q6S8p9iZNzMvpTemiC56HqCJ3F3szNoyRkvqEKfCanY",
    "GxZwsApah3Bsgg14dG7MUtnPCQbGiDqFEwyWYZvDxn6Y",
    "Bnwc3wzE8PYYvgtbriRn9RpRnDH1TvJJthJVacrbgiD7",
    "AZEKRYWew6zAyoksytTeBFJRHyYdwycPMBn1P2QgfDpQ",
    "2e25gRiddjn968aXrLt1oZw3BZ4fYD5D8mCv7uKxu1yL",
    "7TKsqWxU9QkPYVLdjjR1V67ky3FnYogjntUpNLexib4E",
    "BmVBqFL8LD2KiBsDE8fWXLZ2MWVgPR1qor55MCimriGR",
    "HYKRMKiXfs1CedDsUHNyaVmEyuw7gj3E3uY6gJgUeMr6",
];

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
struct SolfiFork {
    def: MarketDef,
    elf: Vec<u8>,
    market: Account,
    oracle: Account,
    cfg: Account,
    base_vault: Account,
    quote_vault: Account,
    base_mint: Account,
    quote_mint: Account,
    slot: u64,
}

async fn forks() -> Arc<Vec<SolfiFork>> {
    static CACHE: tokio::sync::OnceCell<Arc<Vec<SolfiFork>>> = tokio::sync::OnceCell::const_new();
    CACHE
        .get_or_init(|| async {
            let programdata = fetch(&[PROGRAMDATA]).await.remove(0);
            assert!(
                programdata.data.len() > 240_000,
                "programdata is unexpectedly short"
            );
            let elf = programdata.data[45..].to_vec();
            let mut out = Vec::new();
            for def in MARKETS {
                let a = fetch(&[
                    def.market,
                    def.oracle,
                    def.cfg,
                    def.base_vault,
                    def.quote_vault,
                    def.base_mint,
                    def.quote_mint,
                ])
                .await;
                assert_eq!(a[0].data.len(), 1728);
                assert_eq!(a[1].data.len(), 168);
                assert_eq!(a[2].data.len(), 1_048_576);
                assert_eq!(
                    Pubkey::new_from_array(a[0].data[24..56].try_into().unwrap()),
                    Pubkey::from_str_const(def.oracle)
                );
                assert_eq!(
                    Pubkey::new_from_array(a[0].data[120..152].try_into().unwrap()),
                    Pubkey::from_str_const(def.base_vault)
                );
                assert_eq!(
                    Pubkey::new_from_array(a[0].data[152..184].try_into().unwrap()),
                    Pubkey::from_str_const(def.quote_vault)
                );
                let slot = u64::from_le_bytes(a[0].data[1352..1360].try_into().unwrap());
                out.push(SolfiFork {
                    def,
                    elf: elf.clone(),
                    market: a[0].clone(),
                    oracle: a[1].clone(),
                    cfg: a[2].clone(),
                    base_vault: a[3].clone(),
                    quote_vault: a[4].clone(),
                    base_mint: a[5].clone(),
                    quote_mint: a[6].clone(),
                    slot,
                });
            }
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
    t.raw_layout
        .as_ref()
        .expect("raw layout")
        .materialize(data, &t.properties, &map, slot)
        .unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn diff_indices(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.iter()
        .zip(b)
        .enumerate()
        .filter_map(|(i, (a, b))| (a != b).then_some(i))
        .collect()
}

fn token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    data
}

fn amount(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[64..72].try_into().unwrap())
}

fn replay(
    fork: &SolfiFork,
    amount_in: u64,
    direction: u8,
    market: Vec<u8>,
    oracle: Vec<u8>,
    base_vault: Vec<u8>,
    quote_vault: Vec<u8>,
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
    clock.slot = fork.slot;
    svm.set_sysvar(&clock);

    let mut put = |key: &str, mut account: Account, data: Vec<u8>| -> Result<(), String> {
        account.data = data;
        svm.set_account(Pubkey::from_str_const(key), account)
            .map_err(|e| format!("set {key}: {e:?}"))
    };
    put(fork.def.market, fork.market.clone(), market)?;
    put(fork.def.oracle, fork.oracle.clone(), oracle)?;
    put(fork.def.cfg, fork.cfg.clone(), fork.cfg.data.clone())?;
    put(fork.def.base_vault, fork.base_vault.clone(), base_vault)?;
    put(fork.def.quote_vault, fork.quote_vault.clone(), quote_vault)?;
    put(
        fork.def.base_mint,
        fork.base_mint.clone(),
        fork.base_mint.data.clone(),
    )?;
    put(
        fork.def.quote_mint,
        fork.quote_mint.clone(),
        fork.quote_mint.data.clone(),
    )?;

    let taker = Keypair::new();
    svm.airdrop(&taker.pubkey(), 20_000_000_000)
        .map_err(|e| format!("airdrop: {e:?}"))?;
    let user_base = Pubkey::new_unique();
    let user_quote = Pubkey::new_unique();
    let base_key = Pubkey::from_str_const(fork.def.base_mint);
    let quote_key = Pubkey::from_str_const(fork.def.quote_mint);
    let (base_amount, quote_amount) = if direction == 0 {
        (amount_in.saturating_mul(2), 0)
    } else {
        (0, amount_in.saturating_mul(2))
    };
    for (key, mint, balance) in [
        (user_base, base_key, base_amount),
        (user_quote, quote_key, quote_amount),
    ] {
        svm.set_account(
            key,
            Account {
                lamports: balance.saturating_add(2_039_280),
                data: token_account(&mint, &taker.pubkey(), balance),
                owner: token_program,
                executable: false,
                rent_epoch: 0,
            },
        )
        .map_err(|e| format!("user token account: {e:?}"))?;
    }

    let mut data = vec![7u8];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    data.push(direction);
    let ix = Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(taker.pubkey(), true),
            AccountMeta::new(Pubkey::from_str_const(fork.def.market), false),
            AccountMeta::new_readonly(Pubkey::from_str_const(fork.def.oracle), false),
            AccountMeta::new_readonly(Pubkey::from_str_const(fork.def.cfg), false),
            AccountMeta::new(Pubkey::from_str_const(fork.def.base_vault), false),
            AccountMeta::new(Pubkey::from_str_const(fork.def.quote_vault), false),
            AccountMeta::new(user_base, false),
            AccountMeta::new(user_quote, false),
            AccountMeta::new(base_key, false),
            AccountMeta::new(quote_key, false),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(
                Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111"),
                false,
            ),
        ],
        data,
    };
    let mut msg = solana_message::Message::new(&[ix], Some(&taker.pubkey()));
    msg.recent_blockhash = svm.latest_blockhash();
    let mut tx = Transaction::new_unsigned(msg);
    tx.signatures = vec![
        solana_signature::Signature::default();
        tx.message.header.num_required_signatures as usize
    ];
    tx.signatures[0] = taker.sign_message(&tx.message.serialize());
    svm.send_transaction(tx)
        .map_err(|e| format!("{:?}", e.err))?;
    let dst = if direction == 0 {
        user_quote
    } else {
        user_base
    };
    Ok(amount(&svm.get_account(&dst).expect("destination").data))
}

fn run(
    fork: &SolfiFork,
    amount_in: u64,
    direction: u8,
    market: Vec<u8>,
    oracle: Vec<u8>,
) -> Result<u64, String> {
    replay(
        fork,
        amount_in,
        direction,
        market,
        oracle,
        fork.base_vault.data.clone(),
        fork.quote_vault.data.clone(),
    )
}

#[tokio::test]
async fn solfi_raw_layouts_cover_every_initialized_market_and_reject_every_sibling() {
    let registry = TemplateRegistry::new();
    let market_templates = ["solfi-spread", "solfi-size-impact"];
    let oracle_templates = ["solfi-price", "solfi-freshness"];

    let initialized_addresses = INITIALIZED_LAYOUTS
        .iter()
        .flat_map(|(market, oracle)| [*market, *oracle])
        .collect::<Vec<_>>();
    let initialized = fetch(&initialized_addresses).await;
    let mut checked = 0;
    for (accounts, (market_address, oracle_address)) in
        initialized.chunks_exact(2).zip(INITIALIZED_LAYOUTS)
    {
        let market = &accounts[0].data;
        let oracle = &accounts[1].data;
        assert_eq!(market.len(), 1728, "{market_address}");
        assert_eq!(oracle.len(), 168, "{oracle_address}");
        assert_eq!(
            Pubkey::new_from_array(market[24..56].try_into().unwrap()),
            Pubkey::from_str_const(oracle_address),
            "{market_address} no longer embeds the expected oracle"
        );

        for id in market_templates {
            let template = registry.get(id).unwrap();
            template
                .raw_layout
                .as_ref()
                .unwrap()
                .guard(market)
                .unwrap_or_else(|e| panic!("{id} rejected {market_address}: {e}"));
            assert_eq!(
                apply_raw(id, market, &[], 0),
                market.to_vec(),
                "{id} must round-trip {market_address}"
            );
        }
        for id in oracle_templates {
            let template = registry.get(id).unwrap();
            template
                .raw_layout
                .as_ref()
                .unwrap()
                .guard(oracle)
                .unwrap_or_else(|e| panic!("{id} rejected {oracle_address}: {e}"));
            assert_eq!(
                apply_raw(id, oracle, &[], 0),
                oracle.to_vec(),
                "{id} must round-trip {oracle_address}"
            );
        }

        let changed_market = apply_raw(
            "solfi-spread",
            market,
            &[("quote_to_base_curve_y", serde_json::json!(1234))],
            0,
        );
        assert!(
            diff_indices(market, &changed_market)
                .iter()
                .all(|i| (792..856).contains(i)),
            "{market_address} escaped the directional spline write set"
        );
        let changed_oracle = apply_raw(
            "solfi-price",
            oracle,
            &[("price_coefficient", serde_json::json!(12_345_678u64))],
            0,
        );
        assert!(
            diff_indices(oracle, &changed_oracle)
                .iter()
                .all(|i| (8..16).contains(i)),
            "{oracle_address} escaped the price coefficient write set"
        );
        checked += 1;
    }
    assert_eq!(checked, 10, "every initialized market must be exercised");

    let vault_addresses = initialized
        .chunks_exact(2)
        .flat_map(|accounts| {
            let market = &accounts[0].data;
            [
                Pubkey::new_from_array(market[120..152].try_into().unwrap()).to_string(),
                Pubkey::new_from_array(market[152..184].try_into().unwrap()).to_string(),
            ]
        })
        .collect::<Vec<_>>();
    let vault_address_refs = vault_addresses
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let vaults = fetch(&vault_address_refs).await;
    let vault_template = registry.get("solfi-vault-balance").unwrap();
    let mut supported_vaults = 0;
    for (vault, address) in vaults.iter().zip(&vault_addresses) {
        let guard = vault_template
            .raw_layout
            .as_ref()
            .unwrap()
            .guard(&vault.data);
        if vault.data.len() == 165 && vault.data[108] == 1 {
            guard.unwrap_or_else(|e| panic!("vault guard rejected {address}: {e}"));
            assert_eq!(
                apply_raw("solfi-vault-balance", &vault.data, &[], 0),
                vault.data.clone(),
                "vault template must round-trip {address}"
            );
            let changed = apply_raw(
                "solfi-vault-balance",
                &vault.data,
                &[("amount", serde_json::json!(123u64))],
                0,
            );
            assert!(
                diff_indices(&vault.data, &changed)
                    .iter()
                    .all(|i| (64..72).contains(i)),
                "{address} escaped the token amount write set"
            );
            supported_vaults += 1;
        } else {
            assert!(
                guard.is_err(),
                "vault guard admitted unsupported token layout {address}"
            );
        }
    }
    assert!(
        supported_vaults >= 18,
        "expected both vault layouts for almost every initialized market, got {supported_vaults}"
    );

    let siblings = fetch(&UNINITIALIZED_MARKETS).await;
    let mut rejected = 0;
    for (account, address) in siblings.iter().zip(UNINITIALIZED_MARKETS) {
        assert_eq!(account.data.len(), 1728, "{address}");
        for id in market_templates {
            let template = registry.get(id).unwrap();
            assert!(
                template
                    .raw_layout
                    .as_ref()
                    .unwrap()
                    .guard(&account.data)
                    .is_err(),
                "{id} admitted uninitialized sibling {address}"
            );
        }
        rejected += 1;
    }
    assert_eq!(rejected, 9, "every uninitialized sibling must be rejected");
}

#[tokio::test]
async fn solfi_templates_write_only_proven_bytes_on_both_replay_fixtures() {
    let forks = forks().await;
    let mut checked = 0;
    for fork in forks.iter() {
        assert_eq!(
            apply_raw("solfi-price", &fork.oracle.data, &[], fork.slot),
            fork.oracle.data,
            "oracle must round-trip unchanged"
        );
        assert_eq!(
            apply_raw("solfi-spread", &fork.market.data, &[], fork.slot),
            fork.market.data,
            "market must round-trip unchanged"
        );
        assert_eq!(
            apply_raw("solfi-size-impact", &fork.market.data, &[], fork.slot),
            fork.market.data,
            "size-impact template must round-trip unchanged"
        );
        for vault in [&fork.base_vault.data, &fork.quote_vault.data] {
            assert_eq!(
                apply_raw("solfi-vault-balance", vault, &[], fork.slot),
                vault.to_vec(),
                "vault template must round-trip unchanged"
            );
            let replacement = amount(vault) / 2;
            let changed = apply_raw(
                "solfi-vault-balance",
                vault,
                &[("amount", serde_json::json!(replacement))],
                fork.slot,
            );
            assert!(
                diff_indices(vault, &changed)
                    .iter()
                    .all(|i| (64..72).contains(i)),
                "vault balance escaped token-account amount bytes"
            );
            assert_eq!(amount(&changed), replacement);
        }

        let price = apply_raw(
            "solfi-price",
            &fork.oracle.data,
            &[
                ("price_exponent", serde_json::json!(-10)),
                ("price_coefficient", serde_json::json!(12_345_678u64)),
            ],
            fork.slot,
        );
        assert!(
            diff_indices(&fork.oracle.data, &price)
                .iter()
                .all(|i| (0..16).contains(i))
        );
        assert_eq!(
            (u64::from_le_bytes(price[0..8].try_into().unwrap()) ^ EXPONENT_MASK) as i64,
            -10
        );
        assert_eq!(
            u64::from_le_bytes(price[8..16].try_into().unwrap()) ^ PRICE_MASK,
            12_345_678
        );

        let fresh = apply_raw(
            "solfi-freshness",
            &fork.oracle.data,
            &[
                ("publication_slot", serde_json::json!(0)),
                ("validity_horizon", serde_json::json!(200)),
            ],
            fork.slot,
        );
        assert!(
            diff_indices(&fork.oracle.data, &fresh)
                .iter()
                .all(|i| (16..24).contains(i) || (40..48).contains(i))
        );
        assert_eq!(
            u64::from_le_bytes(fresh[16..24].try_into().unwrap()) ^ PUBLICATION_MASK,
            fork.slot
        );
        assert_eq!(
            u64::from_le_bytes(fresh[40..48].try_into().unwrap()) ^ 0x9966_33cc_00ff_aa55,
            fork.slot + 200
        );

        let spread = apply_raw(
            "solfi-spread",
            &fork.market.data,
            &[
                ("quote_to_base_curve_y", serde_json::json!(1234)),
                ("base_to_quote_curve_y", serde_json::json!(2345)),
                ("age_multiplier_curve_y", serde_json::json!(1000)),
                ("additional_widening_curve_y", serde_json::json!(0)),
                ("max_widening", serde_json::json!(100000)),
            ],
            fork.slot,
        );
        let allowed = [792..856, 928..992, 1064..1128, 1200..1264, 1328..1336];
        assert!(
            diff_indices(&fork.market.data, &spread)
                .iter()
                .all(|i| allowed.iter().any(|r| r.contains(i))),
            "{} escaped its write set",
            fork.def.market
        );
        for (offset, expected) in [(792, 1234), (928, 2345), (1064, 1000), (1200, 0)] {
            for i in 0..8 {
                assert_eq!(
                    u64::from_le_bytes(
                        spread[offset + i * 8..offset + (i + 1) * 8]
                            .try_into()
                            .unwrap()
                    ),
                    expected,
                    "{} offset {}",
                    fork.def.market,
                    offset + i * 8
                );
            }
        }
        assert_eq!(
            u64::from_le_bytes(spread[1328..1336].try_into().unwrap()),
            100_000
        );

        let shaped = size_impact_market(fork, 10_000, 100_000);
        assert!(
            diff_indices(&fork.market.data, &shaped)
                .iter()
                .all(|i| allowed.iter().any(|r| r.contains(i))),
            "{} size-impact escaped its proven write set",
            fork.def.market
        );
        assert_eq!(
            &shaped[728..792],
            &fork.market.data[728..792],
            "quote-to-base x knots must remain live"
        );
        assert_eq!(
            &shaped[864..928],
            &fork.market.data[864..928],
            "base-to-quote x knots must remain live"
        );
        checked += 1;
    }
    assert_eq!(
        checked, 2,
        "both complete replay fixtures must be exercised"
    );
}

#[tokio::test]
async fn solfi_raw_guards_reject_corrupted_type_markers() {
    let fork = &forks().await[0];
    let registry = TemplateRegistry::new();

    let price = registry.get("solfi-price").expect("price template");
    let mut bad_oracle = fork.oracle.data.clone();
    bad_oracle[72] ^= 1;
    assert!(
        price
            .raw_layout
            .as_ref()
            .unwrap()
            .guard(&bad_oracle)
            .is_err(),
        "oracle magic is the protection against a wrong 168-byte account"
    );

    let spread = registry.get("solfi-spread").expect("spread template");
    let mut bad_market = fork.market.data.clone();
    bad_market[704] ^= 1;
    assert!(
        spread
            .raw_layout
            .as_ref()
            .unwrap()
            .guard(&bad_market)
            .is_err(),
        "MarketConfig version/initialized word must be part of the guard"
    );

    let vault = registry.get("solfi-vault-balance").expect("vault template");
    assert!(
        vault
            .raw_layout
            .as_ref()
            .unwrap()
            .guard(&vec![0; 164])
            .is_err(),
        "vault layout must reject non-token-account sizes"
    );
    let mut wrong_state = fork.base_vault.data.clone();
    wrong_state[108] = 0;
    assert!(
        vault
            .raw_layout
            .as_ref()
            .unwrap()
            .guard(&wrong_state)
            .is_err(),
        "vault layout must reject a token account that is not initialized"
    );
}

#[tokio::test]
async fn solfi_price_dislocation_scenario_moves_both_directions_reciprocally() {
    let fork = &forks().await[1];
    let coefficient = u64::from_le_bytes(fork.oracle.data[8..16].try_into().unwrap()) ^ PRICE_MASK;
    let exponent =
        (u64::from_le_bytes(fork.oracle.data[0..8].try_into().unwrap()) ^ EXPONENT_MASK) as i64;
    for direction in 0..=1 {
        let amount_in = if direction == 0 {
            fork.def.base_trade
        } else {
            fork.def.quote_trade
        };
        let baseline = run(
            fork,
            amount_in,
            direction,
            fork.market.data.clone(),
            fork.oracle.data.clone(),
        )
        .expect("baseline");
        let doubled_oracle = apply_raw(
            "solfi-price",
            &fork.oracle.data,
            &[("price_coefficient", serde_json::json!(coefficient * 2))],
            fork.slot,
        );
        let doubled = run(
            fork,
            amount_in,
            direction,
            fork.market.data.clone(),
            doubled_oracle,
        )
        .expect("doubled price");
        let ratio = doubled as f64 / baseline as f64;
        let expected = if direction == 0 { 2.0 } else { 0.5 };
        assert!(
            (ratio - expected).abs() < 0.0001,
            "direction {direction}: {ratio}"
        );

        let equivalent = apply_raw(
            "solfi-price",
            &fork.oracle.data,
            &[
                ("price_exponent", serde_json::json!(exponent - 1)),
                ("price_coefficient", serde_json::json!(coefficient * 10)),
            ],
            fork.slot,
        );
        let same = run(
            fork,
            amount_in,
            direction,
            fork.market.data.clone(),
            equivalent,
        )
        .expect("equivalent exponent/coefficient pair");
        assert_eq!(same, baseline, "exponent/coefficient formula drifted");
    }
}

fn deterministic_spread(fork: &SolfiFork, final_units: u64) -> Vec<u8> {
    directional_spread(fork, final_units, final_units, final_units)
}

fn directional_spread(
    fork: &SolfiFork,
    quote_to_base_units: u64,
    base_to_quote_units: u64,
    max_widening: u64,
) -> Vec<u8> {
    let scale = u64::from_le_bytes(fork.oracle.data[32..40].try_into().unwrap()) ^ SCALE_MASK;
    let curve = |final_units: u64| {
        assert_eq!(
            (final_units * 1000) % scale,
            0,
            "test target must divide exactly"
        );
        final_units * 1000 / scale
    };
    apply_raw(
        "solfi-spread",
        &fork.market.data,
        &[
            (
                "quote_to_base_curve_y",
                serde_json::json!(curve(quote_to_base_units)),
            ),
            (
                "base_to_quote_curve_y",
                serde_json::json!(curve(base_to_quote_units)),
            ),
            ("age_multiplier_curve_y", serde_json::json!(1000)),
            ("additional_widening_curve_y", serde_json::json!(0)),
            ("max_widening", serde_json::json!(max_widening)),
        ],
        fork.slot,
    )
}

const QUOTE_TO_BASE_Y: [&str; 8] = [
    "quote_to_base_y_0",
    "quote_to_base_y_1",
    "quote_to_base_y_2",
    "quote_to_base_y_3",
    "quote_to_base_y_4",
    "quote_to_base_y_5",
    "quote_to_base_y_6",
    "quote_to_base_y_7",
];
const BASE_TO_QUOTE_Y: [&str; 8] = [
    "base_to_quote_y_0",
    "base_to_quote_y_1",
    "base_to_quote_y_2",
    "base_to_quote_y_3",
    "base_to_quote_y_4",
    "base_to_quote_y_5",
    "base_to_quote_y_6",
    "base_to_quote_y_7",
];

fn size_impact_market(fork: &SolfiFork, tight_units: u64, wide_units: u64) -> Vec<u8> {
    let scale = u64::from_le_bytes(fork.oracle.data[32..40].try_into().unwrap()) ^ SCALE_MASK;
    let curve = |units: u64| {
        assert_eq!((units * 1000) % scale, 0);
        units * 1000 / scale
    };
    let tight = curve(tight_units);
    let wide = curve(wide_units);
    let mut values = Vec::new();
    for paths in [&QUOTE_TO_BASE_Y, &BASE_TO_QUOTE_Y] {
        for (index, path) in paths.iter().enumerate() {
            values.push((
                *path,
                serde_json::json!(if index < 2 { tight } else { wide }),
            ));
        }
    }
    values.extend([
        ("age_multiplier_curve_y", serde_json::json!(1000)),
        ("additional_widening_curve_y", serde_json::json!(0)),
        ("max_widening", serde_json::json!(wide_units)),
    ]);
    apply_raw("solfi-size-impact", &fork.market.data, &values, fork.slot)
}

#[tokio::test]
async fn solfi_spread_template_has_the_derived_absolute_unit_on_both_markets() {
    let forks = forks().await;
    let mut checked = 0;
    for fork in forks.iter() {
        let oracle = apply_raw(
            "solfi-freshness",
            &fork.oracle.data,
            &[
                ("publication_slot", serde_json::json!(0)),
                ("validity_horizon", serde_json::json!(200)),
            ],
            fork.slot,
        );
        for (direction, amount_in) in [(0, fork.def.base_trade), (1, fork.def.quote_trade)] {
            let tight = run(
                fork,
                amount_in,
                direction,
                deterministic_spread(fork, 10_000),
                oracle.clone(),
            )
            .expect("0.1% spread");
            let wide = run(
                fork,
                amount_in,
                direction,
                deterministic_spread(fork, 100_000),
                oracle.clone(),
            )
            .expect("1% spread");
            assert!(
                wide < tight,
                "{} direction {direction} did not widen",
                fork.def.market
            );
            let incremental = (tight - wide) as f64 / amount_in as f64;
            let exponent =
                (u64::from_le_bytes(oracle[0..8].try_into().unwrap()) ^ EXPONENT_MASK) as i64;
            let raw_price = (u64::from_le_bytes(oracle[8..16].try_into().unwrap()) ^ PRICE_MASK)
                as f64
                * 10f64.powi(exponent as i32);
            let expected = if direction == 0 {
                raw_price * 0.009
            } else {
                0.009 / raw_price
            };
            assert!(
                (incremental - expected).abs() < expected * 0.002 + 1e-9,
                "{} direction {direction}: incremental={incremental}, expected={expected}",
                fork.def.market
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 4, "both directions on both replay fixtures");
}

#[tokio::test]
async fn solfi_directional_risk_off_scenario_is_isolated_to_the_target_side() {
    let forks = forks().await;
    let mut checked = 0;
    for fork in forks.iter() {
        let oracle = apply_raw(
            "solfi-freshness",
            &fork.oracle.data,
            &[
                ("publication_slot", serde_json::json!(0)),
                ("validity_horizon", serde_json::json!(200)),
            ],
            fork.slot,
        );
        let tight = directional_spread(fork, 10_000, 10_000, 100_000);
        let controls = [
            run(fork, fork.def.base_trade, 0, tight.clone(), oracle.clone())
                .expect("tight base-to-quote control"),
            run(fork, fork.def.quote_trade, 1, tight, oracle.clone())
                .expect("tight quote-to-base control"),
        ];

        for (target_direction, market) in [
            (1usize, directional_spread(fork, 100_000, 10_000, 100_000)),
            (0usize, directional_spread(fork, 10_000, 100_000, 100_000)),
        ] {
            let other_direction = 1 - target_direction;
            let target_amount = if target_direction == 0 {
                fork.def.base_trade
            } else {
                fork.def.quote_trade
            };
            let other_amount = if other_direction == 0 {
                fork.def.base_trade
            } else {
                fork.def.quote_trade
            };
            let targeted = run(
                fork,
                target_amount,
                target_direction as u8,
                market.clone(),
                oracle.clone(),
            )
            .expect("targeted risk-off quote");
            let untargeted = run(
                fork,
                other_amount,
                other_direction as u8,
                market,
                oracle.clone(),
            )
            .expect("untargeted quote");
            let targeted_delta = controls[target_direction].saturating_sub(targeted);
            let cross_delta = controls[other_direction].abs_diff(untargeted);
            assert!(
                targeted_delta * 1_000 > controls[target_direction] * 8,
                "{} direction {target_direction} moved by less than 0.8%",
                fork.def.market
            );
            assert!(
                (cross_delta as u128) * 20 < targeted_delta as u128,
                "{} target delta {targeted_delta}, cross delta {cross_delta}",
                fork.def.market
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 4, "both risk directions on both markets");
}

#[tokio::test]
async fn solfi_large_trade_deterioration_scenario_interpolates_between_live_knots() {
    let forks = forks().await;
    let mut checked = 0;
    for fork in forks.iter() {
        let oracle = apply_raw(
            "solfi-freshness",
            &fork.oracle.data,
            &[
                ("publication_slot", serde_json::json!(0)),
                ("validity_horizon", serde_json::json!(200)),
            ],
            fork.slot,
        );
        let tight = directional_spread(fork, 10_000, 10_000, 100_000);
        let shaped = size_impact_market(fork, 10_000, 100_000);
        let exponent =
            (u64::from_le_bytes(oracle[0..8].try_into().unwrap()) ^ EXPONENT_MASK) as i64;
        let raw_price = (u64::from_le_bytes(oracle[8..16].try_into().unwrap()) ^ PRICE_MASK) as f64
            * 10f64.powi(exponent as i32);

        for direction in 0..=1 {
            let x_offset = if direction == 0 { 864 } else { 728 };
            let first = u64::from_le_bytes(
                fork.market.data[x_offset + 8..x_offset + 16]
                    .try_into()
                    .unwrap(),
            );
            let second = u64::from_le_bytes(
                fork.market.data[x_offset + 16..x_offset + 24]
                    .try_into()
                    .unwrap(),
            );
            assert!(first > 0 && second > first, "live knots must be ordered");
            let midpoint = first + (second - first) / 2;

            let impact_ppm = |quote_notional: u64| {
                let amount_in = if direction == 0 {
                    (quote_notional as f64 / raw_price).round() as u64
                } else {
                    quote_notional
                };
                let control = run(fork, amount_in, direction, tight.clone(), oracle.clone())
                    .expect("constant-spread control");
                let deteriorated = run(fork, amount_in, direction, shaped.clone(), oracle.clone())
                    .expect("size-impact quote");
                control.saturating_sub(deteriorated) as f64 * 1_000_000.0 / control as f64
            };

            let at_first = impact_ppm(first);
            let at_midpoint = impact_ppm(midpoint);
            let at_second = impact_ppm(second);
            assert!(
                at_first < 2.0,
                "{} direction {direction}: first-knot impact {at_first} ppm",
                fork.def.market
            );
            assert!(
                (4_000.0..5_100.0).contains(&at_midpoint),
                "{} direction {direction}: midpoint impact {at_midpoint} ppm",
                fork.def.market
            );
            assert!(
                (8_500.0..9_500.0).contains(&at_second),
                "{} direction {direction}: second-knot impact {at_second} ppm",
                fork.def.market
            );
            assert!(at_first < at_midpoint && at_midpoint < at_second);
            checked += 1;
        }
    }
    assert_eq!(checked, 4, "both spline directions on both markets");
}

#[tokio::test]
async fn solfi_freshness_boundary_and_age_widening_are_real_program_behavior() {
    let fork = &forks().await[1];
    let live = apply_raw(
        "solfi-freshness",
        &fork.oracle.data,
        &[
            ("publication_slot", serde_json::json!(0)),
            ("validity_horizon", serde_json::json!(0)),
        ],
        fork.slot,
    );
    let control = run(fork, fork.def.base_trade, 0, fork.market.data.clone(), live)
        .expect("horizon is inclusive");
    assert!(control > 0);

    let aged = apply_raw(
        "solfi-freshness",
        &fork.oracle.data,
        &[
            ("publication_slot", serde_json::json!(-55)),
            ("validity_horizon", serde_json::json!(0)),
        ],
        fork.slot,
    );
    let aged_out =
        run(fork, fork.def.base_trade, 0, fork.market.data.clone(), aged).expect("old but valid");
    assert!(
        aged_out < control,
        "publication age must widen the quote strictly"
    );

    let expired = apply_raw(
        "solfi-freshness",
        &fork.oracle.data,
        &[("validity_horizon", serde_json::json!(-1))],
        fork.slot,
    );
    for direction in 0..=1 {
        let amount_in = if direction == 0 {
            fork.def.base_trade
        } else {
            fork.def.quote_trade
        };
        let err = run(
            fork,
            amount_in,
            direction,
            fork.market.data.clone(),
            expired.clone(),
        )
        .expect_err("expired oracle must reject");
        assert!(err.contains("Custom(23)"), "unexpected expiry error: {err}");
    }
}

#[tokio::test]
async fn solfi_one_sided_inventory_exhaustion_and_inventory_policy_are_real() {
    let fork = &forks().await[1];
    for direction in 0..=1 {
        let amount_in = if direction == 0 {
            fork.def.base_trade
        } else {
            fork.def.quote_trade
        };
        let control = run(
            fork,
            amount_in,
            direction,
            fork.market.data.clone(),
            fork.oracle.data.clone(),
        )
        .expect("funded control");
        assert!(control > 0);
        let mut base = fork.base_vault.data.clone();
        let mut quote = fork.quote_vault.data.clone();
        let payout = if direction == 0 {
            &mut quote
        } else {
            &mut base
        };
        let starved = amount(payout) / 1000;
        *payout = apply_raw(
            "solfi-vault-balance",
            payout,
            &[("amount", serde_json::json!(starved))],
            fork.slot,
        );
        let err = replay(
            fork,
            amount_in,
            direction,
            fork.market.data.clone(),
            fork.oracle.data.clone(),
            base,
            quote,
        )
        .expect_err("starved payout vault must refuse");
        assert!(
            err.contains("Custom(18)"),
            "unexpected liquidity error: {err}"
        );
    }

    // The input-side vault is also read for inventory policy. Sweep sizes and magnitudes: the
    // decision-tree thresholds are live configuration, so a fixed /10 probe is not a protocol
    // guarantee. The mutated vault pays nothing in the tested direction, separating this from a
    // token-transfer failure.
    let mut checked = 0;
    let mut changed = false;
    for direction in 0..=1 {
        let unit = if direction == 0 {
            fork.def.base_trade
        } else {
            fork.def.quote_trade
        };
        for trade_multiple in [1, 10, 100] {
            let amount_in = unit.saturating_mul(trade_multiple);
            let Ok(baseline) = run(
                fork,
                amount_in,
                direction,
                fork.market.data.clone(),
                fork.oracle.data.clone(),
            ) else {
                continue;
            };
            for balance_multiple in [2, 10, 100] {
                let mut base = fork.base_vault.data.clone();
                let mut quote = fork.quote_vault.data.clone();
                let input_vault = if direction == 0 {
                    &mut base
                } else {
                    &mut quote
                };
                let raised = amount(input_vault).saturating_mul(balance_multiple);
                *input_vault = apply_raw(
                    "solfi-vault-balance",
                    input_vault,
                    &[("amount", serde_json::json!(raised))],
                    fork.slot,
                );
                if let Ok(shifted) = replay(
                    fork,
                    amount_in,
                    direction,
                    fork.market.data.clone(),
                    fork.oracle.data.clone(),
                    base,
                    quote,
                ) {
                    checked += 1;
                    changed |= shifted != baseline;
                }
            }
        }
    }
    assert!(checked >= 6, "only {checked} inventory probes settled");
    assert!(
        changed,
        "no swept input-vault magnitude reached inventory pricing"
    );
}

#[test]
fn solfi_swap_wire_format_has_no_fee_or_tier_argument() {
    let amount_in = 123u64;
    let min_out = 456u64;
    let direction = 1u8;
    let mut data = vec![7u8];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_out.to_le_bytes());
    data.push(direction);
    assert_eq!(data.len(), 18);
    assert_eq!(
        &data,
        &[7, 123, 0, 0, 0, 0, 0, 0, 0, 200, 1, 0, 0, 0, 0, 0, 0, 1]
    );
}
