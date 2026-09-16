use std::collections::HashMap;

use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;

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

    let accounts = accounts
        .into_iter()
        .map(|(address, encoded)| {
            let account: Account = encoded.to_account().ok_or_else(|| {
                SurfpoolError::internal(format!("Could not decode HumidiFi market {address}"))
            })?;
            let mints = HumidiFiMarket::mint_addresses(&account)?;
            Ok((address, account, mints))
        })
        .collect::<SurfpoolResult<Vec<_>>>()?;

    let mut mints = accounts
        .iter()
        .flat_map(|(_, _, (base, quote))| [*base, *quote])
        .collect::<Vec<_>>();
    mints.sort_unstable();
    mints.dedup();
    let mut mint_accounts = HashMap::new();
    for batch in mints.chunks(100) {
        let fetched = client
            .get_multiple_accounts(batch, CommitmentConfig::confirmed())
            .await?;
        for (address, account) in batch.iter().zip(fetched) {
            mint_accounts.insert(*address, account.map_account()?);
        }
    }

    let mut markets = accounts
        .iter()
        .map(|(address, account, (base, quote))| {
            let mint = |address| {
                mint_accounts.get(address).ok_or_else(|| {
                    SurfpoolError::internal(format!("HumidiFi mint {address} was not found"))
                })
            };
            HumidiFiMarket::validate(*address, account, mint(base)?, mint(quote)?)
        })
        .collect::<SurfpoolResult<Vec<_>>>()?;
    markets.sort_by_cached_key(|market| (market.label(), market.address));
    Ok(markets)
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
}
