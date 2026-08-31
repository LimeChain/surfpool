//! Shared plumbing for tests that read mainnet.
//!
//! Set `SURFPOOL_TEST_RPC_URL` to use a private endpoint if the public one rate-limits.

use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use crate::surfnet::remote::SurfnetRemoteClient;

pub const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

pub fn client() -> SurfnetRemoteClient {
    SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    )
}

/// Fetches the accounts in one request, so every account returned is from the same slot.
pub async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    // The public endpoint throttles and intermittently 503s, which has nothing to do with what
    // the callers assert. Retry a few times with backoff so a transient refusal is not read as a
    // failure.
    let mut attempt = 0;
    let results = loop {
        match client()
            .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
            .await
        {
            Ok(results) => break results,
            Err(error) if attempt < 4 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
                let _ = error;
            }
            Err(error) => panic!("failed to fetch {addresses:?} from mainnet: {error}"),
        }
    };

    results
        .into_iter()
        .zip(addresses)
        .map(|(result, address)| {
            result.map_account().unwrap_or_else(|_| {
                panic!("{address} no longer exists on mainnet; the integration needs a new address")
            })
        })
        .collect()
}

/// The offsets at which two buffers differ.
pub fn diff_indices(left: &[u8], right: &[u8]) -> Vec<usize> {
    left.iter()
        .zip(right)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(index, _)| index)
        .collect()
}
