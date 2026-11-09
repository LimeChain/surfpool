//! Keep-alive covers what a protocol's own off-chain actor rewrites every slot, such as a market
//! maker republishing its quote. Nobody runs those actors against a surfnet, so once a scenario's
//! override touches an account, the surfnet applies the keep-alive templates its template imports
//! every slot, as ordinary overrides, until the network is reset. An override with `keep_alive`
//! off stops keep-alive for the account it writes, until an override with it on writes that account.

use std::collections::{BTreeSet, HashMap, HashSet};

use solana_account::Account;
use solana_pubkey::Pubkey;
use surfpool_types::{KeepAliveAddress, KeepAliveImport};

use super::TemplateRegistry;

/// An account a scenario's override wrote.
#[derive(Clone, Debug)]
pub struct KeepAliveSource {
    pub template: String,
    /// The address the override wrote.
    pub address: Pubkey,
    /// The override's values, which input and PDA addresses resolve against.
    pub values: HashMap<String, serde_json::Value>,
    /// The override's `keep_alive`: off, keep-alive stops for `address`.
    pub keep_alive: bool,
}

/// Binds a template's keep-alive import to the account it rewrites every slot, resolved when an
/// override of that template first touched it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeepAliveBinding {
    /// The importing template's ID.
    pub template: String,
    /// The import's index in that template's `keep_alive` list.
    pub import: usize,
    /// The address the import rewrites.
    pub address: Pubkey,
}

/// Binds a template's owned keep-alive import, which rewrites every local account of a kind,
/// resolved again each slot since the surfnet keeps loading accounts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeepAliveKind {
    /// The importing template's ID.
    pub template: String,
    /// The import's index in that template's `keep_alive` list.
    pub import: usize,
}

/// What keep-alive rewrites every slot. Kept in memory only, so a restart on a persistent database
/// stops keep-alive.
#[derive(Clone, Debug, Default)]
pub struct KeepAlive {
    pub bindings: BTreeSet<KeepAliveBinding>,
    pub kinds: BTreeSet<KeepAliveKind>,
    /// Accounts an override with `keep_alive` off wrote last. Keep-alive skips them.
    pub excluded: HashSet<Pubkey>,
}

impl KeepAlive {
    /// Applies the overrides that wrote `touched`, in order: one with `keep_alive` off stops
    /// keep-alive for its account, one with it on starts it again and adds its template's imports.
    pub fn enroll(&mut self, registry: &TemplateRegistry, touched: Vec<KeepAliveSource>) {
        for source in touched {
            if !source.keep_alive {
                self.bindings
                    .retain(|binding| binding.address != source.address);
                if self.excluded.insert(source.address) {
                    info!("Keep-alive stops rewriting {}", source.address);
                }
                continue;
            }
            self.excluded.remove(&source.address);
            let Some(template) = registry.get(&source.template) else {
                continue;
            };
            for (index, import) in template.keep_alive.iter().enumerate() {
                if let KeepAliveAddress::Owned { owners, account } = &import.address {
                    let kind = KeepAliveKind {
                        template: template.id.clone(),
                        import: index,
                    };
                    if self.kinds.insert(kind) {
                        info!(
                            "Keep-alive '{}' rewrites every local '{}' of {:?} every slot",
                            import.template, account, owners
                        );
                    }
                    continue;
                }
                let Some(address) = import.address(source.address, &source.values) else {
                    warn!(
                        "Keep-alive '{}' of override template '{}' has no address for {}",
                        import.template, template.id, source.address
                    );
                    continue;
                };
                if self.excluded.contains(&address) {
                    continue;
                }
                let binding = KeepAliveBinding {
                    template: template.id.clone(),
                    import: index,
                    address,
                };
                if self.bindings.insert(binding) {
                    info!(
                        "Keep-alive '{}' rewrites {} every slot",
                        import.template, address
                    );
                }
            }
        }
    }

    /// Each account to rewrite this slot with the import that rewrites it: every binding, then
    /// every account `accounts_owned_by` returns for a kind's owners that holds the kind's account
    /// type. Excluded accounts are skipped, and so is an account a keep-alive template already
    /// rewrites.
    pub fn targets<'r>(
        &self,
        registry: &'r TemplateRegistry,
        accounts_owned_by: impl Fn(&Pubkey) -> Vec<(Pubkey, Account)>,
    ) -> Vec<(&'r KeepAliveImport, Pubkey)> {
        let import_of = |template: &str, index: usize| {
            registry
                .get(template)
                .and_then(|template| template.keep_alive.get(index))
        };
        let mut seen = HashSet::new();
        let mut targets = Vec::new();
        for binding in &self.bindings {
            if let Some(import) = import_of(&binding.template, binding.import)
                && seen.insert((import.template.as_str(), binding.address))
            {
                targets.push((import, binding.address));
            }
        }
        for kind in &self.kinds {
            let Some(import) = import_of(&kind.template, kind.import) else {
                continue;
            };
            let KeepAliveAddress::Owned { owners, account } = &import.address else {
                continue;
            };
            let Some(discriminator) = registry
                .get(&import.template)
                .and_then(|template| template.account_discriminator(account))
            else {
                continue;
            };
            for owner in owners.iter().filter_map(|owner| owner.parse().ok()) {
                for (address, owned) in accounts_owned_by(&owner) {
                    if owned.data.starts_with(discriminator)
                        && !self.excluded.contains(&address)
                        && seen.insert((import.template.as_str(), address))
                    {
                        targets.push((import, address));
                    }
                }
            }
        }
        targets
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::scenarios::registry::PYTH_V2_IDL_CONTENT;

    pub(crate) const OWNER: &str = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";

    pub(crate) const OWNED_OVERRIDES: &str = r#"
protocol: Example
version: v1
account_type: State
raw_layout: true
templates:
  - id: example-vault
    name: Vault
    description: Example vault
    keep_alive:
      - template: example-price-fresh
        address: { type: owned, owner: rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ, account: PriceUpdateV2 }
        values: { posted_slot: $slot }
      - template: example-price-fresh
        address: { type: input, value: oracle }
        values: { posted_slot: $slot }
    address: { type: pubkey }
    properties:
      - { path: balance, offset: 0, encoding: u64, description: Balance }
      - { path: oracle, type: input, value_type: pubkey, description: The vault's oracle }
"#;

    const PRICE_KEEP_ALIVE: &str = r#"
protocol: Example
version: v1
account_type: PriceUpdateV2
templates:
  - id: example-price-fresh
    name: Fresh price
    description: What the publisher republishes
    address: { type: pubkey }
    properties:
      - { path: posted_slot, label: Posted slot, description: Posted slot }
"#;

    pub(crate) fn owned_registry(overrides: &str) -> TemplateRegistry {
        let mut registry = TemplateRegistry::default();
        registry.load_raw_layout_overrides(overrides, "example");
        registry.load_keep_alive(
            Some(serde_json::from_str(PYTH_V2_IDL_CONTENT).unwrap()),
            PRICE_KEEP_ALIVE,
            "example",
        );
        registry
    }

    fn account(owner: Pubkey, data: &[u8]) -> Account {
        Account {
            lamports: 1,
            data: data.to_vec(),
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn source(
        template: &str,
        address: Pubkey,
        oracle: Pubkey,
        keep_alive: bool,
    ) -> KeepAliveSource {
        KeepAliveSource {
            template: template.to_string(),
            address,
            values: HashMap::from([("oracle".to_string(), serde_json::json!(oracle.to_string()))]),
            keep_alive,
        }
    }

    struct LocalAccounts {
        registry: TemplateRegistry,
        accounts: Vec<(Pubkey, Account)>,
        price: [u8; 8],
    }

    impl LocalAccounts {
        fn new() -> Self {
            let registry = owned_registry(OWNED_OVERRIDES);
            let price = registry
                .get("example-price-fresh")
                .and_then(|template| template.account_discriminator("PriceUpdateV2"))
                .unwrap()
                .try_into()
                .unwrap();
            Self {
                registry,
                accounts: Vec::new(),
                price,
            }
        }

        fn load(&mut self, owner: Pubkey, discriminator: [u8; 8]) -> Pubkey {
            let address = Pubkey::new_unique();
            let mut data = discriminator.to_vec();
            data.extend([0; 8]);
            self.accounts.push((address, account(owner, &data)));
            address
        }

        fn targets(&self, keep_alive: &KeepAlive) -> Vec<Pubkey> {
            keep_alive
                .targets(&self.registry, |owner| {
                    self.accounts
                        .iter()
                        .filter(|(_, account)| account.owner == *owner)
                        .cloned()
                        .collect()
                })
                .into_iter()
                .map(|(import, address)| {
                    assert_eq!(import.template, "example-price-fresh");
                    address
                })
                .collect()
        }
    }

    #[test]
    fn an_owned_kind_rewrites_every_local_account_of_its_owner_and_type() {
        let owner = OWNER.parse().unwrap();
        let mut surfnet = LocalAccounts::new();
        let oracle = surfnet.load(owner, surfnet.price);
        let other_price = surfnet.load(owner, surfnet.price);
        surfnet.load(owner, [9; 8]);
        surfnet.load(Pubkey::new_unique(), surfnet.price);

        let mut keep_alive = KeepAlive::default();
        let vault = Pubkey::new_unique();
        keep_alive.enroll(
            &surfnet.registry,
            vec![source("example-vault", vault, oracle, true)],
        );
        assert_eq!(
            surfnet.targets(&keep_alive),
            [oracle, other_price],
            "the oracle is bound and owned, and is written once"
        );

        let loaded_later = surfnet.load(owner, surfnet.price);
        assert_eq!(
            surfnet.targets(&keep_alive),
            [oracle, other_price, loaded_later]
        );
    }

    #[test]
    fn an_override_without_keep_alive_stops_it_for_its_account_until_one_with_it_plays() {
        let owner = OWNER.parse().unwrap();
        let mut surfnet = LocalAccounts::new();
        let oracle = surfnet.load(owner, surfnet.price);
        let other_price = surfnet.load(owner, surfnet.price);
        let vault = Pubkey::new_unique();
        let mut keep_alive = KeepAlive::default();
        keep_alive.enroll(
            &surfnet.registry,
            vec![source("example-vault", vault, oracle, true)],
        );

        keep_alive.enroll(
            &surfnet.registry,
            vec![source("pyth-price-feed-v2", oracle, oracle, false)],
        );
        assert!(keep_alive.bindings.is_empty(), "its binding is dropped");
        assert_eq!(surfnet.targets(&keep_alive), [other_price]);

        keep_alive.enroll(
            &surfnet.registry,
            vec![source("example-vault", vault, oracle, true)],
        );
        assert_eq!(
            surfnet.targets(&keep_alive),
            [other_price],
            "another account's override does not restart it"
        );

        keep_alive.enroll(
            &surfnet.registry,
            vec![source("pyth-price-feed-v2", oracle, oracle, true)],
        );
        assert_eq!(surfnet.targets(&keep_alive), [oracle, other_price]);
    }
}
