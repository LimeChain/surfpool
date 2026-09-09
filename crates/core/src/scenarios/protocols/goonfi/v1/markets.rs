use std::collections::HashMap;

use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::VERIFIED_TOKENS_BY_SYMBOL;

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
    surfnet::remote::SurfnetRemoteClient,
    types::MintAccount,
};

use super::{GOONFI_DEFAULT_MARKET, GOONFI_PROGRAM_ID, GoonfiMarket};

#[derive(Debug, PartialEq)]
pub struct GoonfiDiscoveredMarket {
    pub address: Pubkey,
    pub oracle: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_decimals: u8,
    pub quote_decimals: u8,
}

impl GoonfiDiscoveredMarket {
    pub fn label(&self) -> String {
        market_label(&self.base_mint, &self.quote_mint)
    }
}

/// A human pair label from the two mints, e.g. "SOL/USDC". Falls back to a mint's full address
/// when it is not in the verified token list, so an unknown pair is still uniquely named.
pub fn market_label(base_mint: &Pubkey, quote_mint: &Pubkey) -> String {
    let symbol = |mint: &Pubkey| {
        let address = mint.to_string();
        VERIFIED_TOKENS_BY_SYMBOL
            .values()
            .filter(|token| token.address == address)
            .map(|token| token.symbol.as_str())
            .min()
            .map(str::to_string)
            .unwrap_or(address)
    };
    format!("{}/{}", symbol(base_mint), symbol(quote_mint))
}

fn market_references(account: &Account) -> SurfpoolResult<[Pubkey; 3]> {
    let oracle = GoonfiMarket::oracle_address(account)?;
    let base = Pubkey::new_from_array(account.data[80..112].try_into().unwrap());
    let quote = Pubkey::new_from_array(account.data[112..144].try_into().unwrap());
    if base == Pubkey::default() || quote == Pubkey::default() || base == quote {
        return Err(SurfpoolError::internal(
            "GoonFi market has invalid mint identities",
        ));
    }
    Ok([base, quote, oracle])
}

fn mint_decimals(account: &Account) -> SurfpoolResult<u8> {
    if account.owner != spl_token_interface::ID && account.owner != spl_token_2022_interface::ID {
        return Err(SurfpoolError::internal(
            "GoonFi mint is not owned by a supported token program",
        ));
    }
    Ok(MintAccount::unpack(&account.data)?.decimals())
}

fn resolve_market(
    address: Pubkey,
    account: &Account,
    references: &HashMap<Pubkey, Account>,
) -> SurfpoolResult<GoonfiDiscoveredMarket> {
    let [base, quote, oracle] = market_references(account)?;
    let required = |address: &Pubkey| {
        references.get(address).ok_or_else(|| {
            SurfpoolError::internal(format!("GoonFi referenced account {address} was not found"))
        })
    };
    GoonfiMarket::validate(address, account, required(&oracle)?)?;
    Ok(GoonfiDiscoveredMarket {
        address,
        oracle,
        base_mint: base,
        quote_mint: quote,
        base_decimals: mint_decimals(required(&base)?)?,
        quote_decimals: mint_decimals(required(&quote)?)?,
    })
}

pub async fn discover_goonfi_markets(
    client: &SurfnetRemoteClient,
) -> SurfpoolResult<Vec<GoonfiDiscoveredMarket>> {
    let registry = TemplateRegistry::new();
    let layout = registry
        .get("goonfi-reference-band")
        .and_then(|template| template.raw_layout.as_ref())
        .ok_or_else(|| SurfpoolError::internal("GoonFi market layout is unavailable"))?;
    let mut filters = vec![RpcFilterType::DataSize(layout.account_size as u64)];
    if let Some(magic) = &layout.magic {
        filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            magic.offset,
            magic.bytes.clone(),
        )));
    }
    let accounts = client
        .get_program_accounts(
            &GOONFI_PROGRAM_ID,
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
                SurfpoolError::internal(format!("Could not decode GoonFi market {address}"))
            })?;
            market_references(&account)?;
            Ok((address, account))
        })
        .collect::<SurfpoolResult<Vec<_>>>()?;
    let mut addresses = Vec::new();
    for (_, account) in &accounts {
        addresses.extend(market_references(account)?);
    }
    addresses.sort_unstable();
    addresses.dedup();
    let mut references = HashMap::new();
    for batch in addresses.chunks(100) {
        let fetched = client
            .get_multiple_accounts(batch, CommitmentConfig::confirmed())
            .await?;
        for (address, account) in batch.iter().zip(fetched) {
            references.insert(*address, account.map_account()?);
        }
    }
    let mut markets = accounts
        .iter()
        .map(|(address, account)| resolve_market(*address, account, &references))
        .collect::<SurfpoolResult<Vec<_>>>()?;
    markets.sort_by_cached_key(|market| {
        (
            market.address != GOONFI_DEFAULT_MARKET,
            market.label(),
            market.address,
        )
    });
    Ok(markets)
}

#[cfg(test)]
mod tests {
    use solana_program_pack::Pack;

    use super::*;
    use crate::scenarios::protocols::goonfi::v1::GOONFI_ORACLE_PROGRAM_ID;

    fn fixture() -> (Pubkey, Account, HashMap<Pubkey, Account>) {
        let address = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let quote = Pubkey::new_unique();
        let oracle = Pubkey::new_unique();
        let registry = TemplateRegistry::new();
        let layout = registry
            .get("goonfi-reference-band")
            .unwrap()
            .raw_layout
            .as_ref()
            .unwrap();
        let mut market = Account {
            owner: GOONFI_PROGRAM_ID,
            data: vec![0; layout.account_size],
            ..Account::default()
        };
        let magic = layout.magic.as_ref().unwrap();
        market.data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        market.data[80..112].copy_from_slice(base.as_ref());
        market.data[112..144].copy_from_slice(quote.as_ref());
        market.data[208..240].copy_from_slice(oracle.as_ref());
        let mint = |decimals| {
            let mut account = Account {
                owner: spl_token_interface::ID,
                data: vec![0; spl_token_interface::state::Mint::LEN],
                ..Account::default()
            };
            spl_token_interface::state::Mint {
                decimals,
                is_initialized: true,
                ..Default::default()
            }
            .pack_into_slice(&mut account.data);
            account
        };
        (
            address,
            market,
            HashMap::from([
                (base, mint(9)),
                (quote, mint(6)),
                (
                    oracle,
                    Account {
                        owner: GOONFI_ORACLE_PROGRAM_ID,
                        data: vec![0; 32],
                        ..Account::default()
                    },
                ),
            ]),
        )
    }

    #[test]
    fn goonfi_discovery_accepts_uncataloged_markets_and_preserves_mint_identity() {
        let (address, account, references) = fixture();
        let result = resolve_market(address, &account, &references).unwrap();
        assert_eq!(result.address, address);
        assert_eq!((result.base_decimals, result.quote_decimals), (9, 6));
        assert_eq!(
            result.label(),
            format!("{}/{}", result.base_mint, result.quote_mint)
        );
        assert_eq!(result.oracle, market_references(&account).unwrap()[2]);
    }

    #[test]
    fn goonfi_discovery_rejects_invalid_market_layouts_and_mint_identities() {
        let (_, account, _) = fixture();
        for invalid in 0..5 {
            let mut account = account.clone();
            match invalid {
                0 => account.owner = Pubkey::new_unique(),
                1 => {
                    account.data.pop();
                }
                2 => account.data[0] ^= 1,
                3 => account.data[80..112].fill(0),
                _ => {
                    let base = account.data[80..112].to_vec();
                    account.data[112..144].copy_from_slice(&base);
                }
            }
            assert!(
                market_references(&account).is_err(),
                "invalid case {invalid}"
            );
        }
    }

    #[test]
    fn goonfi_discovery_rejects_missing_or_invalid_referenced_accounts() {
        let (address, account, references) = fixture();
        let [base, _, oracle] = market_references(&account).unwrap();
        for invalid in 0..5 {
            let mut references = references.clone();
            match invalid {
                0 => {
                    references.remove(&oracle);
                }
                1 => references.get_mut(&oracle).unwrap().owner = Pubkey::new_unique(),
                2 => {
                    references.get_mut(&oracle).unwrap().data.pop();
                }
                3 => references.get_mut(&base).unwrap().owner = Pubkey::new_unique(),
                _ => references.get_mut(&base).unwrap().data.fill(0),
            }
            assert!(
                resolve_market(address, &account, &references).is_err(),
                "invalid case {invalid}"
            );
        }
    }
}
