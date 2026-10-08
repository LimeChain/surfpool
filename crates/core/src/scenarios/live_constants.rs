use std::{
    collections::HashMap,
    str::FromStr,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

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
    AccountAddress, AccountField, AccountFieldEncoding, ConstantOption, LiveConstantSource,
    OverrideTemplate, ProgramAccountsSource, option_slug, verified_tokens::VERIFIED_TOKENS,
};

use crate::surfnet::remote::SurfnetRemoteClient;

const CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_MULTIPLE_ACCOUNTS: usize = 100;

type CachedOptions = (Instant, Vec<ConstantOption>);

static CACHE: LazyLock<Mutex<HashMap<String, CachedOptions>>> = LazyLock::new(Default::default);

/// Fills every live constant of `templates` through the surfnet RPC at `rpc_url`, which serves
/// local state first and falls back to its datasource. A template whose address is an empty
/// pubkey targets the first option. A source that fails to resolve is served with no options.
pub async fn resolve_live_constants(
    rpc_url: &str,
    mut templates: Vec<OverrideTemplate>,
) -> Vec<OverrideTemplate> {
    let client = SurfnetRemoteClient::new(rpc_url);
    let mut resolved: HashMap<String, Vec<ConstantOption>> = HashMap::new();
    for template in &mut templates {
        let mut names: Vec<String> = template
            .constants
            .iter()
            .filter(|(_, constant)| constant.source.is_some())
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        for name in &names {
            let constant = template.constants.get_mut(name).expect("listed above");
            let Some(LiveConstantSource::ProgramAccounts(source)) = &constant.source else {
                continue;
            };
            let key = format!("{rpc_url} {source:?}");
            if !resolved.contains_key(&key) {
                let options = cached_options(&client, &key, source)
                    .await
                    .unwrap_or_else(|error| {
                        warn!(
                            "unable to resolve constant '{name}' of template {}: {error}",
                            template.id
                        );
                        Vec::new()
                    });
                resolved.insert(key.clone(), options);
            }
            constant.options = resolved[&key].clone();
        }
        if template.address == AccountAddress::Pubkey(String::new())
            && let Some(first) = names
                .iter()
                .find_map(|name| template.constants[name].options.first())
        {
            template.address = AccountAddress::Pubkey(first.value.clone());
        }
    }
    templates
}

async fn cached_options(
    client: &SurfnetRemoteClient,
    key: &str,
    source: &ProgramAccountsSource,
) -> Result<Vec<ConstantOption>, String> {
    let hit = CACHE.lock().ok().and_then(|cache| {
        cache
            .get(key)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, options)| options.clone())
    });
    if let Some(options) = hit {
        return Ok(options);
    }
    let options = fetch_program_account_options(client, source).await?;
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key.to_string(), (Instant::now(), options.clone()));
    }
    Ok(options)
}

/// Reads the source's accounts and their pair's mints, then builds the options.
pub async fn fetch_program_account_options(
    client: &SurfnetRemoteClient,
    source: &ProgramAccountsSource,
) -> Result<Vec<ConstantOption>, String> {
    let program = Pubkey::from_str(&source.program).map_err(|e| e.to_string())?;
    let mut filters = vec![RpcFilterType::DataSize(source.size as u64)];
    filters.extend(source.filters.iter().map(|filter| {
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(filter.offset, filter.bytes.clone()))
    }));
    let config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        ..Default::default()
    };
    let matched: Vec<Pubkey> = client
        .get_program_accounts(&program, config, Some(filters))
        .await
        .and_then(|result| result.into_result())
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(pubkey, _)| pubkey)
        .collect();
    // The surfnet merges remote program accounts with local ones, so an account overridden
    // locally can come back in its remote form; re-read each one local-first and re-apply
    // the filters to the bytes a scenario would actually see.
    let mut accounts: Vec<(Pubkey, Vec<u8>)> = Vec::with_capacity(matched.len());
    for chunk in matched.chunks(MAX_MULTIPLE_ACCOUNTS) {
        let results = client
            .get_multiple_accounts(chunk, CommitmentConfig::confirmed())
            .await
            .map_err(|e| e.to_string())?;
        for (pubkey, result) in chunk.iter().zip(results) {
            if let Ok(account) = result.map_account()
                && matches_source(source, &account.data)
            {
                accounts.push((*pubkey, account.data));
            }
        }
    }

    let mut mints: Vec<Pubkey> = accounts
        .iter()
        .flat_map(|(_, data)| {
            [&source.pair.base, &source.pair.quote]
                .into_iter()
                .filter_map(|name| read_pubkey(&source.fields[name], data))
        })
        .collect();
    mints.sort_unstable();
    mints.dedup();
    let mut mint_data = HashMap::new();
    for chunk in mints.chunks(MAX_MULTIPLE_ACCOUNTS) {
        let results = client
            .get_multiple_accounts(chunk, CommitmentConfig::confirmed())
            .await
            .map_err(|e| e.to_string())?;
        for (mint, result) in chunk.iter().zip(results) {
            if let Ok(account) = result.map_account() {
                mint_data.insert(*mint, account.data);
            }
        }
    }
    let current_slot = match source.fresh {
        Some(_) => {
            client
                .get_epoch_info()
                .await
                .map_err(|e| e.to_string())?
                .absolute_slot
        }
        None => 0,
    };
    Ok(program_account_options(
        source,
        &accounts,
        &mint_data,
        current_slot,
    ))
}

/// Builds the options for `accounts`, sorted by label with the `default` pair first. An account
/// whose pair mints are missing is skipped, since its decimals are unknown, and so is one that
/// `fresh` finds older than its window at `current_slot`.
pub fn program_account_options(
    source: &ProgramAccountsSource,
    accounts: &[(Pubkey, Vec<u8>)],
    mints: &HashMap<Pubkey, Vec<u8>>,
    current_slot: u64,
) -> Vec<ConstantOption> {
    let mut options: Vec<ConstantOption> = accounts
        .iter()
        .filter(|(_, data)| matches_source(source, data))
        .filter_map(|(address, data)| account_options(source, address, data, mints, current_slot))
        .flatten()
        .collect();

    let mut counts: HashMap<String, usize> = HashMap::new();
    for option in &options {
        *counts.entry(option.id.clone()).or_default() += 1;
    }
    for option in &mut options {
        if counts[&option.id] > 1 {
            let account = option.metadata["account"].as_str().unwrap_or_default();
            let short = account.get(..6).unwrap_or(account).to_string();
            option.id = format!("{}-{}", option.id, option_slug(&short));
            option.label = format!("{} ({short})", option.label);
        }
    }

    options.sort_by(|a, b| (a.label.to_lowercase(), &a.id).cmp(&(b.label.to_lowercase(), &b.id)));
    if let Some(default) = &source.default {
        options.sort_by_key(|option| {
            option.metadata[&source.pair.base] != default.base.as_str()
                || option.metadata[&source.pair.quote] != default.quote.as_str()
        });
    }
    options
}

fn account_options(
    source: &ProgramAccountsSource,
    address: &Pubkey,
    data: &[u8],
    mints: &HashMap<Pubkey, Vec<u8>>,
    current_slot: u64,
) -> Option<Vec<ConstantOption>> {
    if data.len() != source.size {
        return None;
    }
    if let Some(fresh) = &source.fresh {
        let slot = read_field(&source.fields[&fresh.field], data)?.as_u64()?;
        if slot.saturating_add(fresh.within_slots) < current_slot {
            return None;
        }
    }
    let pubkey_field = |name: &str| read_pubkey(&source.fields[name], data);
    let base = pubkey_field(&source.pair.base)?;
    let quote = pubkey_field(&source.pair.quote)?;
    let decimals = |mint: &Pubkey| mints.get(mint).and_then(|data| data.get(44)).copied();
    let (base_decimals, quote_decimals) = (decimals(&base)?, decimals(&quote)?);
    let (base_symbol, quote_symbol) = (symbol(&base), symbol(&quote));
    let label = format!("{base_symbol} / {quote_symbol}");
    let pair = format!("{base_symbol}/{quote_symbol}");
    let slug = option_slug(&pair);

    let mut metadata = HashMap::new();
    for (name, field) in &source.fields {
        metadata.insert(name.clone(), read_field(field, data)?);
    }
    metadata.insert("account".to_string(), Value::from(address.to_string()));
    metadata.insert("pair".to_string(), Value::from(pair));
    metadata.insert("base_decimals".to_string(), Value::from(base_decimals));
    metadata.insert("quote_decimals".to_string(), Value::from(quote_decimals));

    if source.expand.is_empty() {
        let value = match &source.value {
            Some(name) => pubkey_field(name)?,
            None => *address,
        };
        return Some(vec![ConstantOption {
            id: slug,
            label,
            description: None,
            value: value.to_string(),
            metadata,
        }]);
    }
    source
        .expand
        .iter()
        .map(|expansion| {
            let mut metadata = metadata.clone();
            metadata.extend(expansion.metadata.clone());
            Some(ConstantOption {
                id: format!("{slug}-{}", option_slug(&expansion.suffix)),
                label: format!("{label}{}", expansion.suffix),
                description: None,
                value: pubkey_field(&expansion.value)?.to_string(),
                metadata,
            })
        })
        .collect()
}

/// Whether `data` still has the source's size and tag bytes.
pub fn matches_source(source: &ProgramAccountsSource, data: &[u8]) -> bool {
    data.len() == source.size
        && source.filters.iter().all(|filter| {
            data.get(filter.offset..filter.offset + filter.bytes.len())
                == Some(filter.bytes.as_slice())
        })
}

fn symbol(mint: &Pubkey) -> String {
    let address = mint.to_string();
    VERIFIED_TOKENS
        .iter()
        .find(|token| token.address == address)
        .map(|token| token.symbol.clone())
        .unwrap_or_else(|| address[..4].to_string())
}

fn field_bytes(field: &AccountField, data: &[u8]) -> Option<Vec<u8>> {
    let mut bytes = data
        .get(field.offset..field.offset + field.encoding.width())?
        .to_vec();
    for (word, mask) in bytes.chunks_exact_mut(8).zip(&field.mask) {
        let unmasked = u64::from_le_bytes(word.try_into().ok()?) ^ mask;
        word.copy_from_slice(&unmasked.to_le_bytes());
    }
    Some(bytes)
}

fn read_pubkey(field: &AccountField, data: &[u8]) -> Option<Pubkey> {
    Pubkey::try_from(field_bytes(field, data)?.as_slice()).ok()
}

fn read_field(field: &AccountField, data: &[u8]) -> Option<Value> {
    let bytes = field_bytes(field, data)?;
    Some(match field.encoding {
        AccountFieldEncoding::Pubkey => {
            Value::from(Pubkey::try_from(bytes.as_slice()).ok()?.to_string())
        }
        AccountFieldEncoding::U64 => Value::from(u64::from_le_bytes(bytes.try_into().ok()?)),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use solana_pubkey::Pubkey;
    use surfpool_types::ProgramAccountsSource;

    use super::program_account_options;

    const WSOL: &str = "So11111111111111111111111111111111111111112";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn mint(decimals: u8) -> Vec<u8> {
        let mut data = vec![0u8; 82];
        data[44] = decimals;
        data
    }

    fn source(yaml: &str) -> ProgramAccountsSource {
        let source: ProgramAccountsSource = serde_yaml::from_str(yaml).unwrap();
        source.validate().unwrap();
        source
    }

    #[test]
    fn options_carry_the_pair_decimals_and_decoded_fields() {
        let source = source(
            r#"
program: "11111111111111111111111111111111"
size: 80
fields:
  base_mint: { offset: 0, encoding: pubkey }
  quote_mint: { offset: 32, encoding: pubkey }
  limit: { offset: 64, encoding: u64 }
pair: { base: base_mint, quote: quote_mint }
"#,
        );
        let (wsol, usdc) = (Pubkey::from_str_const(WSOL), Pubkey::from_str_const(USDC));
        let account = |limit: u64| {
            let mut data = [wsol.to_bytes(), usdc.to_bytes()].concat();
            data.extend(limit.to_le_bytes());
            data.extend([0u8; 8]);
            data
        };
        let (first, second, unknown) = (
            Pubkey::new_from_array([1; 32]),
            Pubkey::new_from_array([2; 32]),
            Pubkey::new_unique(),
        );
        let mut orphan = account(1);
        orphan[..32].copy_from_slice(unknown.as_ref());
        let accounts = vec![
            (first, account(20)),
            (second, account(25)),
            (Pubkey::new_unique(), orphan),
        ];
        let mints = HashMap::from([(wsol, mint(9)), (usdc, mint(6))]);

        let options = program_account_options(&source, &accounts, &mints, 0);

        assert_eq!(
            options.len(),
            2,
            "a market whose mint is missing is skipped"
        );
        let limits: Vec<_> = options
            .iter()
            .map(|o| o.metadata["limit"].clone())
            .collect();
        for option in &options {
            assert!(option.label.starts_with("SOL / USDC ("), "{}", option.label);
            assert!(option.id.starts_with("sol-usdc-"), "{}", option.id);
            assert_eq!(option.value, option.metadata["account"]);
            assert_eq!(option.metadata["pair"], "SOL/USDC");
            assert_eq!(option.metadata["base_decimals"], 9);
            assert_eq!(option.metadata["quote_decimals"], 6);
            assert_eq!(option.metadata["base_mint"], WSOL);
            assert_eq!(option.metadata["quote_mint"], USDC);
        }
        assert!(limits.contains(&20.into()) && limits.contains(&25.into()));
        assert_ne!(options[0].id, options[1].id);
    }

    #[test]
    fn masked_fields_and_expansions_pick_their_values() {
        let source = source(
            r#"
program: "11111111111111111111111111111111"
size: 128
fields:
  base_mint: { offset: 0, encoding: pubkey, mask: [1, 2, 3, 4] }
  quote_mint: { offset: 32, encoding: pubkey }
  base_vault: { offset: 64, encoding: pubkey }
  quote_vault: { offset: 96, encoding: pubkey }
pair: { base: base_mint, quote: quote_mint }
expand:
  - { value: base_vault, suffix: " base vault", metadata: { side: base } }
  - { value: quote_vault, suffix: " quote vault", metadata: { side: quote } }
"#,
        );
        let (wsol, usdc) = (Pubkey::from_str_const(WSOL), Pubkey::from_str_const(USDC));
        let (base_vault, quote_vault) = (Pubkey::new_unique(), Pubkey::new_unique());
        let mut masked = wsol.to_bytes();
        for (word, mask) in masked.chunks_exact_mut(8).zip([1u64, 2, 3, 4]) {
            let value = u64::from_le_bytes(word.try_into().unwrap()) ^ mask;
            word.copy_from_slice(&value.to_le_bytes());
        }
        let data = [
            masked,
            usdc.to_bytes(),
            base_vault.to_bytes(),
            quote_vault.to_bytes(),
        ]
        .concat();
        let mints = HashMap::from([(wsol, mint(9)), (usdc, mint(6))]);

        let options = program_account_options(&source, &[(Pubkey::new_unique(), data)], &mints, 0);

        let summary: Vec<_> = options
            .iter()
            .map(|o| {
                (
                    o.id.as_str(),
                    o.label.as_str(),
                    o.value.clone(),
                    o.metadata["side"].clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "sol-usdc-base-vault",
                    "SOL / USDC base vault",
                    base_vault.to_string(),
                    "base".into()
                ),
                (
                    "sol-usdc-quote-vault",
                    "SOL / USDC quote vault",
                    quote_vault.to_string(),
                    "quote".into()
                ),
            ]
        );
        assert_eq!(
            options[0].metadata["base_mint"], WSOL,
            "the mask is removed"
        );
    }

    const TWO_FIELD_SOURCE: &str = r#"
program: "11111111111111111111111111111111"
size: 72
fields:
  base_mint: { offset: 0, encoding: pubkey }
  quote_mint: { offset: 32, encoding: pubkey }
  last_update_slot: { offset: 64, encoding: u64 }
pair: { base: base_mint, quote: quote_mint }
"#;

    fn market(base: &Pubkey, quote: &Pubkey, slot: u64) -> Vec<u8> {
        let mut data = [base.to_bytes(), quote.to_bytes()].concat();
        data.extend(slot.to_le_bytes());
        data
    }

    #[test]
    fn the_default_pair_comes_first() {
        let source = source(&format!(
            "{TWO_FIELD_SOURCE}default: {{ base: {WSOL}, quote: {USDC} }}\n"
        ));
        let (wsol, usdc) = (Pubkey::from_str_const(WSOL), Pubkey::from_str_const(USDC));
        let usdt = Pubkey::from_str_const("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");
        let accounts = vec![
            (Pubkey::new_from_array([1; 32]), market(&wsol, &usdt, 0)),
            (Pubkey::new_from_array([2; 32]), market(&usdc, &usdt, 0)),
            (Pubkey::new_from_array([3; 32]), market(&wsol, &usdc, 0)),
        ];
        let mints = HashMap::from([(wsol, mint(9)), (usdc, mint(6)), (usdt, mint(6))]);

        let labels: Vec<_> = program_account_options(&source, &accounts, &mints, 0)
            .into_iter()
            .map(|option| option.label)
            .collect();

        assert_eq!(labels, ["SOL / USDC", "SOL / USDT", "USDC / USDT"]);
    }

    #[test]
    fn fresh_drops_accounts_whose_slot_lags_the_window() {
        let source = source(&format!(
            "{TWO_FIELD_SOURCE}fresh: {{ field: last_update_slot, within_slots: 100 }}\n"
        ));
        let (wsol, usdc) = (Pubkey::from_str_const(WSOL), Pubkey::from_str_const(USDC));
        let accounts: Vec<_> = [899, 900, 1_000, 1_050]
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                (
                    Pubkey::new_from_array([i as u8 + 1; 32]),
                    market(&wsol, &usdc, slot),
                )
            })
            .collect();
        let mints = HashMap::from([(wsol, mint(9)), (usdc, mint(6))]);

        let mut kept: Vec<_> = program_account_options(&source, &accounts, &mints, 1_000)
            .into_iter()
            .map(|option| option.metadata["last_update_slot"].as_u64().unwrap())
            .collect();
        kept.sort_unstable();

        assert_eq!(
            kept,
            [900, 1_000, 1_050],
            "100 slots behind is still fresh, ahead is fresh"
        );
    }

    #[test]
    fn accounts_that_no_longer_match_the_filters_are_dropped() {
        let (wsol, usdc) = (Pubkey::from_str_const(WSOL), Pubkey::from_str_const(USDC));
        let tag = wsol.to_bytes()[0];
        let source = source(&format!(
            "{TWO_FIELD_SOURCE}filters: [{{ offset: 0, bytes: [{tag}] }}]\n"
        ));
        let good = market(&wsol, &usdc, 1_000);
        let mut wrong_tag = good.clone();
        wrong_tag[0] ^= 0xff;
        let short = good[..good.len() - 1].to_vec();
        let accounts = vec![
            (Pubkey::new_from_array([1; 32]), good),
            (Pubkey::new_from_array([2; 32]), wrong_tag),
            (Pubkey::new_from_array([3; 32]), short),
        ];
        let mints = HashMap::from([(wsol, mint(9)), (usdc, mint(6))]);

        let kept = program_account_options(&source, &accounts, &mints, 1_000);

        assert_eq!(
            kept.len(),
            1,
            "only the account that still matches the size and tag"
        );
        assert_eq!(kept[0].value, Pubkey::new_from_array([1; 32]).to_string());
    }
}
