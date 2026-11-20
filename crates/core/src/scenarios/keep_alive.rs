//! Keep-alive covers what a protocol's own off-chain actor rewrites every slot, such as a market
//! maker republishing its quote. Nobody runs those actors against a surfnet, so once a scenario's
//! override touches an account, the surfnet applies the keep-alive templates its template imports
//! every slot, as ordinary overrides, until the network is reset.

use std::collections::HashMap;

use solana_pubkey::Pubkey;
use surfpool_types::KeepAliveImport;

use super::TemplateRegistry;

/// An account a scenario's override wrote through a template that imports keep-alive.
#[derive(Clone, Debug)]
pub struct KeepAliveSource {
    pub template: String,
    /// The address the override wrote.
    pub address: Pubkey,
    /// The override's values, which input and PDA addresses resolve against.
    pub values: HashMap<String, serde_json::Value>,
    /// The override's own keep-alive list, which replaces the template's.
    pub imports: Option<Vec<KeepAliveImport>>,
}

/// Binds a template's keep-alive import to the account it rewrites every slot, resolved when an
/// override of that template first touched it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeepAliveBinding {
    /// The importing template's ID.
    pub template: String,
    /// The import's index in that template's `keep_alive` list, or in the override's own list.
    pub import: usize,
    /// The address the import rewrites.
    pub address: Pubkey,
    /// Whether the import is in the override's own list, kept by `source`.
    pub own: bool,
    /// The address the importing override wrote.
    pub source: Pubkey,
}

impl KeepAliveBinding {
    /// The import this binding applies.
    pub fn find<'a>(
        &self,
        registry: &'a TemplateRegistry,
        own_imports: &'a HashMap<Pubkey, Vec<KeepAliveImport>>,
    ) -> Option<&'a KeepAliveImport> {
        if self.own {
            own_imports.get(&self.source)?.get(self.import)
        } else {
            registry.get(&self.template)?.keep_alive.get(self.import)
        }
    }

    /// Whether this binds the same template import to the same address as `other`, for another
    /// override's account. Such an import is applied once.
    pub fn repeats(&self, other: &Self) -> bool {
        !self.own
            && !other.own
            && self.template == other.template
            && self.import == other.import
            && self.address == other.address
    }
}
