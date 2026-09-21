//! Raydium AMM v4 integration tests.
//!
//! These fetch the real accounts from mainnet rather than embedding captured copies, so they need
//! a network connection and are compiled only behind a feature:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests raydium_amm -- --test-threads=1
//! ```
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.
//!
//! AMM v4 is a native program: its accounts carry no discriminator, so the engine picks the IDL
//! account type by the fixed Borsh size the type declares. Everything below exists to prove that
//! choice is right on live bytes - that a 752-byte pool really is `AmmInfo`, that its 2208-byte
//! target-orders account really is `TargetOrders`, and that decoding from byte 0 reproduces the
//! account exactly.

use std::collections::HashMap;

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

const AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// The pools the milestone verified live.
const SOL_USDC: &str = "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2";
const RAY_USDC: &str = "6UmmUiYoBjSrhakAobJw8BvkmJtDVxaeBtbt7rxWo1mg";
const RAY_SOL: &str = "AVs9TA4nWDzfPJE9gGVNJMVhcQy3V9PGazuz33BfG2RA";

/// Offsets into `AmmInfo`, from raydium-amm `d26944bf` `program/src/state.rs`. The struct is
/// `#[repr(C, packed)]` and holds only u64, u128 and Pubkey, so these are absolute byte offsets
/// and the Borsh encoding of the same field list lands on exactly the same ones.
const STATUS: usize = 0;
const SWAP_FEE_NUMERATOR: usize = 176;
const NEED_TAKE_PNL_COIN: usize = 192;
const COIN_VAULT: usize = 336;
const TARGET_ORDERS: usize = 592;

const AMM_INFO_LEN: usize = 752;
const TARGET_ORDERS_LEN: usize = 2208;

/// Fetches the accounts in one request, so every account returned is from the same slot.
async fn fetch(addresses: &[&str]) -> Vec<(Pubkey, Vec<u8>)> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let pubkeys: Vec<Pubkey> = addresses
        .iter()
        .map(|a| Pubkey::from_str_const(a))
        .collect();

    client
        .get_multiple_accounts(&pubkeys, CommitmentConfig::confirmed())
        .await
        .unwrap_or_else(|e| panic!("failed to fetch {addresses:?} from mainnet: {e}"))
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| match result {
            GetAccountResult::FoundAccount(_, account, _)
            | GetAccountResult::FoundCoupledAccount((_, account), _, _) => {
                assert_eq!(
                    account.owner.to_string(),
                    AMM_V4_PROGRAM,
                    "{address} is not owned by AMM v4 any more"
                );
                (account.owner, account.data)
            }
            GetAccountResult::None(_) => {
                panic!("{address} no longer exists on mainnet; the test needs a new address")
            }
        })
        .collect()
}

/// Byte indices at which two buffers differ.
fn diff_indices(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.iter()
        .zip(b.iter())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i)
        .collect()
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// A failure here means the bundled IDL disagrees with the live on-chain layout. With no
/// discriminator to anchor it, the decode starts at byte 0, so a single field of the wrong width
/// shifts everything after it and the re-encode no longer reproduces the account.
#[tokio::test]
async fn real_mainnet_pools_round_trip_unchanged() {
    let accounts = fetch(&[SOL_USDC, RAY_USDC, RAY_SOL]).await;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let idl = &registry
        .get("raydium-amm-pool-state")
        .expect("template")
        .idl;
    let pubkey = Pubkey::new_unique();

    for (address, (_, data)) in [SOL_USDC, RAY_USDC, RAY_SOL].iter().zip(&accounts) {
        assert_eq!(
            data.len(),
            AMM_INFO_LEN,
            "{address} is not an AmmInfo-sized account"
        );

        let forged = surfnet_svm
            .get_forged_account_data(&pubkey, data, idl, &HashMap::new())
            .unwrap_or_else(|e| {
                panic!("live pool {address} failed to decode/re-encode with the bundled IDL: {e}")
            });

        assert_eq!(forged.len(), data.len(), "{address} changed size");
        let diffs = diff_indices(&forged, data);
        assert!(
            diffs.is_empty(),
            "live pool {} was altered by a no-op round-trip at {} byte(s), first at {:?}",
            address,
            diffs.len(),
            diffs.first()
        );
    }
}

/// Catches collateral damage from the Borsh re-encode against real padding and live values,
/// which a synthetic account cannot exercise.
#[tokio::test]
async fn override_on_a_real_pool_touches_only_target_bytes() {
    let accounts = fetch(&[SOL_USDC]).await;
    let data = &accounts[0].1;

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let pubkey = Pubkey::new_unique();

    let live_fee = read_u64(data, SWAP_FEE_NUMERATOR);
    assert_eq!(
        live_fee, 25,
        "the live SOL/USDC pool should charge the default 25/10000 swap fee"
    );

    let fees = registry.get("raydium-amm-fees").expect("template");
    let forged = surfnet_svm
        .get_forged_account_data(
            &pubkey,
            data,
            &fees.idl,
            &HashMap::from([(
                "fees.swap_fee_numerator".to_string(),
                serde_json::json!(1_000u64),
            )]),
        )
        .expect("fee override on the live pool");

    assert!(
        diff_indices(&forged, data)
            .iter()
            .all(|i| (SWAP_FEE_NUMERATOR..SWAP_FEE_NUMERATOR + 8).contains(i)),
        "only fees.swap_fee_numerator may change, got {:?}",
        diff_indices(&forged, data)
    );
    assert_eq!(read_u64(&forged, SWAP_FEE_NUMERATOR), 1_000);
    assert_eq!(
        read_u64(&forged, NEED_TAKE_PNL_COIN),
        read_u64(data, NEED_TAKE_PNL_COIN),
        "the field after the fee block must not move"
    );

    let state = registry.get("raydium-amm-pool-state").expect("template");
    let live_status = read_u64(data, STATUS);
    assert_eq!(live_status, 6, "the live SOL/USDC pool should be swap-only");

    let forged = surfnet_svm
        .get_forged_account_data(
            &pubkey,
            data,
            &state.idl,
            &HashMap::from([("status".to_string(), serde_json::json!(2u64))]),
        )
        .expect("status override on the live pool");

    assert_eq!(
        diff_indices(&forged, data),
        vec![STATUS],
        "only status' low byte changes when 6 becomes 2"
    );
    assert_eq!(read_u64(&forged, STATUS), 2);
}

/// The whole point of the size-based account selection: a pool whose leading bytes are all zero
/// is still an `AmmInfo`. Under the fabricated discriminators this integration shipped with, a
/// zero-status pool matched `AmmInfo`'s all-zero discriminator and was then decoded from byte 8,
/// writing every field eight bytes off.
#[tokio::test]
async fn a_zero_status_pool_still_resolves_to_amm_info() {
    let accounts = fetch(&[SOL_USDC]).await;
    let mut data = accounts[0].1.clone();
    data[STATUS..STATUS + 8].copy_from_slice(&0u64.to_le_bytes());

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let idl = &registry
        .get("raydium-amm-pool-state")
        .expect("template")
        .idl;

    let forged = surfnet_svm
        .get_forged_account_data(
            &Pubkey::new_unique(),
            &data,
            idl,
            &HashMap::from([("status".to_string(), serde_json::json!(1u64))]),
        )
        .expect("an uninitialized pool must still decode as AmmInfo");

    assert_eq!(
        diff_indices(&forged, &data),
        vec![STATUS],
        "a zero-status pool must not be decoded at an eight-byte offset"
    );
    assert_eq!(read_u64(&forged, STATUS), 1);
    assert_eq!(
        &forged[COIN_VAULT..COIN_VAULT + 32],
        &data[COIN_VAULT..COIN_VAULT + 32],
        "the coin vault must survive untouched"
    );
}

/// `TargetOrders` is the other account type the IDL declares, and the only thing separating it
/// from `AmmInfo` is its declared size. A live one proves the pair is genuinely distinguishable
/// rather than distinguishable in theory.
#[tokio::test]
async fn a_live_target_orders_account_resolves_to_its_own_type() {
    let pools = fetch(&[SOL_USDC]).await;
    let pool = &pools[0].1;
    let target_orders = Pubkey::try_from(&pool[TARGET_ORDERS..TARGET_ORDERS + 32])
        .unwrap()
        .to_string();

    let accounts = fetch(&[target_orders.as_str()]).await;
    let data = &accounts[0].1;
    assert_eq!(
        data.len(),
        TARGET_ORDERS_LEN,
        "{target_orders} is not a TargetOrders-sized account"
    );

    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let registry = TemplateRegistry::new();
    let idl = &registry
        .get("raydium-amm-pool-state")
        .expect("template")
        .idl;

    let forged = surfnet_svm
        .get_forged_account_data(&Pubkey::new_unique(), data, idl, &HashMap::new())
        .expect("a 2208-byte account must resolve to TargetOrders");
    assert_eq!(
        forged, *data,
        "the live TargetOrders was altered by a no-op round-trip"
    );
}
