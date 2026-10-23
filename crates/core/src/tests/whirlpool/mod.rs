//! Whirlpool integration tests. They fetch real mainnet accounts, so they need a network
//! connection and `--features integration-tests`. `SURFPOOL_TEST_RPC_URL` overrides the endpoint.

use std::collections::HashMap;

use serde_json::{Value, json};
use solana_account::Account;
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
use solana_keypair::Keypair;
use solana_loader_v3_interface::get_program_data_address;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{
        GetAccountResult, locker::SurfnetSvmLocker, remote::SurfnetRemoteClient, svm::SurfnetSvm,
    },
    tests::helpers::diff_indices,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const WHIRLPOOL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
const DEPLOYED_SLOT: u64 = 457_152_296;

const SOL_USDC: &str = "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ";
const SOL_USDC_TS4: &str = "Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE";
/// An adaptive-fee pool, so a swap also reads its Oracle.
const SOL_HYPE_ADAPTIVE_FEE: &str = "31KrYUDzgEQhEgr1JSNVfHAknWACcF97CtUaU8enKQsy";

const CONFIG_OFFSET: usize = 8;
const TICK_SPACING_OFFSET: usize = 41;
const FEE_TIER_INDEX_OFFSET: usize = 43;
const FEE_RATE_OFFSET: usize = 45;
const PROTOCOL_FEE_RATE_OFFSET: usize = 47;
const SQRT_PRICE_OFFSET: usize = 65;
const TICK_CURRENT_INDEX_OFFSET: usize = 81;
const PROTOCOL_FEE_OWED_A_OFFSET: usize = 85;
const TOKEN_MINT_A_OFFSET: usize = 101;
const TOKEN_VAULT_A_OFFSET: usize = 133;
const TOKEN_MINT_B_OFFSET: usize = 181;
const TOKEN_VAULT_B_OFFSET: usize = 213;
const REWARD_LAST_UPDATED_TIMESTAMP_OFFSET: usize = 261;
const ORACLE_REFERENCE_UPDATE_TIMESTAMP_OFFSET: usize = 82;
const ORACLE_MAJOR_SWAP_TIMESTAMP_OFFSET: usize = 90;
const TOKEN_AMOUNT_OFFSET: usize = 64;

const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
const MIN_SQRT_PRICE: u128 = 4_295_048_016;
const TICK_ARRAY_SIZE: i32 = 88;
const INVALID_TIMESTAMP: u32 = 6022;
/// About how far the local Clock trails upstream.
const CLOCK_LAG_SECONDS: u64 = 60;

/// Fetches the accounts in one request (same slot) and panics if any is missing.
async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    remote_client()
        .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .map(|result| match result {
            GetAccountResult::FoundAccount(_, account, _)
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
            GetAccountResult::None(pubkey) => {
                panic!("{pubkey} does not exist on mainnet; the test needs a new address")
            }
        })
        .collect()
}

#[tokio::test]
async fn real_mainnet_whirlpool_round_trips_unchanged() {
    let pubkey = Pubkey::from_str_const(SOL_USDC);
    let data = fetch(&[pubkey]).await.remove(0).data;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let template = registry
        .get("whirlpool-pool-state")
        .expect("whirlpool-pool-state template");

    let forged = surfnet_svm
        .get_forged_account_data(&pubkey, &data, template.idl(), &HashMap::new())
        .expect("live mainnet Whirlpool should decode/re-encode with the bundled IDL");

    assert_eq!(
        forged.len(),
        data.len(),
        "Whirlpool changed size on round-trip"
    );
    let diffs = diff_indices(&forged, &data);
    assert!(
        diffs.is_empty(),
        "live mainnet Whirlpool was altered by a no-op round-trip at {} byte(s), first at {:?}",
        diffs.len(),
        diffs.first()
    );
}

#[tokio::test]
async fn whirlpools_config_singleton_matches_a_live_pool() {
    let registry = TemplateRegistry::new();
    let config = registry
        .get("whirlpools-config")
        .expect("whirlpools-config template")
        .address
        .resolve_simple()
        .expect("whirlpools-config address should resolve");

    let data = fetch(&[Pubkey::from_str_const(SOL_USDC)])
        .await
        .remove(0)
        .data;
    assert_eq!(
        &data[CONFIG_OFFSET..CONFIG_OFFSET + 32],
        config.as_ref(),
        "the live pool's whirlpools_config field must equal the whirlpools-config template's address"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overrides_through_the_materializer_touch_only_their_own_bytes() {
    let pubkey = Pubkey::from_str_const(SOL_USDC_TS4);
    let account = fetch(&[pubkey]).await.remove(0);
    let data = account.data.clone();

    let new_fee_rate = u16::from_le_bytes(
        data[FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2]
            .try_into()
            .unwrap(),
    )
    .wrapping_add(500);
    let new_sqrt_price = u128::from_le_bytes(
        data[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16]
            .try_into()
            .unwrap(),
    ) / 2;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let locker = SurfnetSvmLocker::new(surfnet_svm);
    locker
        .with_svm_writer(|svm| svm.set_account(&pubkey, account))
        .expect("seeding the live pool into the local SVM should succeed");

    let pool_override = OverrideInstance::new(
        "whirlpool-pool-state".to_string(),
        1,
        AccountAddress::Pubkey(pubkey.to_string()),
    )
    .with_values(HashMap::from([
        ("fee_rate".to_string(), json!(new_fee_rate)),
        ("sqrt_price".to_string(), json!(new_sqrt_price.to_string())),
    ]))
    .with_label("Whirlpool fee_rate and sqrt_price override".to_string());

    let mut scenario = Scenario::new(
        "Whirlpool byte-diff".to_string(),
        "Override a live pool's fee_rate and sqrt_price through the materializer.".to_string(),
    );
    scenario.add_override(pool_override);
    locker.register_scenario(scenario, Some(0)).unwrap();
    locker
        .materialize_overrides_for_slot(&None, 1)
        .await
        .unwrap();

    let after = locker
        .with_svm_reader(|svm| svm.get_account(&pubkey))
        .unwrap()
        .expect("account should still exist after materialization")
        .data;

    assert_eq!(after.len(), data.len(), "override changed account size");
    let allowed =
        (FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2).chain(SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16);
    let allowed: Vec<usize> = allowed.collect();
    let diffs = diff_indices(&data, &after);
    assert!(
        !diffs.is_empty() && diffs.iter().all(|i| allowed.contains(i)),
        "only fee_rate and sqrt_price bytes may change, touched {diffs:?}"
    );
    assert_eq!(
        u16::from_le_bytes(
            after[FEE_RATE_OFFSET..FEE_RATE_OFFSET + 2]
                .try_into()
                .unwrap()
        ),
        new_fee_rate
    );
    assert_eq!(
        u128::from_le_bytes(
            after[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16]
                .try_into()
                .unwrap()
        ),
        new_sqrt_price
    );
}

fn remote_client() -> SurfnetRemoteClient {
    SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    )
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::try_from(&data[offset..offset + 32]).unwrap()
}

fn with_u64(account: &Account, offset: usize, value: u64) -> Account {
    let mut account = account.clone();
    account.data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    account
}

/// The tick array holding the pool's current tick.
fn tick_array_address(pool: &Pubkey, data: &[u8]) -> Pubkey {
    let tick_spacing = i32::from(u16::from_le_bytes(
        data[TICK_SPACING_OFFSET..TICK_SPACING_OFFSET + 2]
            .try_into()
            .unwrap(),
    ));
    let tick = i32::from_le_bytes(
        data[TICK_CURRENT_INDEX_OFFSET..TICK_CURRENT_INDEX_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    let span = TICK_ARRAY_SIZE * tick_spacing;
    let start = tick.div_euclid(span) * span;
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), start.to_string().as_bytes()],
        &WHIRLPOOL_PROGRAM_ID,
    )
    .0
}

/// What a swap of token A for token B on a live pool reads, as upstream has it now, and who swaps.
struct SwapGraph {
    pool: Pubkey,
    pool_account: Account,
    program_data: Account,
    vault_a: (Pubkey, Account),
    vault_b: (Pubkey, Account),
    tick_array: (Pubkey, Account),
    oracle: Pubkey,
    /// Only an adaptive-fee pool has one.
    oracle_account: Option<Account>,
    /// The local Clock, behind the pool's and the Oracle's last update as it trails upstream.
    now: u64,
    user: Keypair,
    user_tokens: [Pubkey; 2],
}

impl SwapGraph {
    async fn fetch(pool: &str) -> Self {
        let pool = Pubkey::from_str_const(pool);
        let pool_account = fetch(&[pool]).await.remove(0);
        let data = &pool_account.data;
        let tick_array = tick_array_address(&pool, data);
        let oracle =
            Pubkey::find_program_address(&[b"oracle", pool.as_ref()], &WHIRLPOOL_PROGRAM_ID).0;
        let mut addresses = vec![
            get_program_data_address(&WHIRLPOOL_PROGRAM_ID),
            read_pubkey(data, TOKEN_VAULT_A_OFFSET),
            read_pubkey(data, TOKEN_VAULT_B_OFFSET),
            tick_array,
        ];
        if data[TICK_SPACING_OFFSET..TICK_SPACING_OFFSET + 2]
            != data[FEE_TIER_INDEX_OFFSET..FEE_TIER_INDEX_OFFSET + 2]
        {
            addresses.push(oracle);
        }
        let mut accounts = fetch(&addresses).await.into_iter();
        let program_data = accounts.next().unwrap();
        let vault_a = (addresses[1], accounts.next().unwrap());
        let vault_b = (addresses[2], accounts.next().unwrap());
        let tick_array = (tick_array, accounts.next().unwrap());
        let oracle_account = accounts.next();
        let oracle_updated = oracle_account.as_ref().map_or(u64::MAX, |oracle| {
            read_u64(&oracle.data, ORACLE_REFERENCE_UPDATE_TIMESTAMP_OFFSET)
        });
        let now = read_u64(data, REWARD_LAST_UPDATED_TIMESTAMP_OFFSET).min(oracle_updated)
            - CLOCK_LAG_SECONDS;

        Self {
            pool,
            pool_account,
            program_data,
            vault_a,
            vault_b,
            tick_array,
            oracle,
            oracle_account,
            now,
            user: Keypair::new(),
            user_tokens: [Pubkey::new_unique(), Pubkey::new_unique()],
        }
    }

    /// A local VM with the deployed program, the vaults, the tick array and the user holding token
    /// A, its Clock at `now`. The pool and its Oracle are left to the test.
    fn local_vm(&self) -> SurfnetSvm {
        let (mut svm, _events_rx, _geyser_rx) = SurfnetSvm::default();
        let mut clock = svm.inner.get_sysvar::<Clock>();
        clock.unix_timestamp = self.now as i64;
        svm.inner.set_sysvar(&clock);
        assert_eq!(
            read_u64(&self.program_data.data, 4),
            DEPLOYED_SLOT,
            "Whirlpool was redeployed; re-verify the templates and the offsets here"
        );
        // The ELF starts past the upgradeable loader's 45-byte ProgramData header.
        svm.inner
            .svm
            .add_program(WHIRLPOOL_PROGRAM_ID, &self.program_data.data[45..])
            .unwrap();
        for (address, account) in [&self.vault_a, &self.vault_b, &self.tick_array] {
            svm.set_account(address, account.clone()).unwrap();
        }
        svm.inner
            .airdrop(&self.user.pubkey(), 1_000_000_000)
            .unwrap();
        for (address, mint, amount) in [
            (self.user_tokens[0], TOKEN_MINT_A_OFFSET, 1_000_000_000),
            (self.user_tokens[1], TOKEN_MINT_B_OFFSET, 0),
        ] {
            let mut data = vec![0; spl_token_interface::state::Account::LEN];
            spl_token_interface::state::Account {
                mint: read_pubkey(&self.pool_account.data, mint),
                owner: self.user.pubkey(),
                amount,
                state: spl_token_interface::state::AccountState::Initialized,
                ..Default::default()
            }
            .pack_into_slice(&mut data);
            let token_account = Account {
                lamports: svm.inner.minimum_balance_for_rent_exemption(data.len()),
                data,
                owner: spl_token_interface::id(),
                ..Default::default()
            };
            svm.set_account(&address, token_account).unwrap();
        }
        svm
    }

    /// Plays what the Studio presets write, the way Studio plays it against the upstream
    /// datasource: `pool_values` and the pool's reward timestamp and, on an adaptive-fee pool,
    /// both Oracle timestamps, set to the local Clock on accounts fetched from upstream first.
    async fn play(&mut self, svm: &mut SurfnetSvm, mut pool_values: HashMap<String, Value>) {
        let now = svm.inner.get_sysvar::<Clock>().unix_timestamp as u64;
        pool_values.insert("reward_last_updated_timestamp".to_string(), json!(now));
        let mut overrides = vec![
            OverrideInstance::new(
                "whirlpool-pool-state".to_string(),
                0,
                AccountAddress::Pubkey(self.pool.to_string()),
            )
            .with_values(pool_values),
        ];
        if self.oracle_account.is_some() {
            let oracle_address = TemplateRegistry::new()
                .get("whirlpool-oracle")
                .expect("whirlpool-oracle template")
                .address
                .clone();
            overrides.push(
                OverrideInstance::new("whirlpool-oracle".to_string(), 0, oracle_address)
                    .with_values(HashMap::from([
                        ("whirlpool".to_string(), json!(self.pool.to_string())),
                        (
                            "adaptive_fee_variables.last_reference_update_timestamp".to_string(),
                            json!(now),
                        ),
                        (
                            "adaptive_fee_variables.last_major_swap_timestamp".to_string(),
                            json!(now),
                        ),
                    ])),
            );
        }
        let mut scenario = Scenario::new(
            "Whirlpool clock".to_string(),
            "Keep a live pool's timestamps at the local Clock.".to_string(),
        );
        for mut instance in overrides {
            instance.fetch_before_use = true;
            scenario.add_override(instance);
        }
        svm.register_scenario(scenario, Some(100)).unwrap();
        svm.materialize_overrides_for_slot(
            &Some((remote_client(), CommitmentConfig::confirmed())),
            100,
        )
        .await
        .unwrap();

        let pool = svm.get_account(&self.pool).unwrap().expect("pool fetched");
        assert_eq!(
            read_u64(&pool.data, REWARD_LAST_UPDATED_TIMESTAMP_OFFSET),
            now
        );
        // The refetched pool can sit on another tick array than the one seeded.
        let tick_array = tick_array_address(&self.pool, &pool.data);
        if tick_array != self.tick_array.0 {
            self.tick_array = (tick_array, fetch(&[tick_array]).await.remove(0));
            svm.set_account(&tick_array, self.tick_array.1.clone())
                .unwrap();
        }
        if self.oracle_account.is_some() {
            let oracle = svm
                .get_account(&self.oracle)
                .unwrap()
                .expect("Oracle at its PDA");
            for offset in [
                ORACLE_REFERENCE_UPDATE_TIMESTAMP_OFFSET,
                ORACLE_MAJOR_SWAP_TIMESTAMP_OFFSET,
            ] {
                assert_eq!(read_u64(&oracle.data, offset), now);
            }
        }
    }

    /// Swaps `amount` of token A for token B and returns what the user received, or the error.
    fn swap(
        &self,
        svm: &mut SurfnetSvm,
        amount: u64,
    ) -> Result<u64, (TransactionError, Vec<String>)> {
        let mut data = SWAP_DISCRIMINATOR.to_vec();
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&0_u64.to_le_bytes());
        data.extend_from_slice(&MIN_SQRT_PRICE.to_le_bytes());
        data.extend_from_slice(&[1, 1]);
        let accounts = vec![
            AccountMeta::new_readonly(spl_token_interface::id(), false),
            AccountMeta::new_readonly(self.user.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.user_tokens[0], false),
            AccountMeta::new(self.vault_a.0, false),
            AccountMeta::new(self.user_tokens[1], false),
            AccountMeta::new(self.vault_b.0, false),
            AccountMeta::new(self.tick_array.0, false),
            AccountMeta::new(self.tick_array.0, false),
            AccountMeta::new(self.tick_array.0, false),
            AccountMeta::new(self.oracle, false),
        ];
        let transaction = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: WHIRLPOOL_PROGRAM_ID,
                accounts,
                data,
            }],
            Some(&self.user.pubkey()),
            &[&self.user],
            svm.inner.svm.latest_blockhash(),
        );
        svm.inner
            .send_transaction(transaction)
            .map_err(|failed| (failed.err, failed.meta.logs))?;
        let token_b = svm.get_account(&self.user_tokens[1]).unwrap().unwrap();
        Ok(read_u64(&token_b.data, TOKEN_AMOUNT_OFFSET))
    }
}

fn assert_invalid_timestamp(result: Result<u64, (TransactionError, Vec<String>)>, case: &str) {
    match result {
        Err((TransactionError::InstructionError(0, InstructionError::Custom(code)), _))
            if code == INVALID_TIMESTAMP => {}
        other => panic!("{case}: expected InvalidTimestamp ({INVALID_TIMESTAMP}), got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_played_pool_swaps_while_the_local_clock_trails_upstream() {
    let mut graph = SwapGraph::fetch(SOL_USDC_TS4).await;
    assert!(graph.oracle_account.is_none(), "not an adaptive-fee pool");

    let mut svm = graph.local_vm();
    svm.set_account(&graph.pool, graph.pool_account.clone())
        .unwrap();
    assert_invalid_timestamp(
        graph.swap(&mut svm, 1_000_000),
        "a pool copied from upstream is ahead of the local Clock",
    );

    let mut svm = graph.local_vm();
    graph.play(&mut svm, HashMap::new()).await;
    let received = graph
        .swap(&mut svm, 1_000_000)
        .unwrap_or_else(|e| panic!("the played pool should swap: {e:?}"));
    assert!(received > 0, "the swap pays out token B");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_played_adaptive_fee_pool_swaps_while_the_local_clock_trails_upstream() {
    let mut graph = SwapGraph::fetch(SOL_HYPE_ADAPTIVE_FEE).await;
    let (now, oracle) = (graph.now, graph.oracle_account.as_ref().unwrap());

    // Each timestamp alone ahead of the Clock refuses the swap, and none ahead lets it through.
    let pool_behind = with_u64(
        &graph.pool_account,
        REWARD_LAST_UPDATED_TIMESTAMP_OFFSET,
        now,
    );
    let oracle_behind = with_u64(
        &with_u64(oracle, ORACLE_REFERENCE_UPDATE_TIMESTAMP_OFFSET, now),
        ORACLE_MAJOR_SWAP_TIMESTAMP_OFFSET,
        now,
    );
    let ahead = now + CLOCK_LAG_SECONDS;
    let mut svm = graph.local_vm();
    for (index, (case, pool, oracle)) in [
        (
            "pool reward update",
            with_u64(&pool_behind, REWARD_LAST_UPDATED_TIMESTAMP_OFFSET, ahead),
            oracle_behind.clone(),
        ),
        (
            "oracle reference update",
            pool_behind.clone(),
            with_u64(
                &oracle_behind,
                ORACLE_REFERENCE_UPDATE_TIMESTAMP_OFFSET,
                ahead,
            ),
        ),
        (
            "oracle major swap",
            pool_behind.clone(),
            with_u64(&oracle_behind, ORACLE_MAJOR_SWAP_TIMESTAMP_OFFSET, ahead),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        svm.set_account(&graph.pool, pool).unwrap();
        svm.set_account(&graph.oracle, oracle).unwrap();
        // A distinct amount keeps each transaction's signature new.
        assert_invalid_timestamp(graph.swap(&mut svm, 1_000_000 + index as u64), case);
    }
    svm.set_account(&graph.pool, pool_behind).unwrap();
    svm.set_account(&graph.oracle, oracle_behind).unwrap();
    graph
        .swap(&mut svm, 2_000_000)
        .unwrap_or_else(|e| panic!("nothing ahead of the Clock should swap: {e:?}"));

    let mut svm = graph.local_vm();
    graph.play(&mut svm, HashMap::new()).await;
    let received = graph
        .swap(&mut svm, 1_000_000)
        .unwrap_or_else(|e| panic!("the played pool should swap: {e:?}"));
    assert!(received > 0, "the swap pays out token B");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fee_rate_override_charges_exactly_that_fee() {
    const AMOUNT: u64 = 500_000_000;
    const FEE_RATE: u64 = 10_000;
    let mut graph = SwapGraph::fetch(SOL_USDC_TS4).await;
    let mut svm = graph.local_vm();
    let fee_rate = HashMap::from([("fee_rate".to_string(), json!(FEE_RATE))]);
    graph.play(&mut svm, fee_rate).await;
    let pool = svm.get_account(&graph.pool).unwrap().unwrap().data;
    let protocol_fee_rate = u64::from(u16::from_le_bytes(
        pool[PROTOCOL_FEE_RATE_OFFSET..PROTOCOL_FEE_RATE_OFFSET + 2]
            .try_into()
            .unwrap(),
    ));

    graph
        .swap(&mut svm, AMOUNT)
        .unwrap_or_else(|e| panic!("the played pool should swap: {e:?}"));
    let after = svm.get_account(&graph.pool).unwrap().unwrap().data;
    let protocol_fee =
        read_u64(&after, PROTOCOL_FEE_OWED_A_OFFSET) - read_u64(&pool, PROTOCOL_FEE_OWED_A_OFFSET);
    assert_eq!(
        protocol_fee,
        AMOUNT * FEE_RATE / 1_000_000 * protocol_fee_rate / 10_000,
        "the protocol's share of a {FEE_RATE} fee_rate on {AMOUNT} of token A"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_price_override_halves_the_fill() {
    const AMOUNT: u64 = 1_000_000;
    let mut graph = SwapGraph::fetch(SOL_USDC_TS4).await;
    let mut svm = graph.local_vm();
    graph.play(&mut svm, HashMap::new()).await;
    let pool = svm.get_account(&graph.pool).unwrap().unwrap().data;
    let before = graph
        .swap(&mut svm, AMOUNT)
        .unwrap_or_else(|e| panic!("the played pool should swap: {e:?}"));

    // Liquidity stays: a swap this small crosses no tick, so only the price sets the fill.
    let sqrt_price = u128::from_le_bytes(
        pool[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET + 16]
            .try_into()
            .unwrap(),
    ) as f64
        * 0.5_f64.sqrt();
    let tick = (2.0 * (sqrt_price / 2_f64.powi(64)).ln() / 1.0001_f64.ln()).floor() as i32;
    let half_price = HashMap::from([
        (
            "sqrt_price".to_string(),
            json!((sqrt_price as u128).to_string()),
        ),
        ("tick_current_index".to_string(), json!(tick)),
    ]);
    let mut svm = graph.local_vm();
    graph.play(&mut svm, half_price).await;
    let after = graph
        .swap(&mut svm, AMOUNT)
        .unwrap_or_else(|e| panic!("the half-price pool should swap: {e:?}"));
    let half = before as f64 / 2.0;
    assert!(
        (after as f64 - half).abs() <= half * 0.02,
        "at half the price {AMOUNT} of token A should fetch about {half}, got {after}"
    );
}
