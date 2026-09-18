//! Shared plumbing for tests that read a live cluster over RPC.
//!
//! The endpoint comes from `SURFPOOL_TEST_RPC_URL` and defaults to the public mainnet endpoint,
//! which is the cluster every current module targets. Set the variable to a private endpoint if
//! the public one rate-limits. Nothing here is specific to a cluster; each module's hardcoded
//! addresses decide which one it needs.

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::DEFAULT_MAINNET_RPC_URL;

use crate::surfnet::remote::SurfnetRemoteClient;

pub const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";

/// The endpoint under test: the override if set, otherwise the public mainnet endpoint.
pub fn url() -> String {
    std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_MAINNET_RPC_URL.to_string())
}

pub fn client() -> SurfnetRemoteClient {
    SurfnetRemoteClient::new(url())
}

/// Fetches the accounts in one request, so every account returned is from the same slot.
pub async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    // A remote read can fail for reasons unrelated to what the callers assert (rate limit, 5xx,
    // timeout), so retry a few times with backoff before treating it as a failure.
    let mut errors = Vec::new();
    let results = loop {
        match client()
            .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
            .await
        {
            Ok(results) => break results,
            Err(error) => {
                errors.push(error.to_string());
                if errors.len() > 4 {
                    panic!("failed to fetch {addresses:?} from {}: {errors:#?}", url());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500 * errors.len() as u64))
                    .await;
            }
        }
    };

    results
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| {
            result.map_account().unwrap_or_else(|_| {
                panic!(
                    "{address} does not exist at {}; check {RPC_URL_ENV} points at the cluster \
                     this module expects, or give the integration a new address",
                    url()
                )
            })
        })
        .collect()
}
