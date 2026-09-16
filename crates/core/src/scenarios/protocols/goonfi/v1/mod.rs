mod liquidity;
mod markets;
mod price;

use std::collections::HashMap;

pub use liquidity::{GoonfiLiquidityPreparation, build_goonfi_liquidity_scenario, vault_addresses};
pub use markets::{GoonfiDiscoveredMarket, discover_goonfi_markets, market_label};
pub use price::{
    GOONFI_DEFAULT_MARKET, GOONFI_ORACLE_PROGRAM_ID, GOONFI_PROGRAM_ID, GoonfiMarket,
    GoonfiPricePreparation, build_goonfi_price_scenario, validate_goonfi_market_layout,
    validate_goonfi_oracle_layout,
};
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideInstance, OverrideTemplate};

use crate::{
    error::{SurfpoolError, SurfpoolResult},
    scenarios::TemplateRegistry,
};

const FRESHNESS_TEMPLATE: &str = "goonfi-freshness";

/// Every builder override applies on Play, before any slot advance.
const PREPARATION_SLOT: u64 = 0;

fn template<'a>(registry: &'a TemplateRegistry, id: &str) -> SurfpoolResult<&'a OverrideTemplate> {
    registry
        .get(id)
        .ok_or_else(|| SurfpoolError::internal(format!("GoonFi template {id} is unavailable")))
}

fn invalid(message: impl Into<String>) -> SurfpoolError {
    SurfpoolError::internal(message.into())
}

fn read_pubkey(data: &[u8], offset: usize) -> Option<Pubkey> {
    data.get(offset..offset + 32)
        .and_then(|slice| slice.try_into().ok())
        .map(Pubkey::new_from_array)
}

/// Null, not zero: the slot encoder reads a supplied number AS the lead, so only null takes the
/// template's own lead of zero. Persisted, so the prepared quote stays inside the oracle's
/// staleness window however long the scenario is left running.
fn freshness_override(template_id: String, oracle: &Pubkey) -> OverrideInstance {
    OverrideInstance::new(
        template_id,
        PREPARATION_SLOT,
        AccountAddress::Pubkey(oracle.to_string()),
    )
    .with_values(HashMap::from([(
        "last_update_slot".to_string(),
        serde_json::Value::Null,
    )]))
    .with_label("Keep GoonFi quote fresh".to_string())
    .with_persist(true)
}

#[cfg(test)]
mod fixtures {
    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::{
        GOONFI_ORACLE_PROGRAM_ID, GOONFI_PROGRAM_ID,
        liquidity::{BASE_MINT_OFFSET, BASE_VAULT_OFFSET, QUOTE_MINT_OFFSET, QUOTE_VAULT_OFFSET},
        price::{MARKET_LAYOUT, ORACLE_LAYOUT, ORACLE_POINTER_OFFSET},
    };

    pub(super) const FIXTURE_BASE_VAULT: Pubkey = Pubkey::new_from_array([2; 32]);
    pub(super) const FIXTURE_QUOTE_VAULT: Pubkey = Pubkey::new_from_array([3; 32]);
    pub(super) const FIXTURE_ORACLE: Pubkey =
        Pubkey::from_str_const("7yecFG22heommABQ5svcbQLK1Ua4ZrJsHPiktZ17jfm3");

    /// A market of the manifest's size and magic, pointing at the given mints, vaults and oracle.
    pub(super) fn market_account(
        mints: [&Pubkey; 2],
        vaults: [&Pubkey; 2],
        oracle: &Pubkey,
    ) -> Account {
        let mut data = vec![0; MARKET_LAYOUT.account_size];
        let magic = MARKET_LAYOUT.magic.as_ref().expect("manifest layout tag");
        data[magic.offset..magic.offset + magic.bytes.len()].copy_from_slice(&magic.bytes);
        for (offset, pointer) in [
            (BASE_MINT_OFFSET, mints[0]),
            (QUOTE_MINT_OFFSET, mints[1]),
            (BASE_VAULT_OFFSET, vaults[0]),
            (QUOTE_VAULT_OFFSET, vaults[1]),
            (ORACLE_POINTER_OFFSET, oracle),
        ] {
            data[offset..offset + 32].copy_from_slice(pointer.as_ref());
        }
        Account {
            data,
            owner: GOONFI_PROGRAM_ID,
            ..Account::default()
        }
    }

    pub(super) fn oracle_account() -> Account {
        Account {
            data: vec![0; ORACLE_LAYOUT.account_size],
            owner: GOONFI_ORACLE_PROGRAM_ID,
            ..Account::default()
        }
    }

    /// An initialized SPL token account holding `amount` of `mint` for `authority`.
    pub(super) fn token_account(mint: &Pubkey, authority: &Pubkey, amount: u64) -> Account {
        let mut data = vec![0u8; 165];
        data[0..32].copy_from_slice(mint.as_ref());
        data[32..64].copy_from_slice(authority.as_ref());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data[108] = 1;
        Account {
            data,
            owner: spl_token_interface::ID,
            ..Account::default()
        }
    }
}
