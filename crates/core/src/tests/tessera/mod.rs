//! Behavioral proofs for Tessera's raw market layout against the current deployed program.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;

use crate::{
    scenarios::{
        TemplateRegistry,
        protocols::tessera::v1::{TesseraMarket, build_tessera_fair_value_scenario},
    },
    surfnet::svm::SurfnetSvm,
    tests::live,
};

const TESSERA_PROGRAM: &str = "TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH";
const TESSERA_PROGRAMDATA: &str = "BzSXM6KLDpHQQChzr7Fdgbzwp8r8zRYWFFrHK2uZmDYV";
const TESSERA_GLOBAL_STATE: &str = "8ekCy2jHHUbW2yeNGFWYJT9Hm9FW7SvZcZK66dSZCDiF";
const TESSERA_SOL_USDC_MARKET: &str = "FLckHLGMJy5gEoXWwcE68Nprde1D4araK4TGLw4pQq2n";
const TESSERA_CBB_USDC_MARKET: &str = "9NkuAWB4LgCVFV77omEkJEjXqgV5PGupwMTu3B3pBRhc";
const TESSERA_CBB_VAULT: &str = "37hggNyT4Ec8GEcxMLrWrZyrMSSFMSiFT6VBayRYceZH";
const CBB_MINT: &str = "cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij";
const JUPITER_PROGRAM: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const TESSERA_SOL_VAULT: &str = "5pVN5XZB8cYBjNLFrsBCPWkCQBan5K5Mq2dWGzwPgGJV";
const TESSERA_USDC_VAULT: &str = "9t4P5wMwfFkyn92Z7hf463qYKEZf8ERVZsGBEPNp8uJx";
const TESSERA_V11_SENTINEL: &str = "8xeaWCsJYxRoudEZGJWURdfrtFhLYZz9b4iHJnW5tb3d";
const TESSERA_V11_CONFIG: &str = "BAT1Ndpu5gbLTp2AZkSXP79LJBZfCH4B3zGhi6LtvdhK";
const TESSERA_V11_MARKET_RECORD: &str = "4cG31VNF9TzFinNc7BmnjhFvGjxkY3sCETVMtMgbrhPs";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const DFLOW_PROGRAM: &str = "DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH";
const CURRENT_DEPLOY_SLOT: u64 = 438_800_691;
const CURRENT_ELF_SHA256: &str = "433f2a857ffe2045310a478b4aca0fd824308d01f283719275baec60e2aecb3b";
const MAX_PRICE_AGE_SLOTS: u64 = 19;
/// Where the deployed program reads each market's quote rejection age.
const FRESHNESS_LIMIT_OFFSET: usize = 88;

#[derive(Clone, Copy)]
struct JupiterMarketSpec {
    address: &'static str,
    base_vault: &'static str,
    quote_vault: &'static str,
    base_mint: &'static str,
    quote_mint: &'static str,
    amount_in: u64,
    direction: u8,
}

const JUPITER_MARKETS: [JupiterMarketSpec; 5] = [
    JupiterMarketSpec {
        address: TESSERA_CBB_USDC_MARKET,
        base_vault: TESSERA_CBB_VAULT,
        quote_vault: TESSERA_USDC_VAULT,
        base_mint: CBB_MINT,
        quote_mint: USDC_MINT,
        amount_in: 125_853,
        direction: 1,
    },
    JupiterMarketSpec {
        address: "5X9A6PpFQEsc9D5VdTGfgVyfVn8HnsArQpMMUZZfFg1a",
        base_vault: "8FNRrFbq5APT6uZGH6U5DcMNo3U6SDpKoQD3CQMZ5RTU",
        quote_vault: TESSERA_USDC_VAULT,
        base_mint: "SPCXxcqXj6e5dJDVNovHN8744zkbhM2bYudU45BimGb",
        quote_mint: USDC_MINT,
        amount_in: 4_000_000,
        direction: 0,
    },
    JupiterMarketSpec {
        address: "7sJf1SmKDDAFtBmMtg253rTbjG7zVFm3zTNounSgSNc9",
        base_vault: TESSERA_SOL_VAULT,
        quote_vault: "Ci3HZCb6fr5YiYLG9R6XbxHcjm2mDb1R3ugQ3bPZ7oKZ",
        base_mint: WSOL_MINT,
        quote_mint: "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
        amount_in: 77_868_902,
        direction: 0,
    },
    JupiterMarketSpec {
        address: "Ce8WKGKeNPrtk85inFtkpskekaNibZiogSZBrcP7yhTN",
        base_vault: "GYaM9Coc9gG4vzqTLRzGAZS6HFaBrebDohMUMYkyADRm",
        quote_vault: TESSERA_USDC_VAULT,
        base_mint: "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs",
        quote_mint: USDC_MINT,
        amount_in: 3_931_067,
        direction: 1,
    },
    JupiterMarketSpec {
        address: "DNhfyh75AApg1L1Yig3fErvERKutYRqfWLGb496iViSZ",
        base_vault: "FhdiaEWUX8ZrW5TT2iNjWivMCzuBZhUJutrpfw6CvsxU",
        quote_vault: TESSERA_USDC_VAULT,
        base_mint: "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
        quote_mint: USDC_MINT,
        amount_in: 8_702,
        direction: 0,
    },
];

struct JupiterMarketFork {
    spec: JupiterMarketSpec,
    market: Account,
    base_vault: Account,
    quote_vault: Account,
    base_mint: Account,
    quote_mint: Account,
}

struct TesseraFork {
    elf: Vec<u8>,
    global_state: Account,
    market: Account,
    base_vault: Account,
    quote_vault: Account,
    base_mint: Account,
    quote_mint: Account,
    sentinel: Account,
    config: Account,
    market_record: Account,
    jupiter_markets: Vec<JupiterMarketFork>,
}

declare_process_instruction!(TesseraCpiWrapper, 1, |invoke_context| {
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
            program_id: Pubkey::from_str_const(TESSERA_PROGRAM),
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

async fn tessera_fork() -> TesseraFork {
    let (cbb, spcx, wsol_usdt, weth, pump) = tokio::join!(
        fetch_jupiter_market(JUPITER_MARKETS[0]),
        fetch_jupiter_market(JUPITER_MARKETS[1]),
        fetch_jupiter_market(JUPITER_MARKETS[2]),
        fetch_jupiter_market(JUPITER_MARKETS[3]),
        fetch_jupiter_market(JUPITER_MARKETS[4]),
    );
    let jupiter_markets = vec![cbb, spcx, wsol_usdt, weth, pump];
    let mut accounts = fetch_accounts(&[
        TESSERA_PROGRAMDATA,
        TESSERA_GLOBAL_STATE,
        TESSERA_SOL_USDC_MARKET,
        TESSERA_SOL_VAULT,
        TESSERA_USDC_VAULT,
        WSOL_MINT,
        USDC_MINT,
        TESSERA_V11_SENTINEL,
        TESSERA_V11_CONFIG,
        TESSERA_V11_MARKET_RECORD,
    ])
    .await;
    let programdata = accounts.remove(0);
    assert_eq!(programdata.data.len(), 576_977, "ProgramData size changed");
    assert_eq!(
        u64::from_le_bytes(programdata.data[4..12].try_into().unwrap()),
        CURRENT_DEPLOY_SLOT,
        "Tessera was redeployed; revalidate the raw layout"
    );
    let elf = programdata.data[45..].to_vec();
    assert_eq!(
        hex::encode(Sha256::digest(&elf)),
        CURRENT_ELF_SHA256,
        "Tessera ELF changed without a ProgramData address change"
    );

    TesseraFork {
        elf,
        global_state: accounts.remove(0),
        market: accounts.remove(0),
        base_vault: accounts.remove(0),
        quote_vault: accounts.remove(0),
        base_mint: accounts.remove(0),
        quote_mint: accounts.remove(0),
        sentinel: accounts.remove(0),
        config: accounts.remove(0),
        market_record: accounts.remove(0),
        jupiter_markets,
    }
}

async fn fetch_jupiter_market(spec: JupiterMarketSpec) -> JupiterMarketFork {
    let mut accounts = fetch_accounts(&[
        spec.address,
        spec.base_vault,
        spec.quote_vault,
        spec.base_mint,
        spec.quote_mint,
    ])
    .await;
    JupiterMarketFork {
        spec,
        market: accounts.remove(0),
        base_vault: accounts.remove(0),
        quote_vault: accounts.remove(0),
        base_mint: accounts.remove(0),
        quote_mint: accounts.remove(0),
    }
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

fn token_owner(data: &[u8]) -> Pubkey {
    Pubkey::new_from_array(data[32..64].try_into().expect("token owner"))
}

fn tessera_run(
    fork: &TesseraFork,
    amount_in: u64,
    direction: u8,
    sentinel_signer: bool,
    global_writable: bool,
    mutate: impl FnOnce(&mut Vec<u8>),
) -> Result<u64, String> {
    use litesvm::LiteSVM;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    let program_id = Pubkey::from_str_const(TESSERA_PROGRAM);
    let global_state_key = Pubkey::from_str_const(TESSERA_GLOBAL_STATE);
    let market_key = Pubkey::from_str_const(TESSERA_SOL_USDC_MARKET);
    let base_vault_key = Pubkey::from_str_const(TESSERA_SOL_VAULT);
    let quote_vault_key = Pubkey::from_str_const(TESSERA_USDC_VAULT);
    let base_mint_key = Pubkey::from_str_const(WSOL_MINT);
    let quote_mint_key = Pubkey::from_str_const(USDC_MINT);
    let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);
    let sentinel_key = Pubkey::from_str_const(TESSERA_V11_SENTINEL);
    let config_key = Pubkey::from_str_const(TESSERA_V11_CONFIG);
    let market_record_key = Pubkey::from_str_const(TESSERA_V11_MARKET_RECORD);
    let mut market = fork.market.data.clone();
    let market_slot = u64::from_le_bytes(market[120..128].try_into().unwrap());
    mutate(&mut market);

    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(program_id, &fork.elf)
        .map_err(|error| format!("add_program: {error:?}"))?;
    svm.add_builtin(
        Pubkey::from_str_const(DFLOW_PROGRAM),
        TesseraCpiWrapper::register,
    );
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = market_slot + 1;
    clock.unix_timestamp = 1_787_551_143;
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
    svm.set_account(global_state_key, fork.global_state.clone())
        .map_err(|error| format!("set global state: {error:?}"))?;
    let mut market_account = fork.market.clone();
    market_account.data = market;
    svm.set_account(market_key, market_account)
        .map_err(|error| format!("set market: {error:?}"))?;
    svm.set_account(base_vault_key, fork.base_vault.clone())
        .map_err(|error| format!("set base vault: {error:?}"))?;
    svm.set_account(quote_vault_key, fork.quote_vault.clone())
        .map_err(|error| format!("set quote vault: {error:?}"))?;
    svm.set_account(base_mint_key, fork.base_mint.clone())
        .map_err(|error| format!("set base mint: {error:?}"))?;
    svm.set_account(quote_mint_key, fork.quote_mint.clone())
        .map_err(|error| format!("set quote mint: {error:?}"))?;
    svm.set_account(sentinel_key, fork.sentinel.clone())
        .map_err(|error| format!("set sentinel: {error:?}"))?;
    svm.set_account(config_key, fork.config.clone())
        .map_err(|error| format!("set config: {error:?}"))?;
    svm.set_account(market_record_key, fork.market_record.clone())
        .map_err(|error| format!("set market record: {error:?}"))?;

    let taker = Keypair::new();
    svm.airdrop(&taker.pubkey(), 10_000_000_000)
        .map_err(|error| format!("airdrop: {error:?}"))?;
    let source_key = Pubkey::new_unique();
    let destination_key = Pubkey::new_unique();
    let (source_mint, destination_mint) = if direction == 1 {
        (base_mint_key, quote_mint_key)
    } else {
        (quote_mint_key, base_mint_key)
    };
    let (user_base_key, user_quote_key) = if direction == 1 {
        (source_key, destination_key)
    } else {
        (destination_key, source_key)
    };
    let user_account = |mint: &Pubkey, amount: u64| {
        let is_native = mint == &base_mint_key;
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
    svm.set_account(source_key, user_account(&source_mint, amount_in))
        .map_err(|error| format!("set source: {error:?}"))?;
    svm.set_account(destination_key, user_account(&destination_mint, 0))
        .map_err(|error| format!("set destination: {error:?}"))?;

    let mut data = vec![0x11, direction];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.push(0);
    let mut budget = vec![2u8];
    budget.extend_from_slice(&1_400_000u32.to_le_bytes());
    let global_state_meta = if global_writable {
        AccountMeta::new(global_state_key, false)
    } else {
        AccountMeta::new_readonly(global_state_key, false)
    };
    let instructions = vec![
        Instruction {
            program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
            accounts: vec![],
            data: budget,
        },
        Instruction {
            program_id: Pubkey::from_str_const(DFLOW_PROGRAM),
            accounts: vec![
                AccountMeta::new_readonly(program_id, false),
                global_state_meta,
                AccountMeta::new(market_key, false),
                AccountMeta::new(taker.pubkey(), true),
                AccountMeta::new(base_vault_key, false),
                AccountMeta::new(quote_vault_key, false),
                AccountMeta::new(user_base_key, false),
                AccountMeta::new(user_quote_key, false),
                AccountMeta::new_readonly(base_mint_key, false),
                AccountMeta::new_readonly(quote_mint_key, false),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(sentinel_key, sentinel_signer),
                AccountMeta::new_readonly(config_key, false),
                AccountMeta::new_readonly(market_record_key, false),
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
    Ok(token_amount(
        &svm.get_account(&destination_key)
            .expect("destination account")
            .data,
    ))
}

fn tessera_run_jupiter(
    fork: &TesseraFork,
    market_fork: &JupiterMarketFork,
    amount_in: u64,
    direction: u8,
    mutate: impl FnOnce(&mut Vec<u8>),
) -> Result<u64, String> {
    use litesvm::LiteSVM;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    let program_id = Pubkey::from_str_const(TESSERA_PROGRAM);
    let global_state_key = Pubkey::from_str_const(TESSERA_GLOBAL_STATE);
    let market_key = Pubkey::from_str_const(market_fork.spec.address);
    let base_vault_key = Pubkey::from_str_const(market_fork.spec.base_vault);
    let quote_vault_key = Pubkey::from_str_const(market_fork.spec.quote_vault);
    let base_mint_key = Pubkey::from_str_const(market_fork.spec.base_mint);
    let quote_mint_key = Pubkey::from_str_const(market_fork.spec.quote_mint);
    let base_token_program = market_fork.base_mint.owner;
    let quote_token_program = market_fork.quote_mint.owner;
    let instructions_sysvar = Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111");
    let mut market = market_fork.market.data.clone();
    let market_slot = read_u64(&market, 120);
    mutate(&mut market);

    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(program_id, &fork.elf)
        .map_err(|error| format!("add_program: {error:?}"))?;
    svm.add_builtin(
        Pubkey::from_str_const(JUPITER_PROGRAM),
        TesseraCpiWrapper::register,
    );
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = market_slot + 1;
    clock.unix_timestamp = 1_787_662_925;
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
    svm.set_account(global_state_key, fork.global_state.clone())
        .map_err(|error| format!("set global state: {error:?}"))?;
    let mut market_account = market_fork.market.clone();
    market_account.data = market;
    svm.set_account(market_key, market_account)
        .map_err(|error| format!("set market: {error:?}"))?;
    svm.set_account(base_vault_key, market_fork.base_vault.clone())
        .map_err(|error| format!("set base vault: {error:?}"))?;
    svm.set_account(quote_vault_key, market_fork.quote_vault.clone())
        .map_err(|error| format!("set quote vault: {error:?}"))?;
    svm.set_account(base_mint_key, market_fork.base_mint.clone())
        .map_err(|error| format!("set base mint: {error:?}"))?;
    svm.set_account(quote_mint_key, market_fork.quote_mint.clone())
        .map_err(|error| format!("set quote mint: {error:?}"))?;

    let taker = Keypair::new();
    svm.airdrop(&taker.pubkey(), 10_000_000_000)
        .map_err(|error| format!("airdrop: {error:?}"))?;
    let source_key = Pubkey::new_unique();
    let destination_key = Pubkey::new_unique();
    let (source_mint, source_program, destination_mint, destination_program) = if direction == 1 {
        (
            base_mint_key,
            base_token_program,
            quote_mint_key,
            quote_token_program,
        )
    } else {
        (
            quote_mint_key,
            quote_token_program,
            base_mint_key,
            base_token_program,
        )
    };
    let (user_base_key, user_quote_key) = if direction == 1 {
        (source_key, destination_key)
    } else {
        (destination_key, source_key)
    };
    let user_account = |mint: &Pubkey, token_program: Pubkey, amount: u64| {
        let is_native = mint == &Pubkey::from_str_const(WSOL_MINT);
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
    svm.set_account(
        source_key,
        user_account(&source_mint, source_program, amount_in),
    )
    .map_err(|error| format!("set source: {error:?}"))?;
    svm.set_account(
        destination_key,
        user_account(&destination_mint, destination_program, 0),
    )
    .map_err(|error| format!("set destination: {error:?}"))?;

    let mut data = vec![0x10, direction];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    let instruction = Instruction {
        program_id: Pubkey::from_str_const(JUPITER_PROGRAM),
        accounts: vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(global_state_key, false),
            AccountMeta::new(market_key, false),
            AccountMeta::new(taker.pubkey(), true),
            AccountMeta::new(base_vault_key, false),
            AccountMeta::new(quote_vault_key, false),
            AccountMeta::new(user_base_key, false),
            AccountMeta::new(user_quote_key, false),
            AccountMeta::new_readonly(base_mint_key, false),
            AccountMeta::new_readonly(quote_mint_key, false),
            AccountMeta::new_readonly(base_token_program, false),
            AccountMeta::new_readonly(quote_token_program, false),
            AccountMeta::new_readonly(instructions_sysvar, false),
        ],
        data,
    };
    let mut message = solana_message::Message::new(&[instruction], Some(&taker.pubkey()));
    message.recent_blockhash = svm.latest_blockhash();
    let signature_count = message.header.num_required_signatures as usize;
    let mut transaction = Transaction::new_unsigned(message);
    transaction.signatures = vec![solana_signature::Signature::default(); signature_count];
    transaction.signatures[0] = taker.sign_message(&transaction.message.serialize());

    svm.send_transaction(transaction)
        .map_err(|error| format!("{error:?}"))?;
    Ok(token_amount(
        &svm.get_account(&destination_key)
            .expect("destination account")
            .data,
    ))
}

fn write_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("u64 field"))
}

fn expected_first_level_output(market: &[u8], amount_in: u64, direction: u8) -> u64 {
    let (price_offset, factor_offset) = match direction {
        0 => (144, 648),
        1 => (128, 168),
        _ => panic!("unsupported Tessera direction {direction}"),
    };
    let output = u128::from(amount_in)
        .checked_mul(u128::from(read_u64(market, price_offset)))
        .and_then(|value| value.checked_mul(u128::from(read_u64(market, factor_offset))))
        .expect("Tessera first-level quote multiplication")
        / 1_000_000_000_000_000_000_000_u128;
    u64::try_from(output).expect("Tessera first-level quote fits u64")
}

/// Scales one side of a ladder the way a caller composing the raw template would.
///
/// `first_offset` is the field's offset in level 0; records are 24 bytes apart. Returns the
/// template values for both directions, only one of which is actually scaled.
fn scale_ladder(
    market: &[u8],
    field: &str,
    sell_bps: u16,
    buy_bps: u16,
) -> HashMap<String, serde_json::Value> {
    const SELL_AMOUNT: usize = 160;
    const BUY_AMOUNT: usize = 640;
    const FACTOR_IN_RECORD: usize = 8;

    let base = if field == "factor" {
        FACTOR_IN_RECORD
    } else {
        0
    };
    let mut values = HashMap::with_capacity(LADDER_LEVELS * 2);
    for (side, first_offset, bps) in [
        ("sell_levels", SELL_AMOUNT + base, sell_bps),
        ("buy_levels", BUY_AMOUNT + base, buy_bps),
    ] {
        for level in 0..LADDER_LEVELS {
            let live = read_u64(market, first_offset + level * LADDER_RECORD_SIZE);
            let scaled = (u128::from(live) * u128::from(bps) / 10_000) as u64;
            assert!(
                live == 0 || scaled > 0,
                "{side}.{level}.{field} rounds a live nonzero value to zero at {bps} bps"
            );
            values.insert(
                format!("{side}.{level}.{field}"),
                serde_json::json!(scaled.to_string()),
            );
        }
    }
    values
}

const LADDER_LEVELS: usize = 20;
const LADDER_RECORD_SIZE: usize = 24;

fn apply_template(
    data: &mut Vec<u8>,
    template_id: &str,
    values: HashMap<String, serde_json::Value>,
    target_slot: u64,
) {
    let registry = TemplateRegistry::new();
    let template = registry.get(template_id).expect("Tessera template");
    *data = template
        .raw_layout
        .as_ref()
        .expect("Tessera raw layout")
        .materialize(data, &template.properties, &values, target_slot)
        .unwrap_or_else(|error| panic!("{template_id} did not materialize: {error}"));
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

#[tokio::test]
async fn tessera_templates_guard_market_and_preserve_unwritten_bytes() {
    let fork = tessera_fork().await;
    let program_id = Pubkey::from_str_const(TESSERA_PROGRAM);
    assert_eq!(fork.market.owner, program_id);
    assert_eq!(fork.market.data.len(), 1264);
    let vault_authority = token_owner(&fork.base_vault.data);
    assert_eq!(token_owner(&fork.quote_vault.data), vault_authority);
    for market in &fork.jupiter_markets {
        assert_eq!(token_owner(&market.base_vault.data), vault_authority);
        assert_eq!(token_owner(&market.quote_vault.data), vault_authority);
    }
    eprintln!("Tessera shared vault authority: {vault_authority}");

    let registry = TemplateRegistry::new();
    let fair_value = registry.get("tessera-fair-value").expect("fair value");
    let layout = fair_value.raw_layout.as_ref().expect("raw layout");
    assert!(layout.guard(&fork.market.data).is_ok());
    for market in &fork.jupiter_markets {
        assert_eq!(market.market.owner, program_id);
        assert_eq!(market.market.data.len(), 1264);
        assert!(layout.guard(&market.market.data).is_ok());
    }
    let mut wrong_layout_tag = fork.market.data.clone();
    wrong_layout_tag[96] ^= 1;
    assert!(layout.guard(&wrong_layout_tag).is_err());
    assert!(layout.guard(&fork.market.data[..1263]).is_err());

    let price_values = HashMap::from([
        (
            "quote_atoms_per_base_atom_x1e15".to_string(),
            serde_json::json!(100_000_000_000_000u64),
        ),
        (
            "base_atoms_per_quote_atom_x1e15".to_string(),
            serde_json::json!(10_000_000_000_000_000u64),
        ),
    ]);
    let repriced = layout
        .materialize(&fork.market.data, &fair_value.properties, &price_values, 0)
        .expect("fair value materializes");
    assert_eq!(
        u64::from_le_bytes(repriced[128..136].try_into().unwrap()),
        100_000_000_000_000
    );
    assert_eq!(
        u64::from_le_bytes(repriced[144..152].try_into().unwrap()),
        10_000_000_000_000_000
    );
    assert_only_ranges_changed(&fork.market.data, &repriced, &[(128, 136), (144, 152)]);

    let template = registry.get("tessera-depth").expect("ladder template");
    let values: HashMap<String, serde_json::Value> = template
        .properties
        .iter()
        .map(|property| {
            let offset = property.offset.expect("raw property offset");
            let current =
                u64::from_le_bytes(fork.market.data[offset..offset + 8].try_into().unwrap());
            (property.path.clone(), serde_json::json!(current / 2))
        })
        .collect();
    let forged = template
        .raw_layout
        .as_ref()
        .expect("raw layout")
        .materialize(&fork.market.data, &template.properties, &values, 0)
        .expect("depth materializes");
    let ranges: Vec<(usize, usize)> = template
        .properties
        .iter()
        .map(|property| {
            let offset = property.offset.expect("raw property offset");
            let expected = values[&property.path].as_u64().expect("u64 value");
            assert_eq!(
                u64::from_le_bytes(forged[offset..offset + 8].try_into().unwrap()),
                expected
            );
            (offset, offset + 8)
        })
        .collect();
    assert_only_ranges_changed(&fork.market.data, &forged, &ranges);

    let freshness = registry.get("tessera-freshness").expect("freshness");
    let refreshed = freshness
        .raw_layout
        .as_ref()
        .expect("raw layout")
        .materialize(
            &fork.market.data,
            &freshness.properties,
            &HashMap::from([("last_update_slot".to_string(), serde_json::json!(0))]),
            987_654,
        )
        .expect("freshness materializes");
    assert_eq!(
        u64::from_le_bytes(refreshed[120..128].try_into().unwrap()),
        987_654
    );
    assert_only_ranges_changed(&fork.market.data, &refreshed, &[(120, 128)]);
}

#[tokio::test]
async fn tessera_builder_scenario_materializes_atomically_and_keeps_quotes_fresh() {
    const BASE_SLOT: u64 = 1_000_000;

    let fork = tessera_fork().await;
    let market_key = Pubkey::from_str_const(TESSERA_SOL_USDC_MARKET);
    let market =
        TesseraMarket::validate(market_key, &fork.market, &fork.base_mint, &fork.quote_mint)
            .expect("validate WSOL/USDC market");
    let preparation = build_tessera_fair_value_scenario(&market, "100.25")
        .expect("build Tessera fair-value scenario");
    let original = fork.market.data.clone();
    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    svm.inner
        .set_account(market_key, fork.market)
        .expect("seed Tessera market");
    svm.register_scenario(preparation.scenario, Some(BASE_SLOT))
        .expect("register Tessera scenario");

    svm.materialize_overrides_for_slot(&None, BASE_SLOT)
        .await
        .expect("materialize Tessera scenario");
    let materialized = svm
        .inner
        .get_account(&market_key)
        .expect("get Tessera market")
        .expect("Tessera market present")
        .data;
    assert_eq!(read_u64(&materialized, 120), BASE_SLOT);
    assert_eq!(
        read_u64(&materialized, 128),
        preparation.quote_atoms_per_base_atom_x1e15
    );
    assert_eq!(
        read_u64(&materialized, 144),
        preparation.base_atoms_per_quote_atom_x1e15
    );
    assert_only_ranges_changed(
        &original,
        &materialized,
        &[(120, 128), (128, 136), (144, 152)],
    );

    svm.materialize_overrides_for_slot(&None, BASE_SLOT + 1)
        .await
        .expect("materialize persistent Tessera freshness");
    let next_slot = svm
        .inner
        .get_account(&market_key)
        .expect("get Tessera market")
        .expect("Tessera market present")
        .data;
    assert_eq!(read_u64(&next_slot, 120), BASE_SLOT + 1);
    assert_eq!(
        read_u64(&next_slot, 128),
        preparation.quote_atoms_per_base_atom_x1e15
    );
    assert_eq!(
        read_u64(&next_slot, 144),
        preparation.base_atoms_per_quote_atom_x1e15
    );
    assert_only_ranges_changed(&materialized, &next_slot, &[(120, 128)]);
}

#[tokio::test]
async fn tessera_stale_quote_template_lands_every_configured_rejection_boundary() {
    let fork = tessera_fork().await;
    let base_slot = read_u64(&fork.market.data, 120) + 1;
    let amount_in = 238_781_608;
    let baseline = tessera_run(&fork, amount_in, 1, true, false, |_| {})
        .expect("fresh quote must fill the control");
    assert!(baseline > 0);

    // One template covers every limit because the supplied value is the lead. Null takes the
    // template's own -20, which is what the default market needs.
    for (lead, age_slots) in [
        (serde_json::Value::Null, 20),
        (serde_json::json!(-20), 20),
        (serde_json::json!(-25), 25),
        (serde_json::json!(-55), 55),
    ] {
        let mut configured = fork.market.clone();
        write_u64(&mut configured.data, 88, age_slots);
        let original = configured.data.clone();

        let mut staged = configured.data.clone();
        apply_template(
            &mut staged,
            "tessera-stale-quote",
            HashMap::from([("last_update_slot".to_string(), lead.clone())]),
            base_slot,
        );
        assert_eq!(
            read_u64(&staged, 120),
            base_slot - age_slots,
            "lead {lead} must write the materialization slot minus {age_slots}"
        );
        assert_only_ranges_changed(&original, &staged, &[(88, 96), (120, 128)]);

        let stale = tessera_run(&fork, amount_in, 1, true, false, |market| {
            *market = staged;
        })
        .expect_err("a quote at the configured rejection age must be rejected");
        assert!(stale.contains("Custom(65535)"), "lead {lead}: {stale}");
    }

    // One slot younger than the boundary still fills, which is what makes the boundary a boundary.
    let mut fresh_enough = fork.market.data.clone();
    apply_template(
        &mut fresh_enough,
        "tessera-stale-quote",
        HashMap::from([("last_update_slot".to_string(), serde_json::json!(-19))]),
        base_slot,
    );
    let accepted = tessera_run(&fork, amount_in, 1, true, false, |market| {
        *market = fresh_enough;
    })
    .expect("age 19 must still fill");
    assert!(accepted > 0);
}

#[tokio::test]
async fn tessera_current_layout_controls_price_depth_and_freshness() {
    let fork = tessera_fork().await;
    assert_eq!(fork.market.data.len(), 1264, "market layout size changed");
    assert_eq!(
        &fork.market.data[24..56],
        Pubkey::from_str_const(WSOL_MINT).as_ref()
    );
    assert_eq!(
        &fork.market.data[56..88],
        Pubkey::from_str_const(USDC_MINT).as_ref()
    );

    let amount_in = 238_781_608;
    let baseline = tessera_run(&fork, amount_in, 1, true, false, |_| {}).expect("baseline sell");
    let inverse_only = tessera_run(&fork, amount_in, 1, true, false, |market| {
        let inverse = u64::from_le_bytes(market[144..152].try_into().unwrap());
        write_u64(market, 144, inverse / 2);
    })
    .expect("sell with buy-side-only mutation");
    let doubled = tessera_run(&fork, amount_in, 1, true, false, |market| {
        let price = u64::from_le_bytes(market[128..136].try_into().unwrap());
        let inverse = u64::from_le_bytes(market[144..152].try_into().unwrap());
        apply_template(
            market,
            "tessera-fair-value",
            HashMap::from([
                (
                    "quote_atoms_per_base_atom_x1e15".to_string(),
                    serde_json::json!(price * 2),
                ),
                (
                    "base_atoms_per_quote_atom_x1e15".to_string(),
                    serde_json::json!(inverse / 2),
                ),
            ]),
            0,
        );
    })
    .expect("doubled-price swap");
    let buy_amount_in = 22_000_000;
    let baseline_buy =
        tessera_run(&fork, buy_amount_in, 0, true, false, |_| {}).expect("baseline buy");
    let direct_only_buy = tessera_run(&fork, buy_amount_in, 0, true, false, |market| {
        let price = u64::from_le_bytes(market[128..136].try_into().unwrap());
        write_u64(market, 128, price * 2);
    })
    .expect("buy with sell-side-only mutation");
    let doubled_price_buy = tessera_run(&fork, buy_amount_in, 0, true, false, |market| {
        let price = u64::from_le_bytes(market[128..136].try_into().unwrap());
        let inverse = u64::from_le_bytes(market[144..152].try_into().unwrap());
        apply_template(
            market,
            "tessera-fair-value",
            HashMap::from([
                (
                    "quote_atoms_per_base_atom_x1e15".to_string(),
                    serde_json::json!(price * 2),
                ),
                (
                    "base_atoms_per_quote_atom_x1e15".to_string(),
                    serde_json::json!(inverse / 2),
                ),
            ]),
            0,
        );
    })
    .expect("doubled-price buy");
    let large_sell = 30_000_000_000;
    let thin_sell_values = scale_ladder(&fork.market.data, "amount", 1_000, 10_000);
    let thin_buy_values = scale_ladder(&fork.market.data, "amount", 10_000, 1_000);
    let thin_sell_curve_values = scale_ladder(&fork.market.data, "factor", 5_000, 10_000);
    let thin_buy_curve_values = scale_ladder(&fork.market.data, "factor", 10_000, 5_000);
    let half_sell_curve = tessera_run(&fork, amount_in, 1, true, false, |market| {
        apply_template(market, "tessera-curve", thin_sell_curve_values.clone(), 0);
    })
    .expect("sell with half sell factors");
    let inactive_sell_curve = tessera_run(&fork, amount_in, 1, true, false, |market| {
        apply_template(market, "tessera-curve", thin_buy_curve_values.clone(), 0);
    })
    .expect("sell with half buy factors");
    let half_buy_curve = tessera_run(&fork, buy_amount_in, 0, true, false, |market| {
        apply_template(market, "tessera-curve", thin_buy_curve_values.clone(), 0);
    })
    .expect("buy with half buy factors");
    let inactive_buy_curve = tessera_run(&fork, buy_amount_in, 0, true, false, |market| {
        apply_template(market, "tessera-curve", thin_sell_curve_values.clone(), 0);
    })
    .expect("buy with half sell factors");
    let large_sell_baseline =
        tessera_run(&fork, large_sell, 1, true, false, |_| {}).expect("large sell");
    let thin_sell = tessera_run(&fork, large_sell, 1, true, false, |market| {
        apply_template(market, "tessera-depth", thin_sell_values.clone(), 0);
    })
    .expect("large sell with thin sell ladder");
    let inactive_sell_depth = tessera_run(&fork, large_sell, 1, true, false, |market| {
        apply_template(market, "tessera-depth", thin_buy_values.clone(), 0);
    })
    .expect("large sell with buy-side depth mutation");
    let large_buy = 3_000_000_000;
    let large_buy_baseline =
        tessera_run(&fork, large_buy, 0, true, false, |_| {}).expect("large buy");
    let thin_buy = tessera_run(&fork, large_buy, 0, true, false, |market| {
        apply_template(market, "tessera-depth", thin_buy_values.clone(), 0);
    })
    .expect("large buy with thin buy ladder");
    let inactive_buy_depth = tessera_run(&fork, large_buy, 0, true, false, |market| {
        apply_template(market, "tessera-depth", thin_sell_values.clone(), 0);
    })
    .expect("large buy with sell-side depth mutation");
    let market_slot = u64::from_le_bytes(fork.market.data[120..128].try_into().unwrap());
    let clock_slot = market_slot + 1;
    let fresh_at_boundary = tessera_run(&fork, amount_in, 1, true, false, |market| {
        apply_template(
            market,
            "tessera-freshness",
            HashMap::from([("last_update_slot".to_string(), serde_json::json!(0))]),
            clock_slot.saturating_sub(MAX_PRICE_AGE_SLOTS),
        );
    });
    let stale_after_boundary = tessera_run(&fork, amount_in, 1, true, false, |market| {
        apply_template(
            market,
            "tessera-freshness",
            HashMap::from([("last_update_slot".to_string(), serde_json::json!(0))]),
            clock_slot.saturating_sub(MAX_PRICE_AGE_SLOTS + 1),
        );
    });
    let configured_freshness_boundary = 5;
    let fresh_at_configured_boundary = tessera_run(&fork, amount_in, 1, true, false, |market| {
        write_u64(market, 88, configured_freshness_boundary);
        apply_template(
            market,
            "tessera-freshness",
            HashMap::from([("last_update_slot".to_string(), serde_json::json!(0))]),
            clock_slot.saturating_sub(configured_freshness_boundary - 1),
        );
    });
    let stale_at_configured_boundary = tessera_run(&fork, amount_in, 1, true, false, |market| {
        write_u64(market, 88, configured_freshness_boundary);
        apply_template(
            market,
            "tessera-freshness",
            HashMap::from([("last_update_slot".to_string(), serde_json::json!(0))]),
            clock_slot.saturating_sub(configured_freshness_boundary),
        );
    });
    let unsigned_sentinel = tessera_run(&fork, amount_in, 1, false, false, |_| {})
        .expect_err("unsigned DFlow sentinel must be rejected");
    let writable_global = tessera_run(&fork, amount_in, 1, true, true, |_| {})
        .expect_err("writable global state must be rejected");

    let expected_first_level_sell_output =
        expected_first_level_output(&fork.market.data, amount_in, 1);
    let expected_first_level_buy_output =
        expected_first_level_output(&fork.market.data, buy_amount_in, 0);

    eprintln!(
        "Tessera sell={baseline}, expected_sell={expected_first_level_sell_output}, inverse_only={inverse_only}, doubled={doubled}, buy={baseline_buy}, expected_buy={expected_first_level_buy_output}, direct_only_buy={direct_only_buy}, doubled_price_buy={doubled_price_buy}, large_sell={large_sell_baseline}, thin_sell={thin_sell}, large_buy={large_buy_baseline}, thin_buy={thin_buy}, freshness_boundary={MAX_PRICE_AGE_SLOTS}, fresh={fresh_at_boundary:?}, stale={stale_after_boundary:?}"
    );
    assert!(
        baseline > 0,
        "the current deployed program must fill the control"
    );
    assert_eq!(baseline, expected_first_level_sell_output);
    assert_eq!(baseline_buy, expected_first_level_buy_output);
    assert!(
        doubled > baseline * 19 / 10 && doubled < baseline * 21 / 10,
        "atomic price override should approximately double the quote"
    );
    assert_eq!(
        inverse_only, baseline,
        "the buy-side inverse must not affect a base-to-quote sell"
    );
    assert!(
        doubled_price_buy > baseline_buy * 4 / 10 && doubled_price_buy < baseline_buy * 6 / 10,
        "doubling quote/base price must approximately halve base bought with quote"
    );
    assert_eq!(
        direct_only_buy, baseline_buy,
        "the sell-side direct price must not affect a quote-to-base buy"
    );
    assert!(unsigned_sentinel.contains("Custom(0)"));
    assert!(writable_global.contains("Custom(1)"));
    assert!(thin_sell < large_sell_baseline);
    assert_eq!(inactive_sell_depth, large_sell_baseline);
    assert!(thin_buy < large_buy_baseline);
    assert_eq!(inactive_buy_depth, large_buy_baseline);
    assert!(half_sell_curve > baseline * 49 / 100 && half_sell_curve < baseline * 51 / 100);
    assert_eq!(inactive_sell_curve, baseline);
    assert!(half_buy_curve > baseline_buy * 49 / 100 && half_buy_curve < baseline_buy * 51 / 100);
    assert_eq!(inactive_buy_curve, baseline_buy);
    assert!(fresh_at_boundary.is_ok());
    assert!(
        stale_after_boundary
            .expect_err("age 20 must be rejected")
            .contains("Custom(65535)")
    );
    assert!(fresh_at_configured_boundary.is_ok());
    assert!(
        stale_at_configured_boundary
            .expect_err("configured freshness boundary must reject")
            .contains("Custom(65535)")
    );
}

#[tokio::test]
async fn tessera_cbb_market_proves_generic_price_and_curve_layout() {
    let fork = tessera_fork().await;
    let cbb = &fork.jupiter_markets[0];
    let market_key = Pubkey::from_str_const(TESSERA_CBB_USDC_MARKET);
    let market = TesseraMarket::validate(market_key, &cbb.market, &cbb.base_mint, &cbb.quote_mint)
        .expect("validate CBB/USDC market");
    assert_eq!(market.base_mint, Pubkey::from_str_const(CBB_MINT));
    assert_eq!(market.quote_mint, Pubkey::from_str_const(USDC_MINT));
    assert_eq!(market.base_decimals, 8);
    assert_eq!(market.quote_decimals, 6);

    let preparation = build_tessera_fair_value_scenario(&market, "78.8477010015472512")
        .expect("build decimal-aware CBB fair value");
    assert_eq!(preparation.market, market_key);
    assert_eq!(
        preparation.quote_atoms_per_base_atom_x1e15,
        788_477_010_015_472
    );

    let amount_in = cbb.spec.amount_in;
    let active_curve_values = scale_ladder(&cbb.market.data, "factor", 5_000, 10_000);
    let inactive_curve_values = scale_ladder(&cbb.market.data, "factor", 10_000, 5_000);
    let baseline =
        tessera_run_jupiter(&fork, cbb, amount_in, 1, |_| {}).expect("CBB baseline sell");
    let doubled = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        let direct = read_u64(market, 128);
        let inverse = read_u64(market, 144);
        apply_template(
            market,
            "tessera-fair-value",
            HashMap::from([
                (
                    "quote_atoms_per_base_atom_x1e15".to_string(),
                    serde_json::json!((direct * 2).to_string()),
                ),
                (
                    "base_atoms_per_quote_atom_x1e15".to_string(),
                    serde_json::json!((inverse / 2).to_string()),
                ),
            ]),
            0,
        );
    })
    .expect("CBB doubled-price sell");
    let factors_half = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        apply_template(market, "tessera-curve", active_curve_values.clone(), 0);
    })
    .expect("CBB half-factor sell");
    let inactive_factors_half = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        apply_template(market, "tessera-curve", inactive_curve_values.clone(), 0);
    })
    .expect("CBB inactive-factor control");
    let trailing_candidates_double = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        for offset in [1140usize, 1152, 1164, 1176, 1188] {
            let value = u32::from_le_bytes(market[offset..offset + 4].try_into().unwrap());
            market[offset..offset + 4].copy_from_slice(&(value * 2).to_le_bytes());
        }
    })
    .expect("CBB doubled trailing-candidate sell");
    let trailing_candidates_lower = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        for offset in [1140usize, 1152, 1164, 1176, 1188] {
            market[offset..offset + 4].copy_from_slice(&999_999u32.to_le_bytes());
        }
    });
    let invalid_single_factor = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        write_u64(market, 168, 500_000);
    })
    .expect_err("a single unordered factor must be rejected");
    let disabled_first_level = tessera_run_jupiter(&fork, cbb, amount_in, 1, |market| {
        market[176] = 0;
    })
    .expect_err("disabling the required first level must reject the quote");

    assert!(baseline > 0);
    assert_eq!(
        baseline,
        expected_first_level_output(&cbb.market.data, amount_in, 1)
    );
    assert!(doubled > baseline * 19 / 10 && doubled < baseline * 21 / 10);
    assert!(factors_half > baseline * 49 / 100 && factors_half < baseline * 51 / 100);
    assert_eq!(inactive_factors_half, baseline);
    assert_eq!(trailing_candidates_double, baseline);
    assert_eq!(trailing_candidates_lower, Ok(baseline));
    assert!(invalid_single_factor.contains("Custom(8)"));
    assert!(disabled_first_level.contains("Custom(65535)"));
}

#[tokio::test]
async fn tessera_halt_template_rejects_both_quote_directions() {
    let fork = tessera_fork().await;
    let cbb = &fork.jupiter_markets[0];
    let apply_halt = |market: &mut Vec<u8>| {
        apply_template(
            market,
            "tessera-halt",
            HashMap::from([
                ("sell_level_0_enabled".to_string(), serde_json::json!(0)),
                ("buy_level_0_enabled".to_string(), serde_json::json!(0)),
            ]),
            0,
        );
    };

    let sell_amount = cbb.spec.amount_in;
    let buy_amount = 4_000_000;
    assert!(tessera_run_jupiter(&fork, cbb, sell_amount, 1, |_| {}).is_ok());
    assert!(tessera_run_jupiter(&fork, cbb, buy_amount, 0, |_| {}).is_ok());

    let halted_sell = tessera_run_jupiter(&fork, cbb, sell_amount, 1, apply_halt)
        .expect_err("halted sell direction must fail");
    let halted_buy = tessera_run_jupiter(&fork, cbb, buy_amount, 0, apply_halt)
        .expect_err("halted buy direction must fail");
    assert!(halted_sell.contains("Custom(65535)"));
    assert!(halted_buy.contains("Custom(65535)"));
}

#[tokio::test]
async fn tessera_four_additional_markets_prove_price_and_curve_directions() {
    let fork = tessera_fork().await;
    for market_fork in &fork.jupiter_markets[1..] {
        let market_key = Pubkey::from_str_const(market_fork.spec.address);
        // Every one of these markets must clear the same owner, size and layout-tag guard.
        TesseraMarket::validate(
            market_key,
            &market_fork.market,
            &market_fork.base_mint,
            &market_fork.quote_mint,
        )
        .unwrap_or_else(|error| panic!("{} validation failed: {error}", market_fork.spec.address));

        let direction = market_fork.spec.direction;
        let amount_in = market_fork.spec.amount_in;
        let baseline = tessera_run_jupiter(&fork, market_fork, amount_in, direction, |_| {})
            .unwrap_or_else(|error| {
                panic!("{} baseline failed: {error}", market_fork.spec.address)
            });
        let repriced = tessera_run_jupiter(&fork, market_fork, amount_in, direction, |market| {
            let direct = read_u64(market, 128);
            let inverse = read_u64(market, 144);
            write_u64(market, 128, direct * 2);
            write_u64(market, 144, inverse / 2);
        })
        .unwrap_or_else(|error| {
            panic!("{} repriced swap failed: {error}", market_fork.spec.address)
        });
        let (active_sell_bps, active_buy_bps) = if direction == 1 {
            (5_000, 10_000)
        } else {
            (10_000, 5_000)
        };
        let active_curve_values = scale_ladder(
            &market_fork.market.data,
            "factor",
            active_sell_bps,
            active_buy_bps,
        );
        let inactive_curve_values = scale_ladder(
            &market_fork.market.data,
            "factor",
            active_buy_bps,
            active_sell_bps,
        );
        let active_factors_half =
            tessera_run_jupiter(&fork, market_fork, amount_in, direction, |market| {
                apply_template(market, "tessera-curve", active_curve_values.clone(), 0)
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{} active-factor swap failed: {error}",
                    market_fork.spec.address
                )
            });
        let inactive_factors_half =
            tessera_run_jupiter(&fork, market_fork, amount_in, direction, |market| {
                apply_template(market, "tessera-curve", inactive_curve_values.clone(), 0)
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{} inactive-factor swap failed: {error}",
                    market_fork.spec.address
                )
            });

        eprintln!(
            "Tessera market={} direction={} baseline={} repriced={} active_factors_half={} inactive_factors_half={}",
            market_fork.spec.address,
            direction,
            baseline,
            repriced,
            active_factors_half,
            inactive_factors_half
        );
        assert!(baseline > 0);
        assert_eq!(
            baseline,
            expected_first_level_output(&market_fork.market.data, amount_in, direction)
        );
        if direction == 1 {
            assert!(repriced > baseline * 19 / 10 && repriced < baseline * 21 / 10);
        } else {
            assert!(repriced > baseline * 4 / 10 && repriced < baseline * 6 / 10);
        }
        assert!(
            active_factors_half > baseline * 49 / 100 && active_factors_half < baseline * 51 / 100
        );
        assert_eq!(inactive_factors_half, baseline);
    }
}

#[tokio::test]
async fn tessera_catalog_matches_live_markets() {
    let registry = TemplateRegistry::new();
    let template = registry
        .get("tessera-fair-value")
        .expect("Tessera fair-value template");
    let catalog = template
        .constants
        .get("market")
        .expect("Tessera market catalog");

    let addresses: Vec<Pubkey> = catalog
        .options
        .iter()
        .map(|option| {
            option
                .value
                .parse()
                .unwrap_or_else(|_| panic!("catalog entry {} is not a pubkey", option.id))
        })
        .collect();
    let markets = live::fetch(&addresses).await;

    let mut mints: Vec<Pubkey> = Vec::with_capacity(addresses.len() * 2);
    for market in &markets {
        let (base, quote) =
            TesseraMarket::mint_addresses(market).expect("every catalog entry is a live market");
        mints.extend([base, quote]);
    }
    mints.sort_unstable();
    mints.dedup();
    let mint_accounts = live::fetch(&mints).await;

    for (option, market_account) in catalog.options.iter().zip(&markets) {
        let address: Pubkey = option.value.parse().expect("catalog pubkey");
        let (base, quote) = TesseraMarket::mint_addresses(market_account).expect("live market");
        let index = |mint: &Pubkey| mints.binary_search(mint).expect("fetched mint");
        let market = TesseraMarket::validate(
            address,
            market_account,
            &mint_accounts[index(&base)],
            &mint_accounts[index(&quote)],
        )
        .unwrap_or_else(|error| panic!("{} failed validation: {error}", option.id));

        let metadata = |key: &str| {
            option
                .metadata
                .get(key)
                .unwrap_or_else(|| panic!("{} has no {key}", option.id))
        };
        assert_eq!(metadata("base_mint"), &market.base_mint.to_string());
        assert_eq!(metadata("quote_mint"), &market.quote_mint.to_string());
        assert_eq!(metadata("base_decimals"), &market.base_decimals);
        assert_eq!(metadata("quote_decimals"), &market.quote_decimals);
        // The catalog's freshness limit is what picks the stale template, so it has to be the
        // number the deployed program actually reads at offset 88.
        assert_eq!(
            metadata("freshness_limit_slots"),
            &read_u64(&market_account.data, FRESHNESS_LIMIT_OFFSET)
        );
    }
}
