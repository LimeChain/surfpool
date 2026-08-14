use super::*;
use crate::scenarios::{
    pump_graduation::build_pump_graduation_scenario,
    pump_swap_price_shock::build_pump_swap_price_shock_scenario,
};

const PUMP: Pubkey = Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const PAMM: Pubkey = Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
const FEE_PROGRAM: Pubkey = Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
const TOKEN_2022: Pubkey = Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const TOKENKEG: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const WSOL: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const ATA_PROGRAM: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
const RENT_SYSVAR: Pubkey = Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");

const MINT: Pubkey = Pubkey::from_str_const("HRTzNRJNnY78xe8e4a9DuMotw6qA97GwSQLzpVw9pump");
const CURVE: Pubkey = Pubkey::from_str_const("GBpTHrtF8dGwxC7thRD7T6VfGtbVYEabKkQ7k6g3u7QF");
const BASE_VAULT: Pubkey = Pubkey::from_str_const("9sXf9hAtryY1mncMxKGZnLMJzQbnTsUoSu8GJTX3FpFh");
const QUOTE_VAULT: Pubkey = Pubkey::from_str_const("CyugdSkzUoF1srFgCJGuMaAGjCUQ8ys4ca8cqLkWPXFJ");
const PUMP_GLOBAL: Pubkey = Pubkey::from_str_const("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf");
const PUMP_FEE_RECIPIENT: Pubkey =
    Pubkey::from_str_const("6AUH3WEHucYZyC61hqpqYUWVto5qA5hjHuNQ32GNnNxA");
const PUMP_FEE_RECIPIENT_ATA: Pubkey =
    Pubkey::from_str_const("ghSBUgyxyvyurm1vJBkU4rUyLJoUipCZhFeiBogKCSy");
const PUMP_BUYBACK_RECIPIENT: Pubkey =
    Pubkey::from_str_const("GXPFM2caqTtQYC2cJ5yJRi9VDkpsYZXzYdwYpGnLmtDL");
const PUMP_BUYBACK_RECIPIENT_ATA: Pubkey =
    Pubkey::from_str_const("AktftA98kSWAxn6kVSoqBXBELUArjKu2H9WmKB48ULFY");
const PUMP_CREATOR_VAULT: Pubkey =
    Pubkey::from_str_const("7rnCZqrwmd4L1ZcX7VaKvYZ9n2BjgxmoysrmGhweW97C");
const PUMP_CREATOR_VAULT_ATA: Pubkey =
    Pubkey::from_str_const("C7jfrHkdirzU8F5r1Z1BKacwmwEMgKmLn9Ct3nKpLzmA");
const PUMP_SHARING_CONFIG: Pubkey =
    Pubkey::from_str_const("3NFHbr82N29vRbNHewWuuBHcNzdNuSU6zUBJBaRBPqj8");
const PUMP_GLOBAL_VOLUME_ACCUMULATOR: Pubkey =
    Pubkey::from_str_const("Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y");
const PUMP_FEE_CONFIG: Pubkey =
    Pubkey::from_str_const("8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt");
const PUMP_WITHDRAW_AUTHORITY: Pubkey =
    Pubkey::from_str_const("39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg");
const AMM_GLOBAL_CONFIG: Pubkey =
    Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw");
const BREAKING_FEE_RECIPIENT: Pubkey =
    Pubkey::from_str_const("EHAAiTxcdDwQ3U4bU6YcMsQGaekdzLS3B5SmYo46kJtL");

const BUY_V2_DISCRIMINATOR: [u8; 8] = [184, 23, 238, 97, 103, 197, 211, 61];
const MIGRATE_V2_DISCRIMINATOR: [u8; 8] = [187, 203, 18, 31, 206, 237, 254, 41];
const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

const MAX_SOL_COST: u64 = 1_000_000_000;

const CURVE_REAL_TOKEN_RESERVES_OFFSET: usize = 24;
const CURVE_COMPLETE_OFFSET: usize = 48;
const TOKEN_AMOUNT_OFFSET: usize = 64;
const AMM_RESERVED_FEE_RECIPIENT_OFFSET: usize = 385;
const AMM_MAYHEM_MODE_OFFSET: usize = 417;
const POOL_COIN_CREATOR_OFFSET: usize = 211;
const POOL_CASHBACK_FLAG_OFFSET: usize = 244;

struct GraduationFixture {
    user: Pubkey,
    user_base: Pubkey,
    user_quote: Pubkey,
    user_volume_accumulator: Pubkey,
    user_volume_accumulator_quote: Pubkey,
    pool_authority: Pubkey,
    pool: Pubkey,
    lp_mint: Pubkey,
    pool_authority_base: Pubkey,
    pool_authority_quote: Pubkey,
    pool_authority_lp: Pubkey,
    pool_base: Pubkey,
    pool_quote: Pubkey,
    pump_event_authority: Pubkey,
    pamm_event_authority: Pubkey,
    pool_v2: Pubkey,
    sell_fee_config: Pubkey,
}

impl GraduationFixture {
    fn new(user: Pubkey) -> Self {
        let user_base = associated_token_address(&user, &MINT, &TOKEN_2022);
        let user_quote = associated_token_address(&user, &WSOL, &TOKENKEG);
        let user_volume_accumulator =
            Pubkey::find_program_address(&[b"user_volume_accumulator", user.as_ref()], &PUMP).0;
        let user_volume_accumulator_quote =
            associated_token_address(&user_volume_accumulator, &WSOL, &TOKENKEG);
        let pool_authority =
            Pubkey::find_program_address(&[b"pool-authority", MINT.as_ref()], &PUMP).0;
        let pool = Pubkey::find_program_address(
            &[
                b"pool",
                &0u16.to_le_bytes(),
                pool_authority.as_ref(),
                MINT.as_ref(),
                WSOL.as_ref(),
            ],
            &PAMM,
        )
        .0;
        let lp_mint = Pubkey::find_program_address(&[b"pool_lp_mint", pool.as_ref()], &PAMM).0;

        Self {
            user,
            user_base,
            user_quote,
            user_volume_accumulator,
            user_volume_accumulator_quote,
            pool_authority,
            pool,
            lp_mint,
            pool_authority_base: associated_token_address(&pool_authority, &MINT, &TOKEN_2022),
            pool_authority_quote: associated_token_address(&pool_authority, &WSOL, &TOKENKEG),
            pool_authority_lp: associated_token_address(&pool_authority, &lp_mint, &TOKEN_2022),
            pool_base: associated_token_address(&pool, &MINT, &TOKEN_2022),
            pool_quote: associated_token_address(&pool, &WSOL, &TOKENKEG),
            pump_event_authority: Pubkey::find_program_address(&[b"__event_authority"], &PUMP).0,
            pamm_event_authority: Pubkey::find_program_address(&[b"__event_authority"], &PAMM).0,
            pool_v2: Pubkey::find_program_address(&[b"pool-v2", MINT.as_ref()], &PAMM).0,
            sell_fee_config: Pubkey::find_program_address(
                &[b"fee_config", PAMM.as_ref()],
                &FEE_PROGRAM,
            )
            .0,
        }
    }
}

fn associated_token_address(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

fn account_meta(pubkey: Pubkey, signer: bool, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(pubkey, signer)
    } else {
        AccountMeta::new_readonly(pubkey, signer)
    }
}

fn start_snapshot_surfnet() -> (RpcClient, SurfnetSvmLocker, RunloopGuard) {
    let snapshot: std::collections::BTreeMap<String, Option<AccountSnapshot>> =
        serde_json::from_str(include_str!(
            "../assets/pump_token2022_graduation.snapshot.json"
        ))
        .expect("graduation snapshot should deserialize");
    let bind_host = "127.0.0.1";
    let bind_port = get_free_port().unwrap();
    let ws_port = get_free_port().unwrap();
    let config = SurfpoolConfig {
        simnets: vec![SimnetConfig {
            snapshot,
            ..SimnetConfig::default()
        }],
        rpc: RpcConfig {
            bind_host: bind_host.to_string(),
            bind_port,
            ws_port,
            ..RpcConfig::default()
        },
        ..SurfpoolConfig::default()
    };
    let (surfnet_svm, simnet_events_rx, geyser_events_rx) = TestType::no_db().initialize_svm();
    let (simnet_commands_tx, simnet_commands_rx) = unbounded();
    let locker = SurfnetSvmLocker::new(surfnet_svm);
    let runloop = spawn_runloop(
        locker.clone(),
        config,
        (simnet_commands_tx, simnet_commands_rx),
        geyser_events_rx,
    )
    .expect("the runloop should start");
    wait_for_ready_and_connected(&simnet_events_rx)
        .expect("surfnet should be ready and connected to the datasource");
    let rpc = RpcClient::new_with_commitment(
        format!("http://{bind_host}:{bind_port}"),
        CommitmentConfig::confirmed(),
    );

    (rpc, locker, runloop)
}

async fn cheatcode(rpc: &RpcClient, method: &'static str, params: serde_json::Value) {
    let _: serde_json::Value = rpc
        .send(
            solana_client::rpc_request::RpcRequest::Custom { method },
            params,
        )
        .await
        .unwrap_or_else(|error| panic!("{method} cheatcode failed: {error:?}"));
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        data.get(offset..offset + 8)
            .unwrap_or_else(|| panic!("missing u64 at account-data offset {offset}"))
            .try_into()
            .unwrap(),
    )
}

async fn token_amount(rpc: &RpcClient, address: &Pubkey) -> u64 {
    let account = rpc
        .get_account(address)
        .await
        .unwrap_or_else(|error| panic!("token account {address} should exist: {error:?}"));
    read_u64(&account.data, TOKEN_AMOUNT_OFFSET)
}

async fn send_transaction(rpc: &RpcClient, payer: &Keypair, instructions: Vec<Instruction>) {
    let transaction = signed_transaction(rpc, payer, instructions).await;
    rpc.send_and_confirm_transaction(&transaction)
        .await
        .unwrap_or_else(|error| panic!("transaction failed: {error:?}"));
}

async fn signed_transaction(
    rpc: &RpcClient,
    payer: &Keypair,
    instructions: Vec<Instruction>,
) -> Transaction {
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .expect("recent blockhash should be available");
    Transaction::new_signed_with_payer(&instructions, Some(&payer.pubkey()), &[payer], blockhash)
}

async fn simulate_token_amount_after_transaction(
    rpc: &RpcClient,
    payer: &Keypair,
    instruction: Instruction,
    token_account: Pubkey,
) -> u64 {
    let transaction = signed_transaction(rpc, payer, vec![instruction]).await;
    let simulation = rpc
        .simulate_transaction_with_config(
            &transaction,
            RpcSimulateTransactionConfig {
                sig_verify: true,
                commitment: Some(CommitmentConfig::confirmed()),
                accounts: Some(RpcSimulateTransactionAccountsConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    addresses: vec![token_account.to_string()],
                }),
                ..RpcSimulateTransactionConfig::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(simulation.value.err, None, "swap simulation should succeed");
    let account_data = simulation
        .value
        .accounts
        .unwrap()
        .into_iter()
        .next()
        .flatten()
        .and_then(|account| account.data.decode())
        .expect("simulation should return the requested token account");
    read_u64(&account_data, TOKEN_AMOUNT_OFFSET)
}

async fn fund_user(rpc: &RpcClient, user: Pubkey) {
    cheatcode(
        rpc,
        "surfnet_setAccount",
        serde_json::json!([user.to_string(), { "lamports": 2_000_000_000u64 }]),
    )
    .await;
    cheatcode(
        rpc,
        "surfnet_setTokenAccount",
        serde_json::json!([user.to_string(), WSOL.to_string(), { "amount": 1_500_000_000u64 }, null]),
    )
    .await;
    // buy_v2 requires the user's Token-2022 base ATA to exist.
    cheatcode(
        rpc,
        "surfnet_setTokenAccount",
        serde_json::json!([user.to_string(), MINT.to_string(), { "amount": 0u64 }, TOKEN_2022.to_string()]),
    )
    .await;
}

fn build_buy_v2(fixture: &GraduationFixture, completing_buy_amount: u64) -> Instruction {
    let accounts = vec![
        account_meta(PUMP_GLOBAL, false, false),           // 0 global
        account_meta(MINT, false, false),                  // 1 base_mint
        account_meta(WSOL, false, false),                  // 2 quote_mint
        account_meta(TOKEN_2022, false, false),            // 3 base_token_program
        account_meta(TOKENKEG, false, false),              // 4 quote_token_program
        account_meta(ATA_PROGRAM, false, false),           // 5 associated_token_program
        account_meta(PUMP_FEE_RECIPIENT, false, true),     // 6 fee_recipient
        account_meta(PUMP_FEE_RECIPIENT_ATA, false, true), // 7 associated_quote_fee_recipient
        account_meta(PUMP_BUYBACK_RECIPIENT, false, true), // 8 buyback_fee_recipient
        account_meta(PUMP_BUYBACK_RECIPIENT_ATA, false, true), // 9 associated_quote_buyback_fee_recipient
        account_meta(CURVE, false, true),                      // 10 bonding_curve
        account_meta(BASE_VAULT, false, true),                 // 11 associated_base_bonding_curve
        account_meta(QUOTE_VAULT, false, true),                // 12 associated_quote_bonding_curve
        account_meta(fixture.user, true, true),                // 13 user
        account_meta(fixture.user_base, false, true),          // 14 associated_base_user
        account_meta(fixture.user_quote, false, true),         // 15 associated_quote_user
        account_meta(PUMP_CREATOR_VAULT, false, true),         // 16 creator_vault
        account_meta(PUMP_CREATOR_VAULT_ATA, false, true),     // 17 associated_creator_vault
        account_meta(PUMP_SHARING_CONFIG, false, false),       // 18 sharing_config
        account_meta(PUMP_GLOBAL_VOLUME_ACCUMULATOR, false, false), // 19 global_volume_accumulator
        account_meta(fixture.user_volume_accumulator, false, true), // 20 user_volume_accumulator
        account_meta(fixture.user_volume_accumulator_quote, false, true), // 21 associated_user_volume_accumulator
        account_meta(PUMP_FEE_CONFIG, false, false),                      // 22 fee_config
        account_meta(FEE_PROGRAM, false, false),                          // 23 fee_program
        account_meta(SYSTEM_PROGRAM, false, false),                       // 24 system_program
        account_meta(fixture.pump_event_authority, false, false),         // 25 event_authority
        account_meta(PUMP, false, false),                                 // 26 program
    ];
    let mut data = BUY_V2_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&completing_buy_amount.to_le_bytes());
    data.extend_from_slice(&MAX_SOL_COST.to_le_bytes());

    Instruction {
        program_id: PUMP,
        accounts,
        data,
    }
}

fn build_migrate_v2(fixture: &GraduationFixture) -> Vec<Instruction> {
    let accounts = vec![
        account_meta(PUMP_GLOBAL, false, false), // 0 global
        account_meta(PUMP_WITHDRAW_AUTHORITY, false, true), // 1 withdraw_authority
        account_meta(MINT, false, false),        // 2 base_mint
        account_meta(WSOL, false, false),        // 3 quote_mint
        account_meta(CURVE, false, true),        // 4 bonding_curve
        account_meta(BASE_VAULT, false, true),   // 5 associated_base_bonding_curve
        account_meta(QUOTE_VAULT, false, true),  // 6 associated_quote_bonding_curve
        account_meta(fixture.user, true, false), // 7 user
        account_meta(SYSTEM_PROGRAM, false, false), // 8 system_program
        account_meta(PAMM, false, false),        // 9 pump_amm_program
        account_meta(fixture.pool, false, true), // 10 pool
        account_meta(fixture.pool_authority, false, true), // 11 pool_authority
        account_meta(fixture.pool_authority_base, false, true), // 12 pool_authority_mint_account
        account_meta(fixture.pool_authority_quote, false, true), // 13 pool_authority_quote_account
        account_meta(AMM_GLOBAL_CONFIG, false, false), // 14 amm_global_config
        account_meta(fixture.lp_mint, false, true), // 15 pool_lp_mint
        account_meta(fixture.pool_authority_lp, false, true), // 16 user_pool_token_account
        account_meta(fixture.pool_base, false, true), // 17 pool_base_token_account
        account_meta(fixture.pool_quote, false, true), // 18 pool_quote_token_account
        account_meta(TOKEN_2022, false, false),  // 19 base_token_program
        account_meta(TOKENKEG, false, false),    // 20 quote_token_program
        account_meta(TOKEN_2022, false, false),  // 21 token_2022_program
        account_meta(ATA_PROGRAM, false, false), // 22 associated_token_program
        account_meta(fixture.pamm_event_authority, false, false), // 23 pump_amm_event_authority
        account_meta(RENT_SYSVAR, false, false), // 24 rent
        account_meta(fixture.pump_event_authority, false, false), // 25 event_authority
        account_meta(PUMP, false, false),        // 26 program
    ];

    vec![
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
        Instruction {
            program_id: PUMP,
            accounts,
            data: MIGRATE_V2_DISCRIMINATOR.to_vec(),
        },
    ]
}

async fn build_sell(
    rpc: &RpcClient,
    fixture: &GraduationFixture,
    base_amount_in: u64,
) -> Instruction {
    let global_config = rpc
        .get_account(&AMM_GLOBAL_CONFIG)
        .await
        .expect("frozen AMM global config should load");
    assert_eq!(
        global_config.data[AMM_MAYHEM_MODE_OFFSET], 1,
        "fixture expects mayhem mode"
    );
    let reserved_fee_recipient = Pubkey::try_from(
        &global_config.data
            [AMM_RESERVED_FEE_RECIPIENT_OFFSET..AMM_RESERVED_FEE_RECIPIENT_OFFSET + 32],
    )
    .unwrap();
    let pool_data = rpc
        .get_account(&fixture.pool)
        .await
        .expect("migrated pool should exist")
        .data;
    assert_eq!(
        pool_data[POOL_CASHBACK_FLAG_OFFSET], 0,
        "24-account sell is only valid for a non-cashback pool"
    );
    let coin_creator =
        Pubkey::try_from(&pool_data[POOL_COIN_CREATOR_OFFSET..POOL_COIN_CREATOR_OFFSET + 32])
            .unwrap();
    let coin_creator_vault_authority =
        Pubkey::find_program_address(&[b"creator_vault", coin_creator.as_ref()], &PAMM).0;
    let accounts = vec![
        account_meta(fixture.pool, false, true),            // 0 pool
        account_meta(fixture.user, true, true),             // 1 user
        account_meta(AMM_GLOBAL_CONFIG, false, false),      // 2 global_config
        account_meta(MINT, false, false),                   // 3 base_mint
        account_meta(WSOL, false, false),                   // 4 quote_mint
        account_meta(fixture.user_base, false, true),       // 5 user_base_token_account
        account_meta(fixture.user_quote, false, true),      // 6 user_quote_token_account
        account_meta(fixture.pool_base, false, true),       // 7 pool_base_token_account
        account_meta(fixture.pool_quote, false, true),      // 8 pool_quote_token_account
        account_meta(reserved_fee_recipient, false, false), // 9 protocol_fee_recipient
        account_meta(
            associated_token_address(&reserved_fee_recipient, &WSOL, &TOKENKEG),
            false,
            true,
        ), // 10 protocol_fee_recipient_token_account
        account_meta(TOKEN_2022, false, false),             // 11 base_token_program
        account_meta(TOKENKEG, false, false),               // 12 quote_token_program
        account_meta(SYSTEM_PROGRAM, false, false),         // 13 system_program
        account_meta(ATA_PROGRAM, false, false),            // 14 associated_token_program
        account_meta(fixture.pamm_event_authority, false, false), // 15 event_authority
        account_meta(PAMM, false, false),                   // 16 program
        account_meta(
            associated_token_address(&coin_creator_vault_authority, &WSOL, &TOKENKEG),
            false,
            true,
        ), // 17 coin_creator_vault_ata
        account_meta(coin_creator_vault_authority, false, false), // 18 coin_creator_vault_authority
        account_meta(fixture.sell_fee_config, false, false), // 19 fee_config
        account_meta(FEE_PROGRAM, false, false),            // 20 fee_program
        account_meta(fixture.pool_v2, false, false),        // 21 pool_v2
        account_meta(BREAKING_FEE_RECIPIENT, false, false), // 22 fee_recipient
        account_meta(
            associated_token_address(&BREAKING_FEE_RECIPIENT, &WSOL, &TOKENKEG),
            false,
            true,
        ), // 23 fee_recipient_token_account
    ];
    let mut data = SELL_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&base_amount_in.to_le_bytes());
    // Zero slippage protection is acceptable only in this regression test.
    data.extend_from_slice(&0u64.to_le_bytes());

    Instruction {
        program_id: PAMM,
        accounts,
        data,
    }
}

/// The snapshot freezes account state while the live programs expose behavioral upgrades.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires network: forks mainnet for the live pump and pAMM programs"]
async fn test_pump_token2022_graduation_lifecycle() {
    let (rpc, locker, _runloop) = start_snapshot_surfnet();
    let user = Keypair::new();
    let fixture = GraduationFixture::new(user.pubkey());
    let preparation = build_pump_graduation_scenario(
        MINT,
        &rpc.get_account(&MINT).await.unwrap(),
        &rpc.get_account(&CURVE).await.unwrap(),
        &rpc.get_account(&BASE_VAULT).await.unwrap(),
        None,
        &rpc.get_account(&PUMP_GLOBAL).await.unwrap(),
    )
    .unwrap();
    let completing_buy_amount = preparation.completing_buy_amount;
    let migration_reserve = preparation.migration_reserve;
    locker
        .register_scenario(preparation.scenario, Some(0))
        .unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();
    fund_user(&rpc, fixture.user).await;

    send_transaction(
        &rpc,
        &user,
        vec![build_buy_v2(&fixture, completing_buy_amount)],
    )
    .await;

    let curve_after = rpc.get_account(&CURVE).await.unwrap();
    assert_eq!(
        read_u64(&curve_after.data, CURVE_REAL_TOKEN_RESERVES_OFFSET),
        0,
        "buy should exhaust the curve's real token reserves"
    );
    assert_eq!(
        curve_after.data[CURVE_COMPLETE_OFFSET], 1,
        "buy should complete the curve"
    );
    assert_eq!(
        token_amount(&rpc, &fixture.user_base).await,
        completing_buy_amount,
        "user should receive the purchased base tokens"
    );
    assert_eq!(
        token_amount(&rpc, &BASE_VAULT).await,
        migration_reserve,
        "buy should leave the migration reserve in the curve vault"
    );

    send_transaction(&rpc, &user, build_migrate_v2(&fixture)).await;

    assert_eq!(
        rpc.get_account(&fixture.pool).await.unwrap().owner,
        PAMM,
        "migrate should create a pAMM-owned pool"
    );
    assert_eq!(
        rpc.get_account(&fixture.lp_mint).await.unwrap().owner,
        TOKEN_2022,
        "migrate should create a Token-2022 LP mint"
    );
    assert_eq!(
        token_amount(&rpc, &fixture.pool_base).await,
        migration_reserve,
        "migrate should seed the pool with the reserved base liquidity"
    );
    assert!(
        token_amount(&rpc, &fixture.pool_quote).await > 0,
        "migrate should seed the pool with quote liquidity"
    );

    let pre_user_base = token_amount(&rpc, &fixture.user_base).await;
    let pre_user_quote = token_amount(&rpc, &fixture.user_quote).await;
    let pre_pool_base = token_amount(&rpc, &fixture.pool_base).await;
    let pre_pool_quote = token_amount(&rpc, &fixture.pool_quote).await;
    let base_amount_in = pre_user_base / 2;
    let sell = build_sell(&rpc, &fixture, base_amount_in).await;
    let baseline_user_quote =
        simulate_token_amount_after_transaction(&rpc, &user, sell.clone(), fixture.user_quote)
            .await;
    let price_shock = build_pump_swap_price_shock_scenario(
        MINT,
        &rpc.get_account(&fixture.pool).await.unwrap(),
        pre_pool_quote.checked_mul(9).unwrap(),
    )
    .unwrap();
    locker
        .register_scenario(price_shock.scenario, Some(0))
        .unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();
    let shocked_user_quote =
        simulate_token_amount_after_transaction(&rpc, &user, sell.clone(), fixture.user_quote)
            .await;
    assert_ne!(
        shocked_user_quote - pre_user_quote,
        baseline_user_quote - pre_user_quote,
        "the price-shock scenario should change the real swap output"
    );
    send_transaction(&rpc, &user, vec![sell]).await;

    assert_eq!(
        token_amount(&rpc, &fixture.user_base).await,
        pre_user_base - base_amount_in,
        "sell should debit the user's base tokens"
    );
    assert!(
        token_amount(&rpc, &fixture.user_quote).await > pre_user_quote,
        "sell should credit the user's quote tokens"
    );
    assert_eq!(
        token_amount(&rpc, &fixture.pool_base).await,
        pre_pool_base + base_amount_in,
        "sell should credit the pool's base vault"
    );
    assert!(
        token_amount(&rpc, &fixture.pool_quote).await < pre_pool_quote,
        "sell should debit the pool's quote vault"
    );
}
