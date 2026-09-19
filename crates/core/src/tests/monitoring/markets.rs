//! Deprecation: are the markets our templates name still worth pointing a scenario at?
//!
//! A template that addresses a pool nobody trades any more still applies cleanly and still
//! produces a swap, so nothing in the test suite notices. The user finds out when their scenario
//! reproduces a market that stopped existing economically. Two signals together say that:
//! whether the account is still there, and whether anything has touched it lately.
//!
//! An RPC error is not zero activity. Every failed read is reported as unknown rather than dead,
//! because the opposite mistake deletes a working market from the product.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use solana_client::rpc_config::RpcSignaturesForAddressConfig;
use solana_pubkey::Pubkey;

use super::{Report, live};
use crate::scenarios::TemplateRegistry;

const CHECK: &str = "market-deprecation";

/// How long a market may sit untouched before it is worth asking whether it still belongs in the
/// product. Three days is the milestone's figure: long enough to survive a quiet weekend on a
/// minor pair, short enough to catch a venue that has actually stopped.
const STALE_AFTER_DAYS: i64 = 3;

/// Constants that hold token mints rather than venues. A mint is not a market, so it gets an
/// existence check and no activity check.
fn is_mint_constant(name: &str) -> bool {
    let name = name.to_lowercase();
    name.contains("mint") || name.contains("token")
}

/// The addresses a scenario reaches by default, one per template that carries a literal address.
fn default_addresses(registry: &TemplateRegistry) -> BTreeMap<Pubkey, (String, String)> {
    let mut addresses = BTreeMap::new();
    for template in registry.all() {
        if let surfpool_types::AccountAddress::Pubkey(literal) = &template.address
            && let Ok(address) = Pubkey::from_str(literal)
        {
            addresses.insert(
                address,
                (template.protocol.to_lowercase(), template.id.clone()),
            );
        }
    }
    addresses
}

/// Every address offered in a constant list, which is the menu the product and the model choose
/// from. A dead entry here is a bad suggestion rather than a broken default.
fn offered_addresses(registry: &TemplateRegistry) -> BTreeMap<Pubkey, (String, String)> {
    let mut addresses = BTreeMap::new();
    for template in registry.all() {
        for (name, definition) in &template.constants {
            if is_mint_constant(name) {
                continue;
            }
            for option in &definition.options {
                if let Ok(address) = Pubkey::from_str(&option.value) {
                    addresses.insert(
                        address,
                        (
                            template.protocol.to_lowercase(),
                            format!("{name}={}", option.id),
                        ),
                    );
                }
            }
        }
    }
    addresses
}

async fn last_activity(address: &Pubkey) -> Result<Option<i64>, String> {
    let config = RpcSignaturesForAddressConfig {
        limit: Some(1),
        ..Default::default()
    };

    let mut attempt = 0;
    let mut errors = Vec::new();
    loop {
        match live::client()
            .get_signatures_for_address(address, Some(&config))
            .await
        {
            Ok(signatures) => {
                return Ok(signatures.first().and_then(|entry| entry.block_time));
            }
            Err(error) if attempt < 3 => {
                attempt += 1;
                errors.push(format!("attempt {attempt}: {error}"));
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
            }
            Err(error) => {
                errors.push(format!("attempt {}: {error}", attempt + 1));
                return Err(error.to_string());
            }
        }
    }
}

#[tokio::test]
async fn monitored_markets_are_still_alive() {
    let registry = TemplateRegistry::new();
    let defaults = default_addresses(&registry);
    let offered = offered_addresses(&registry);
    let mut report = Report::new(CHECK);

    let cutoff = chrono::Utc::now().timestamp() - STALE_AFTER_DAYS * 24 * 60 * 60;

    for (address, (protocol, subject)) in &defaults {
        let Some(account) = live::try_fetch(&[*address]).await.remove(0) else {
            report.error(
                protocol,
                subject,
                format!("the template's default account {address} no longer exists on mainnet"),
            );
            continue;
        };
        if account.lamports == 0 {
            report.error(
                protocol,
                subject,
                format!("the template's default account {address} has been closed"),
            );
            continue;
        }

        match last_activity(address).await {
            Ok(Some(block_time)) if block_time < cutoff => report.warn(
                protocol,
                subject,
                format!(
                    "{address} has not been touched since {}, over {STALE_AFTER_DAYS} days ago; \
                     it may have been abandoned",
                    chrono::DateTime::from_timestamp(block_time, 0)
                        .map(|time| time.to_rfc3339())
                        .unwrap_or_else(|| block_time.to_string())
                ),
            ),
            Ok(Some(_)) => {}
            Ok(None) => report.warn(
                protocol,
                subject,
                format!("{address} has no confirmed signature with a block time"),
            ),
            Err(reason) => report.warn(
                protocol,
                subject,
                format!(
                    "activity for {address} could not be read, so it is unknown rather than \
                     absent: {reason}"
                ),
            ),
        }
    }

    // The menus are large, so they get one batched existence read and no per-address history.
    let menu: Vec<Pubkey> = offered
        .keys()
        .filter(|address| !defaults.contains_key(*address))
        .copied()
        .collect();
    let mut missing = BTreeSet::new();
    for chunk in menu.chunks(100) {
        for (address, account) in chunk.iter().zip(live::try_fetch(chunk).await) {
            match account {
                Some(account) if account.lamports > 0 => {}
                _ => {
                    missing.insert(*address);
                }
            }
        }
    }
    for address in &missing {
        let (protocol, subject) = &offered[address];
        report.warn(
            protocol,
            subject,
            format!(
                "{address} is offered as an option but does not exist on mainnet; the model and \
                 the UI will suggest a dead market"
            ),
        );
    }

    report.info(
        "all",
        "coverage",
        format!(
            "{} default account(s) and {} offered option(s) checked",
            defaults.len(),
            menu.len()
        ),
    );
    report.finish();
}
