//! Scenario bookkeeping on a surfnet: which scenarios are registered, their overrides, and the
//! accounts their keep-alive templates rewrite every slot.
//!
//! Keep-alive covers what a protocol's own off-chain actor rewrites every slot, such as a market
//! maker republishing its quote. Nobody runs those actors against a surfnet, so while a scenario is
//! registered the surfnet applies its keep-alive templates every slot, as ordinary overrides, to
//! the accounts the scenario's overrides touched. Unregistering the scenario stops them.

use std::collections::{BTreeSet, HashMap, HashSet};

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

/// A registered scenario: its overrides, and the accounts it keeps alive.
#[derive(Clone, Debug, Default)]
struct RegisteredScenario {
    overrides: HashSet<String>,
    keep_alive: BTreeSet<KeepAliveTarget>,
}

/// The scenarios registered on a surfnet. Overrides only carry their own ID, so this is also what
/// ties one back to its scenario.
#[derive(Clone, Debug, Default)]
pub struct RegisteredScenarios {
    scenarios: HashMap<String, RegisteredScenario>,
    override_scenarios: HashMap<String, String>,
}

impl RegisteredScenarios {
    /// Records a scenario and its overrides. Registering it again adds to it.
    pub fn register(&mut self, scenario_id: &str, override_ids: impl IntoIterator<Item = String>) {
        let scenario = self.scenarios.entry(scenario_id.to_string()).or_default();
        for override_id in override_ids {
            scenario.overrides.insert(override_id.clone());
            self.override_scenarios
                .insert(override_id, scenario_id.to_string());
        }
    }

    /// The registered scenario an override belongs to.
    pub fn scenario_of(&self, override_id: &str) -> Option<&str> {
        self.override_scenarios.get(override_id).map(String::as_str)
    }

    pub fn is_registered(&self, scenario_id: &str) -> bool {
        self.scenarios.contains_key(scenario_id)
    }

    /// Forgets a scenario, which stops its keep-alive, returning the IDs of its overrides, or
    /// `None` if it was not registered. An account another scenario keeps alive stays alive.
    pub fn unregister(&mut self, scenario_id: &str) -> Option<HashSet<String>> {
        let scenario = self.scenarios.remove(scenario_id)?;
        for override_id in &scenario.overrides {
            if self.scenario_of(override_id) == Some(scenario_id) {
                self.override_scenarios.remove(override_id);
            }
        }
        Some(scenario.overrides)
    }

    /// Keeps `target` alive for a registered scenario, returning whether no scenario kept it alive
    /// before.
    pub fn keep_alive(&mut self, scenario_id: &str, target: KeepAliveTarget) -> bool {
        let already = self.is_kept_alive(&target);
        match self.scenarios.get_mut(scenario_id) {
            Some(scenario) => scenario.keep_alive.insert(target) && !already,
            None => false,
        }
    }

    fn is_kept_alive(&self, target: &KeepAliveTarget) -> bool {
        self.scenarios
            .values()
            .any(|scenario| scenario.keep_alive.contains(target))
    }

    /// Every account kept alive by at least one scenario, once.
    pub fn keep_alive_targets(&self) -> BTreeSet<KeepAliveTarget> {
        self.scenarios
            .values()
            .flat_map(|scenario| scenario.keep_alive.iter().cloned())
            .collect()
    }

    pub fn clear(&mut self) {
        self.scenarios.clear();
        self.override_scenarios.clear();
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
    fn a_scenario_ties_its_overrides_back_to_it_until_unregistered() {
        let mut scenarios = RegisteredScenarios::default();
        scenarios.register("s1", ["o1".to_string(), "o2".to_string()]);
        scenarios.register("s2", ["o3".to_string()]);
        assert_eq!(scenarios.scenario_of("o1"), Some("s1"));
        assert!(scenarios.is_registered("s2"));

        let removed = scenarios.unregister("s1").expect("s1 was registered");
        assert_eq!(removed, HashSet::from(["o1".to_string(), "o2".to_string()]));
        assert_eq!(scenarios.scenario_of("o1"), None);
        assert_eq!(scenarios.scenario_of("o3"), Some("s2"));
        assert!(scenarios.unregister("s1").is_none());
    }

    #[test]
    fn an_account_stays_alive_until_its_last_scenario_stops() {
        let mut scenarios = RegisteredScenarios::default();
        scenarios.register("s1", []);
        scenarios.register("s2", []);
        let shared = target("tick", Pubkey::new_unique());
        assert!(scenarios.keep_alive("s1", shared.clone()));
        assert!(!scenarios.keep_alive("s1", shared.clone()), "added once");
        assert!(
            !scenarios.keep_alive("s2", shared.clone()),
            "already kept alive by s1"
        );
        assert!(
            !scenarios.keep_alive("s3", target("tick", Pubkey::new_unique())),
            "only registered scenarios keep accounts alive"
        );

        scenarios.unregister("s1");
        assert!(
            scenarios.keep_alive_targets().contains(&shared),
            "s2 still keeps it alive"
        );
        scenarios.unregister("s2");
        assert!(scenarios.keep_alive_targets().is_empty());
    }
}

#[cfg(test)]
mod surfnet_tests {
    use std::collections::HashMap;

    use solana_pubkey::Pubkey;
    use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

    use crate::surfnet::svm::SurfnetSvm;

    #[test]
    fn unregistering_drops_the_overrides_still_to_play() {
        let (mut svm, _events, _geyser) = SurfnetSvm::default();
        let mut instance = OverrideInstance::new(
            "pyth-price-feed-v2".to_string(),
            5,
            AccountAddress::Pubkey(Pubkey::new_unique().to_string()),
        )
        .with_values(HashMap::from([(
            "price_message.price".to_string(),
            serde_json::json!(7),
        )]));
        instance.id = "s1-override".to_string();
        let scenario = Scenario {
            id: "s1".to_string(),
            name: "s1".to_string(),
            description: String::new(),
            tags: vec![],
            overrides: vec![instance],
        };
        let base = svm.get_latest_absolute_slot();
        svm.register_scenario(scenario, Some(base)).unwrap();
        assert!(svm.scheduled_overrides.get(&(base + 5)).unwrap().is_some());

        assert!(svm.unregister_scenario("s1").unwrap());
        assert!(svm.scheduled_overrides.get(&(base + 5)).unwrap().is_none());
        assert!(!svm.scenarios.is_registered("s1"));
        assert!(!svm.unregister_scenario("s1").unwrap(), "already gone");
    }
}
