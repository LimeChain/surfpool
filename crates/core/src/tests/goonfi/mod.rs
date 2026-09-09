//! Behavioral proofs for GoonFi's oracle and market layouts against the current deployed program.
//!
//! GoonFi V2 prices swaps from a per-market oracle account owned by a companion publisher
//! program, not from the market account itself. The market account carries the pair's identities
//! (mints, vaults, oracle pointer) in cleartext plus the reference band that guards the oracle
//! price; the oracle carries bid/ask, a u32 freshness slot, and a dynamic staleness multiplier.
//!
//! Run serially against mainnet:
//! `cargo test -p surfpool-core --features integration-tests tests::goonfi -- --test-threads=1`

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_pack::Pack;
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::goonfi::v1::{
            GoonfiMarket, build_goonfi_price_scenario, discover_goonfi_markets,
        },
    },
    surfnet::svm::SurfnetSvm,
    tests::live,
};

const GOONFI_PROGRAM: &str = "goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE";
const GOONFI_PROGRAMDATA: &str = "124gUYwjVnJQ4sJsFug9gHPzPLEtwCbAQC5LkbaDgx9s";
const ORACLE_PROGRAMDATA: &str = "7btzN5NEjnZqdQECwT88XhixeGnZjz5YKqjYGYKxKE5z";
const GOONFI_ORACLE_PROGRAM: &str = "dijkbkCAKfFTCxQg3u1pg82gVU1jJGHBBRcteD11mBu";
const GOONFI_GLOBAL: &str = "BNrK9LpEn65QA4TyBLVSMdngW3XHj3xLfFPwGdCBv8wV";
const JUPITER_PROGRAM: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const CURRENT_DEPLOY_SLOT: u64 = 438_563_879;
const CURRENT_ELF_SHA256: &str = "73e580830356c7a086d8bec422790b2600108a8129faebdfc055bd46d8936c2e";
const ORACLE_DEPLOY_SLOT: u64 = 404_369_628;
const ORACLE_ELF_SHA256: &str = "0fc545beb6abd12682ae68a27fa1e2a22d86d5d1dbbbe6d1e8f49e53ef762695";

/// Deployed-program error codes, proven by the replay runs below.
const ERROR_STALE_ORACLE: &str = "Custom(21)";
const ERROR_PRICE_OUT_OF_BAND: &str = "Custom(36)";
const ERROR_MIN_AMOUNT_OUT: &str = "Custom(15)";
const ERROR_INSUFFICIENT_LIQUIDITY: &str = "Custom(1)";

/// Oracle layout: both prices are the human pair price times 10^6, independent of mint decimals.
/// The freshness slot is 4 bytes; the u32 beside it is the decay-rate multiplier around 10^6 -
/// it scales how fast a quote degrades with age and does not move the rejection boundary.
const ORACLE_BID_OFFSET: usize = 0;
const ORACLE_ASK_OFFSET: usize = 8;
const ORACLE_SLOT_OFFSET: usize = 16;
const ORACLE_MULTIPLIER_OFFSET: usize = 20;
const ORACLE_TS_MS_OFFSET: usize = 24;

/// Market-account fields the flows touch or read. The two reference prices band-guard the oracle;
/// the mint and oracle pointers identify the pair.
const MARKET_BASE_MINT_OFFSET: usize = 80;
const MARKET_QUOTE_MINT_OFFSET: usize = 112;
const MARKET_ORACLE_OFFSET: usize = 208;
const MARKET_REF_A_OFFSET: usize = 1712;
const MARKET_REF_B_OFFSET: usize = 1720;

#[derive(Clone, Copy)]
struct MarketSpec {
    market: &'static str,
    base_vault: &'static str,
    quote_vault: &'static str,
    base_mint: &'static str,
    quote_mint: &'static str,
    oracle: &'static str,
    amount_in: u64,
}

/// The pair the captured reference swap traded, so the replay mirrors a known-good transaction.
const PRIMARY_MARKET: MarketSpec = MarketSpec {
    market: "HBDaV4ndLuVe6qK1vGCXReon4B1DJKa9UrbqP8cVqywx",
    base_vault: "4KDPiofhBxLMuTuvaYtMAqY6e5DnzbHLB6i7eeU239f6",
    quote_vault: "DAogoedaaCcn2SzTc3yi7bWgTWYv5MwoTj6ySgw9snLS",
    base_mint: "A7bdiYdS5GjqGFtxf17ppRHtDKPkkRqbKtR27dxvQXaS",
    quote_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    oracle: "vCDwWKdqPHYAP7q5zXY6xk3XC5Ct5oqCs5fdpoosPNq",
    amount_in: 25_109_852,
};

const SOL_USDC_MARKET: MarketSpec = MarketSpec {
    market: "GMCJvYGf5Ex2ARiMquaBDqU6iKM8uiEQkB8jCnoNfHpC",
    base_vault: "8ncU5YW1CQwvr4gs7buH57bW58e86TDau4STrCJBuz8z",
    quote_vault: "EunHLeqeJKvxnCPQSytnBP63HJVk2fbHceiKKpngyAo8",
    base_mint: "So11111111111111111111111111111111111111112",
    quote_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    oracle: "7yecFG22heommABQ5svcbQLK1Ua4ZrJsHPiktZ17jfm3",
    amount_in: 1_000_000_000,
};

#[derive(Clone)]
struct GoonfiFork {
    spec: MarketSpec,
    elf: Vec<u8>,
    global: Account,
    market: Account,
    base_vault: Account,
    quote_vault: Account,
    base_mint: Account,
    quote_mint: Account,
    oracle: Account,
}

declare_process_instruction!(GoonfiCpiWrapper, 1, |invoke_context| {
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
            program_id: Pubkey::from_str_const(GOONFI_PROGRAM),
            accounts,
            data: context.get_instruction_data().to_vec(),
        }
    };
    invoke_context.native_invoke_signed(instruction, &[])
});

async fn fetch_accounts(addresses: &[&str]) -> Vec<Account> {
    let pubkeys: Vec<Pubkey> = addresses
        .iter()
        .map(|address| Pubkey::from_str_const(address))
        .collect();
    live::fetch(&pubkeys).await
}

async fn goonfi_fork(spec: MarketSpec) -> GoonfiFork {
    // The ProgramData and global accounts total near a megabyte, which the public endpoint
    // refuses to return alongside the market graph. Fetch the two big slow-moving accounts
    // separately and keep the price-coupled market graph in one same-slot batch.
    let mut big = fetch_accounts(&[GOONFI_PROGRAMDATA, GOONFI_GLOBAL, ORACLE_PROGRAMDATA]).await;
    let mut accounts = fetch_accounts(&[
        spec.market,
        spec.base_vault,
        spec.quote_vault,
        spec.base_mint,
        spec.quote_mint,
        spec.oracle,
    ])
    .await;
    let programdata = big.remove(0);
    assert_eq!(programdata.data.len(), 252_429, "ProgramData size changed");
    assert_eq!(
        u64::from_le_bytes(programdata.data[4..12].try_into().unwrap()),
        CURRENT_DEPLOY_SLOT,
        "GoonFi was redeployed; revalidate the raw layout"
    );
    let elf = programdata.data[45..].to_vec();
    assert_eq!(
        hex::encode(Sha256::digest(&elf)),
        CURRENT_ELF_SHA256,
        "GoonFi ELF changed without a ProgramData address change"
    );
    // The publisher's identity is pinned too: its oracle accounts are the price templates' write
    // targets, so a redeploy there also voids the layout evidence.
    let oracle_programdata = big.pop().expect("oracle programdata fetched");
    assert_eq!(
        oracle_programdata.data.len(),
        557,
        "oracle publisher ProgramData size changed"
    );
    assert_eq!(
        u64::from_le_bytes(oracle_programdata.data[4..12].try_into().unwrap()),
        ORACLE_DEPLOY_SLOT,
        "the oracle publisher was redeployed; revalidate the oracle layout"
    );
    assert_eq!(
        hex::encode(Sha256::digest(&oracle_programdata.data[45..])),
        ORACLE_ELF_SHA256,
        "oracle publisher ELF changed without a ProgramData address change"
    );

    GoonfiFork {
        spec,
        elf,
        global: big.remove(0),
        market: accounts.remove(0),
        base_vault: accounts.remove(0),
        quote_vault: accounts.remove(0),
        base_mint: accounts.remove(0),
        quote_mint: accounts.remove(0),
        oracle: accounts.remove(0),
    }
}

fn with_controlled_inventory(mut fork: GoonfiFork) -> GoonfiFork {
    // Publishers can drain live vaults to dust. Fund only the local fixture so price and age
    // assertions measure those controls rather than unrelated, time-varying inventory limits.
    for (address, vault, mint_address, mint) in [
        (
            fork.spec.base_vault,
            &mut fork.base_vault,
            fork.spec.base_mint,
            &fork.base_mint,
        ),
        (
            fork.spec.quote_vault,
            &mut fork.quote_vault,
            fork.spec.quote_mint,
            &fork.quote_mint,
        ),
    ] {
        assert_eq!(vault.owner, spl_token_interface::ID);
        assert_eq!(mint.owner, spl_token_interface::ID);
        let mint_state = spl_token_interface::state::Mint::unpack(&mint.data)
            .expect("controlled fixture mint must remain valid");
        let mut token = spl_token_interface::state::Account::unpack(&vault.data)
            .expect("controlled fixture vault must remain valid");
        assert_eq!(token.mint, Pubkey::from_str_const(mint_address));
        assert_eq!(token.owner, Pubkey::from_str_const(fork.spec.market));
        let minimum_amount = 10u64
            .checked_pow(u32::from(mint_state.decimals))
            .and_then(|unit| unit.checked_mul(10_000))
            .expect("10,000 whole fixture tokens must fit u64");
        let original_amount = token.amount;
        token.amount = token.amount.max(minimum_amount);
        let original_data = vault.data.clone();
        spl_token_interface::state::Account::pack(token, &mut vault.data)
            .expect("pack controlled fixture vault");
        if let solana_program_option::COption::Some(reserve) = token.is_native {
            vault.lamports = reserve
                .checked_add(token.amount)
                .expect("controlled native vault funding fits u64");
        }
        assert_only_ranges_changed(&original_data, &vault.data, &[(64, 72)]);
        eprintln!(
            "GoonFi controlled local inventory {address}: captured {original_amount}, prepared {} raw units; market, oracle and deployed ELF remain captured",
            token.amount
        );
    }
    fork
}

fn token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    data
}

fn native_token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut data = token_account(mint, owner, amount);
    data[109..113].copy_from_slice(&1u32.to_le_bytes());
    data[113..121].copy_from_slice(&2_039_280u64.to_le_bytes());
    data
}

fn token_amount(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[64..72].try_into().unwrap())
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn write_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn oracle_slot(data: &[u8]) -> u64 {
    u64::from(read_u32(data, ORACLE_SLOT_OFFSET))
}

fn scale_prices(data: &mut [u8], numerator: u64, denominator: u64) {
    for offset in [ORACLE_BID_OFFSET, ORACLE_ASK_OFFSET] {
        let scaled = (u128::from(read_u64(data, offset)) * u128::from(numerator)
            / u128::from(denominator)) as u64;
        write_u64(data, offset, scaled);
    }
}

fn scale_refs(data: &mut [u8], numerator: u64, denominator: u64) {
    for offset in [MARKET_REF_A_OFFSET, MARKET_REF_B_OFFSET] {
        let scaled = (u128::from(read_u64(data, offset)) * u128::from(numerator)
            / u128::from(denominator)) as u64;
        write_u64(data, offset, scaled);
    }
}

fn assert_only_ranges_changed(before: &[u8], after: &[u8], ranges: &[(usize, usize)]) {
    assert_eq!(after.len(), before.len());
    for index in live::diff_indices(before, after) {
        assert!(
            ranges
                .iter()
                .any(|(start, end)| (*start..*end).contains(&index)),
            "unexpected changed byte at {index}"
        );
    }
}

struct RunConfig {
    amount_in: u64,
    is_bid: u8,
    min_amount_out: u64,
    /// Slots past the oracle's snapshot update slot at which the swap executes.
    clock_slot_age: u64,
    /// Seconds past the oracle's snapshot publish time at which the swap executes.
    clock_ts_age: i64,
}

impl RunConfig {
    fn sell(amount_in: u64) -> Self {
        Self {
            amount_in,
            is_bid: 0,
            min_amount_out: 1,
            clock_slot_age: 1,
            clock_ts_age: 1,
        }
    }

    fn buy(amount_in: u64) -> Self {
        Self {
            is_bid: 1,
            ..Self::sell(amount_in)
        }
    }

    fn sell_at_age(amount_in: u64, clock_slot_age: u64) -> Self {
        Self {
            clock_slot_age,
            ..Self::sell(amount_in)
        }
    }
}

fn goonfi_run(
    fork: &GoonfiFork,
    config: RunConfig,
    mutate_oracle: impl FnOnce(&mut Vec<u8>),
) -> Result<u64, String> {
    goonfi_run_full(fork, config, mutate_oracle, |_| {})
}

fn goonfi_run_full(
    fork: &GoonfiFork,
    config: RunConfig,
    mutate_oracle: impl FnOnce(&mut Vec<u8>),
    mutate_market: impl FnOnce(&mut Vec<u8>),
) -> Result<u64, String> {
    goonfi_run_capturing_oracle(fork, config, mutate_oracle, mutate_market)
        .map(|(amount_out, _)| amount_out)
}

/// Executes one GoonFi swap in LiteSVM against forked mainnet state: the deployed ELF, driven
/// through a wrapper builtin standing in for Jupiter, reproducing the aggregator-routed shape
/// every live swap has. Returns the fill and the oracle's post-execution bytes.
fn goonfi_run_capturing_oracle(
    fork: &GoonfiFork,
    config: RunConfig,
    mutate_oracle: impl FnOnce(&mut Vec<u8>),
    mutate_market: impl FnOnce(&mut Vec<u8>),
) -> Result<(u64, Vec<u8>), String> {
    use litesvm::LiteSVM;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    let program_id = Pubkey::from_str_const(GOONFI_PROGRAM);
    let global_key = Pubkey::from_str_const(GOONFI_GLOBAL);
    let market_key = Pubkey::from_str_const(fork.spec.market);
    let base_vault_key = Pubkey::from_str_const(fork.spec.base_vault);
    let quote_vault_key = Pubkey::from_str_const(fork.spec.quote_vault);
    let base_mint_key = Pubkey::from_str_const(fork.spec.base_mint);
    let quote_mint_key = Pubkey::from_str_const(fork.spec.quote_mint);
    let oracle_key = Pubkey::from_str_const(fork.spec.oracle);
    let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);

    let mut oracle = fork.oracle.data.clone();
    mutate_oracle(&mut oracle);
    let mut market = fork.market.data.clone();
    mutate_market(&mut market);
    // Ages are measured from the snapshot the fork fetched, not from mutated bytes, so a
    // re-stamped freshness field changes the account's age rather than moving the clock.
    let oracle_update_slot = oracle_slot(&fork.oracle.data);
    let oracle_ts_seconds = (read_u64(&fork.oracle.data, ORACLE_TS_MS_OFFSET) / 1_000) as i64;

    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(program_id, &fork.elf)
        .map_err(|error| format!("add_program: {error:?}"))?;
    svm.add_builtin(
        Pubkey::from_str_const(JUPITER_PROGRAM),
        GoonfiCpiWrapper::register,
    );
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = oracle_update_slot + config.clock_slot_age;
    clock.unix_timestamp = oracle_ts_seconds + config.clock_ts_age;
    svm.set_sysvar(&clock);
    svm.set_account(
        Pubkey::from_str_const("SysvarLastRestartS1ot1111111111111111111111"),
        Account {
            lamports: 1_000_000,
            data: 246_464_040u64.to_le_bytes().to_vec(),
            owner: Pubkey::from_str_const("Sysvar1111111111111111111111111111111111111"),
            executable: false,
            rent_epoch: 0,
        },
    )
    .map_err(|error| format!("set last restart slot: {error:?}"))?;

    let mut oracle_account = fork.oracle.clone();
    oracle_account.data = oracle;
    let mut market_account = fork.market.clone();
    market_account.data = market;
    for (key, account) in [
        (global_key, fork.global.clone()),
        (market_key, market_account),
        (base_vault_key, fork.base_vault.clone()),
        (quote_vault_key, fork.quote_vault.clone()),
        (base_mint_key, fork.base_mint.clone()),
        (quote_mint_key, fork.quote_mint.clone()),
        (oracle_key, oracle_account),
    ] {
        svm.set_account(key, account)
            .map_err(|error| format!("set {key}: {error:?}"))?;
    }

    let taker = Keypair::new();
    svm.airdrop(&taker.pubkey(), 10_000_000_000)
        .map_err(|error| format!("airdrop: {error:?}"))?;
    let user_base_key = Pubkey::new_unique();
    let user_quote_key = Pubkey::new_unique();
    let (base_funds, quote_funds) = if config.is_bid == 0 {
        (config.amount_in, 0)
    } else {
        (0, config.amount_in)
    };
    let user_account = |mint: &Pubkey, amount: u64| {
        let is_native =
            mint == &Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        Account {
            lamports: if is_native {
                amount.saturating_add(2_039_280)
            } else {
                10_000_000
            },
            data: if is_native {
                native_token_account(mint, &taker.pubkey(), amount)
            } else {
                token_account(mint, &taker.pubkey(), amount)
            },
            owner: token_program,
            executable: false,
            rent_epoch: 0,
        }
    };
    svm.set_account(user_base_key, user_account(&base_mint_key, base_funds))
        .map_err(|error| format!("set user base: {error:?}"))?;
    svm.set_account(user_quote_key, user_account(&quote_mint_key, quote_funds))
        .map_err(|error| format!("set user quote: {error:?}"))?;

    let mut data = vec![1u8, config.is_bid];
    data.extend_from_slice(&config.amount_in.to_le_bytes());
    data.extend_from_slice(&config.min_amount_out.to_le_bytes());
    let mut budget = vec![2u8];
    budget.extend_from_slice(&1_400_000u32.to_le_bytes());
    let instructions = vec![
        Instruction {
            program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
            accounts: vec![],
            data: budget,
        },
        Instruction {
            program_id: Pubkey::from_str_const(JUPITER_PROGRAM),
            accounts: vec![
                AccountMeta::new_readonly(program_id, false),
                AccountMeta::new(taker.pubkey(), true),
                AccountMeta::new(market_key, false),
                AccountMeta::new(user_base_key, false),
                AccountMeta::new(user_quote_key, false),
                AccountMeta::new(base_vault_key, false),
                AccountMeta::new(quote_vault_key, false),
                AccountMeta::new_readonly(base_mint_key, false),
                AccountMeta::new_readonly(quote_mint_key, false),
                AccountMeta::new_readonly(oracle_key, false),
                AccountMeta::new_readonly(global_key, false),
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
    let mut message = solana_message::Message::new(&instructions, Some(&taker.pubkey()));
    message.recent_blockhash = svm.latest_blockhash();
    let signature_count = message.header.num_required_signatures as usize;
    let mut transaction = Transaction::new_unsigned(message);
    transaction.signatures = vec![solana_signature::Signature::default(); signature_count];
    transaction.signatures[0] = taker.sign_message(&transaction.message.serialize());

    svm.send_transaction(transaction)
        .map_err(|error| format!("{error:?}"))?;
    let destination = if config.is_bid == 0 {
        user_quote_key
    } else {
        user_base_key
    };
    let amount_out = token_amount(
        &svm.get_account(&destination)
            .expect("destination account")
            .data,
    );
    let oracle_after = svm.get_account(&oracle_key).expect("oracle account").data;
    Ok((amount_out, oracle_after))
}

/// Forks a market by its address alone, resolving vaults, mints, and oracle from the market
/// account's own pointers. Used where a fixture market outside the two hardcoded specs is needed.
async fn fork_from_market(market: &'static str, amount_in: u64) -> GoonfiFork {
    let accounts = fetch_accounts(&[market]).await;
    let data = &accounts[0].data;
    let field = |offset: usize| -> &'static str {
        Box::leak(
            Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
                .to_string()
                .into_boxed_str(),
        )
    };
    let spec = MarketSpec {
        market,
        base_vault: field(144),
        quote_vault: field(176),
        base_mint: field(MARKET_BASE_MINT_OFFSET),
        quote_mint: field(MARKET_QUOTE_MINT_OFFSET),
        oracle: field(MARKET_ORACLE_OFFSET),
        amount_in,
    };
    goonfi_fork(spec).await
}

/// Materializes the goonfi-stale-quote template with its default lead onto the fork's live
/// oracle bytes, asserts the exact 4-byte slot it wrote, and proves the deployed program then
/// rejects the swap. This is the template's own default doing the aging, not a hand-picked age.
fn stale_template_default_rejects(fork: &GoonfiFork, amount: u64) {
    let registry = TemplateRegistry::new();
    let stale = registry.get("goonfi-stale-quote").expect("stale template");
    let snapshot_slot = oracle_slot(&fork.oracle.data);
    let aged = stale
        .raw_layout
        .as_ref()
        .expect("oracle raw layout")
        .materialize(
            &fork.oracle.data,
            &stale.properties,
            &HashMap::from([("last_update_slot".to_string(), serde_json::Value::Null)]),
            snapshot_slot,
        )
        .expect("materialize stale default");
    assert_eq!(
        oracle_slot(&aged),
        snapshot_slot - 2_000,
        "the default lead must write exactly slot minus 2000"
    );
    assert_only_ranges_changed(&fork.oracle.data, &aged, &[(16, 20)]);
    assert_rejects_with(
        goonfi_run(fork, RunConfig::sell(amount), |oracle| {
            *oracle = aged.clone()
        }),
        ERROR_STALE_ORACLE,
        "a quote aged by the stale template's default lead",
    );
}

fn assert_rejects_with(result: Result<u64, String>, code: &str, context: &str) {
    match result {
        Ok(amount) => panic!("{context}: expected {code}, got a fill of {amount}"),
        Err(error) => assert!(
            error.contains(code),
            "{context}: expected {code} in: {error}"
        ),
    }
}

#[tokio::test]
async fn goonfi_templates_guard_oracle_and_market_and_preserve_unwritten_bytes() {
    let fork = goonfi_fork(PRIMARY_MARKET).await;
    let registry = TemplateRegistry::new();
    let price = registry.get("goonfi-price").expect("price template");
    let stale = registry.get("goonfi-stale-quote").expect("stale template");
    let fresh = registry
        .get("goonfi-freshness")
        .expect("freshness template");
    let band = registry
        .get("goonfi-reference-band")
        .expect("reference-band template");

    let oracle_layout = price.raw_layout.as_ref().expect("oracle raw layout");
    let market_layout = band.raw_layout.as_ref().expect("market raw layout");
    assert!(oracle_layout.guard(&fork.oracle.data).is_ok());
    assert!(market_layout.guard(&fork.market.data).is_ok());
    assert!(oracle_layout.guard(&fork.oracle.data[..16]).is_err());
    assert!(market_layout.guard(&fork.market.data[..2000]).is_err());
    let mut flipped = fork.market.data.clone();
    flipped[0] ^= 0xff;
    assert!(market_layout.guard(&flipped).is_err());

    let priced = oracle_layout
        .materialize(
            &fork.oracle.data,
            &price.properties,
            &HashMap::from([
                ("bid_price_x1e6".to_string(), serde_json::json!("123456789")),
                ("ask_price_x1e6".to_string(), serde_json::json!("123456790")),
            ]),
            0,
        )
        .expect("materialize price");
    assert_eq!(read_u64(&priced, ORACLE_BID_OFFSET), 123_456_789);
    assert_eq!(read_u64(&priced, ORACLE_ASK_OFFSET), 123_456_790);
    assert_only_ranges_changed(&fork.oracle.data, &priced, &[(0, 16)]);

    // The freshness slot is 4 bytes wide: the dynamic multiplier right after it must survive.
    let target_slot = 500_000_123;
    for (template, label) in [(stale, "stale"), (fresh, "freshness")] {
        let stamped = template
            .raw_layout
            .as_ref()
            .expect("oracle raw layout")
            .materialize(
                &fork.oracle.data,
                &template.properties,
                &HashMap::from([("last_update_slot".to_string(), serde_json::Value::Null)]),
                target_slot,
            )
            .unwrap_or_else(|error| panic!("materialize {label}: {error}"));
        assert_only_ranges_changed(&fork.oracle.data, &stamped, &[(16, 20)]);
        assert_eq!(
            read_u32(&stamped, ORACLE_MULTIPLIER_OFFSET),
            read_u32(&fork.oracle.data, ORACLE_MULTIPLIER_OFFSET),
            "{label} clobbered the staleness multiplier"
        );
    }

    let banded = market_layout
        .materialize(
            &fork.market.data,
            &band.properties,
            &HashMap::from([
                (
                    "reference_price_a_x1e6".to_string(),
                    serde_json::json!("123456789"),
                ),
                (
                    "reference_price_b_x1e6".to_string(),
                    serde_json::json!("123456789"),
                ),
            ]),
            0,
        )
        .expect("materialize reference band");
    assert_eq!(read_u64(&banded, MARKET_REF_A_OFFSET), 123_456_789);
    assert_eq!(read_u64(&banded, MARKET_REF_B_OFFSET), 123_456_789);
    assert_only_ranges_changed(&fork.market.data, &banded, &[(1712, 1728)]);
}

/// Proves the exact state the real builder prepares, end to end: `build_goonfi_price_scenario`
/// output registers and materializes through the production path, touching only its declared
/// bytes, and the deployed program then fills at the prepared price. The scenario is anchored at
/// the oracle's snapshot slot so the materialized freshness stamp matches the replay clock.
async fn builder_prepares_and_the_program_fills(fork: &GoonfiFork) {
    let market_key = Pubkey::from_str_const(fork.spec.market);
    let oracle_key = Pubkey::from_str_const(fork.spec.oracle);
    let market =
        GoonfiMarket::validate(market_key, &fork.market, &fork.oracle).expect("validate market");
    let live_bid = read_u64(&fork.oracle.data, ORACLE_BID_OFFSET);
    let target = live_bid * 3 / 2;
    let price = format!("{}.{:06}", target / 1_000_000, target % 1_000_000);
    let preparation =
        build_goonfi_price_scenario(&market, &price).expect("build GoonFi price scenario");
    assert_eq!(preparation.price_x1e6, target);

    let base_slot = oracle_slot(&fork.oracle.data);
    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    svm.inner
        .set_account(market_key, fork.market.clone())
        .expect("seed GoonFi market");
    svm.inner
        .set_account(oracle_key, fork.oracle.clone())
        .expect("seed GoonFi oracle");
    svm.register_scenario(preparation.scenario, Some(base_slot))
        .expect("register GoonFi scenario");
    svm.materialize_overrides_for_slot(&None, base_slot)
        .await
        .expect("materialize GoonFi scenario");

    let oracle = svm
        .inner
        .get_account(&oracle_key)
        .expect("get oracle")
        .expect("oracle present")
        .data;
    let market_data = svm
        .inner
        .get_account(&market_key)
        .expect("get market")
        .expect("market present")
        .data;
    assert_eq!(read_u64(&oracle, ORACLE_BID_OFFSET), target);
    assert_eq!(read_u64(&oracle, ORACLE_ASK_OFFSET), target);
    assert_eq!(oracle_slot(&oracle), base_slot);
    assert_eq!(read_u64(&market_data, MARKET_REF_A_OFFSET), target);
    assert_eq!(read_u64(&market_data, MARKET_REF_B_OFFSET), target);
    assert_only_ranges_changed(&fork.oracle.data, &oracle, &[(0, 20)]);
    assert_only_ranges_changed(&fork.market.data, &market_data, &[(1712, 1728)]);

    // The deployed program fills at the prepared price, against the exact materialized bytes.
    let baseline =
        goonfi_run(fork, RunConfig::sell(fork.spec.amount_in), |_| {}).expect("baseline sell");
    let prepared = goonfi_run_full(
        fork,
        RunConfig::sell(fork.spec.amount_in),
        |data| *data = oracle.clone(),
        |data| *data = market_data.clone(),
    )
    .expect("sell against the builder-prepared state");
    let expected = (u128::from(baseline) * u128::from(target) / u128::from(live_bid)) as u64;
    assert!(
        prepared.abs_diff(expected) <= expected / 500,
        "the prepared price must set the fill: {prepared} vs ~{expected}"
    );

    // Only the persistent freshness override re-applies on the next slot.
    svm.materialize_overrides_for_slot(&None, base_slot + 1)
        .await
        .expect("materialize persistent GoonFi freshness");
    let next = svm
        .inner
        .get_account(&oracle_key)
        .expect("get oracle")
        .expect("oracle present")
        .data;
    assert_eq!(oracle_slot(&next), base_slot + 1);
    assert_eq!(read_u64(&next, ORACLE_BID_OFFSET), target);
    assert_only_ranges_changed(&oracle, &next, &[(16, 20)]);
}

#[tokio::test]
async fn goonfi_builder_scenario_materializes_and_fills_across_oracle_and_market() {
    let fork = with_controlled_inventory(goonfi_fork(PRIMARY_MARKET).await);
    builder_prepares_and_the_program_fills(&fork).await;
}

#[tokio::test]
async fn goonfi_price_and_reference_band_control_the_deployed_program() {
    let fork = with_controlled_inventory(goonfi_fork(PRIMARY_MARKET).await);
    let amount = fork.spec.amount_in;

    let baseline = goonfi_run(&fork, RunConfig::sell(amount), |_| {}).expect("baseline sell");
    assert!(baseline > 0);

    // No-op rewrite proves the encoding round-trips; the program cannot tell the bytes moved.
    let noop = goonfi_run(&fork, RunConfig::sell(amount), |oracle| {
        let restated = read_u64(oracle, ORACLE_BID_OFFSET);
        write_u64(oracle, ORACLE_BID_OFFSET, restated);
    })
    .expect("no-op sell");
    assert_eq!(noop, baseline);

    // Coupled halve and double move the fill linearly in both directions.
    let halved = goonfi_run_full(
        &fork,
        RunConfig::sell(amount),
        |oracle| scale_prices(oracle, 1, 2),
        |market| scale_refs(market, 1, 2),
    )
    .expect("coupled halved sell");
    assert!(
        (halved * 2).abs_diff(baseline) <= 4,
        "halving the price must halve the fill: {halved} * 2 vs {baseline}"
    );
    let doubled = goonfi_run_full(
        &fork,
        RunConfig::sell(amount),
        |oracle| scale_prices(oracle, 2, 1),
        |market| scale_refs(market, 2, 1),
    )
    .expect("coupled doubled sell");
    assert!(
        doubled.abs_diff(baseline * 2) <= baseline / 500,
        "doubling the price must double the fill: {doubled} vs 2 * {baseline}"
    );

    // Decoupled moves reject: the band guards each direction against the venue-unfavorable side.
    assert_rejects_with(
        goonfi_run(&fork, RunConfig::sell(amount), |oracle| {
            scale_prices(oracle, 2, 1)
        }),
        ERROR_PRICE_OUT_OF_BAND,
        "sell with raised oracle and untouched reference band",
    );
    assert_rejects_with(
        goonfi_run(&fork, RunConfig::buy(100_000_000), |oracle| {
            scale_prices(oracle, 1, 2)
        }),
        ERROR_PRICE_OUT_OF_BAND,
        "buy with lowered oracle and untouched reference band",
    );
    let coupled_buy = goonfi_run_full(
        &fork,
        RunConfig::buy(100_000_000),
        |oracle| scale_prices(oracle, 1, 2),
        |market| scale_refs(market, 1, 2),
    )
    .expect("coupled halved buy");
    assert!(coupled_buy > 0);

    assert_rejects_with(
        goonfi_run(
            &fork,
            RunConfig {
                min_amount_out: u64::MAX,
                ..RunConfig::sell(amount)
            },
            |_| {},
        ),
        ERROR_MIN_AMOUNT_OUT,
        "sell with an impossible min_amount_out",
    );

    // Keep the successful trade size fixed so other input limits cannot mask vault depletion.
    let mut limited = fork.clone();
    write_u64(&mut limited.quote_vault.data, 64, baseline);
    let exact_inventory = goonfi_run(&limited, RunConfig::sell(amount), |_| {})
        .expect("sell with exactly enough quote inventory");
    assert_eq!(exact_inventory, baseline);

    write_u64(&mut limited.quote_vault.data, 64, baseline - 1);
    assert_rejects_with(
        goonfi_run(&limited, RunConfig::sell(amount), |_| {}),
        ERROR_INSUFFICIENT_LIQUIDITY,
        "sell with quote inventory one atomic unit below the measured output",
    );
    write_u64(&mut limited.quote_vault.data, 64, 0);
    assert_rejects_with(
        goonfi_run(&limited, RunConfig::sell(amount), |_| {}),
        ERROR_INSUFFICIENT_LIQUIDITY,
        "sell against a drained quote vault",
    );
}

fn stamp_multiplier(data: &mut [u8], multiplier: u32) {
    data[ORACLE_MULTIPLIER_OFFSET..ORACLE_MULTIPLIER_OFFSET + 4]
        .copy_from_slice(&multiplier.to_le_bytes());
}

/// First rejection age in 15..=40 under the given multiplier, asserting fills decay
/// monotonically before it and every rejection carries the staleness error.
fn rejection_boundary(fork: &GoonfiFork, amount: u64, multiplier: u32) -> u64 {
    let mut previous = u64::MAX;
    let mut first_rejection = None;
    for age in 15..=40 {
        let result = goonfi_run(fork, RunConfig::sell_at_age(amount, age), |oracle| {
            stamp_multiplier(oracle, multiplier)
        });
        match result {
            Ok(output) => {
                assert!(
                    first_rejection.is_none(),
                    "age {age} filled after the window closed at {first_rejection:?}"
                );
                assert!(output <= previous, "decay reversed at age {age}");
                previous = output;
            }
            Err(error) => {
                assert!(
                    error.contains(ERROR_STALE_ORACLE),
                    "age {age}: expected {ERROR_STALE_ORACLE} in: {error}"
                );
                first_rejection.get_or_insert(age);
            }
        }
    }
    first_rejection.expect("no rejection up to age 40")
}

#[tokio::test]
async fn goonfi_stale_quote_decays_then_rejects_and_freshness_restores() {
    let fork = with_controlled_inventory(goonfi_fork(PRIMARY_MARKET).await);
    let amount = fork.spec.amount_in;

    let fresh = goonfi_run(&fork, RunConfig::sell(amount), |_| {}).expect("fresh sell");
    let aged = goonfi_run(&fork, RunConfig::sell_at_age(amount, 10), |_| {}).expect("aged sell");
    assert!(
        aged < fresh,
        "the program decays a quote with age: {aged} at age 10 vs {fresh} at age 1"
    );

    // The boundary's source is per-market and unidentified; this range is a safety canary
    // around the observed value, not a fixed protocol constant.
    let live_multiplier = read_u32(&fork.oracle.data, ORACLE_MULTIPLIER_OFFSET);
    let boundary = rejection_boundary(&fork, amount, live_multiplier);
    assert!(
        (15..=35).contains(&boundary),
        "rejection boundary {boundary} left the observed range"
    );

    // The multiplier at offset 20 scales the decay, not the window: at half and double the live
    // value the boundary stays put, the decay rate scales with it, and the program leaves the
    // oracle bytes untouched.
    let mut decay_per_multiplier = Vec::new();
    for (label, numerator, denominator) in [("half", 1u64, 2u64), ("live", 1, 1), ("double", 2, 1)]
    {
        let multiplier =
            u32::try_from(u64::from(live_multiplier) * numerator / denominator).expect("fits u32");
        let mut expected_oracle = fork.oracle.data.clone();
        stamp_multiplier(&mut expected_oracle, multiplier);

        let (at_age_1, oracle_after) = goonfi_run_capturing_oracle(
            &fork,
            RunConfig::sell(amount),
            |oracle| stamp_multiplier(oracle, multiplier),
            |_| {},
        )
        .unwrap_or_else(|error| panic!("sell at {label} multiplier: {error}"));
        assert_eq!(
            oracle_after, expected_oracle,
            "the swap must not write the oracle ({label} multiplier)"
        );
        let at_age_10 = goonfi_run(&fork, RunConfig::sell_at_age(amount, 10), |oracle| {
            stamp_multiplier(oracle, multiplier)
        })
        .unwrap_or_else(|error| panic!("aged sell at {label} multiplier: {error}"));
        decay_per_multiplier.push(at_age_1 - at_age_10);

        assert_eq!(
            rejection_boundary(&fork, amount, multiplier),
            boundary,
            "the {label} multiplier must not move the rejection boundary"
        );
    }
    let [half, live, double] = decay_per_multiplier[..] else {
        unreachable!()
    };
    assert!(
        double.abs_diff(live * 2) <= live / 25,
        "doubling the multiplier must double the decay: {double} vs 2 * {live}"
    );
    assert!(
        (half * 2).abs_diff(live) <= live / 25,
        "halving the multiplier must halve the decay: {half} * 2 vs {live}"
    );

    // The wall-clock timestamp beside the slot is not consulted.
    let ts_aged = goonfi_run(
        &fork,
        RunConfig {
            clock_ts_age: 3_600,
            ..RunConfig::sell(amount)
        },
        |_| {},
    )
    .expect("sell an hour of wall-clock later");
    assert_eq!(ts_aged, fresh);

    // Deep staleness rejects; re-stamping the u32 slot alone restores the quote, which is what
    // the goonfi-freshness template does at every materialization.
    assert_rejects_with(
        goonfi_run(&fork, RunConfig::sell_at_age(amount, 1_000), |_| {}),
        ERROR_STALE_ORACLE,
        "sell at age 1000",
    );
    stale_template_default_rejects(&fork, amount);
    let restamped_slot = oracle_slot(&fork.oracle.data) + 1_000;
    let restamped = goonfi_run(&fork, RunConfig::sell_at_age(amount, 1_000), |oracle| {
        oracle[ORACLE_SLOT_OFFSET..ORACLE_SLOT_OFFSET + 4]
            .copy_from_slice(&(restamped_slot as u32).to_le_bytes());
    })
    .expect("sell at age 1000 with a re-stamped slot");
    assert!(
        restamped * 100 >= fresh * 99,
        "a re-stamped quote must fill near full price: {restamped} vs {fresh}"
    );
}

#[tokio::test]
async fn goonfi_second_market_proves_generic_price_and_staleness_layout() {
    let fork = with_controlled_inventory(goonfi_fork(SOL_USDC_MARKET).await);
    let amount = fork.spec.amount_in;

    let baseline = goonfi_run(&fork, RunConfig::sell(amount), |_| {}).expect("SOL/USDC sell");
    let halved = goonfi_run_full(
        &fork,
        RunConfig::sell(amount),
        |oracle| scale_prices(oracle, 1, 2),
        |market| scale_refs(market, 1, 2),
    )
    .expect("SOL/USDC coupled halved sell");
    assert!(
        (halved * 2).abs_diff(baseline) <= 4,
        "halving must halve on the second market too: {halved} * 2 vs {baseline}"
    );

    let bought = goonfi_run(&fork, RunConfig::buy(100_000_000), |_| {}).expect("SOL/USDC buy");
    assert!(bought > 0);

    builder_prepares_and_the_program_fills(&fork).await;

    // Well past every observed window on this market tier; the stablecoin tier's deeper windows
    // are covered by the stale-template default proof below.
    assert_rejects_with(
        goonfi_run(&fork, RunConfig::sell_at_age(amount, 200), |_| {}),
        ERROR_STALE_ORACLE,
        "SOL/USDC sell past the staleness window",
    );

    // The stablecoin tier fills at ages that reject every other market (USDT/USDC filled at age
    // 100 live), so the stale template's -2000 default must out-age even that window.
    let stable = with_controlled_inventory(
        fork_from_market("EEUNhHsRoUVgJUFpkupmdF4v7uLUw1zhYLp7u9s8zFqG", 0).await,
    );
    let stable_amount = 1_000_000;
    let filled = goonfi_run(&stable, RunConfig::sell_at_age(stable_amount, 50), |_| {})
        .expect("USDT/USDC fills at an age that rejects every volatile market");
    assert!(filled > 0);
    stale_template_default_rejects(&stable, stable_amount);
}

#[tokio::test]
async fn goonfi_discovery_fetches_live_market_and_oracle_relationships() {
    use std::collections::HashSet;

    let markets = discover_goonfi_markets(&live::client())
        .await
        .expect("discover GoonFi markets through the real RPC client");
    assert!(
        !markets.is_empty(),
        "live GoonFi discovery returned no markets"
    );
    let default = markets
        .iter()
        .find(|market| market.address == Pubkey::from_str_const(SOL_USDC_MARKET.market))
        .expect("live discovery must include the default SOL/USDC market");
    assert_eq!(
        default.oracle,
        Pubkey::from_str_const(SOL_USDC_MARKET.oracle)
    );
    assert_eq!(
        default.base_mint,
        Pubkey::from_str_const(SOL_USDC_MARKET.base_mint)
    );
    assert_eq!(
        default.quote_mint,
        Pubkey::from_str_const(SOL_USDC_MARKET.quote_mint)
    );
    assert_eq!((default.base_decimals, default.quote_decimals), (9, 6));
    let mut addresses = HashSet::new();
    let mut oracles = HashSet::new();
    for market in &markets {
        assert!(
            addresses.insert(market.address),
            "duplicate discovered market {}",
            market.address
        );
        assert!(
            oracles.insert(market.oracle),
            "duplicate discovered oracle {}",
            market.oracle
        );
    }
    for chunk in markets.chunks(40) {
        let addresses: Vec<Pubkey> = chunk
            .iter()
            .flat_map(|market| [market.address, market.oracle])
            .collect();
        let accounts = live::fetch(&addresses).await;
        for (discovered, accounts) in chunk.iter().zip(accounts.chunks_exact(2)) {
            let validated = GoonfiMarket::validate(discovered.address, &accounts[0], &accounts[1])
                .expect("discovered market and oracle must retain their live owners and layouts");
            assert_eq!(
                validated.oracle, discovered.oracle,
                "live market oracle pointer changed"
            );
            assert_eq!(&accounts[0].data[80..112], discovered.base_mint.as_ref());
            assert_eq!(&accounts[0].data[112..144], discovered.quote_mint.as_ref());
        }
    }
    eprintln!(
        "GoonFi real RPC discovery verified {} unique live market/oracle pairs",
        markets.len()
    );
}
