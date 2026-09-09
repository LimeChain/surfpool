use std::collections::HashMap;

use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    surfnet::remote::SurfnetRemoteClient,
};

use super::{TESSERA_DEFAULT_MARKET, TESSERA_PROGRAM_ID, TesseraMarket, fair_value::MARKET_LAYOUT};

pub async fn discover_tessera_markets(
    client: &SurfnetRemoteClient,
) -> SurfpoolResult<Vec<TesseraMarket>> {
    let mut filters = vec![RpcFilterType::DataSize(MARKET_LAYOUT.account_size as u64)];
    if let Some(magic) = &MARKET_LAYOUT.magic {
        filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            magic.offset,
            magic.bytes.clone(),
        )));
    }
    let accounts = client
        .get_program_accounts(
            &TESSERA_PROGRAM_ID,
            RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            Some(filters),
        )
        .await?
        .into_result()?;

    let accounts = accounts
        .into_iter()
        .map(|(address, encoded)| {
            let account: Account = encoded.to_account().ok_or_else(|| {
                SurfpoolError::internal(format!("Could not decode Tessera market {address}"))
            })?;
            let mints = TesseraMarket::mint_addresses(&account)?;
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
                    SurfpoolError::internal(format!("Tessera mint {address} was not found"))
                })
            };
            TesseraMarket::validate(*address, account, mint(base)?, mint(quote)?)
        })
        .collect::<SurfpoolResult<Vec<_>>>()?;
    markets.sort_by_cached_key(|market| {
        (
            market.address != TESSERA_DEFAULT_MARKET,
            market.label(),
            market.address,
        )
    });
    Ok(markets)
}
