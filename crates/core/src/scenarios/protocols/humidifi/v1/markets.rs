use std::collections::HashMap;

use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use super::{
    HUMIDIFI_PROGRAM_ID, HumidiFiMarket,
    fair_value::{MARKET_LAYOUT, SCHEMA_VERSION_OFFSET, schema_version_bytes},
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    surfnet::remote::SurfnetRemoteClient,
};

pub async fn discover_humidifi_markets(
    client: &SurfnetRemoteClient,
) -> SurfpoolResult<Vec<HumidiFiMarket>> {
    let accounts = client
        .get_program_accounts(
            &HUMIDIFI_PROGRAM_ID,
            RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            Some(discovery_filters()),
        )
        .await?
        .into_result()?;

    // One obsolete or malformed market must not hide every valid one, so a market that fails to
    // decode or validate is skipped with a warning and the rest of the catalog is still returned.
    // This is the same warn-and-continue rule the materializer applies per override.
    let candidates = accounts.len();
    let mut retained = Vec::new();
    for (address, encoded) in accounts {
        let Some(account) = encoded.to_account() else {
            warn!("Skipping HumidiFi market {address}: its account data could not be decoded");
            continue;
        };
        match HumidiFiMarket::mint_addresses(&account) {
            Ok(mints) => retained.push((address, account, mints)),
            Err(error) => warn!("Skipping HumidiFi market {address}: {error}"),
        }
    }
    let accounts = retained;

    let mut mints = accounts
        .iter()
        .flat_map(|(_, _, (base, quote))| [*base, *quote])
        .collect::<Vec<_>>();
    mints.sort_unstable();
    mints.dedup();
    let mut mint_accounts = HashMap::new();
    for batch in mints.chunks(100) {
        // A datasource failure or an unreadable mint disqualifies only the markets that point at
        // it, which the validation below reports per market.
        let fetched = match client
            .get_multiple_accounts(batch, CommitmentConfig::confirmed())
            .await
        {
            Ok(fetched) => fetched,
            Err(error) => {
                warn!("Skipping {} HumidiFi mints: {error}", batch.len());
                continue;
            }
        };
        for (address, account) in batch.iter().zip(fetched) {
            match account.map_account() {
                Ok(account) => {
                    mint_accounts.insert(*address, account);
                }
                Err(error) => warn!("Skipping HumidiFi mint {address}: {error}"),
            }
        }
    }

    let mut markets = Vec::new();
    for (address, account, mints) in &accounts {
        match resolve_market(*address, account, *mints, &mint_accounts) {
            Ok(market) => markets.push(market),
            Err(error) => warn!("Skipping HumidiFi market {address}: {error}"),
        }
    }
    // An empty catalog from a program that does own markets is a failure, not a partial result.
    if markets.is_empty() && candidates > 0 {
        return Err(SurfpoolError::internal(format!(
            "none of the {candidates} discovered HumidiFi markets validated; the integration needs a refresh"
        )));
    }
    markets.sort_by_cached_key(|market| (market.label(), market.address));
    Ok(markets)
}

/// Validates one discovered market against the mint accounts fetched for the whole catalog. A
/// market whose mints are missing or unreadable fails here alone, so the rest of the catalog
/// still resolves.
fn resolve_market(
    address: Pubkey,
    account: &Account,
    mints: (Pubkey, Pubkey),
    mint_accounts: &HashMap<Pubkey, Account>,
) -> SurfpoolResult<HumidiFiMarket> {
    let mint = |address: &Pubkey| {
        mint_accounts.get(address).ok_or_else(|| {
            SurfpoolError::internal(format!("HumidiFi mint {address} was not found"))
        })
    };
    let (base, quote) = mints;
    HumidiFiMarket::validate(address, account, mint(&base)?, mint(&quote)?)
}

pub(super) fn discovery_filters() -> Vec<RpcFilterType> {
    let mut filters = vec![RpcFilterType::DataSize(MARKET_LAYOUT.account_size as u64)];
    if let Some(magic) = &MARKET_LAYOUT.magic {
        filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            magic.offset,
            magic.bytes.clone(),
        )));
    }
    filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
        SCHEMA_VERSION_OFFSET,
        schema_version_bytes().to_vec(),
    )));
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_filters_match_the_validator_gate() {
        let filters = discovery_filters();
        let magic = MARKET_LAYOUT.magic.as_ref().unwrap();
        let [
            RpcFilterType::DataSize(size),
            RpcFilterType::Memcmp(tag),
            RpcFilterType::Memcmp(version),
        ] = &filters[..]
        else {
            panic!("expected size, magic and schema-version filters");
        };
        assert_eq!(*size, 1728);
        assert_eq!(tag.offset(), 8);
        assert_eq!(tag.bytes().unwrap().as_ref(), &magic.bytes);
        assert_eq!(version.offset(), 1720);
        assert_eq!(version.bytes().unwrap().as_slice(), schema_version_bytes());
        assert_eq!(schema_version_bytes(), 8u64.to_le_bytes());

        let mut data = vec![0; MARKET_LAYOUT.account_size];
        data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        data[SCHEMA_VERSION_OFFSET..SCHEMA_VERSION_OFFSET + 8].copy_from_slice(&8u64.to_le_bytes());
        assert!(tag.bytes_match(&data));
        assert!(version.bytes_match(&data));
        data[SCHEMA_VERSION_OFFSET..SCHEMA_VERSION_OFFSET + 8].copy_from_slice(&5u64.to_le_bytes());
        assert!(tag.bytes_match(&data));
        assert!(!version.bytes_match(&data));
    }

    #[test]
    fn a_market_whose_mints_are_missing_fails_alone() {
        let market = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let quote = Pubkey::new_unique();
        let account = Account {
            data: vec![0; MARKET_LAYOUT.account_size],
            owner: HUMIDIFI_PROGRAM_ID,
            ..Account::default()
        };

        let error = resolve_market(market, &account, (base, quote), &HashMap::new())
            .expect_err("a market with no mint accounts must not resolve");
        assert!(
            error.to_string().contains(&base.to_string()),
            "the error must name the missing mint: {error}"
        );
    }
}
