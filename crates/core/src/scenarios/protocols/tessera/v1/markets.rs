use std::collections::HashMap;

use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use super::{TESSERA_DEFAULT_MARKET, TESSERA_PROGRAM_ID, TesseraMarket, fair_value::MARKET_LAYOUT};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    surfnet::remote::SurfnetRemoteClient,
};

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

    // One obsolete or malformed market must not hide every valid one, so a market that fails to
    // decode or validate is skipped with a warning and the rest of the catalog is still returned.
    // This is the same warn-and-continue rule the materializer applies per override.
    let candidates = accounts.len();
    let mut retained = Vec::new();
    for (address, encoded) in accounts {
        let Some(account) = encoded.to_account() else {
            warn!("Skipping Tessera market {address}: its account data could not be decoded");
            continue;
        };
        match TesseraMarket::mint_addresses(&account) {
            Ok(mints) => retained.push((address, account, mints)),
            Err(error) => warn!("Skipping Tessera market {address}: {error}"),
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
                warn!("Skipping {} Tessera mints: {error}", batch.len());
                continue;
            }
        };
        for (address, account) in batch.iter().zip(fetched) {
            match account.map_account() {
                Ok(account) => {
                    mint_accounts.insert(*address, account);
                }
                Err(error) => warn!("Skipping Tessera mint {address}: {error}"),
            }
        }
    }

    let mut markets = Vec::new();
    for (address, account, (base, quote)) in &accounts {
        let mint = |address| {
            mint_accounts.get(address).ok_or_else(|| {
                SurfpoolError::internal(format!("Tessera mint {address} was not found"))
            })
        };
        match mint(base)
            .and_then(|base| Ok((base, mint(quote)?)))
            .and_then(|(base, quote)| TesseraMarket::validate(*address, account, base, quote))
        {
            Ok(market) => markets.push(market),
            Err(error) => warn!("Skipping Tessera market {address}: {error}"),
        }
    }
    // An empty catalog from a program that does own markets is a failure, not a partial result.
    if markets.is_empty() && candidates > 0 {
        return Err(SurfpoolError::internal(format!(
            "none of the {candidates} discovered Tessera markets validated; the integration needs a refresh"
        )));
    }
    markets.sort_by_cached_key(|market| {
        (
            market.address != TESSERA_DEFAULT_MARKET,
            market.label(),
            market.address,
        )
    });
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
) -> SurfpoolResult<TesseraMarket> {
    let mint = |address: &Pubkey| {
        mint_accounts
            .get(address)
            .ok_or_else(|| SurfpoolError::internal(format!("Tessera mint {address} was not found")))
    };
    let (base, quote) = mints;
    TesseraMarket::validate(address, account, mint(&base)?, mint(&quote)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_market_whose_mints_are_missing_fails_alone() {
        let market = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let quote = Pubkey::new_unique();
        let account = Account {
            data: vec![0; MARKET_LAYOUT.account_size],
            owner: TESSERA_PROGRAM_ID,
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
