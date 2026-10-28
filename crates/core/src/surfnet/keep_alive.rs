//! Scenario bookkeeping on a surfnet: which scenario each registered override belongs to, and the
//! accounts keep-alive templates rewrite every slot.
//!
//! Keep-alive covers what a protocol's own off-chain actor rewrites every slot, such as a market
//! maker republishing its quote. Nobody runs those actors against a surfnet, so once a registered
//! scenario's override touches an account, the surfnet applies the keep-alive templates its template
//! imports every slot, as ordinary overrides, until the network is reset.

use std::collections::{BTreeSet, HashMap};

use solana_pubkey::Pubkey;

/// An account a registered scenario's override wrote through a template that imports keep-alive.
#[derive(Clone, Debug)]
pub struct KeepAliveSource {
    pub scenario: String,
    pub template: String,
    pub account: Pubkey,
    /// The override's values, which input and PDA addresses resolve against.
    pub values: HashMap<String, serde_json::Value>,
}

/// One account a keep-alive import rewrites.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeepAliveTarget {
    /// The importing template's ID.
    pub template: String,
    /// The import's index in that template's `keep_alive` list.
    pub import: usize,
    pub account: Pubkey,
}

/// The scenarios registered on a surfnet. Overrides only carry their own ID, so this is what ties
/// one back to its scenario.
#[derive(Clone, Debug, Default)]
pub struct RegisteredScenarios {
    override_scenarios: HashMap<String, String>,
    keep_alive: BTreeSet<KeepAliveTarget>,
}

impl RegisteredScenarios {
    /// Records a scenario's overrides. Registering it again adds to it.
    pub fn register(&mut self, scenario_id: &str, override_ids: impl IntoIterator<Item = String>) {
        for override_id in override_ids {
            self.override_scenarios
                .insert(override_id, scenario_id.to_string());
        }
    }

    /// The registered scenario an override belongs to.
    pub fn scenario_of(&self, override_id: &str) -> Option<&str> {
        self.override_scenarios.get(override_id).map(String::as_str)
    }

    /// Keeps `target` alive, returning whether it was not kept alive before.
    pub fn keep_alive(&mut self, target: KeepAliveTarget) -> bool {
        self.keep_alive.insert(target)
    }

    /// Every account kept alive, once.
    pub fn keep_alive_targets(&self) -> BTreeSet<KeepAliveTarget> {
        self.keep_alive.clone()
    }

    pub fn clear(&mut self) {
        self.override_scenarios.clear();
        self.keep_alive.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(template: &str, account: Pubkey) -> KeepAliveTarget {
        KeepAliveTarget {
            template: template.to_string(),
            import: 0,
            account,
        }
    }

    #[test]
    fn a_scenario_ties_its_overrides_back_to_it() {
        let mut scenarios = RegisteredScenarios::default();
        scenarios.register("s1", ["o1".to_string(), "o2".to_string()]);
        scenarios.register("s2", ["o3".to_string()]);
        assert_eq!(scenarios.scenario_of("o1"), Some("s1"));
        assert_eq!(scenarios.scenario_of("o3"), Some("s2"));
        assert_eq!(scenarios.scenario_of("o4"), None);

        scenarios.clear();
        assert_eq!(scenarios.scenario_of("o1"), None);
    }

    #[test]
    fn an_account_two_scenarios_keep_alive_is_rewritten_once() {
        let mut scenarios = RegisteredScenarios::default();
        let shared = target("tick", Pubkey::new_unique());
        assert!(scenarios.keep_alive(shared.clone()));
        assert!(!scenarios.keep_alive(shared.clone()), "added once");
        assert_eq!(scenarios.keep_alive_targets().len(), 1);

        scenarios.clear();
        assert!(scenarios.keep_alive_targets().is_empty());
    }
}
