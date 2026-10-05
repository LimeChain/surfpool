use std::collections::{HashMap, HashSet};

use log::warn;
use serde_json::Value;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use surfpool_types::{
    AccountAddress, ConstantOption, OverrideTemplate, verified_tokens::VERIFIED_TOKENS,
};

use crate::surfnet::remote::SurfnetRemoteClient;

const PROGRAM: Pubkey = Pubkey::from_str_const("TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH");
const MARKET_SIZE: usize = 1264;
const MARKET_TAG: [u8; 8] = [5, 0, 0, 0, 0, 0, 0, 0];
const WSOL: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const USDC: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const MAX_MULTIPLE_ACCOUNTS: usize = 100;

/// Lists every priced Tessera market on the surfnet at `rpc_url` as the `market` options of the
/// Tessera templates and points a template without an address at the first one. An unreachable
/// surfnet lists none.
pub async fn fill_market_options(rpc_url: &str, templates: &mut [OverrideTemplate]) {
    if !templates
        .iter()
        .any(|template| template.protocol == "Tessera")
    {
        return;
    }
    let options = match fetch_markets(&SurfnetRemoteClient::new(rpc_url)).await {
        Ok((markets, decimals)) => market_options(&markets, &decimals),
        Err(error) => {
            warn!("unable to list Tessera markets: {error}");
            return;
        }
    };
    for template in templates
        .iter_mut()
        .filter(|template| template.protocol == "Tessera")
    {
        if let Some(market) = template.constants.get_mut("market") {
            market.options = options.clone();
        }
        if template.address == AccountAddress::Pubkey(String::new())
            && let Some(first) = options.first()
        {
            template.address = AccountAddress::Pubkey(first.value.clone());
        }
    }
}

type Markets = Vec<(Pubkey, Vec<u8>)>;

async fn fetch_markets(
    client: &SurfnetRemoteClient,
) -> Result<(Markets, HashMap<Pubkey, u8>), String> {
    let config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        ..Default::default()
    };
    let filters = vec![
        RpcFilterType::DataSize(MARKET_SIZE as u64),
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(96, MARKET_TAG.to_vec())),
    ];
    let markets: Markets = client
        .get_program_accounts(&PROGRAM, config, Some(filters))
        .await
        .and_then(|result| result.into_result())
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|(pubkey, account)| Some((pubkey, account.data.decode()?)))
        .filter(|(_, data)| data.len() == MARKET_SIZE)
        .collect();
    let mints: Vec<Pubkey> = markets
        .iter()
        .flat_map(|(_, data)| [pubkey_at(data, 24), pubkey_at(data, 56)])
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut decimals = HashMap::new();
    for chunk in mints.chunks(MAX_MULTIPLE_ACCOUNTS) {
        let results = client
            .get_multiple_accounts(chunk, CommitmentConfig::confirmed())
            .await
            .map_err(|e| e.to_string())?;
        for (mint, result) in chunk.iter().zip(results) {
            if let Ok(account) = result.map_account()
                && let Some(value) = account.data.get(44)
            {
                decimals.insert(*mint, *value);
            }
        }
    }
    Ok((markets, decimals))
}

/// One option per market with a price in both directions and known mint decimals: SOL / USDC
/// first, then the most recently quoted. A market without a price has every level disabled and
/// cannot fill; halts and freshness overrides leave the price alone, so they never hide a market.
/// Nothing is filtered by age, so no market drops out as a fork ages or time travels; the ones a
/// maker stopped quoting sink to the end.
pub fn market_options(
    markets: &[(Pubkey, Vec<u8>)],
    decimals: &HashMap<Pubkey, u8>,
) -> Vec<ConstantOption> {
    let markets = markets.iter().filter(|(_, data)| {
        data.len() == MARKET_SIZE && u64_at(data, 128) > 0 && u64_at(data, 144) > 0
    });
    let mut options: Vec<(bool, u64, ConstantOption)> = markets
        .filter_map(|(address, data)| {
            let (base, quote) = (pubkey_at(data, 24), pubkey_at(data, 56));
            let (base_decimals, quote_decimals) = (*decimals.get(&base)?, *decimals.get(&quote)?);
            let last_update = u64_at(data, 120);
            let (base_symbol, quote_symbol) = (symbol(&base), symbol(&quote));
            let metadata = HashMap::from([
                ("account".to_string(), Value::from(address.to_string())),
                (
                    "pair".to_string(),
                    Value::from(format!("{base_symbol}/{quote_symbol}")),
                ),
                ("base_mint".to_string(), Value::from(base.to_string())),
                ("quote_mint".to_string(), Value::from(quote.to_string())),
                ("base_decimals".to_string(), Value::from(base_decimals)),
                ("quote_decimals".to_string(), Value::from(quote_decimals)),
                (
                    "freshness_limit_slots".to_string(),
                    Value::from(u64_at(data, 88)),
                ),
                ("last_update_slot".to_string(), Value::from(last_update)),
            ]);
            let option = ConstantOption {
                id: address.to_string(),
                label: format!("{base_symbol} / {quote_symbol}"),
                description: None,
                value: address.to_string(),
                metadata,
            };
            Some(((base, quote) != (WSOL, USDC), last_update, option))
        })
        .collect();
    options.sort_by_key(|(not_default, last_update, _)| {
        (*not_default, std::cmp::Reverse(*last_update))
    });
    options.into_iter().map(|(_, _, option)| option).collect()
}

fn symbol(mint: &Pubkey) -> String {
    let address = mint.to_string();
    VERIFIED_TOKENS
        .iter()
        .find(|token| token.address == address)
        .map(|token| token.symbol.clone())
        .unwrap_or_else(|| address[..4].to_string())
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::try_from(&data[offset..offset + 32]).expect("market accounts hold 32-byte mints")
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use solana_pubkey::Pubkey;
    use surfpool_types::AccountAddress;

    use super::{MARKET_SIZE, MARKET_TAG, USDC, WSOL, fill_market_options, market_options};
    use crate::scenarios::TemplateRegistry;

    const USDT: Pubkey = Pubkey::from_str_const("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");

    fn market(base: &Pubkey, quote: &Pubkey, last_update: u64) -> Vec<u8> {
        let mut data = vec![0u8; MARKET_SIZE];
        data[24..56].copy_from_slice(base.as_ref());
        data[56..88].copy_from_slice(quote.as_ref());
        data[88..96].copy_from_slice(&20u64.to_le_bytes());
        data[96..104].copy_from_slice(&MARKET_TAG);
        data[120..128].copy_from_slice(&last_update.to_le_bytes());
        data[128..136].copy_from_slice(&1u64.to_le_bytes());
        data[144..152].copy_from_slice(&1u64.to_le_bytes());
        data
    }

    #[test]
    fn sol_usdc_comes_first_then_the_newest_and_old_markets_stay_listed() {
        let unnamed = Pubkey::new_from_array([7; 32]);
        let unknown_decimals = Pubkey::new_from_array([8; 32]);
        let mut unpriced = market(&USDT, &USDC, 1_000_000);
        unpriced[128..136].fill(0);
        let markets = vec![
            (Pubkey::new_from_array([5; 32]), unpriced),
            (Pubkey::new_from_array([1; 32]), market(&unnamed, &USDC, 1)),
            (
                Pubkey::new_from_array([2; 32]),
                market(&USDT, &USDC, 1_000_000),
            ),
            (
                Pubkey::new_from_array([3; 32]),
                market(&WSOL, &USDC, 999_990),
            ),
            (
                Pubkey::new_from_array([4; 32]),
                market(&unknown_decimals, &USDC, 1_000_000),
            ),
        ];
        let decimals = HashMap::from([(WSOL, 9), (USDC, 6), (USDT, 6), (unnamed, 6)]);

        let options = market_options(&markets, &decimals);

        let labels: Vec<_> = options.iter().map(|option| option.label.as_str()).collect();
        assert_eq!(
            labels,
            ["SOL / USDC", "USDT / USDC", "US51 / USDC"],
            "an unpriced or unknown-mint market is skipped, an old one stays at the end"
        );
        let sol = &options[0];
        assert_eq!(sol.value, Pubkey::new_from_array([3; 32]).to_string());
        assert_eq!(sol.metadata["pair"], "SOL/USDC");
        assert_eq!(sol.metadata["base_decimals"], 9);
        assert_eq!(sol.metadata["quote_decimals"], 6);
        assert_eq!(sol.metadata["freshness_limit_slots"], 20);
        assert_eq!(sol.metadata["last_update_slot"], 999_990);
    }

    #[tokio::test]
    async fn an_unreachable_surfnet_lists_no_markets_and_keeps_the_addresses() {
        let registry = TemplateRegistry::new();
        let mut templates: Vec<_> = registry
            .by_protocol("Tessera")
            .into_iter()
            .cloned()
            .collect();
        assert_eq!(templates.len(), 5);

        fill_market_options("http://127.0.0.1:1", &mut templates).await;

        for template in &templates {
            assert!(
                template.constants["market"].options.is_empty(),
                "{}",
                template.id
            );
            assert_eq!(
                template.address,
                AccountAddress::Pubkey(String::new()),
                "{}",
                template.id
            );
        }
    }
}
