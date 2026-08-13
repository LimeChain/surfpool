use std::collections::BTreeMap;

use surfpool_types::{OverrideTemplate, YamlOverrideTemplateCollection};

pub const PYTH_V2_IDL_CONTENT: &str = include_str!("./protocols/pyth/v2/idl.json");
pub const PYTH_V2_OVERRIDES_CONTENT: &str = include_str!("./protocols/pyth/v2/overrides.yaml");

pub const JUPITER_V6_IDL_CONTENT: &str = include_str!("./protocols/jupiter/v6/idl.json");
pub const JUPITER_V6_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/jupiter/v6/overrides.yaml");

pub const RAYDIUM_CLMM_IDL_CONTENT: &str = include_str!("./protocols/raydium/v3/idl.json");
pub const RAYDIUM_CLMM_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/raydium/v3/overrides.yaml");

pub const RAYDIUM_AMM_V4_IDL_CONTENT: &str = include_str!("./protocols/raydium/v4/idl.json");
pub const RAYDIUM_AMM_V4_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/raydium/v4/overrides.yaml");

pub const METEORA_DLMM_IDL_CONTENT: &str = include_str!("./protocols/meteora/dlmm/v1/idl.json");
pub const METEORA_DLMM_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/meteora/dlmm/v1/overrides.yaml");
pub const KAMINO_V1_IDL_CONTENT: &str = include_str!("./protocols/kamino/v1/idl.json");
pub const KAMINO_V1_OVERRIDES_CONTENT: &str = include_str!("./protocols/kamino/v1/overrides.yaml");

pub const KAMINO_SCOPE_IDL_CONTENT: &str = include_str!("./protocols/kamino/scope/v1/idl.json");
pub const KAMINO_SCOPE_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/kamino/scope/v1/overrides.yaml");

pub const KAMINO_FARMS_IDL_CONTENT: &str = include_str!("./protocols/kamino/farms/v1/idl.json");
pub const KAMINO_FARMS_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/kamino/farms/v1/overrides.yaml");

pub const KAMINO_SWAP_IDL_CONTENT: &str = include_str!("./protocols/kamino/swap/v1/idl.json");
pub const KAMINO_SWAP_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/kamino/swap/v1/overrides.yaml");

pub const KAMINO_VAULT_IDL_CONTENT: &str = include_str!("./protocols/kamino/vault/v1/idl.json");
pub const KAMINO_VAULT_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/kamino/vault/v1/overrides.yaml");

pub const KAMINO_LIQUIDITY_IDL_CONTENT: &str =
    include_str!("./protocols/kamino/liquidity/v1/idl.json");
pub const KAMINO_LIQUIDITY_OVERRIDES_CONTENT: &str =
    include_str!("./protocols/kamino/liquidity/v1/overrides.yaml");

pub const DRIFT_V2_IDL_CONTENT: &str = include_str!("./protocols/drift/v2/idl.json");
pub const DRIFT_V2_OVERRIDES_CONTENT: &str = include_str!("./protocols/drift/v2/overrides.yaml");

pub const WHIRLPOOL_IDL_CONTENT: &str = include_str!("./protocols/whirlpool/idl.json");
pub const WHIRLPOOL_OVERRIDES_CONTENT: &str = include_str!("./protocols/whirlpool/overrides.yaml");

pub const SPL_TOKEN_IDL_CONTENT: &str = include_str!("./protocols/spl-token/idl.json");
pub const SPL_TOKEN_OVERRIDES_CONTENT: &str = include_str!("./protocols/spl-token/overrides.yaml");

/// Registry for managing override templates loaded from YAML files
#[derive(Clone, Debug, Default)]
pub struct TemplateRegistry {
    /// Map of template ID to template
    pub templates: BTreeMap<String, OverrideTemplate>,
}

impl TemplateRegistry {
    /// Create a new template registry
    pub fn new() -> Self {
        let mut default = Self::default();
        default.load_pyth_overrides();
        default.load_jupiter_overrides();
        default.load_raydium_overrides();
        default.load_meteora_overrides();
        default.load_kamino_overrides();
        default.load_drift_overrides();
        default.load_whirlpool_overrides();
        default.load_spl_token_overrides();
        default
    }

    pub fn load_pyth_overrides(&mut self) {
        self.load_protocol_overrides(PYTH_V2_IDL_CONTENT, PYTH_V2_OVERRIDES_CONTENT, "pyth");
    }

    pub fn load_jupiter_overrides(&mut self) {
        self.load_protocol_overrides(
            JUPITER_V6_IDL_CONTENT,
            JUPITER_V6_OVERRIDES_CONTENT,
            "jupiter",
        );
    }

    pub fn load_meteora_overrides(&mut self) {
        self.load_protocol_overrides(
            METEORA_DLMM_IDL_CONTENT,
            METEORA_DLMM_OVERRIDES_CONTENT,
            "meteora",
        );
    }

    pub fn load_raydium_overrides(&mut self) {
        self.load_protocol_overrides(
            RAYDIUM_CLMM_IDL_CONTENT,
            RAYDIUM_CLMM_OVERRIDES_CONTENT,
            "raydium",
        );
        self.load_protocol_overrides(
            RAYDIUM_AMM_V4_IDL_CONTENT,
            RAYDIUM_AMM_V4_OVERRIDES_CONTENT,
            "raydium",
        );
    }

    pub fn load_kamino_overrides(&mut self) {
        self.load_protocol_overrides(KAMINO_V1_IDL_CONTENT, KAMINO_V1_OVERRIDES_CONTENT, "kamino");

        self.load_protocol_overrides(
            KAMINO_SCOPE_IDL_CONTENT,
            KAMINO_SCOPE_OVERRIDES_CONTENT,
            "kamino-scope",
        );

        self.load_protocol_overrides(
            KAMINO_FARMS_IDL_CONTENT,
            KAMINO_FARMS_OVERRIDES_CONTENT,
            "kamino-farms",
        );

        self.load_protocol_overrides(
            KAMINO_SWAP_IDL_CONTENT,
            KAMINO_SWAP_OVERRIDES_CONTENT,
            "kamino-swap",
        );

        self.load_protocol_overrides(
            KAMINO_VAULT_IDL_CONTENT,
            KAMINO_VAULT_OVERRIDES_CONTENT,
            "kamino-vault",
        );

        self.load_protocol_overrides(
            KAMINO_LIQUIDITY_IDL_CONTENT,
            KAMINO_LIQUIDITY_OVERRIDES_CONTENT,
            "kamino-liquidity",
        );
    }

    pub fn load_drift_overrides(&mut self) {
        self.load_protocol_overrides(DRIFT_V2_IDL_CONTENT, DRIFT_V2_OVERRIDES_CONTENT, "drift");
    }

    pub fn load_whirlpool_overrides(&mut self) {
        self.load_protocol_overrides(
            WHIRLPOOL_IDL_CONTENT,
            WHIRLPOOL_OVERRIDES_CONTENT,
            "whirlpool",
        );
    }

    pub fn load_spl_token_overrides(&mut self) {
        self.load_protocol_overrides(
            SPL_TOKEN_IDL_CONTENT,
            SPL_TOKEN_OVERRIDES_CONTENT,
            "spl-token",
        );
    }

    fn load_protocol_overrides(
        &mut self,
        idl_content: &str,
        overrides_content: &str,
        protocol_name: &str,
    ) {
        let idl = match serde_json::from_str(idl_content) {
            Ok(idl) => idl,
            Err(e) => panic!("unable to load {} idl: {}", protocol_name, e),
        };

        let collection =
            match serde_yaml::from_str::<YamlOverrideTemplateCollection>(overrides_content) {
                Ok(c) => c,
                Err(e) => panic!("unable to load {} overrides: {}", protocol_name, e),
            };

        // Convert all templates in the collection
        let templates = collection.to_override_templates(idl);

        // Register each template
        for template in templates {
            let template_id = template.id.clone();
            self.templates.insert(template_id.clone(), template);
        }
    }

    /// Get a template by ID
    pub fn get(&self, template_id: &str) -> Option<&OverrideTemplate> {
        self.templates.get(template_id)
    }

    /// Get all templates
    pub fn all(&self) -> Vec<&OverrideTemplate> {
        self.templates.values().collect()
    }

    /// Get templates for a specific protocol
    pub fn by_protocol(&self, protocol: &str) -> Vec<&OverrideTemplate> {
        self.templates
            .values()
            .filter(|t| t.protocol.eq_ignore_ascii_case(protocol))
            .collect()
    }

    /// Get templates matching any of the given tags
    pub fn by_tags(&self, tags: &[String]) -> Vec<&OverrideTemplate> {
        self.templates
            .values()
            .filter(|t| t.tags.iter().any(|tag| tags.contains(tag)))
            .collect()
    }

    /// Get the number of loaded templates
    pub fn count(&self) -> usize {
        self.templates.len()
    }

    /// Check if a template exists
    pub fn contains(&self, template_id: &str) -> bool {
        self.templates.contains_key(template_id)
    }

    /// List all template IDs
    pub fn list_ids(&self) -> Vec<String> {
        self.templates.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use anchor_lang_idl::types::IdlType;
    use std::{collections::HashMap, collections::BTreeSet, str::FromStr};

    use solana_pubkey::Pubkey;
    use surfpool_types::{AccountAddress, PdaSeed};

    use super::*;

    /// A valid JSON value for a scalar IDL type, or `None` for composites.
    fn sample_scalar_value(ty: &IdlType) -> Option<serde_json::Value> {
        match ty {
            IdlType::Bool => Some(serde_json::json!(true)),
            IdlType::U8
            | IdlType::U16
            | IdlType::U32
            | IdlType::U64
            | IdlType::U128
            | IdlType::I8
            | IdlType::I16
            | IdlType::I32
            | IdlType::I64
            | IdlType::I128 => Some(serde_json::json!(1)),
            IdlType::Pubkey => Some(serde_json::json!(
                "11111111111111111111111111111111".to_string()
            )),
            _ => None,
        }
    }

    #[test]
    fn raydium_config_index_options_derive_their_documented_address() {
        let registry = TemplateRegistry::new();
        let template = registry.get("raydium-clmm-custom").expect("template");

        let AccountAddress::Pda { seeds, .. } = &template.address else {
            panic!("the pool address is a PDA");
        };
        let derived_pda_seed = seeds
            .iter()
            .find(|seed| matches!(seed, PdaSeed::DerivedPda { .. }))
            .expect("the pool PDA derives the amm config PDA");

        let options = &template
            .constants
            .get("amm_config_index")
            .expect("amm_config_index constant")
            .options;
        assert!(!options.is_empty(), "the fee tiers are the fixture here");

        for option in options {
            let expected = option
                .metadata
                .get("derived_address")
                .and_then(|address| address.as_str())
                .map(|address| Pubkey::from_str(address).expect("a valid address"))
                .unwrap_or_else(|| panic!("option {} documents no derived_address", option.id));

            let values = HashMap::from([(
                "config_index".to_string(),
                serde_json::Value::String(option.value.clone()),
            )]);
            let bytes = derived_pda_seed
                .to_bytes(Some(&values))
                .unwrap_or_else(|| panic!("option {} did not resolve", option.id));

            assert_eq!(
                Pubkey::try_from(bytes.as_slice()).expect("32 bytes"),
                expected,
                "option {}",
                option.id
            );
        }
    }

    /// The expected address is not ours: SOL/USDC at fee tier 1 holds a live CLMM
    /// PoolState on mainnet (owner CAMMCzo5…, discriminator 247 237 227 245 215 195 222 70),
    /// so this pins the whole chain — catalogue value, config PDA, pool PDA — against
    /// something outside the fixtures. Mint order is part of the recipe: Raydium expects
    /// the lower mint first, and the reversed order derives an address that holds nothing.
    #[test]
    fn raydium_template_derives_the_live_sol_usdc_pool() {
        let registry = TemplateRegistry::new();
        let template = registry.get("raydium-clmm-custom").expect("template");
        let sol = "So11111111111111111111111111111111111111112";
        let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

        let values = |mint_0: &str, mint_1: &str| {
            HashMap::from([
                (
                    "config_index".to_string(),
                    serde_json::Value::String("1".to_string()),
                ),
                (
                    "token_mint_0".to_string(),
                    serde_json::Value::String(mint_0.to_string()),
                ),
                (
                    "token_mint_1".to_string(),
                    serde_json::Value::String(mint_1.to_string()),
                ),
            ])
        };

        assert_eq!(
            template
                .address
                .resolve(Some(&values(sol, usdc)))
                .expect("resolves"),
            Pubkey::from_str("3tD34VtprDSkYCnATtQLCiVgTkECU3d12KtjupeR6N2X").expect("address"),
        );

        assert_ne!(
            template
                .address
                .resolve(Some(&values(usdc, sol)))
                .expect("resolves"),
            Pubkey::from_str("3tD34VtprDSkYCnATtQLCiVgTkECU3d12KtjupeR6N2X").expect("address"),
            "swapping the mints must not land on the same pool"
        );
    }

    #[test]
    fn raydium_pool_address_needs_every_seed_to_resolve() {
        let registry = TemplateRegistry::new();
        let template = registry.get("raydium-clmm-custom").expect("template");

        let mints: Vec<String> = template
            .constants
            .get("token_mint")
            .expect("token_mint constant")
            .options
            .iter()
            .take(2)
            .map(|option| option.value.clone())
            .collect();
        assert_eq!(mints.len(), 2, "the pool address needs two mints");

        let mut values = HashMap::from([
            (
                "config_index".to_string(),
                serde_json::Value::String("1".to_string()),
            ),
            (
                "token_mint_0".to_string(),
                serde_json::Value::String(mints[0].clone()),
            ),
            (
                "token_mint_1".to_string(),
                serde_json::Value::String(mints[1].clone()),
            ),
        ]);

        assert!(
            template.address.resolve(Some(&values)).is_some(),
            "every seed resolves, so the pool address does too"
        );

        values.remove("config_index");
        assert_eq!(
            template.address.resolve(Some(&values)),
            None,
            "a seed that cannot resolve must not derive a shorter address"
        );
    }

    #[test]
    fn test_registry_loads_all_protocols() {
        let registry = TemplateRegistry::new();

        // Should have Pyth (1 template) + Jupiter (1) + Raydium CLMM (1) + Raydium AMM v4 (4) + Drift(4) + Meteora (2) + Kamino(Lend 17, Scope 3, Farms 5, Swap 2, Vault 5, Liquidity 4) + Whirlpool(6) + SPL Token (2) = 57 total
        assert_eq!(
            registry.count(),
            57,
            "Registry should load 57 templates total"
        );

        assert!(registry.contains("pyth-price-feed-v2"));

        assert!(registry.contains("jupiter-token-ledger-override"));

        assert!(registry.contains("raydium-clmm-custom"));

        assert!(registry.contains("raydium-amm-pool-state"));
        assert!(registry.contains("raydium-amm-fees"));
        assert!(registry.contains("raydium-amm-swap-stats"));
        assert!(registry.contains("raydium-amm-custom"));

        assert!(registry.contains("meteora-dlmm-sol-usdc"));
        assert!(registry.contains("meteora-dlmm-usdt-sol"));

        assert!(registry.contains("kamino-reserve-state"));
        assert!(registry.contains("kamino-reserve-config"));
        assert!(registry.contains("kamino-reserve-status"));
        assert!(registry.contains("kamino-reserve-limits"));
        assert!(registry.contains("kamino-reserve-fees"));
        assert!(registry.contains("kamino-reserve-interest-rate"));
        assert!(registry.contains("kamino-reserve-oracle"));
        assert!(registry.contains("kamino-obligation-health"));
        assert!(registry.contains("kamino-obligation-positions"));
        assert!(registry.contains("kamino-obligation-orders"));
        assert!(registry.contains("kamino-lending-market-risk"));
        assert!(registry.contains("kamino-lending-market-elevation-groups"));
        assert!(registry.contains("kamino-reserve-rewards"));
        assert!(registry.contains("kamino-reserve-debt-term"));
        assert!(registry.contains("kamino-withdraw-ticket"));
        assert!(registry.contains("kamino-scope-price"));
        assert!(registry.contains("kamino-scope-price-source"));
        assert!(registry.contains("kamino-scope-twap"));
        assert!(registry.contains("kamino-farms-reward-emissions"));
        assert!(registry.contains("kamino-farms-reward-accumulator"));
        assert!(registry.contains("kamino-farms-user-rewards"));
        assert!(registry.contains("kamino-farms-farm-config"));
        assert!(registry.contains("kamino-farms-global-config"));
        assert!(registry.contains("kamino-swap-order"));
        assert!(registry.contains("kamino-swap-global-config"));
        assert!(registry.contains("kamino-vault-state"));
        assert!(registry.contains("kamino-vault-allocation"));
        assert!(registry.contains("kamino-vault-rewards"));
        assert!(registry.contains("kamino-vault-reserve-whitelist"));
        assert!(registry.contains("kamino-liquidity-strategy-balances"));
        assert!(registry.contains("kamino-liquidity-strategy-rewards"));
        assert!(registry.contains("kamino-liquidity-strategy-guards"));

        assert!(registry.contains("drift-perp-market"));
        assert!(registry.contains("drift-spot-market"));
        assert!(registry.contains("drift-user-state"));
        assert!(registry.contains("drift-global-state"));

        assert!(registry.contains("whirlpool-sol-usdc"));
        assert!(registry.contains("whirlpool-sol-usdt"));
        assert!(registry.contains("whirlpool-msol-sol"));
        assert!(registry.contains("whirlpool-orca-usdc"));
        assert!(registry.contains("whirlpool-popcat-sol"));
        assert!(registry.contains("whirlpool-custom"));

        assert!(registry.contains("spl-token-account-balance"));
        assert!(registry.contains("spl-token-mint-supply"));
    }

    #[test]
    fn test_jupiter_template_loads_correctly() {
        let registry = TemplateRegistry::new();

        let jupiter_template = registry
            .get("jupiter-token-ledger-override")
            .expect("Jupiter template should exist");

        assert_eq!(jupiter_template.protocol, "Jupiter");
        assert_eq!(jupiter_template.account_type, "TokenLedger");
        assert_eq!(jupiter_template.name, "Override Jupiter Token Ledger");
        assert_eq!(jupiter_template.properties.len(), 2);

        let property_paths: Vec<&str> = jupiter_template.property_paths();
        assert!(property_paths.contains(&"tokenAccount"));
        assert!(property_paths.contains(&"amount"));
        assert!(jupiter_template.tags.contains(&"dex".to_string()));
        assert!(jupiter_template.tags.contains(&"aggregator".to_string()));
        assert!(jupiter_template.tags.contains(&"swap".to_string()));
        assert!(jupiter_template.tags.contains(&"defi".to_string()));
    }

    #[test]
    fn test_filter_by_protocol() {
        let registry = TemplateRegistry::new();

        let pyth_templates = registry.by_protocol("Pyth");
        assert_eq!(pyth_templates.len(), 1, "Should have 1 Pyth template");

        let jupiter_templates = registry.by_protocol("Jupiter");
        assert_eq!(jupiter_templates.len(), 1, "Should have 1 Jupiter template");

        let raydium_templates = registry.by_protocol("Raydium");
        assert_eq!(
            raydium_templates.len(),
            5,
            "Should have 5 Raydium templates (1 CLMM + 4 AMM v4)"
        );

        let kamino_templates = registry.by_protocol("kamino");
        assert_eq!(
            kamino_templates.len(),
            17,
            "Should have 17 Kamino Lend templates"
        );
        assert_eq!(
            registry.by_protocol("kamino-scope").len(),
            3,
            "Should have 3 Kamino Scope templates"
        );
        assert_eq!(
            registry.by_protocol("kamino-farms").len(),
            5,
            "Should have 5 Kamino Farms templates"
        );
        assert_eq!(
            registry.by_protocol("kamino-swap").len(),
            2,
            "Should have 2 Kamino Swap templates"
        );
        assert_eq!(
            registry.by_protocol("kamino-vault").len(),
            5,
            "Should have 5 Kamino Earn vault templates"
        );
        assert_eq!(
            registry.by_protocol("kamino-liquidity").len(),
            4,
            "Should have 4 Kamino Liquidity templates"
        );

        // Each Kamino-family protocol must cover the accounts worth overriding
        for (protocol, expected_accounts) in [
            (
                "kamino",
                vec!["Reserve", "Obligation", "LendingMarket", "WithdrawTicket"],
            ),
            (
                "kamino-scope",
                vec!["OraclePrices", "OracleMappings", "OracleTwaps"],
            ),
            (
                "kamino-farms",
                vec!["FarmState", "UserState", "GlobalConfig"],
            ),
            ("kamino-swap", vec!["Order", "GlobalConfig"]),
            ("kamino-vault", vec!["VaultState", "ReserveWhitelistEntry"]),
            ("kamino-liquidity", vec!["WhirlpoolStrategy"]),
        ] {
            let account_types: BTreeSet<&str> = registry
                .by_protocol(protocol)
                .iter()
                .map(|t| t.account_type.as_str())
                .collect();
            for expected in expected_accounts {
                assert!(
                    account_types.contains(expected),
                    "{} should have at least one template for the {} account",
                    protocol,
                    expected
                );
            }
        }

        let whirlpool_templates = registry.by_protocol("Whirlpool");
        assert_eq!(
            whirlpool_templates.len(),
            6,
            "Should have 6 Whirlpool templates"
        );
    }

    #[test]
    fn test_filter_by_tags() {
        let registry = TemplateRegistry::new();

        let oracle_templates = registry.by_tags(&[vec!["oracle".to_string()]].concat());
        assert_eq!(
            oracle_templates.len(),
            4,
            "Should find 4 oracle templates (Pyth + 3 Kamino Scope)"
        );

        let rewards_templates = registry.by_tags(&[vec!["rewards".to_string()]].concat());
        assert_eq!(
            rewards_templates.len(),
            5,
            "Should find 5 rewards templates (Kamino Farms)"
        );

        let dex_templates = registry.by_tags(&[vec!["dex".to_string()]].concat());
        assert_eq!(
            dex_templates.len(),
            1,
            "Should find 1 dex template (Jupiter)"
        );

        let aggregator_templates = registry.by_tags(&[vec!["aggregator".to_string()]].concat());
        assert_eq!(
            aggregator_templates.len(),
            1,
            "Should find 1 aggregator template (Jupiter)"
        );
    }

    #[test]
    fn test_jupiter_idl_has_token_ledger_account() {
        let registry = TemplateRegistry::new();
        let jupiter_template = registry.get("jupiter-token-ledger-override").unwrap();
        let has_token_ledger = jupiter_template
            .idl
            .accounts
            .iter()
            .any(|acc| acc.name == "TokenLedger");

        assert!(has_token_ledger, "IDL should contain TokenLedger account");
    }

    #[test]
    fn test_list_all_template_ids() {
        let registry = TemplateRegistry::new();
        let ids = registry.list_ids();

        assert!(ids.contains(&"raydium-clmm-custom".to_string()));
        assert!(ids.contains(&"raydium-amm-pool-state".to_string()));
        assert!(ids.contains(&"raydium-amm-custom".to_string()));
        assert!(ids.contains(&"jupiter-token-ledger-override".to_string()));
        assert!(ids.contains(&"pyth-price-feed-v2".to_string()));
        assert!(ids.contains(&"meteora-dlmm-sol-usdc".to_string()));
        assert!(ids.contains(&"kamino-reserve-state".to_string()));
        assert!(ids.contains(&"kamino-reserve-config".to_string()));
        assert!(ids.contains(&"kamino-obligation-health".to_string()));
        assert!(ids.contains(&"kamino-obligation-positions".to_string()));
        assert!(ids.contains(&"kamino-reserve-oracle".to_string()));
        assert!(ids.contains(&"kamino-lending-market-risk".to_string()));
        assert!(ids.contains(&"kamino-scope-price".to_string()));
        assert!(ids.contains(&"kamino-farms-user-rewards".to_string()));
        assert!(ids.contains(&"drift-perp-market".to_string()));
        assert!(ids.contains(&"whirlpool-sol-usdc".to_string()));
        assert!(ids.contains(&"whirlpool-sol-usdt".to_string()));
        assert!(ids.contains(&"whirlpool-msol-sol".to_string()));
        assert!(ids.contains(&"whirlpool-orca-usdc".to_string()));
        assert!(ids.contains(&"whirlpool-popcat-sol".to_string()));
        assert!(ids.contains(&"whirlpool-custom".to_string()));
    }

    #[test]
    fn test_raydium_clmm_custom_loads_verified_tokens() {
        let registry = TemplateRegistry::new();

        let raydium_template = registry
            .get("raydium-clmm-custom")
            .expect("Raydium CLMM custom template should exist");

        // Check that token_mint constant exists and has options from verified_tokens
        let token_mint_constant = raydium_template
            .constants
            .get("token_mint")
            .expect("token_mint constant should exist");

        // Should have many tokens loaded from verified_tokens.csv
        assert!(
            token_mint_constant.options.len() > 100,
            "Should have many verified tokens loaded, got {}",
            token_mint_constant.options.len()
        );

        // Check that common tokens are present with correct addresses
        let sol_option = token_mint_constant
            .options
            .iter()
            .find(|o| o.id == "sol")
            .expect("SOL token should be present");
        assert_eq!(
            sol_option.value, "So11111111111111111111111111111111111111112",
            "SOL address should match"
        );

        let usdc_option = token_mint_constant
            .options
            .iter()
            .find(|o| o.id == "usdc")
            .expect("USDC token should be present");
        assert_eq!(
            usdc_option.value, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "USDC address should match"
        );

        // Check metadata is populated
        assert!(
            usdc_option.metadata.contains_key("symbol"),
            "Token should have symbol in metadata"
        );
        assert!(
            usdc_option.metadata.contains_key("decimals"),
            "Token should have decimals in metadata"
        );
    }

    #[test]
    fn test_raydium_amm_v4_has_only_openbook_market_options() {
        let registry = TemplateRegistry::new();

        // Test the raydium-amm-custom template which uses openbook_market constant_ref
        let raydium_v4_template = registry
            .get("raydium-amm-custom")
            .expect("Raydium AMM v4 custom template should exist");

        // Print ALL constants in this template to debug
        println!("Constants in raydium-amm-custom template:");
        for (name, constant) in &raydium_v4_template.constants {
            println!("  - {}: {} options", name, constant.options.len());
            for (i, opt) in constant.options.iter().take(3).enumerate() {
                println!("      {}: id={}, value={}", i, opt.id, opt.value);
            }
        }

        // Check that openbook_market constant exists
        let openbook_market_constant = raydium_v4_template
            .constants
            .get("openbook_market")
            .expect("openbook_market constant should exist");

        println!(
            "\nopenbook_market has {} options",
            openbook_market_constant.options.len()
        );

        // Print first 5 options to debug
        for (i, opt) in openbook_market_constant.options.iter().take(5).enumerate() {
            println!(
                "  Option {}: id={}, label={}, value={}",
                i, opt.id, opt.label, opt.value
            );
        }

        // Should have around 100 OpenBook markets (not thousands of tokens)
        assert!(
            openbook_market_constant.options.len() <= 200,
            "openbook_market should have only market options, not verified tokens. Got {} options",
            openbook_market_constant.options.len()
        );

        // Should NOT contain token symbols like "sol" or "usdc" as IDs
        // Market IDs should be like "sol-usdc" or "ray-sol"
        let has_standalone_sol = openbook_market_constant
            .options
            .iter()
            .any(|o| o.id == "sol");
        assert!(
            !has_standalone_sol,
            "openbook_market should NOT have standalone 'sol' option (that's a token, not a market)"
        );

        // Should have market pair IDs like "sol-usdc"
        let has_sol_usdc_market = openbook_market_constant
            .options
            .iter()
            .any(|o| o.id == "sol-usdc" || o.id.contains("-usdc") || o.id.contains("-sol"));
        assert!(
            has_sol_usdc_market,
            "openbook_market should have market pair IDs like 'sol-usdc'"
        );

        // Also make sure raydium-amm-custom does NOT have token_mint constant
        // (that's for CLMM v3, not AMM v4)
        let has_token_mint = raydium_v4_template.constants.contains_key("token_mint");
        assert!(
            !has_token_mint,
            "AMM v4 template should NOT have token_mint constant (that's for CLMM v3)"
        );
    }

    #[test]
    fn test_pyth_price_feed_pda_derivation() {
        use std::{collections::HashMap, str::FromStr};

        use solana_pubkey::Pubkey;

        // Test direct derivation first to verify the algorithm
        let program_id = Pubkey::from_str("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT")
            .expect("Valid program ID");

        // SOL/USD feed ID (32 bytes)
        let feed_id_hex = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
        let feed_id_bytes = hex::decode(feed_id_hex).expect("Valid hex");
        assert_eq!(feed_id_bytes.len(), 32, "Feed ID must be 32 bytes");

        // Shard ID 0 as u16 little-endian (2 bytes)
        let shard_id: u16 = 0;
        let shard_bytes = shard_id.to_le_bytes();

        // Derive PDA with seeds: [shard_id (u16 LE), feed_id (32 bytes)]
        let seeds: &[&[u8]] = &[&shard_bytes, &feed_id_bytes];
        let (direct_pda, _bump) = Pubkey::find_program_address(seeds, &program_id);

        println!("Direct PDA derivation:");
        println!("  Program ID: {}", program_id);
        println!("  Shard bytes (u16 LE): {:?}", shard_bytes);
        println!("  Feed ID bytes (first 8): {:?}...", &feed_id_bytes[..8]);
        println!("  Derived PDA: {}", direct_pda);

        // Expected address (verified on-chain as SOL/USD price feed)
        let expected_address =
            Pubkey::from_str("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE").expect("Valid pubkey");

        println!("  Expected PDA: {}", expected_address);

        // Now test via the registry
        let registry = TemplateRegistry::new();
        let pyth_template = registry
            .get("pyth-price-feed-v2")
            .expect("Pyth price feed template should exist");

        let sol_feed_id = "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

        let mut values = HashMap::new();
        values.insert(
            "feed_id".to_string(),
            serde_json::Value::String(sol_feed_id.to_string()),
        );

        let resolved_address = pyth_template
            .address
            .resolve(Some(&values))
            .expect("Should resolve PDA address");

        println!("\nRegistry PDA derivation:");
        println!("  Resolved PDA: {}", resolved_address);

        // Check if both match
        assert_eq!(
            direct_pda, resolved_address,
            "Direct and registry derivation should match"
        );

        assert_eq!(
            resolved_address, expected_address,
            "Pyth SOL/USD PDA should match expected address.\nGot: {}\nExpected: {}",
            resolved_address, expected_address
        );

        // Also verify direct derivation matches
        assert_eq!(
            direct_pda, expected_address,
            "Direct PDA derivation should match expected SOL/USD address"
        );
    }

    #[test]
    fn test_get_pda_seed_references() {
        use surfpool_types::AccountAddress;

        // Test with Bytes32Ref seed (Pyth feed_id)
        let account_json = r#"{
            "pda": {
                "programId": "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT",
                "seeds": [
                    {"u16Le": 0},
                    {"bytes32Ref": "feed_id"}
                ]
            }
        }"#;

        let account: AccountAddress =
            serde_json::from_str(account_json).expect("Should parse AccountAddress from JSON");

        let refs = account.get_pda_seed_references();
        assert_eq!(
            refs,
            vec!["feed_id"],
            "Should extract feed_id as PDA seed reference"
        );

        // Test with PropertyRef seed (Raydium token mints)
        let raydium_json = r#"{
            "pda": {
                "programId": "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
                "seeds": [
                    {"string": "pool"},
                    {"propertyRef": "token_mint_0"},
                    {"propertyRef": "token_mint_1"},
                    {"u16Be": 100}
                ]
            }
        }"#;

        let raydium_account: AccountAddress =
            serde_json::from_str(raydium_json).expect("Should parse Raydium AccountAddress");

        let raydium_refs = raydium_account.get_pda_seed_references();
        assert_eq!(
            raydium_refs,
            vec!["token_mint_0", "token_mint_1"],
            "Should extract both token mint refs"
        );

        // Test with plain Pubkey (no PDA refs)
        let pubkey_json = r#"{"pubkey": "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE"}"#;
        let pubkey_account: AccountAddress =
            serde_json::from_str(pubkey_json).expect("Should parse Pubkey AccountAddress");

        let pubkey_refs = pubkey_account.get_pda_seed_references();
        assert!(
            pubkey_refs.is_empty(),
            "Pubkey address should have no PDA refs"
        );
    }

    #[test]
    fn test_filter_pda_refs_from_override_values() {
        use std::collections::HashMap;

        use surfpool_types::AccountAddress;

        // Simulate what happens in materialize_overrides_for_slot
        let account_json = r#"{
            "pda": {
                "programId": "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT",
                "seeds": [
                    {"u16Le": 0},
                    {"bytes32Ref": "feed_id"}
                ]
            }
        }"#;

        let account: AccountAddress = serde_json::from_str(account_json).unwrap();

        // Values from the override instance (includes both PDA ref and account data fields)
        let mut values: HashMap<String, serde_json::Value> = HashMap::new();
        values.insert(
            "feed_id".to_string(),
            serde_json::Value::String(
                "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d".to_string(),
            ),
        );
        values.insert(
            "price_message.price".to_string(),
            serde_json::json!(15000000000i64),
        );
        values.insert("price_message.conf".to_string(), serde_json::json!(100));

        // Filter out PDA refs (this is what materialize_overrides_for_slot does)
        let pda_refs = account.get_pda_seed_references();
        let account_values: HashMap<String, serde_json::Value> = values
            .iter()
            .filter(|(key, _)| !pda_refs.contains(key))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // feed_id should be filtered out, only account data fields remain
        assert!(
            !account_values.contains_key("feed_id"),
            "feed_id should be filtered out as it's a PDA seed ref"
        );
        assert!(
            account_values.contains_key("price_message.price"),
            "price_message.price should remain"
        );
        assert!(
            account_values.contains_key("price_message.conf"),
            "price_message.conf should remain"
        );
        assert_eq!(
            account_values.len(),
            2,
            "Should have 2 account data fields after filtering"
        );
    }

    #[test]
    fn test_pda_derivation_from_json_override_instance() {
        use std::str::FromStr;

        use solana_pubkey::Pubkey;
        use surfpool_types::{AccountAddress, OverrideInstance};

        // First, test AccountAddress deserialization directly
        let account_json = r#"{
            "pda": {
                "programId": "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT",
                "seeds": [
                    {"u16Le": 0},
                    {"bytes32Ref": "feed_id"}
                ]
            }
        }"#;

        let account: AccountAddress =
            serde_json::from_str(account_json).expect("Should parse AccountAddress from JSON");
        println!("Parsed AccountAddress: {:?}", account);

        // This JSON is exactly what the LLM sends
        let json = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440004",
            "templateId": "pyth-price-feed-v2",
            "values": {
                "feed_id": "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
                "price_message.price": 11000000000
            },
            "scenarioRelativeSlot": 2,
            "label": "SOL Price Rebounds to $110",
            "enabled": true,
            "fetchBeforeUse": false,
            "account": {
                "pda": {
                    "programId": "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT",
                    "seeds": [
                        {"u16Le": 0},
                        {"bytes32Ref": "feed_id"}
                    ]
                }
            }
        }"#;

        let override_instance: OverrideInstance =
            serde_json::from_str(json).expect("Should parse OverrideInstance from JSON");

        println!("Parsed OverrideInstance:");
        println!("  Template ID: {}", override_instance.template_id);
        println!("  Values: {:?}", override_instance.values);
        println!("  Account: {:?}", override_instance.account);

        // Resolve the PDA using the values from the override instance
        let resolved_address = override_instance
            .account
            .resolve(Some(&override_instance.values))
            .expect("Should resolve PDA address from JSON");

        println!("  Resolved PDA: {}", resolved_address);

        // Expected SOL/USD price feed address
        let expected_address =
            Pubkey::from_str("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE").expect("Valid pubkey");

        assert_eq!(
            resolved_address, expected_address,
            "PDA from JSON should match expected SOL/USD address.\nGot: {}\nExpected: {}",
            resolved_address, expected_address
        );
    }

    /// A property that does not exist in the IDL is dropped at materialization time with only
    /// a warning, so the scenario appears to run while changing nothing.
    #[test]
    fn test_all_template_property_paths_exist_in_idl() {
        let registry = TemplateRegistry::new();
        let mut errors = Vec::new();

        for template in registry.all() {
            for property in &template.properties {
                // constant_ref properties are UI dropdowns (e.g. token pickers), not
                // account fields, so they are not expected to resolve against the IDL.
                if property.is_constant_ref() {
                    continue;
                }
                if let Err(e) = surfpool_types::resolve_idl_type(
                    &template.idl,
                    &template.account_type,
                    &property.path,
                ) {
                    errors.push(format!("[{}] {}: {}", template.id, property.path, e));
                }
            }
        }

        assert!(
            errors.is_empty(),
            "{} template propert(ies) do not exist in their IDL:\n  {}",
            errors.len(),
            errors.join("\n  ")
        );
    }

    #[test]
    fn test_kamino_templates_round_trip_through_forge() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        // Live mainnet sizes. Keyed by (protocol, account) because `GlobalConfig` is a
        // different struct in four of these programs.
        const ACCOUNT_SIZES: &[(&str, &str, usize)] = &[
            // Kamino Lend (KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD)
            ("kamino", "Reserve", 8624),
            ("kamino", "Obligation", 3344),
            ("kamino", "LendingMarket", 4664),
            // No WithdrawTicket existed on mainnet when this was written (the feature is new
            // in klend 1.23.0), so this size is derived from the IDL rather than observed.
            ("kamino", "WithdrawTicket", 520),
            // Scope (HFn8GnPADiny6XqUoWE8uRPPxb29ikn4yTuPa9MF2fWJ)
            ("kamino-scope", "OraclePrices", 28712),
            ("kamino-scope", "OracleMappings", 29704),
            ("kamino-scope", "OracleTwaps", 344136),
            // Kamino Farms (FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr)
            ("kamino-farms", "FarmState", 8336),
            ("kamino-farms", "UserState", 920),
            ("kamino-farms", "GlobalConfig", 2136),
            // LIMO / Kamino Swap (LiMoM9rMhrdYrfzUCxQppvxCSG1FcrUK9G8uLq4A1GF)
            ("kamino-swap", "Order", 424),
            ("kamino-swap", "GlobalConfig", 2168),
            // Kamino Vaults / Earn (KvauGMspG5k6rtzrqqn7WNn3oZdyKqLKwK2XWQ8FLjd)
            ("kamino-vault", "VaultState", 62552),
            ("kamino-vault", "ReserveWhitelistEntry", 136),
            // Kamino Liquidity / yvaults (6LtLpnUFNByNXLyCoK9wA2MykKAmQNZKBdY8s47dehDc)
            ("kamino-liquidity", "WhirlpoolStrategy", 4064),
        ];

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let pubkey = Pubkey::new_unique();
        let mut checked = 0;

        for protocol in [
            "kamino",
            "kamino-scope",
            "kamino-farms",
            "kamino-swap",
            "kamino-vault",
            "kamino-liquidity",
        ] {
            let templates = registry.by_protocol(protocol);
            assert!(
                !templates.is_empty(),
                "expected templates for protocol {}",
                protocol
            );

            for template in templates {
                let (_, _, size) = ACCOUNT_SIZES
                    .iter()
                    .find(|(proto, name, _)| *proto == protocol && *name == template.account_type)
                    .unwrap_or_else(|| {
                        panic!(
                            "template {} targets {}/{} with no known size; add it to ACCOUNT_SIZES",
                            template.id, protocol, template.account_type
                        )
                    });

                let account_def = template
                    .idl
                    .accounts
                    .iter()
                    .find(|a| a.name == template.account_type)
                    .unwrap_or_else(|| {
                        panic!(
                            "account '{}' not found in the {} IDL (template {})",
                            template.account_type, protocol, template.id
                        )
                    });

                let mut data = vec![0u8; *size];
                data[..8].copy_from_slice(&account_def.discriminator);

                // A zeroed account with no overrides must survive the decode/re-encode cycle
                // byte-for-byte, otherwise the pipeline is silently rewriting account state.
                let identity = surfnet_svm
                    .get_forged_account_data(&pubkey, &data, &template.idl, &HashMap::new())
                    .unwrap_or_else(|e| {
                        panic!("identity round-trip failed for {}: {}", template.id, e)
                    });
                assert_eq!(
                    identity, data,
                    "identity round-trip changed bytes for {}",
                    template.id
                );

                // Now write every scalar property the template advertises, in one pass.
                let mut overrides: HashMap<String, serde_json::Value> = HashMap::new();
                for property in &template.properties {
                    let ty = surfpool_types::resolve_idl_type(
                        &template.idl,
                        &template.account_type,
                        &property.path,
                    )
                    .unwrap_or_else(|e| panic!("[{}] {}: {}", template.id, property.path, e));
                    if let Some(value) = sample_scalar_value(ty) {
                        overrides.insert(property.path.clone(), value);
                    }
                }

                if overrides.is_empty() {
                    // Composite-only template (e.g. kamino-reserve-interest-rate exposes a
                    // single struct); its llm_context documents the required full shape.
                    continue;
                }

                let forged = surfnet_svm
                    .get_forged_account_data(&pubkey, &data, &template.idl, &overrides)
                    .unwrap_or_else(|e| {
                        panic!(
                            "forge failed for {} with {} scalar override(s): {}",
                            template.id,
                            overrides.len(),
                            e
                        )
                    });

                assert_eq!(
                    forged.len(),
                    data.len(),
                    "forged account size changed for {}",
                    template.id
                );
                assert_ne!(
                    forged, data,
                    "overrides for {} did not change any bytes",
                    template.id
                );
                checked += 1;
            }
        }

        assert!(
            checked >= 25,
            "expected to exercise at least 25 Kamino-family templates, got {}",
            checked
        );
    }

    /// The default pubkey "1111...1111" is all hex characters, which the encoder used to
    /// misread as hex bytes and panic on.
    #[test]
    fn test_kamino_obligation_array_index_and_pubkey_overrides() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        // Obligation offsets incl. discriminator: header is 88 bytes, then 136 per deposit.
        const DEPOSIT_0_RESERVE: usize = 8 + 88;
        const DEPOSIT_0_AMOUNT: usize = DEPOSIT_0_RESERVE + 32;
        const DEPOSIT_1_RESERVE: usize = 8 + 88 + 136;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let template = registry
            .get("kamino-obligation-positions")
            .expect("kamino-obligation-positions template should exist");

        let account_def = template
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "Obligation")
            .expect("Obligation account in Kamino IDL");
        let mut data = vec![0u8; 3344];
        data[..8].copy_from_slice(&account_def.discriminator);

        let wsol = "So11111111111111111111111111111111111111112";
        let overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                "deposits.0.deposit_reserve".to_string(),
                serde_json::json!("11111111111111111111111111111111"),
            ),
            (
                "deposits.0.deposited_amount".to_string(),
                serde_json::json!(4_200_000_000u64),
            ),
            (
                "deposits.1.deposit_reserve".to_string(),
                serde_json::json!(wsol),
            ),
            ("has_debt".to_string(), serde_json::json!(1)),
        ]);

        let forged = surfnet_svm
            .get_forged_account_data(&Pubkey::new_unique(), &data, &template.idl, &overrides)
            .expect("array-index and pubkey overrides should apply");

        assert_eq!(forged.len(), data.len(), "account size must be preserved");

        assert_eq!(
            &forged[DEPOSIT_0_RESERVE..DEPOSIT_0_RESERVE + 32],
            Pubkey::default().as_ref(),
            "deposits[0].deposit_reserve should be the default pubkey"
        );
        assert_eq!(
            u64::from_le_bytes(
                forged[DEPOSIT_0_AMOUNT..DEPOSIT_0_AMOUNT + 8]
                    .try_into()
                    .unwrap()
            ),
            4_200_000_000u64,
            "deposits[0].deposited_amount should be written at its array index"
        );
        assert_eq!(
            &forged[DEPOSIT_1_RESERVE..DEPOSIT_1_RESERVE + 32],
            Pubkey::from_str_const(wsol).as_ref(),
            "deposits[1].deposit_reserve should be the wSOL mint"
        );
    }

    #[test]
    fn test_array_index_override_path_errors() {
        use txtx_addon_kit::{indexmap::IndexMap, types::types::Value};

        use crate::surfnet::svm::apply_override_to_decoded_account;

        let mut decoded = Value::Object(IndexMap::from([(
            "deposits".to_string(),
            Value::Array(Box::new(vec![Value::Integer(1), Value::Integer(2)])),
        )]));

        assert!(
            apply_override_to_decoded_account(&mut decoded, "deposits.1", &serde_json::json!(9))
                .is_ok()
        );
        match &decoded {
            Value::Object(map) => match map.get("deposits") {
                Some(Value::Array(items)) => assert_eq!(items[1], Value::Integer(9)),
                _ => panic!("expected deposits array"),
            },
            _ => panic!("expected object"),
        }

        // out-of-bounds index
        let err =
            apply_override_to_decoded_account(&mut decoded, "deposits.7", &serde_json::json!(1))
                .expect_err("index 7 is out of bounds for a 2-element array");
        assert!(
            format!("{err}").contains("out of bounds"),
            "unexpected error: {err}"
        );

        // non-numeric segment on an array
        let err = apply_override_to_decoded_account(
            &mut decoded,
            "deposits.first",
            &serde_json::json!(1),
        )
        .expect_err("'first' is not an array index");
        assert!(
            format!("{err}").contains("zero-based array index"),
            "unexpected error: {err}"
        );

        // empty segment
        assert!(
            apply_override_to_decoded_account(&mut decoded, "deposits..0", &serde_json::json!(1))
                .is_err()
        );
    }

    #[test]
    fn test_kamino_scope_price_override_writes_expected_bytes() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        // OraclePrices: discriminator + oracle_mappings pubkey, then 56 bytes per entry.
        const PRICES_BASE: usize = 8 + 32;
        const DATED_PRICE_SIZE: usize = 56;

        // A mechanical target; real per-token indices differ per price account.
        const SOL_INDEX: usize = 0;
        // $125.50 with exp = 8
        const SOL_VALUE: u64 = 12_550_000_000;
        const SOL_EXP: u64 = 8;
        const AT_SLOT: u64 = 370_000_000;
        const AT_TS: u64 = 1_800_000_000;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let template = registry
            .get("kamino-scope-price")
            .expect("kamino-scope-price template should exist");

        assert_eq!(
            template.address,
            surfpool_types::AccountAddress::Pubkey(
                "3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH".to_string()
            ),
            "template should default to the Main Market's Scope prices account"
        );

        let account_def = template
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "OraclePrices")
            .expect("OraclePrices in the Scope IDL");
        let mut data = vec![0u8; 28712];
        data[..8].copy_from_slice(&account_def.discriminator);

        let overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                format!("prices.{SOL_INDEX}.price.value"),
                serde_json::json!(SOL_VALUE),
            ),
            (
                format!("prices.{SOL_INDEX}.price.exp"),
                serde_json::json!(SOL_EXP),
            ),
            (
                format!("prices.{SOL_INDEX}.last_updated_slot"),
                serde_json::json!(AT_SLOT),
            ),
            (
                format!("prices.{SOL_INDEX}.unix_timestamp"),
                serde_json::json!(AT_TS),
            ),
        ]);

        let forged = surfnet_svm
            .get_forged_account_data(&Pubkey::new_unique(), &data, &template.idl, &overrides)
            .expect("scope price override should apply");

        assert_eq!(forged.len(), data.len(), "account size must be preserved");

        let base = PRICES_BASE + SOL_INDEX * DATED_PRICE_SIZE;
        let read = |off: usize| u64::from_le_bytes(forged[off..off + 8].try_into().unwrap());
        assert_eq!(read(base), SOL_VALUE, "price.value");
        assert_eq!(read(base + 8), SOL_EXP, "price.exp");
        assert_eq!(read(base + 16), AT_SLOT, "last_updated_slot");
        assert_eq!(read(base + 24), AT_TS, "unix_timestamp");

        // price = value / 10^exp
        assert_eq!(SOL_VALUE as f64 / 10f64.powi(SOL_EXP as i32), 125.50);

        // Neighbouring entries must be untouched.
        let next = PRICES_BASE + (SOL_INDEX + 1) * DATED_PRICE_SIZE;
        assert!(
            forged[next..next + DATED_PRICE_SIZE]
                .iter()
                .all(|b| *b == 0),
            "writing one price index must not disturb the next entry"
        );
    }

    /// A reward accrues from the gap between the farm accumulator and the user's tally, so
    /// both halves must be writable.
    #[test]
    fn test_kamino_farms_reward_override_writes_both_halves() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let pubkey = Pubkey::new_unique();

        let farm = registry
            .get("kamino-farms-reward-accumulator")
            .expect("kamino-farms-reward-accumulator template");
        let farm_def = farm
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "FarmState")
            .expect("FarmState in the Farms IDL");
        let mut farm_data = vec![0u8; 8336];
        farm_data[..8].copy_from_slice(&farm_def.discriminator);

        let farm_overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                "reward_infos.0.reward_per_share_scaled".to_string(),
                serde_json::json!(5_000_000u64),
            ),
            (
                "total_active_stake_scaled".to_string(),
                serde_json::json!(1_000_000u64),
            ),
        ]);
        let forged_farm = surfnet_svm
            .get_forged_account_data(&pubkey, &farm_data, &farm.idl, &farm_overrides)
            .expect("farm accumulator override should apply");
        assert_eq!(forged_farm.len(), farm_data.len());
        assert_ne!(forged_farm, farm_data);

        let user = registry
            .get("kamino-farms-user-rewards")
            .expect("kamino-farms-user-rewards template");
        let user_def = user
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "UserState")
            .expect("UserState in the Farms IDL");
        let mut user_data = vec![0u8; 920];
        user_data[..8].copy_from_slice(&user_def.discriminator);

        // UserState offsets incl. discriminator: 80-byte header, then the [u128; 10] tally.
        const TALLY_0: usize = 88;
        const UNCLAIMED_0: usize = TALLY_0 + 160;

        let user_overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                "rewards_issued_unclaimed.0".to_string(),
                serde_json::json!(777_000u64),
            ),
            (
                "rewards_tally_scaled.0".to_string(),
                serde_json::json!(0u64),
            ),
            (
                "active_stake_scaled".to_string(),
                serde_json::json!(1_000u64),
            ),
        ]);
        let forged_user = surfnet_svm
            .get_forged_account_data(&pubkey, &user_data, &user.idl, &user_overrides)
            .expect("user reward override should apply");

        assert_eq!(forged_user.len(), user_data.len());
        assert_eq!(
            u64::from_le_bytes(
                forged_user[UNCLAIMED_0..UNCLAIMED_0 + 8]
                    .try_into()
                    .unwrap()
            ),
            777_000u64,
            "rewards_issued_unclaimed[0] should be written at its array index"
        );
    }

    /// The two overrides that survive `refresh_obligation`: crash the Scope price, then
    /// tighten the deposit reserve's liquidation threshold.
    #[test]
    fn test_kamino_liquidation_setup_writes_durable_inputs() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        const LTV_PCT: usize = 4872;
        const LIQ_THRESHOLD_PCT: usize = 4873;
        const SCOPE_PRICES_BASE: usize = 8 + 32;
        const DATED_PRICE_SIZE: usize = 56;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let pubkey = Pubkey::new_unique();

        // Crash the Scope price the reserve prices from.
        let scope = registry.get("kamino-scope-price").expect("scope template");
        let scope_disc = &scope
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "OraclePrices")
            .expect("OraclePrices")
            .discriminator;
        let mut scope_data = vec![0u8; 28712];
        scope_data[..8].copy_from_slice(scope_disc);

        const IDX: usize = 45;
        const CRASHED: u64 = 15_000_000;
        let scope_overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                format!("prices.{IDX}.price.value"),
                serde_json::json!(CRASHED),
            ),
            (format!("prices.{IDX}.price.exp"), serde_json::json!(8u64)),
        ]);
        let forged_scope = surfnet_svm
            .get_forged_account_data(&pubkey, &scope_data, &scope.idl, &scope_overrides)
            .expect("scope crash should apply");

        let off = SCOPE_PRICES_BASE + IDX * DATED_PRICE_SIZE;
        assert_eq!(
            u64::from_le_bytes(forged_scope[off..off + 8].try_into().unwrap()),
            CRASHED,
            "crashed price must land at the Scope entry the reserve names"
        );
        assert_eq!(
            CRASHED as f64 / 10f64.powi(8),
            0.15,
            "value/exp must decode to $0.15"
        );

        // Tighten the deposit reserve's liquidation threshold.
        let reserve = registry
            .get("kamino-reserve-config")
            .expect("reserve config template");
        let reserve_disc = &reserve
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "Reserve")
            .expect("Reserve")
            .discriminator;
        let mut reserve_data = vec![0u8; 8624];
        reserve_data[..8].copy_from_slice(reserve_disc);
        // A healthy 70/75 configuration.
        reserve_data[LTV_PCT] = 70;
        reserve_data[LIQ_THRESHOLD_PCT] = 75;

        let reserve_overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                "config.liquidation_threshold_pct".to_string(),
                serde_json::json!(50u8),
            ),
            (
                "config.max_liquidation_bonus_bps".to_string(),
                serde_json::json!(1000u16),
            ),
        ]);
        let forged_reserve = surfnet_svm
            .get_forged_account_data(&pubkey, &reserve_data, &reserve.idl, &reserve_overrides)
            .expect("reserve config override should apply");

        assert_eq!(
            forged_reserve[LIQ_THRESHOLD_PCT], 50,
            "liquidation threshold must be lowered"
        );
        assert_eq!(
            forged_reserve[LTV_PCT], 70,
            "loan-to-value must be left untouched, so a position at 70% LTV is now above the \
             50% liquidation threshold and therefore liquidatable"
        );
        assert_eq!(
            forged_reserve.len(),
            reserve_data.len(),
            "reserve size must be preserved"
        );
    }

    /// A ticket becomes redeemable once the reserve's queue cursor reaches its sequence number.
    #[test]
    fn test_kamino_withdraw_ticket_and_queue_cursor() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let pubkey = Pubkey::new_unique();

        let ticket = registry
            .get("kamino-withdraw-ticket")
            .expect("withdraw ticket template");
        let ticket_disc = &ticket
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "WithdrawTicket")
            .expect("WithdrawTicket")
            .discriminator;
        let mut ticket_data = vec![0u8; 520];
        ticket_data[..8].copy_from_slice(ticket_disc);

        let ticket_overrides: HashMap<String, serde_json::Value> = HashMap::from([
            ("sequence_number".to_string(), serde_json::json!(7u64)),
            (
                "queued_collateral_amount".to_string(),
                serde_json::json!(500u64),
            ),
            ("invalid".to_string(), serde_json::json!(0u8)),
        ]);
        let forged_ticket = surfnet_svm
            .get_forged_account_data(&pubkey, &ticket_data, &ticket.idl, &ticket_overrides)
            .expect("withdraw ticket override should apply");
        assert_eq!(
            u64::from_le_bytes(forged_ticket[8..16].try_into().unwrap()),
            7,
            "ticket sequence number"
        );

        // Advance the reserve's cursor to 7, making ticket 7 serveable.
        let limits = registry
            .get("kamino-reserve-limits")
            .expect("reserve limits template");
        let reserve_disc = &limits
            .idl
            .accounts
            .iter()
            .find(|a| a.name == "Reserve")
            .expect("Reserve")
            .discriminator;
        let mut reserve_data = vec![0u8; 8624];
        reserve_data[..8].copy_from_slice(reserve_disc);

        let queue_overrides: HashMap<String, serde_json::Value> = HashMap::from([
            (
                "withdraw_queue.queued_collateral_amount".to_string(),
                serde_json::json!(500u64),
            ),
            (
                "withdraw_queue.next_withdrawable_ticket_sequence_number".to_string(),
                serde_json::json!(7u64),
            ),
            (
                "withdraw_queue.next_issued_ticket_sequence_number".to_string(),
                serde_json::json!(8u64),
            ),
            (
                "liquidity.total_available_amount".to_string(),
                serde_json::json!(0u64),
            ),
        ]);
        let forged_reserve = surfnet_svm
            .get_forged_account_data(&pubkey, &reserve_data, &limits.idl, &queue_overrides)
            .expect("withdraw queue override should apply");

        assert_eq!(forged_reserve.len(), reserve_data.len());
        assert_ne!(forged_reserve, reserve_data);
    }

    // Unmodified mainnet account data, captured 2026-08-06, with the source address of each so
    // it can be re-captured. Zeroed accounts never exercise real enum discriminants or non-zero
    // padding; these do. The reserve and Scope prices accounts are a matched pair -
    // test_reserve_price_is_derived_from_scope depends on it.
    // 14sqx2pLioXamoBFxE6CvHNth6uEAvJhXuJ2iwZMccAS
    const FIXTURE_RESERVE: &[u8] = include_bytes!("./fixtures/kamino_reserve.bin");
    // 3iprSGrEQdBxhmqV399tYQQPG8Z1Hh2aYFrBwgqFXjGS
    const FIXTURE_OBLIGATION: &[u8] = include_bytes!("./fixtures/kamino_obligation.bin");
    // 3NJYftD5sjVfxSnUdZ1wVML8f3aC6mp1CXCL6L7TnU8C
    const FIXTURE_SCOPE_PRICES: &[u8] = include_bytes!("./fixtures/kamino_scope_oracle_prices.bin");
    // 18DizwAbBuuNGwfav3v6yWMbunnye4RnMLwLp67jAtj
    const FIXTURE_FARM_STATE: &[u8] = include_bytes!("./fixtures/kamino_farms_farm_state.bin");
    // 14Buhfy7WBpiv2e6RMZNN5R7w3ua8MY1ZJ3WQyd29uJ
    const FIXTURE_SWAP_ORDER: &[u8] = include_bytes!("./fixtures/kamino_swap_order.bin");
    // 1EXN5b1z7wucGb2uZoQmqjHdPoK1PNfUNWuwq8AqLTV
    const FIXTURE_STRATEGY: &[u8] = include_bytes!("./fixtures/kamino_liquidity_strategy.bin");

    /// Byte indices at which two buffers differ.
    fn diff_indices(a: &[u8], b: &[u8]) -> Vec<usize> {
        a.iter()
            .zip(b.iter())
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .map(|(i, _)| i)
            .collect()
    }

    /// A failure here means a bundled IDL disagrees with the live on-chain layout.
    #[test]
    fn test_real_mainnet_accounts_round_trip_unchanged() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let pubkey = Pubkey::new_unique();

        let cases: &[(&str, &str, &[u8])] = &[
            ("kamino-reserve-config", "Reserve", FIXTURE_RESERVE),
            ("kamino-obligation-health", "Obligation", FIXTURE_OBLIGATION),
            ("kamino-scope-price", "OraclePrices", FIXTURE_SCOPE_PRICES),
            (
                "kamino-farms-reward-accumulator",
                "FarmState",
                FIXTURE_FARM_STATE,
            ),
            ("kamino-swap-order", "Order", FIXTURE_SWAP_ORDER),
            (
                "kamino-liquidity-strategy-balances",
                "WhirlpoolStrategy",
                FIXTURE_STRATEGY,
            ),
        ];

        for (template_id, account_name, data) in cases {
            let template = registry
                .get(template_id)
                .unwrap_or_else(|| panic!("template {} should exist", template_id));

            let account_def = template
                .idl
                .accounts
                .iter()
                .find(|a| a.name == *account_name)
                .unwrap_or_else(|| panic!("{} not in the IDL", account_name));
            assert_eq!(
                &data[..8],
                account_def.discriminator.as_slice(),
                "{} fixture discriminator does not match the IDL - wrong account type?",
                account_name
            );

            let forged = surfnet_svm
                .get_forged_account_data(&pubkey, data, &template.idl, &HashMap::new())
                .unwrap_or_else(|e| {
                    panic!(
                        "real mainnet {} failed to decode/re-encode with the bundled IDL: {}",
                        account_name, e
                    )
                });

            assert_eq!(
                forged.len(),
                data.len(),
                "{} changed size on round-trip",
                account_name
            );
            let diffs = diff_indices(&forged, data);
            assert!(
                diffs.is_empty(),
                "real mainnet {} was altered by a no-op round-trip at {} byte(s), first at {:?}",
                account_name,
                diffs.len(),
                diffs.first()
            );
        }
    }

    /// Catches collateral damage from the Borsh re-encode that a zeroed fixture would hide.
    #[test]
    fn test_override_on_real_account_touches_only_target_bytes() {
        use std::collections::HashMap;

        use solana_pubkey::Pubkey;

        use crate::surfnet::svm::SurfnetSvm;

        let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
        let registry = TemplateRegistry::new();
        let pubkey = Pubkey::new_unique();

        // Reserve: one u8 at a known offset.
        const LIQ_THRESHOLD_PCT: usize = 4873;
        let reserve = registry.get("kamino-reserve-config").unwrap();
        let original_threshold = FIXTURE_RESERVE[LIQ_THRESHOLD_PCT];
        assert!(
            original_threshold > 50,
            "fixture should start above the value we set, got {}",
            original_threshold
        );

        let forged = surfnet_svm
            .get_forged_account_data(
                &pubkey,
                FIXTURE_RESERVE,
                &reserve.idl,
                &HashMap::from([(
                    "config.liquidation_threshold_pct".to_string(),
                    serde_json::json!(50u8),
                )]),
            )
            .expect("threshold override on real reserve");

        assert_eq!(
            diff_indices(&forged, FIXTURE_RESERVE),
            vec![LIQ_THRESHOLD_PCT],
            "exactly one byte should change, and only the liquidation threshold"
        );
        assert_eq!(forged[LIQ_THRESHOLD_PCT], 50);

        // Scope: one u64 inside a 512-element array.
        const PRICES_BASE: usize = 8 + 32;
        const DATED_PRICE_SIZE: usize = 56;
        const IDX: usize = 0;
        let scope = registry.get("kamino-scope-price").unwrap();
        let value_off = PRICES_BASE + IDX * DATED_PRICE_SIZE;

        let original_value = u64::from_le_bytes(
            FIXTURE_SCOPE_PRICES[value_off..value_off + 8]
                .try_into()
                .unwrap(),
        );
        assert!(
            original_value > 0,
            "fixture SOL price should be non-zero, got {}",
            original_value
        );
        let new_value = original_value / 2; // halve SOL

        let forged = surfnet_svm
            .get_forged_account_data(
                &pubkey,
                FIXTURE_SCOPE_PRICES,
                &scope.idl,
                &HashMap::from([(
                    format!("prices.{IDX}.price.value"),
                    serde_json::json!(new_value),
                )]),
            )
            .expect("price override on real Scope account");

        let diffs = diff_indices(&forged, FIXTURE_SCOPE_PRICES);
        assert!(!diffs.is_empty(), "the price should have changed");
        assert!(
            diffs.iter().all(|i| (value_off..value_off + 8).contains(i)),
            "only the 8 bytes of prices[{}].price.value should change, got {:?}",
            IDX,
            diffs
        );
        assert_eq!(
            u64::from_le_bytes(forged[value_off..value_off + 8].try_into().unwrap()),
            new_value
        );

        let next = PRICES_BASE + DATED_PRICE_SIZE;
        assert_eq!(
            &forged[next..next + DATED_PRICE_SIZE],
            &FIXTURE_SCOPE_PRICES[next..next + DATED_PRICE_SIZE],
            "neighbouring Scope entry must not move"
        );
    }

    /// These addresses are hardcoded facts about mainnet, so guard their shape and uniqueness.
    /// A liveness check would need network access.
    #[test]
    fn test_named_kamino_reserve_templates_have_baked_addresses() {
        use std::{collections::BTreeSet, str::FromStr};

        use solana_pubkey::Pubkey;

        let registry = TemplateRegistry::new();

        const NAMED: &[&str] = &["kamino-reserve-main-sol", "kamino-reserve-main-usdc"];

        let mut addresses = BTreeSet::new();
        for id in NAMED {
            let template = registry
                .get(id)
                .unwrap_or_else(|| panic!("named reserve template {} should exist", id));

            assert_eq!(
                template.account_type, "Reserve",
                "{} should target a Reserve",
                id
            );

            let surfpool_types::AccountAddress::Pubkey(address) = &template.address else {
                panic!("{} should carry a plain pubkey address, not a PDA", id);
            };
            assert!(
                Pubkey::from_str(address).is_ok(),
                "{} has an unparseable address: {}",
                id,
                address
            );
            assert!(
                addresses.insert(address.clone()),
                "{} reuses an address already used by another named template",
                id
            );

            let paths: Vec<&str> = template.property_paths();
            for required in [
                "config.liquidation_threshold_pct",
                "liquidity.market_price_sf",
            ] {
                assert!(
                    paths.contains(&required),
                    "{} should expose {}",
                    id,
                    required
                );
            }

            // Each must point at the template that moves its price, and name its Scope index -
            // the lookup a user would otherwise do by hand.
            let context = template.llm_context.as_deref().unwrap_or_default();
            assert!(
                context.contains("kamino-scope-price"),
                "{} should point at kamino-scope-price for moving its price",
                id
            );
            assert!(
                context.contains("index"),
                "{} should name the Scope index its price comes from",
                id
            );
        }

        assert_eq!(
            addresses.len(),
            NAMED.len(),
            "all addresses must be distinct"
        );
    }

    /// Evidence that a Reserve's cached price is derived from Scope, which is why
    /// `kamino-scope-price` is the durable lever. The two fixtures are a matched pair: the
    /// reserve names this Scope account, and its `price_chain` product reproduces the cache.
    #[test]
    fn test_reserve_price_is_derived_from_scope() {
        use solana_pubkey::Pubkey;

        // Reserve offsets incl. discriminator.
        const MARKET_PRICE_SF: usize = 248; // u128 scaled fraction (value << 60)
        const SCOPE_PRICE_FEED: usize = 5112;
        const SCOPE_PRICE_CHAIN: usize = 5144; // [u16; 4], 65535 = unused
        const PRICES_BASE: usize = 8 + 32;
        const DATED_PRICE_SIZE: usize = 56;
        const UNUSED_CHAIN_ENTRY: u16 = 65535;

        let scope_account = Pubkey::from_str_const("3NJYftD5sjVfxSnUdZ1wVML8f3aC6mp1CXCL6L7TnU8C");

        assert_eq!(
            &FIXTURE_RESERVE[SCOPE_PRICE_FEED..SCOPE_PRICE_FEED + 32],
            scope_account.as_ref(),
            "the reserve fixture must price through the Scope account the other fixture holds"
        );

        let chain: Vec<u16> = (0..4)
            .map(|i| {
                let off = SCOPE_PRICE_CHAIN + i * 2;
                u16::from_le_bytes(FIXTURE_RESERVE[off..off + 2].try_into().unwrap())
            })
            .take_while(|entry| *entry != UNUSED_CHAIN_ENTRY)
            .collect();
        assert!(
            !chain.is_empty(),
            "the reserve fixture should name at least one Scope index"
        );

        // A chained price is the product of its entries, each value / 10^exp.
        let mut scope_price = 1.0f64;
        for index in &chain {
            let base = PRICES_BASE + (*index as usize) * DATED_PRICE_SIZE;
            let value =
                u64::from_le_bytes(FIXTURE_SCOPE_PRICES[base..base + 8].try_into().unwrap());
            let exp = u64::from_le_bytes(
                FIXTURE_SCOPE_PRICES[base + 8..base + 16]
                    .try_into()
                    .unwrap(),
            );
            assert!(
                value > 0 && exp < 30,
                "Scope entry {} looks unpopulated (value {}, exp {})",
                index,
                value,
                exp
            );
            scope_price *= value as f64 / 10f64.powi(exp as i32);
        }

        let cached_sf = u128::from_le_bytes(
            FIXTURE_RESERVE[MARKET_PRICE_SF..MARKET_PRICE_SF + 16]
                .try_into()
                .unwrap(),
        );
        let cached_price = cached_sf as f64 / 2f64.powi(60);
        assert!(cached_price > 0.0, "reserve fixture should have a price");

        // Captured together, so this is exact rather than approximate.
        let relative_error = (scope_price - cached_price).abs() / cached_price;
        assert!(
            relative_error < 1e-6,
            "reserve cached price ${cached_price} should equal the Scope chain {chain:?} product \
             ${scope_price} - if these have diverged, either the scaled-fraction interpretation \
             (value << 60), the price_chain semantics (a product), or an offset is wrong. \
             Relative error {relative_error}"
        );
    }

    /// A path ending on an index must resolve to the array's ELEMENT type. Resolving it to the
    /// array instead sends the value down the untyped conversion, where an all-hex base58 pubkey
    /// such as the default one is mistaken for hex and panics the request.
    #[test]
    fn test_terminal_array_index_resolves_to_the_element_type() {
        use anchor_lang_idl::types::IdlType;

        let registry = TemplateRegistry::new();
        let template = registry
            .get("kamino-scope-price-source")
            .expect("kamino-scope-price-source should exist");

        for (path, expected) in [
            ("price_info_accounts.0", IdlType::Pubkey),
            ("price_types.0", IdlType::U8),
            ("ref_price.0", IdlType::U16),
        ] {
            let resolved =
                surfpool_types::resolve_idl_type(&template.idl, &template.account_type, path)
                    .unwrap_or_else(|e| panic!("{path} should resolve: {e}"));
            assert_eq!(
                *resolved, expected,
                "{path} should resolve to its element type, not the array"
            );
        }

        // An index mid-path already worked; keep it that way.
        let obligation = registry
            .get("kamino-obligation-positions")
            .expect("kamino-obligation-positions should exist");
        let resolved = surfpool_types::resolve_idl_type(
            &obligation.idl,
            &obligation.account_type,
            "deposits.0.deposit_reserve",
        )
        .expect("deposits.0.deposit_reserve should resolve");
        assert_eq!(*resolved, IdlType::Pubkey);
    }

    /// Descriptions come from the IDL's own `docs`, or from an explicit `description` in the
    /// YAML. Studio and any LLM reading a template rely on them.
    #[test]
    fn test_every_kamino_property_has_a_description() {
        let registry = TemplateRegistry::new();
        let mut missing = Vec::new();
        let mut described = 0;

        for protocol in [
            "kamino",
            "kamino-scope",
            "kamino-farms",
            "kamino-swap",
            "kamino-vault",
            "kamino-liquidity",
        ] {
            for template in registry.by_protocol(protocol) {
                for property in &template.properties {
                    match property.description.as_deref() {
                        Some(text) if !text.trim().is_empty() => described += 1,
                        _ => missing.push(format!("{}:{}", template.id, property.path)),
                    }
                }
            }
        }

        assert!(
            missing.is_empty(),
            "{} Kamino propert(ies) have no description ({} do):\n  {}",
            missing.len(),
            described,
            missing.join("\n  ")
        );
    }
}
