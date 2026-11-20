use std::{
    collections::{BTreeMap, HashMap, HashSet},
    str::FromStr,
    sync::{Arc, RwLock},
};

use rmcp::{
    ServerHandler,
    handler::server::{
        router::{prompt::PromptRouter, tool::ToolRouter},
        wrapper::Parameters,
    },
    model::{ErrorData as McpError, *},
    prompt, prompt_handler, prompt_router, schemars, tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use set_token_account::{SeededAccount, SetAccountSuccess, SetTokenAccountsResponse};
use solana_pubkey::Pubkey;
use start_surfnet::StartSurfnetResponse;
use surfpool_core::{
    scenarios::{
        AUTHORING_GUIDE, TemplateRegistry,
        protocols::{
            phoenix_eternal::v1::{
                market::phoenix_markets,
                state_builder::PHOENIX_PERP_ASSET_MAP,
                trader::{MarginView, TraderPositions, trader_positions},
            },
            pump::v1::graduation_builder::{
                build_pump_graduation_scenario, pump_graduation_addresses,
            },
        },
    },
    solana_account::Account,
    solana_commitment_config::CommitmentConfig,
    surfnet::remote::SurfnetRemoteClient,
};
use surfpool_types::{
    AccountAddress, CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED, DEFAULT_RPC_PORT, Idl,
    OverrideInstance, OverrideTemplate, Scenario, VERIFIED_TOKENS_BY_SYMBOL,
    YamlOverrideTemplateCollection,
};

use crate::helpers::find_next_available_surfnet_port;

mod set_token_account;
mod start_surfnet;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StartSurfnetParams {
    #[schemars(
        description = "If `false` (default), returns a command for the AI to execute. If `true`, starts surfnet directly as a background process."
    )]
    pub run_as_subprocess: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetTokenAccountsParams {
    #[schemars(description = "The RPC URL of a running local surfnet instance.")]
    pub surfnet_address: String,
    #[schemars(
        description = "A list of accounts to create or fund. For each account, an optional owner can be specified; if omitted, a new wallet is generated. Token parameters include the mint, program ID, and amount."
    )]
    pub token_params_with_owner: Vec<CreateTokenAccountForOwnerParams>,
}

/// Page size of search_constant_options results. The exact value is a
/// judgment call: big enough that an ambiguous query ("USD") still returns a
/// useful choice, small enough that a response stays cheap in LLM context.
/// `totalMatches`/`truncated` tell the model to refine the query when more
/// options exist.
const MAX_SEARCH_RESULTS: usize = 20;

/// Compact template JSON shared by the get_override_templates tool and the
/// str:///override_templates resource. Constant options are summarized to a
/// count: inlined option lists run to hundreds of entries and are duplicated
/// across templates, pushing LLM clients past context and rate limits. Models
/// resolve concrete values through search_constant_options instead.
fn compact_template_json(template: &surfpool_types::OverrideTemplate) -> serde_json::Value {
    let constants: serde_json::Map<String, serde_json::Value> = template
        .constants
        .iter()
        .map(|(name, def)| {
            (
                name.clone(),
                serde_json::json!({
                    "label": def.label,
                    "description": def.description,
                    "optionsCount": def.options.len(),
                }),
            )
        })
        .collect();

    let mut obj = serde_json::json!({
        "id": template.id,
        "name": template.name,
        "description": template.description,
        "protocol": template.protocol,
        "accountType": template.account_type,
        "rawLayout": template.raw_layout,
        "properties": template.properties,
        "address": template.address,
        "constants": constants,
        "tags": template.tags
    });
    if let Some(ref ctx) = template.llm_context {
        obj["llmContext"] = serde_json::Value::String(ctx.clone());
    }
    obj
}

fn index_template_json(template: &surfpool_types::OverrideTemplate) -> serde_json::Value {
    serde_json::json!({
        "id": template.id,
        "name": template.name,
        "description": template.description,
        "protocol": template.protocol,
        "accountType": template.account_type,
        "tags": template.tags,
        "hasLlmContext": template.llm_context.is_some(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchConstantOptionsParams {
    #[schemars(
        description = "Template id from get_override_templates (e.g., \"pyth-price-feed-v2\")."
    )]
    pub template_id: String,
    #[schemars(
        description = "Name of the constant to search in (e.g., \"price_feed\", \"openbook_market\"). Omit to search every constant on the template."
    )]
    pub constant: Option<String>,
    #[schemars(
        description = "Case-insensitive text matched against option id, label, description and value (e.g., \"SOL/USD\"). An empty string returns the first options."
    )]
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatePumpGraduationScenarioParams {
    #[schemars(
        description = "Live Token-2022 Pump mint. If validation fails, report the error and do not retry without tokenMint."
    )]
    pub token_mint: String,
    #[schemars(
        description = "The port of the target running local surfnet instance (e.g., 8899, 18899, 28899, etc.). Omit to use the default port, 8899."
    )]
    pub surfnet_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetTemplateParams {
    #[schemars(
        description = "Template id from get_override_templates (e.g., \"pyth-price-feed-v2\")."
    )]
    pub template_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListPhoenixMarketsParams {
    #[schemars(
        description = "The port of the target running local surfnet instance (e.g., 8899, 18899, 28899, etc.). Omit to use the default port, 8899."
    )]
    pub surfnet_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListPhoenixTraderPositionsParams {
    #[schemars(description = "The address of the trader's Phoenix Trader account.")]
    pub trader: String,
    #[schemars(
        description = "The port of the target running local surfnet instance (e.g., 8899, 18899, 28899, etc.). Omit to use the default port, 8899."
    )]
    pub surfnet_port: Option<u16>,
    #[schemars(
        description = "Markets to move for marginAfterMoves, each { symbol, targetTicks } with targetTicks a decimal string or a number, the ticks phoenix-market-move takes. Omit for none."
    )]
    #[serde(default)]
    pub moves: Vec<PhoenixMove>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoenixMove {
    pub symbol: String,
    /// A decimal string or a JSON number.
    pub target_ticks: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StartSurfnetWithTokenAccountsParams {
    #[schemars(
        description = "A list of accounts to create or fund. For each account, an optional owner can be specified; if omitted, a new wallet is generated. Token parameters include the mint, program ID, and amount."
    )]
    pub token_params_with_owner: Vec<CreateTokenAccountForOwnerParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallSurfnetRpcParams {
    #[schemars(
        description = "The port of a running local surfnet instance (e.g., 8899, 18899, 28899, etc.)."
    )]
    pub surfnet_port: u16,
    #[schemars(
        description = "The RPC method name to call,for example: 'sendTransaction', 'simulateTransaction', 'getAccountInfo', 'getBalance', 'getTokenAccountBalance', 'getTokenSupply', 'getProgramAccounts', 'getTokenAccountsByOwner', 'getSlot',
        'getEpochInfo', 'requestAirdrop', 'surfnet_setAccount', 'getHealth', 'getTokenAccountsByOwner', 'getTokenAccountsByDelegate',
        'getTokenAccountsByDelegateAndMint', 'getTokenAccountsByDelegateAndMintAndOwner', 'getTokenAccountsByDelegateAndMintAndOwnerAndProgramId', 'getTokenAccountsByDelegateAndMintAndOwnerAndProgramIdAndOwner', surfnet_getProfileResults, etc.
        A list of all the RPC methods available can be found at str:///rpc_endpoints"
    )]
    pub method: String,
    #[schemars(description = "The parameters to pass to the RPC method")]
    pub params: Vec<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OverrideTemplateCollectionParams {
    #[schemars(
        description = "The text of the template collection YAML file, in the shape the authoring guide shows."
    )]
    pub collection_yaml: String,
    #[schemars(
        description = "The program's Anchor IDL in the 0.30+ format, as the JSON object or as the file's text. A legacy IDL is refused: run `anchor idl convert` on it first."
    )]
    pub idl: Value,
    #[schemars(
        description = "Replace the collection saved earlier in this session under the same protocol and version; its ids may be reused."
    )]
    #[serde(default)]
    pub overwrite: bool,
    #[schemars(
        description = "Port of the surfnet save_override_template registers the IDL on. Defaults to 8899. Validation does not use it."
    )]
    pub surfnet_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthorTemplateArgs {
    #[schemars(description = "Program id to write templates for.")]
    pub program_id: String,
    #[schemars(
        description = "What the templates should let a user do, e.g. \"freeze swaps on a pool\"."
    )]
    pub goal: String,
}

/// The template ids saved in this session, by the protocol and version of their collection.
type SavedCollections = BTreeMap<(String, String), Vec<String>>;

#[derive(Debug, Clone)]
pub struct Surfpool {
    pub surfnets: Arc<RwLock<HashMap<u16, u16>>>,
    pub template_registry: Arc<RwLock<TemplateRegistry>>,
    saved_collections: Arc<RwLock<SavedCollections>>,
    tool_router: ToolRouter<Surfpool>,
    prompt_router: PromptRouter<Surfpool>,
}

impl Surfpool {
    pub fn new() -> Self {
        Self {
            surfnets: Arc::new(RwLock::new(HashMap::new())),
            template_registry: Arc::new(RwLock::new(TemplateRegistry::new())),
            saved_collections: Arc::default(),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    fn checked_collection(
        &self,
        params: &OverrideTemplateCollectionParams,
    ) -> Result<CheckedCollection, String> {
        let registry = self
            .template_registry
            .read()
            .map_err(|_| "Failed to read template registry".to_string())?;
        let saved = self
            .saved_collections
            .read()
            .map_err(|_| "Failed to read saved templates".to_string())?;
        check_collection(&registry, &saved, params)
    }

    /// Adds a checked collection to this session in place of the one saved under its protocol
    /// and version, and returns its template ids.
    fn store_collection(
        &self,
        checked: CheckedCollection,
        overwrite: bool,
    ) -> Result<Vec<String>, String> {
        let mut registry = self
            .template_registry
            .write()
            .map_err(|_| "Failed to write template registry".to_string())?;
        let mut saved = self
            .saved_collections
            .write()
            .map_err(|_| "Failed to write saved templates".to_string())?;
        // Another save may have landed since the collection was checked.
        check_ids(
            &registry,
            &saved,
            &checked.key,
            &checked.templates,
            overwrite,
        )?;
        // The replaced collection's ids that the new one drops leave the session with it.
        for id in saved.remove(&checked.key).unwrap_or_default() {
            registry.templates.remove(&id);
        }
        let ids: Vec<String> = checked.templates.iter().map(|t| t.id.clone()).collect();
        for template in checked.templates {
            registry.templates.insert(template.id.clone(), template);
        }
        saved.insert(checked.key, ids.clone());
        Ok(ids)
    }

    /// The surfnet does not know a template saved in this session, so it would write that
    /// template's helper values as account fields. Such overrides get a resolved pubkey and only
    /// field values; built-in ones are left alone. Says whether any override changed.
    fn resolve_saved_overrides(&self, scenario: &mut Scenario) -> Result<bool, String> {
        let registry = self
            .template_registry
            .read()
            .map_err(|_| "Failed to read template registry".to_string())?;
        let saved = self
            .saved_collections
            .read()
            .map_err(|_| "Failed to read saved templates".to_string())?;
        let mut changed = false;
        for instance in &mut scenario.overrides {
            let is_saved = saved
                .values()
                .any(|ids| ids.contains(&instance.template_id));
            let Some(template) = registry.get(&instance.template_id).filter(|_| is_saved) else {
                continue;
            };
            resolve_override(template, instance)?;
            changed = true;
        }
        Ok(changed)
    }

    /// The scenario a `surfnet_registerScenario` call carries, resolved as `create_scenario`
    /// resolves it. A parameter that is no scenario goes through for the surfnet to judge.
    fn resolve_registered_scenario(&self, param: &mut Value) -> Result<(), String> {
        let Ok(mut scenario) = serde_json::from_value::<Scenario>(param.clone()) else {
            return Ok(());
        };
        if self.resolve_saved_overrides(&mut scenario)? {
            *param = serde_json::to_value(&scenario).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Resolves the account as the surfnet would and keeps only field values: `input`, `constant_ref`
/// and `dynamic_ref` values and the values PDA seeds reference are dropped.
fn resolve_override(
    template: &OverrideTemplate,
    instance: &mut OverrideInstance,
) -> Result<(), String> {
    if matches!(template.address, AccountAddress::Pda { .. }) {
        instance.account = template.address.clone();
    }
    let seeds = instance.account.get_pda_seed_references();
    let Some(pubkey) = instance.account.resolve(Some(&instance.values)) else {
        let missing: Vec<&String> = seeds
            .iter()
            .filter(|seed| !instance.values.contains_key(*seed))
            .collect();
        let reason = match &instance.account {
            AccountAddress::Pubkey(account) => format!("the account {account:?} is not a pubkey"),
            AccountAddress::Pda { .. } if !missing.is_empty() => {
                format!("the account's PDA needs a value for {missing:?}")
            }
            AccountAddress::Pda { .. } => {
                format!("the account's PDA cannot be derived from the values of {seeds:?}")
            }
        };
        return Err(format!(
            "Override '{}' (template '{}'): {reason}",
            instance.id, instance.template_id
        ));
    };
    let helpers: HashSet<&str> = template
        .properties
        .iter()
        .filter(|p| !p.is_field())
        .map(|p| p.path.as_str())
        .chain(seeds.iter().map(String::as_str))
        .collect();
    instance
        .values
        .retain(|key, _| !helpers.contains(key.as_str()));
    instance.account = AccountAddress::Pubkey(pubkey.to_string());
    Ok(())
}

struct CheckedCollection {
    key: (String, String),
    templates: Vec<OverrideTemplate>,
    idl: Idl,
}

/// The surfnet registers only the IDL format of Anchor 0.30 and later, the one with
/// `metadata.spec`.
fn parse_idl(idl: Value) -> Result<Idl, String> {
    let idl = match idl {
        Value::String(text) => serde_json::from_str(&text).map_err(|e| format!("idl: {e}"))?,
        idl => idl,
    };
    if !idl.is_object() {
        return Err("idl: expected the IDL's JSON object".into());
    }
    if !idl["metadata"]["spec"].is_string() {
        return Err(
            "idl: this is a legacy (pre-0.30) Anchor IDL; run `anchor idl convert` on it and pass the result"
                .into(),
        );
    }
    serde_json::from_value(idl).map_err(|e| format!("idl: {e}"))
}

/// Checks a user's collection against its IDL and the templates this session has. Catalogs are
/// refused because the surfnet filters a `constant_ref` selector out of a write only for a
/// template it was built with.
fn check_collection(
    registry: &TemplateRegistry,
    saved: &SavedCollections,
    params: &OverrideTemplateCollectionParams,
) -> Result<CheckedCollection, String> {
    let idl = parse_idl(params.idl.clone())?;
    Pubkey::from_str(&idl.address)
        .map_err(|e| format!("idl: address {:?} is not a pubkey: {e}", idl.address))?;
    let collection: YamlOverrideTemplateCollection = serde_yaml::from_str(&params.collection_yaml)
        .map_err(|e| format!("collectionYaml: {e}"))?;
    if collection.raw_layout {
        return Err("collectionYaml: raw_layout collections cannot be added at runtime".into());
    }
    if !collection.constants.is_empty() {
        return Err("collectionYaml: constants are not supported in user templates".into());
    }
    // An explicit empty list is accepted: a file-level `keep_alive: []` reads the same as none.
    let keep_alive = !collection.keep_alive.is_empty()
        || collection
            .templates
            .iter()
            .any(|t| t.keep_alive.as_ref().is_some_and(|k| !k.is_empty()));
    if keep_alive {
        return Err(
            "collectionYaml: keep_alive is not supported in user templates; they cannot keep accounts fresh, keep-alive comes only with surfpool's own templates"
                .into(),
        );
    }
    if let Some(entry) = collection
        .templates
        .iter()
        .find(|t| t.llm_context.is_none())
    {
        return Err(format!(
            "collectionYaml: template {} needs an llm_context",
            entry.id
        ));
    }
    let mut ids = HashSet::new();
    if let Some(entry) = collection.templates.iter().find(|t| !ids.insert(&t.id)) {
        return Err(format!("collectionYaml: the id {} is used twice", entry.id));
    }
    let key = (collection.protocol.clone(), collection.version.clone());
    let templates = collection.to_override_templates(Some(idl.clone()))?;
    for template in &templates {
        if !idl.accounts.iter().any(|a| a.name == template.account_type) {
            return Err(format!(
                "{}: the IDL has no account named {:?}",
                template.id, template.account_type
            ));
        }
        for property in template.properties.iter().filter(|p| p.is_field()) {
            surfpool_types::resolve_idl_type(&idl, &template.account_type, &property.path)
                .map_err(|e| format!("{}: {}: {e}", template.id, property.path))?;
        }
    }
    check_ids(registry, saved, &key, &templates, params.overwrite)?;
    Ok(CheckedCollection {
        key,
        templates,
        idl,
    })
}

/// Saving again under a saved protocol and version needs `overwrite`, which may reuse only that
/// collection's ids. A built-in id is never shadowed.
fn check_ids(
    registry: &TemplateRegistry,
    saved: &SavedCollections,
    key: &(String, String),
    templates: &[OverrideTemplate],
    overwrite: bool,
) -> Result<(), String> {
    if saved.contains_key(key) && !overwrite {
        return Err(format!(
            "{} {} was saved earlier in this session; pass overwrite: true to replace it",
            key.0, key.1
        ));
    }
    for template in templates {
        let saved_in = saved
            .iter()
            .find(|(_, ids)| ids.contains(&template.id))
            .map(|(key, _)| key);
        let built_in = (saved_in.is_none() && registry.contains(&template.id))
            || registry.keep_alive_template(&template.id).is_some();
        if built_in {
            return Err(format!(
                "{}: a built-in template already uses this id; pick another",
                template.id
            ));
        }
        if let Some(other) = saved_in.filter(|other| *other != key) {
            return Err(format!(
                "{}: the collection {} {} saved in this session already uses this id; pick another",
                template.id, other.0, other.1
            ));
        }
    }
    Ok(())
}

async fn surfnet_rpc(port: u16, method: &str, params: Value) -> Result<Value, String> {
    let request =
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response: Value = async {
        reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}"))
            .json(&request)
            .send()
            .await?
            .json()
            .await
    }
    .await
    .map_err(|e| format!("{method} on port {port}: {e}"))?;
    match response.get("error") {
        Some(error) => Err(format!("{method} on port {port}: {error}")),
        None => Ok(response.get("result").cloned().unwrap_or(Value::Null)),
    }
}

/// Registers the IDL unless the surfnet's active IDL for the program is already this one, and
/// says whether it registered.
async fn register_idl(port: u16, idl: &Idl) -> Result<bool, String> {
    let active = surfnet_rpc(
        port,
        "surfnet_getActiveIdl",
        serde_json::json!([idl.address]),
    )
    .await?;
    let active = active
        .get("value")
        .and_then(|value| serde_json::from_value::<Idl>(value.clone()).ok());
    if active.as_ref() == Some(idl) {
        return Ok(false);
    }
    surfnet_rpc(port, "surfnet_registerIdl", serde_json::json!([idl])).await?;
    Ok(true)
}

fn tool_result(result: Result<serde_json::Value, String>) -> CallToolResult {
    match result {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json.to_string())]),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
    }
}

#[prompt_router]
impl Surfpool {
    #[prompt(
        name = "author_override_template",
        description = "Write, validate and save override templates for a program from its IDL, for use in this MCP session."
    )]
    async fn author_override_template(
        &self,
        Parameters(args): Parameters<AuthorTemplateArgs>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "{AUTHORING_GUIDE}\n\nProgram: {}\nGoal: {}\n",
                args.program_id, args.goal
            ),
        )]
    }
}

impl Default for Surfpool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum JsonValue {
    #[schemars(description = "A JSON representation of null")]
    Null,
    #[schemars(description = "A JSON representation of a boolean")]
    Bool(bool),
    #[schemars(description = "A JSON representation of a number")]
    Number(i64),
    #[schemars(description = "A JSON representation of a string")]
    String(String),
    #[schemars(description = "A JSON representation of an array")]
    Array(Vec<JsonValue>),
    #[schemars(description = "A JSON representation of an object with key-value pairs")]
    Object(HashMap<String, JsonValue>),
    #[schemars(description = "A JSON representation of a base58-encoded public key")]
    PublicKey(String),
    #[schemars(description = "A JSON representation of a base58-encoded mint address")]
    MintAddress(String),
    #[schemars(description = "A JSON representation of a program ID")]
    ProgramId(String),
}

impl From<JsonValue> for Value {
    fn from(val: JsonValue) -> Self {
        match val {
            JsonValue::Null => Value::Null,
            JsonValue::Bool(b) => Value::Bool(b),
            JsonValue::Number(n) => Value::Number(serde_json::Number::from(n)),
            JsonValue::String(s) => Value::String(s),
            JsonValue::PublicKey(s) => Value::String(s),
            JsonValue::MintAddress(s) => Value::String(s),
            JsonValue::ProgramId(s) => Value::String(s),
            JsonValue::Array(arr) => Value::Array(arr.into_iter().map(Value::from).collect()),
            JsonValue::Object(obj) => {
                Value::Object(obj.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CreateTokenAccountForOwnerParams {
    #[schemars(
        description = "The owner address for the token accounts. If omitted, a new wallet will be generated and its address returned."
    )]
    pub owner: Option<String>,
    #[schemars(
        description = "A list of parameters for the tokens to be funded, including mint, program ID, and amount."
    )]
    pub params: Vec<CreateTokenAccountParams>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CreateTokenAccountParams {
    #[schemars(
        description = "The base58-encoded mint address of the token to fund. If provided, the token symbol will be inferred from the token mint address. If not provided, the token symbol must be provided to infer the token mint address."
    )]
    pub token_mint: Option<String>,
    #[schemars(
        description = "The token program ID (e.g., `Token` or `Token2022`). Defaults to the SPL Token Program if not provided."
    )]
    pub token_program_id: Option<String>,
    #[schemars(
        description = "The token symbol (e.g., USDC, JUP). If not provided, it will be inferred from the mint address."
    )]
    pub token_symbol: Option<String>,
    #[schemars(
        description = "The amount of tokens to fund, in human-readable units. Defaults to 100_000 if not provided."
    )]
    pub token_amount: Option<u64>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct StartSurfnetWithTokenAccountsSuccess {
    #[schemars(description = "The RPC URL of the newly started surfnet instance.")]
    pub surfnet_url: String,
    #[schemars(description = "The ID of the newly started surfnet instance.")]
    pub surfnet_id: u16,
    #[schemars(description = "A list of accounts that were created or funded.")]
    pub accounts: Vec<SetAccountSuccess>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct StartSurfnetWithTokenAccountsResponse {
    pub success: Option<StartSurfnetWithTokenAccountsSuccess>,
    pub error: Option<String>,
}

/// The successful token account updates and the failed ones' errors: a failure is
/// reported, never dropped. With no success at all there is no success list.
fn collect_token_results(
    results: Vec<SetTokenAccountsResponse>,
) -> (Option<Vec<SetAccountSuccess>>, Option<String>) {
    let mut accounts = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        accounts.extend(result.success.into_iter().flatten());
        errors.extend(result.error);
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    let accounts = (error.is_none() || !accounts.is_empty()).then_some(accounts);
    (accounts, error)
}

impl StartSurfnetWithTokenAccountsResponse {
    pub fn success(data: StartSurfnetWithTokenAccountsSuccess) -> Self {
        Self {
            success: Some(data),
            error: None,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            success: None,
            error: Some(message),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SurfnetRpcCallSuccess {
    #[schemars(description = "The RPC method that was called")]
    pub method: String,
    #[schemars(description = "The result returned by the RPC method")]
    pub result: Value,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SurfnetRpcCallResponse {
    pub success: Option<SurfnetRpcCallSuccess>,
    pub error: Option<String>,
}

impl SurfnetRpcCallResponse {
    pub fn success(method: String, result: Value) -> Self {
        Self {
            success: Some(SurfnetRpcCallSuccess { method, result }),
            error: None,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            success: None,
            error: Some(message),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct RegisterScenarioResponse {
    pub error: Option<String>,
    pub url: Option<String>,
}
impl RegisterScenarioResponse {
    pub fn success(url: String) -> Self {
        Self {
            error: None,
            url: Some(url),
        }
    }
    pub fn error(message: String) -> Self {
        Self {
            error: Some(message),
            url: None,
        }
    }
}

fn scenario_tool_error(message: String) -> CallToolResult {
    let response = RegisterScenarioResponse::error(message);
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string(&response).unwrap_or_default(),
    )])
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetTokenAddressParams {
    #[schemars(description = "The token symbol to look up (e.g., 'USDC', 'SOL', 'JUP')")]
    pub symbol: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TokenAddressResponse {
    pub symbol: String,
    pub address: Option<String>,
    pub name: Option<String>,
    pub decimals: Option<u8>,
    pub error: Option<String>,
}

impl TokenAddressResponse {
    pub fn success(symbol: String, address: String, name: String, decimals: u8) -> Self {
        Self {
            symbol,
            address: Some(address),
            name: Some(name),
            decimals: Some(decimals),
            error: None,
        }
    }
    pub fn not_found(symbol: String) -> Self {
        Self {
            symbol: symbol.clone(),
            address: None,
            name: None,
            decimals: None,
            error: Some(format!(
                "Token '{}' not found in verified tokens list",
                symbol
            )),
        }
    }
}

impl Surfpool {
    async fn fetch_surfnet_accounts(
        &self,
        surfnet_port: Option<u16>,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<Account>>, String> {
        let port = surfnet_port.unwrap_or(DEFAULT_RPC_PORT);
        self.fetch_accounts_at(&format!("http://127.0.0.1:{port}"), pubkeys)
            .await
    }

    /// Reads through the surfnet's own RPC: local state wins, only missing
    /// accounts fall back to its remote source.
    async fn fetch_accounts_at(
        &self,
        rpc_url: &str,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<Account>>, String> {
        let client = SurfnetRemoteClient::new(rpc_url);
        let accounts = client
            .get_multiple_accounts(pubkeys, CommitmentConfig::confirmed())
            .await
            .map_err(|error| format!("Failed to read accounts from Surfnet: {error}"))?;

        Ok(accounts
            .into_iter()
            .map(|result| result.map_account().ok())
            .collect())
    }

    /// The RPC URL Studio plays scenarios on, from the same `/config` its pages read.
    async fn studio_rpc_url(&self, studio_url: &str) -> Result<String, String> {
        let endpoint = format!("{studio_url}/config");
        let config = async {
            reqwest::get(&endpoint)
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }
        .await
        .map_err(|error| format!("Failed to read Studio's config at {endpoint}: {error}"))?;
        config
            .get("rpc_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("Studio's config at {endpoint} has no rpc_url"))
    }

    async fn stage_scenario(&self, scenario: Scenario) -> Result<CallToolResult, McpError> {
        let endpoint = format!(
            "http://127.0.0.1:{}/v1/scenarios",
            CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED
        );
        let response = match reqwest::Client::new()
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .json(&scenario)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let response = RegisterScenarioResponse::error(format!(
                    "Failed to load scenarios at {endpoint}: {error}"
                ));
                let json = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json)]));
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                let response = RegisterScenarioResponse::error(format!(
                    "Failed to read response text: {error}"
                ));
                let json = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json)]));
            }
        };
        let response: serde_json::Value = match serde_json::from_str(&body) {
            Ok(response) => response,
            Err(error) => {
                let response = RegisterScenarioResponse::error(format!(
                    "Failed to parse JSON response: {error}. Response: {body}"
                ));
                let json = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json)]));
            }
        };
        if status == reqwest::StatusCode::CONFLICT {
            let response = RegisterScenarioResponse::error(format!(
                "A different scenario is already stored under id {:?}. Pick another id, or delete the existing one first.",
                scenario.id
            ));
            let json = serde_json::to_string(&response).unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(json)]));
        }
        if let Some(error) = response.get("error") {
            let response = RegisterScenarioResponse::error(format!("RPC error: {error}"));
            let json = serde_json::to_string(&response).unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(json)]));
        }

        let scenario_id = response
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or(&scenario.id);
        let url = format!(
            "http://127.0.0.1:{}/scenarios?id={scenario_id}&tab=editor",
            CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED
        );
        let response = RegisterScenarioResponse::success(url);
        let json = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }
}

#[tool_router]
impl Surfpool {
    /// Returns a command to start a new local Solana network (surfnet).
    /// This method is exposed as a tool and can be invoked remotely.
    #[tool(description = "Starts a new local Solana network (surfnet).
        This tool returns a shell command that the AI must execute in a new terminal.
        This is the default behavior (`run_as_subprocess: false`).
        If the user explicitly asks to run surfnet as a background process,
        set `run_as_subprocess: true` to start it directly. When in doubt, return the command for execution.")]
    async fn start_surfnet(
        &self,
        Parameters(params): Parameters<StartSurfnetParams>,
    ) -> Result<CallToolResult, McpError> {
        let (surfnet_id, port) = match find_next_available_surfnet_port() {
            Ok((id, p)) => (id, p),
            Err(e) => {
                let response = StartSurfnetResponse::error(e);
                let json_str = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
            }
        };

        let res = match params.run_as_subprocess {
            true => start_surfnet::run_headless(surfnet_id, port, port.saturating_sub(9)),
            false => start_surfnet::run_command(surfnet_id, port, port.saturating_sub(9)),
        };

        // Keep track of the surfnet instance in the registry
        if res.success.is_some() {
            self.surfnets.write().unwrap().insert(surfnet_id, port);
        }

        let json_str = serde_json::to_string(&res).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    /// Sets token balances for multiple accounts in your local Solana network (surfnet).
    /// This method is exposed as a tool and can be invoked remotely.
    #[tool(
        description = "Sets token balances for one or more accounts on a local Solana network. Supports both SOL and SPL tokens."
    )]
    async fn set_token_accounts(
        &self,
        Parameters(params): Parameters<SetTokenAccountsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut results = Vec::new();
        for CreateTokenAccountForOwnerParams {
            owner,
            params: token_params,
        } in params.token_params_with_owner
        {
            let owner_seeded_account = SeededAccount::new(owner);

            for CreateTokenAccountParams {
                token_mint,
                token_amount,
                token_program_id,
                token_symbol,
            } in token_params
            {
                let set_token_result = set_token_account::run(
                    params.surfnet_address.clone(),
                    owner_seeded_account.clone(),
                    token_mint,
                    token_amount,
                    token_program_id,
                    token_symbol.clone(),
                )
                .await;
                results.push(set_token_result);
            }
        }

        let (accounts, error) = collect_token_results(results);
        let response = SetTokenAccountsResponse {
            success: accounts,
            error,
        };
        let json_str = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    /// Starts a new local Solana network and sets token balances on it.
    /// This is a convenience method that combines `start_surfnet` and `set_token_accounts`.
    #[tool(
        description = "Starts a new local Solana network and sets token balances for one or more accounts. This combines `start_surfnet` and `set_token_accounts`."
    )]
    async fn start_surfnet_with_token_accounts(
        &self,
        Parameters(params): Parameters<StartSurfnetWithTokenAccountsParams>,
    ) -> Result<CallToolResult, McpError> {
        let (surfnet_id, port) = match find_next_available_surfnet_port() {
            Ok((id, p)) => (id, p),
            Err(e) => {
                let response = StartSurfnetWithTokenAccountsResponse::error(e);
                let json_str = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
            }
        };

        let start_response = start_surfnet::run_headless(surfnet_id, port, port.saturating_sub(9));

        let surfnet_url = match start_response.success {
            Some(ref success_data) => {
                let mut surfnets_writer = self.surfnets.write().unwrap();
                surfnets_writer.insert(surfnet_id, port);
                success_data.surfnet_url.clone()
            }
            None => {
                let response = StartSurfnetWithTokenAccountsResponse::error(format!(
                    "Failed to start Surfnet (ID {}): {}. Token account not set.",
                    surfnet_id,
                    start_response
                        .error
                        .unwrap_or_else(|| "Unknown error starting surfnet".to_string())
                ));
                let json_str = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
            }
        };

        let mut results = Vec::new();
        for CreateTokenAccountForOwnerParams {
            owner,
            params: token_params,
        } in params.token_params_with_owner
        {
            let owner_seeded_account = SeededAccount::new(owner);

            for CreateTokenAccountParams {
                token_mint,
                token_amount,
                token_program_id,
                token_symbol,
            } in token_params
            {
                let set_token_result = set_token_account::run(
                    surfnet_url.clone(),
                    owner_seeded_account.clone(),
                    token_mint,
                    token_amount,
                    token_program_id,
                    token_symbol.clone(),
                )
                .await;
                results.push(set_token_result);
            }
        }

        let (accounts, error) = collect_token_results(results);
        let mut response =
            StartSurfnetWithTokenAccountsResponse::success(StartSurfnetWithTokenAccountsSuccess {
                surfnet_url,
                surfnet_id,
                accounts: accounts.unwrap_or_default(),
            });
        response.error = error;
        let json_str = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    /// Calls any RPC method on a running surfnet instance.
    /// This generic method allows calling any of the available surfnet cheatcode RPC methods.
    /// The LLM will interpret user requests and determine which method to call with appropriate parameters.
    #[tool(description = r#"
        Calls any RPC method on a running surfnet instance.
        This is a generic method that can invoke any surfnet RPC method.
        The LLM should interpret user requests and determine the appropriate method and parameters to call. To retrieve the list of RPC endpoints available check the resource str:///rpc_endpoints

        IMPORTANT:
        - If a user asks to create a scenario, DO NOT USE this method. Instead, use the dedicated `create_scenario` tool.
        - There is NO RPC method called `surfnet_getOverrideTemplates`. The override templates index is available via the MCP resource str:///override_templates (use read_resource, not this RPC tool); one template's full detail comes from the get_override_template tool.
        "#)]
    async fn call_surfnet_rpc(
        &self,
        Parameters(call_params): Parameters<CallSurfnetRpcParams>,
    ) -> Result<CallToolResult, McpError> {
        let surfnet_endpoint = format!("http://127.0.0.1:{}", call_params.surfnet_port);
        let mut rpc_params: Vec<Value> = call_params.params.into_iter().map(Into::into).collect();
        if call_params.method == "surfnet_registerScenario"
            && let Some(scenario) = rpc_params.first_mut()
            && let Err(error) = self.resolve_registered_scenario(scenario)
        {
            let response = SurfnetRpcCallResponse::error(error);
            let json_str = serde_json::to_string(&response).unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
        }
        let rpc_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": call_params.method,
            "params": rpc_params
        });

        // Async client: a blocking one inside this async tool can fail after the
        // request has already reached the surfnet
        let client = reqwest::Client::new();
        let http_response = match client
            .post(&surfnet_endpoint)
            .header("Content-Type", "application/json")
            .json(&rpc_request)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let response = SurfnetRpcCallResponse::error(format!(
                    "Failed to send RPC request to {}: {}",
                    surfnet_endpoint, e
                ));
                let json_str = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
            }
        };

        let response_text = match http_response.text().await {
            Ok(text) => text,
            Err(e) => {
                let response =
                    SurfnetRpcCallResponse::error(format!("Failed to read response text: {}", e));
                let json_str = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
            }
        };

        let rpc_response: serde_json::Value = match serde_json::from_str(&response_text) {
            Ok(json) => json,
            Err(e) => {
                let response = SurfnetRpcCallResponse::error(format!(
                    "Failed to parse JSON response: {}. Response: {}",
                    e, response_text
                ));
                let json_str = serde_json::to_string(&response).unwrap_or_default();
                return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
            }
        };

        if let Some(error) = rpc_response.get("error") {
            let response = SurfnetRpcCallResponse::error(format!("RPC error: {}", error));
            let json_str = serde_json::to_string(&response).unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
        }

        // Extract the result
        let result = rpc_response
            .get("result")
            .unwrap_or(&serde_json::Value::Null)
            .clone();

        let response = SurfnetRpcCallResponse::success(call_params.method, result);
        let json_str = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    #[tool(description = r#"
        Creates a Scenario - a list of account state overrides applied at specific slots.
        Returns a URL to Surfpool Studio for testing. 1 slot = the surfnet's slot time (250ms by default, set with `--slot-time`).

        ⚠️ CRITICAL JSON FORMAT RULES:
        - The `overrides` field MUST be a JSON array [], NOT a JSON string
        - The `values` field MUST be a JSON object {}, NOT a JSON string
        - The `tags` field MUST be a JSON array [], NOT a JSON string
        - DO NOT stringify nested objects - pass them as native JSON

        ⚠️ CRITICAL: You MUST call `get_override_templates` FIRST to pick a templateId, then
        `get_override_template` on that id for its properties and account shape.
        DO NOT invent templateId values, property names, or account addresses - they MUST come from the templates.

        STRICT RULES - VIOLATIONS WILL CAUSE ERRORS:
        1. `templateId` MUST exactly match a template's `id` field (e.g., "raydium-clmm-custom", NOT "raydium_clmm_v1")
        2. `values` keys MUST be from the template's `properties` array
        3. For PDA addresses, DO NOT provide `account` - it will be generated from template + values
        4. For constant_ref properties (like feed_id), the value MUST come from search_constant_options results
        5. For dynamic_ref properties, the value MUST come from the tool named in the property's `source` (e.g. list_phoenix_markets)

        CORRECT JSON STRUCTURE FOR PYTH PRICE FEED:
        {
          "id": "my-scenario-id",
          "name": "Price Crash Scenario",
          "description": "SOL crashes to $85",
          "overrides": [
            {
              "id": "override-1",
              "templateId": "pyth-price-feed-v2",
              "values": {
                "feed_id": "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
                "price_message.price": 8500000000
              },
              "scenarioRelativeSlot": 0,
              "enabled": true,
              "fetchBeforeUse": true,
              "label": "SOL price crash"
            }
          ],
          "tags": ["pyth", "price"]
        }

        NOTE: The `account` field will be auto-generated from the template's PDA configuration.
        For Pyth feeds, resolve the feed_id value via search_constant_options (e.g., query "SOL/USD").
        "#)]
    async fn create_scenario(
        &self,
        Parameters(mut scenario): Parameters<Scenario>,
    ) -> Result<CallToolResult, McpError> {
        // Validate all templateIds exist in the registry
        // The registry lock must not be held across the HTTP await below, so the
        // validation runs in its own scope and only its outcome escapes
        let (validation_error, phoenix_symbols) = {
            let registry = self.template_registry.read().map_err(|_| {
                use std::borrow::Cow;
                McpError {
                    code: ErrorCode(-32603),
                    message: Cow::from("Failed to read template registry"),
                    data: None,
                }
            })?;

            let mut invalid_templates: Vec<String> = Vec::new();
            let mut validation_errors: Vec<String> = Vec::new();
            let mut phoenix_symbols: Vec<(String, String)> = Vec::new();

            for override_instance in &mut scenario.overrides {
                // Check if template exists
                if !registry.contains(&override_instance.template_id) {
                    invalid_templates.push(override_instance.template_id.clone());
                    continue;
                }

                // Get the template for validation and normalization
                if let Some(template) = registry.get(&override_instance.template_id) {
                    // Normalize: Extract values from malformed PDA seeds
                    // LLMs sometimes put values directly in seeds instead of in values map
                    if let surfpool_types::AccountAddress::Pda { seeds, .. } =
                        &override_instance.account
                    {
                        for seed in seeds {
                            match seed {
                                // If bytes32Ref contains a hex value (starts with 0x), extract it
                                surfpool_types::PdaSeed::Bytes32Ref(value)
                                    if value.starts_with("0x") =>
                                {
                                    // Find the corresponding property reference from template
                                    if let surfpool_types::AccountAddress::Pda {
                                        seeds: template_seeds,
                                        ..
                                    } = &template.address
                                    {
                                        for template_seed in template_seeds.iter() {
                                            if let surfpool_types::PdaSeed::Bytes32Ref(prop_name) =
                                                template_seed
                                            {
                                                // The template has a property reference, but LLM put the value directly
                                                // Add the value to the values map using the property name
                                                if !override_instance.values.contains_key(prop_name)
                                                {
                                                    override_instance.values.insert(
                                                        prop_name.clone(),
                                                        serde_json::Value::String(value.clone()),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    // PDA templates derive their account from template seeds plus values.
                    // Fixed-address templates may intentionally receive a caller-provided pubkey.
                    if matches!(
                        &template.address,
                        surfpool_types::AccountAddress::Pda { .. }
                    ) {
                        override_instance.account = template.address.clone();
                    }

                    // Validate constant_ref values against template constants
                    for prop in &template.properties {
                        if prop.is_dynamic_ref() {
                            let value = override_instance
                                .values
                                .get(&prop.path)
                                .and_then(|value| value.as_str())
                                .filter(|value| !value.is_empty());
                            if let Some(symbol) = value
                                && prop.source_name() == Some("list_phoenix_markets")
                            {
                                phoenix_symbols.push((
                                    format!(
                                        "Override '{}' (template '{}')",
                                        override_instance.id, override_instance.template_id
                                    ),
                                    symbol.to_string(),
                                ));
                            }
                            if value.is_none() {
                                validation_errors.push(format!(
                                    "Override '{}' (template '{}'): Missing required value for '{}'. Resolve it with the `{}` tool and pass it as a non-empty string.",
                                    override_instance.id,
                                    override_instance.template_id,
                                    prop.path,
                                    prop.source_name().unwrap_or("source")
                                ));
                            }
                            continue;
                        }
                        if prop.is_constant_ref() {
                            if let Some(constant_name) = prop.constant_name() {
                                if let Some(constant_def) = template.constants.get(constant_name) {
                                    // Check if the value exists in values map
                                    if let Some(value) = override_instance.values.get(&prop.path) {
                                        if let Some(value_str) = value.as_str() {
                                            // Base58 is case-sensitive: a case-folded match would
                                            // accept a value that derives a different PDA.
                                            let is_valid = constant_def
                                                .options
                                                .iter()
                                                .any(|opt| opt.value == value_str);
                                            if !is_valid {
                                                // Show only first 10 options to avoid overwhelming error messages
                                                let sample_options: Vec<String> = constant_def
                                                    .options
                                                    .iter()
                                                    .take(10)
                                                    .map(|opt| {
                                                        format!("{}: {}", opt.label, opt.value)
                                                    })
                                                    .collect();
                                                let total = constant_def.options.len();
                                                let more_msg = if total > 10 {
                                                    format!(" (showing 10 of {} options)", total)
                                                } else {
                                                    String::new()
                                                };
                                                validation_errors.push(format!(
                                                "Override '{}' (template '{}'): Invalid value '{}' for '{}' (constant: '{}'). Valid options{}:\n  {}",
                                                override_instance.id,
                                                override_instance.template_id,
                                                value_str,
                                                prop.path,
                                                constant_name,
                                                more_msg,
                                                sample_options.join("\n  ")
                                            ));
                                            }
                                        } else {
                                            validation_errors.push(format!(
                                                "Override '{}' (template '{}'): Value for '{}' (constant: '{}') must be a base-58 mint address string, got: {}",
                                                override_instance.id,
                                                override_instance.template_id,
                                                prop.path,
                                                constant_name,
                                                value
                                            ));
                                        }
                                    } else {
                                        // Value is missing - required for PDA derivation
                                        let valid_options: Vec<String> = constant_def
                                            .options
                                            .iter()
                                            .take(5) // Show first 5 options
                                            .map(|opt| format!("{}: {}", opt.label, opt.value))
                                            .collect();
                                        validation_errors.push(format!(
                                        "Override '{}' (template '{}'): Missing required value for '{}' (constant: '{}', used in PDA derivation). Add it to values. Example options:\n  {}",
                                        override_instance.id,
                                        override_instance.template_id,
                                        prop.path,
                                        constant_name,
                                        valid_options.join("\n  ")
                                    ));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let validation_error = if !invalid_templates.is_empty() {
                let valid_ids: Vec<String> = registry.list_ids();
                Some(format!(
                    "Invalid templateId(s): {:?}. You MUST use templateIds from get_override_templates. Valid IDs are: {:?}",
                    invalid_templates, valid_ids
                ))
            } else if !validation_errors.is_empty() {
                Some(format!(
                    "Validation errors:\n{}",
                    validation_errors.join("\n")
                ))
            } else {
                None
            };
            (validation_error, phoenix_symbols)
        };

        // Play only warns about an unknown market symbol, so check it while the caller can fix it,
        // on the surfnet Studio plays the scenario on. If the market list cannot be read, the
        // scenario is staged unchecked.
        let validation_error = match validation_error {
            None if !phoenix_symbols.is_empty() => {
                let studio_url = format!(
                    "http://127.0.0.1:{}",
                    CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED
                );
                let listing = async {
                    let rpc_url = self.studio_rpc_url(&studio_url).await?;
                    self.list_phoenix_markets_at(&rpc_url).await
                }
                .await;
                match listing {
                    Ok(listing) => {
                        let errors = unlisted_phoenix_symbols(&listing, &phoenix_symbols);
                        (!errors.is_empty())
                            .then(|| format!("Validation errors:\n{}", errors.join("\n")))
                    }
                    Err(_) => None,
                }
            }
            validation_error => validation_error,
        };
        let validation_error =
            validation_error.or_else(|| self.resolve_saved_overrides(&mut scenario).err());

        if let Some(message) = validation_error {
            let response = RegisterScenarioResponse::error(message);
            let json_str = serde_json::to_string(&response).unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]));
        }

        self.stage_scenario(scenario).await
    }

    #[tool(
        description = "Creates an editable Pump Graduation state-preparation scenario for the required tokenMint. If validation fails, report that error and do not retry. The backend validates the mint, incomplete bonding curve, curve vault, and absent canonical PumpSwap pool."
    )]
    async fn create_pump_graduation_scenario(
        &self,
        Parameters(params): Parameters<CreatePumpGraduationScenarioParams>,
    ) -> Result<CallToolResult, McpError> {
        let token_mint = match Pubkey::from_str(params.token_mint.trim()) {
            Ok(token_mint) => token_mint,
            Err(error) => return Ok(scenario_tool_error(format!("Invalid token mint: {error}"))),
        };
        let addresses = match pump_graduation_addresses(&token_mint) {
            Ok(addresses) => addresses,
            Err(error) => return Ok(scenario_tool_error(error.to_string())),
        };
        let pubkeys = [
            token_mint,
            addresses.bonding_curve,
            addresses.curve_vault,
            addresses.canonical_pool,
            addresses.global,
        ];
        let accounts = match self
            .fetch_surfnet_accounts(params.surfnet_port, &pubkeys)
            .await
        {
            Ok(accounts) => accounts,
            Err(error) => return Ok(scenario_tool_error(error)),
        };
        let required = |index: usize, name: &str| {
            accounts[index]
                .as_ref()
                .ok_or_else(|| format!("Pump {name} account not found"))
        };
        let preparation = match build_pump_graduation_scenario(
            token_mint,
            match required(0, "mint") {
                Ok(account) => account,
                Err(error) => return Ok(scenario_tool_error(error)),
            },
            match required(1, "bonding curve") {
                Ok(account) => account,
                Err(error) => return Ok(scenario_tool_error(error)),
            },
            match required(2, "curve vault") {
                Ok(account) => account,
                Err(error) => return Ok(scenario_tool_error(error)),
            },
            accounts[3].as_ref(),
            match required(4, "global") {
                Ok(account) => account,
                Err(error) => return Ok(scenario_tool_error(error)),
            },
        ) {
            Ok(preparation) => preparation,
            Err(error) => return Ok(scenario_tool_error(error.to_string())),
        };

        self.stage_scenario(preparation.scenario).await
    }

    async fn list_phoenix_markets_at(&self, rpc_url: &str) -> Result<serde_json::Value, String> {
        let perp_asset_map = PHOENIX_PERP_ASSET_MAP;
        let maps = self.fetch_accounts_at(rpc_url, &[perp_asset_map]).await?;
        let map_account = maps[0].as_ref().ok_or_else(|| {
            format!("Phoenix PerpAssetMap account {perp_asset_map} was not found on the surfnet")
        })?;
        let markets =
            phoenix_markets(perp_asset_map, map_account).map_err(|error| error.to_string())?;
        Ok(serde_json::json!({
            "perpAssetMap": perp_asset_map.to_string(),
            "count": markets.len(),
            "symbols": markets.iter().map(|market| &market.symbol).collect::<Vec<_>>(),
            "markets": markets
                .iter()
                .map(|market| {
                    serde_json::json!({
                        "symbol": market.symbol,
                        "orderbook": market.orderbook.to_string(),
                        "markTicks": market.mark_ticks,
                        "tickSize": market.tick_size,
                        "baseLotDecimals": market.base_lot_decimals,
                        "maintenanceRiskFactorBps": market.risk_factors[0],
                        "backstopRiskFactorBps": market.risk_factors[1],
                    })
                })
                .collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "Lists the Phoenix Eternal perp markets in the PerpAssetMap of the local VM, so the catalog reflects current state rather than a hardcoded snapshot. Each market comes with its symbol, its orderbook account address, its current mark price in ticks (markTicks), its tick size in quote lots per base lot (tickSize), its signed base lot decimals (baseLotDecimals), its maintenance risk factor (maintenanceRiskFactorBps) and the backstop risk factor a maintenance factor must stay above (backstopRiskFactorBps). Use it to resolve a market given by symbol or by orderbook address to the symbol the Phoenix templates take, and compute relative changes (such as a 40% mark drop) from markTicks. USD per base unit = markTicks * tickSize * 10^(baseLotDecimals - 6). Never read or decode the PerpAssetMap account yourself: it is 1.6 MB."
    )]
    async fn list_phoenix_markets(
        &self,
        Parameters(params): Parameters<ListPhoenixMarketsParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .list_phoenix_markets_at(&format!(
                "http://127.0.0.1:{}",
                params.surfnet_port.unwrap_or(DEFAULT_RPC_PORT)
            ))
            .await
        {
            Ok(payload) => Ok(CallToolResult::success(vec![ContentBlock::text(
                payload.to_string(),
            )])),
            Err(error) => Ok(scenario_tool_error(error)),
        }
    }

    async fn list_phoenix_trader_positions_at(
        &self,
        rpc_url: &str,
        trader: &str,
        moves: &[PhoenixMove],
    ) -> Result<serde_json::Value, String> {
        let trader = Pubkey::from_str(trader.trim())
            .map_err(|_| format!("'{trader}' is not a valid Phoenix Trader account address"))?;
        let moves = moves
            .iter()
            .map(|moved| {
                let ticks = match &moved.target_ticks {
                    serde_json::Value::String(text) => text.trim().parse().ok(),
                    serde_json::Value::Number(number) => number.as_u64(),
                    _ => None,
                };
                ticks
                    .map(|ticks| (moved.symbol.clone(), ticks))
                    .ok_or_else(|| {
                        format!("targetTicks of {} must be a whole number", moved.symbol)
                    })
            })
            .collect::<Result<Vec<(String, u64)>, String>>()?;
        let positions = trader_positions(SurfnetRemoteClient::new(rpc_url), trader, &moves)
            .await
            .map_err(|error| error.to_string())?;
        Ok(trader_positions_json(&trader, &positions))
    }

    #[tool(
        description = "Lists the markets where one Phoenix Eternal trader holds a position, as Phoenix's Hawkeye program reports them on a copy of the surfnet's state, sorted by symbol. Each position comes with its market symbol, its orderbook address, its side (long or short), its size in base lots (baseLots) and its maintenance margin in quote lots (maintenanceMarginQuoteLots), both decimal strings. Hawkeye reads positions where Phoenix keeps them, so this is right even when the Trader account's own copy is not; never decode the Trader account yourself. hot is true when Phoenix's active trader index lists the trader, such as one with resting orders. margin is Hawkeye's view as phoenix-trader-cancel-orders leaves the trader, a hot trader's orders canceled: collateral, effectiveCollateral, maintenanceMargin, backstopMargin and highRiskMargin in quote lots as decimal strings, and liquidatable. refusal is why phoenix-trader-cancel-orders or phoenix-trader-collateral would refuse the trader, or null. phoenix-trader-collateral takes the effective collateral wanted in USDC, a quote lot being a millionth of a USDC. Given moves, marginAfterMoves is the same view once those markets move to targetTicks and uncross the way phoenix-market-move and phoenix-market-uncross play them, for a trader that should cross into liquidation at a later slot."
    )]
    async fn list_phoenix_trader_positions(
        &self,
        Parameters(params): Parameters<ListPhoenixTraderPositionsParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .list_phoenix_trader_positions_at(
                &format!(
                    "http://127.0.0.1:{}",
                    params.surfnet_port.unwrap_or(DEFAULT_RPC_PORT)
                ),
                &params.trader,
                &params.moves,
            )
            .await
        {
            Ok(payload) => Ok(CallToolResult::success(vec![ContentBlock::text(
                payload.to_string(),
            )])),
            Err(error) => Ok(scenario_tool_error(error)),
        }
    }

    #[tool(
        description = "Lists all override templates as a light index: {id, name, description, protocol, accountType, tags, hasLlmContext}. Call this first to pick a templateId, then get_override_template for that one template's full detail (properties, address, llmContext). Constants are resolved with search_constant_options; dynamic_ref properties with the tool named in their source."
    )]
    async fn get_override_templates(&self) -> Result<CallToolResult, McpError> {
        let registry = self.template_registry.read().map_err(|_| {
            use std::borrow::Cow;
            McpError {
                code: ErrorCode(-32603),
                message: Cow::from("Failed to read template registry"),
                data: None,
            }
        })?;

        let templates: Vec<serde_json::Value> = registry
            .all()
            .iter()
            .map(|t| index_template_json(t))
            .collect();

        let json_str = serde_json::to_string(&templates).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    #[tool(
        description = "Fetches one template's full detail (properties, address, rawLayout write-path flag, constants summarized as {label, description, optionsCount}, and llmContext). Call after get_override_templates with the id you picked, before create_scenario. Resolve an actual constant option value with search_constant_options, and a dynamic_ref value with the tool named in its source."
    )]
    async fn get_override_template(
        &self,
        Parameters(params): Parameters<GetTemplateParams>,
    ) -> Result<CallToolResult, McpError> {
        let registry = self.template_registry.read().map_err(|_| {
            use std::borrow::Cow;
            McpError {
                code: ErrorCode(-32603),
                message: Cow::from("Failed to read template registry"),
                data: None,
            }
        })?;

        let all_templates = registry.all();
        let Some(template) = all_templates.iter().find(|t| t.id == params.template_id) else {
            let valid_ids: Vec<&String> = all_templates.iter().map(|t| &t.id).collect();
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Unknown templateId {:?}. Valid IDs are: {:?}",
                params.template_id, valid_ids
            ))]));
        };

        let json_str = serde_json::to_string(&compact_template_json(template)).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    #[tool(
        description = "Validates a template collection against the program's IDL before save_override_template. Pass the contents of both files: collectionYaml (the YAML text) and idl (the Anchor 0.30+ IDL; a legacy IDL is refused, run `anchor idl convert` on it first). Checks the YAML shape, every property path against the IDL account type, no raw_layout, no constants, no keep_alive, an llm_context per template, unique ids, and no id a built-in template uses. Returns the templates as get_override_template will show them, or the first error."
    )]
    async fn validate_override_template(
        &self,
        Parameters(params): Parameters<OverrideTemplateCollectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.checked_collection(&params).map(|checked| {
            let templates: Vec<serde_json::Value> = checked
                .templates
                .iter()
                .map(compact_template_json)
                .collect();
            serde_json::json!({"templates": templates})
        });
        Ok(tool_result(result))
    }

    #[tool(
        description = "Validates a template collection like validate_override_template (same parameters), registers its IDL on the surfnet at surfnetPort unless the surfnet's active IDL for the program is already the same, and adds the templates to this MCP session so get_override_templates and create_scenario see them. Nothing is written to disk: after the MCP server restarts, save again. Saving again under the same protocol and version needs overwrite: true."
    )]
    async fn save_override_template(
        &self,
        Parameters(params): Parameters<OverrideTemplateCollectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let port = params.surfnet_port.unwrap_or(DEFAULT_RPC_PORT);
        let result = async {
            let checked = self.checked_collection(&params)?;
            let idl_registered = register_idl(port, &checked.idl).await?;
            let ids = self.store_collection(checked, params.overwrite)?;
            Ok(serde_json::json!({"templateIds": ids, "idlRegistered": idl_registered}))
        }
        .await;
        Ok(tool_result(result))
    }

    #[tool(
        description = "Searches the options of a template's constants (price feeds, markets, token mints). Use after get_override_templates to resolve a constant_ref value: pass the templateId, optionally the constant name, and a query like \"SOL/USD\". Returns matching options whose `value` field is what create_scenario expects."
    )]
    async fn search_constant_options(
        &self,
        Parameters(params): Parameters<SearchConstantOptionsParams>,
    ) -> Result<CallToolResult, McpError> {
        let registry = self.template_registry.read().map_err(|_| {
            use std::borrow::Cow;
            McpError {
                code: ErrorCode(-32603),
                message: Cow::from("Failed to read template registry"),
                data: None,
            }
        })?;

        let all_templates = registry.all();
        let Some(template) = all_templates.iter().find(|t| t.id == params.template_id) else {
            let valid_ids: Vec<&String> = all_templates.iter().map(|t| &t.id).collect();
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Unknown templateId {:?}. Valid IDs are: {:?}",
                params.template_id, valid_ids
            ))]));
        };

        if let Some(ref wanted) = params.constant {
            if !template.constants.contains_key(wanted) {
                let available: Vec<&String> = template.constants.keys().collect();
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Template {:?} has no constant {:?}. Available constants: {:?}",
                    params.template_id, wanted, available
                ))]));
            }
        }

        let query = params.query.to_lowercase();
        let mut results = Vec::new();
        let mut total_matches = 0usize;

        // HashMap iteration order is random per process; sort so paged results
        // are stable across calls for templates with several constants
        let mut constants: Vec<_> = template.constants.iter().collect();
        constants.sort_by(|a, b| a.0.cmp(b.0));
        for (constant_name, def) in constants {
            if let Some(ref wanted) = params.constant {
                if constant_name != wanted {
                    continue;
                }
            }
            for opt in &def.options {
                let matches = query.is_empty()
                    || opt.id.to_lowercase().contains(&query)
                    || opt.label.to_lowercase().contains(&query)
                    || opt.value.to_lowercase().contains(&query)
                    || opt
                        .description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&query));
                if matches {
                    total_matches += 1;
                    if results.len() < MAX_SEARCH_RESULTS {
                        results.push(serde_json::json!({
                            "constant": constant_name,
                            "id": opt.id,
                            "label": opt.label,
                            "description": opt.description,
                            "value": opt.value,
                            "metadata": opt.metadata,
                        }));
                    }
                }
            }
        }

        let response = serde_json::json!({
            "templateId": params.template_id,
            "results": results,
            "totalMatches": total_matches,
            "truncated": total_matches > results.len(),
        });
        let json_str = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }

    #[tool(
        description = "Translates a token symbol (e.g., 'USDC', 'SOL', 'JUP') into its mint address. Uses the verified tokens list."
    )]
    async fn get_token_address(
        &self,
        Parameters(params): Parameters<GetTokenAddressParams>,
    ) -> Result<CallToolResult, McpError> {
        let symbol_upper = params.symbol.to_uppercase();

        let response = match VERIFIED_TOKENS_BY_SYMBOL.get(&symbol_upper) {
            Some(token_info) => TokenAddressResponse::success(
                token_info.symbol.clone(),
                token_info.address.clone(),
                token_info.name.clone(),
                token_info.decimals,
            ),
            None => TokenAddressResponse::not_found(params.symbol),
        };

        let json_str = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json_str)]))
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for Surfpool {
    /// Return information about the server, including its capabilities and description.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_instructions(
            "Surfpool MCP server, your personal surfnet manager to start surfing on Solana!",
        )
    }
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2024_11_05))
    }
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("str:///rpc_endpoints", "List of available RPC endpoints")
                .with_title("RPC Endpoints")
                .with_description("A json file containing all the RPC methods and the parameters available for being able to handle any RPC call with the tool call_surfnet_rpc")
                .with_mime_type("application/json"),
            Resource::new("str:///override_templates", "List of override templates")
                .with_title("Override Templates")
                .with_description("A json file containing all the override templates available for scenario registration with account overrides")
                .with_mime_type("application/json"),
            Resource::new("str:///template_authoring_guide", "Template authoring guide")
                .with_title("Template Authoring Guide")
                .with_description("How to write, validate and save an override template collection for a program from its IDL with validate_override_template and save_override_template")
                .with_mime_type("text/markdown"),
        ]))
    }
    async fn read_resource(
        &self,
        ReadResourceRequestParams { uri, .. }: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match uri.as_str() {
            "str:///rpc_endpoints" => {
                let rpc_endpoints = include_str!("../../../types/src/rpc_endpoints.json");
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(rpc_endpoints, uri).with_mime_type("application/json"),
                ])
                .into())
            }
            "str:///override_templates" => {
                let registry = self.template_registry.read().map_err(|_| {
                    use std::borrow::Cow;
                    McpError {
                        code: ErrorCode(-32603),
                        message: Cow::from("Failed to read template registry"),
                        data: None,
                    }
                })?;

                let templates: Vec<serde_json::Value> = registry
                    .all()
                    .iter()
                    .map(|t| index_template_json(t))
                    .collect();

                let templates_json = serde_json::to_string(&templates).map_err(|_| {
                    use std::borrow::Cow;
                    McpError {
                        code: ErrorCode(-32603),
                        message: Cow::from("Failed to serialize templates"),
                        data: None,
                    }
                })?;

                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(templates_json, uri).with_mime_type("application/json"),
                ])
                .into())
            }
            "str:///template_authoring_guide" => Ok(ReadResourceResult::new(vec![
                ResourceContents::text(AUTHORING_GUIDE, uri).with_mime_type("text/markdown"),
            ])
            .into()),
            _ => {
                use std::borrow::Cow;
                Err(McpError {
                    code: ErrorCode(-32002),
                    message: Cow::from("Resource not found"),
                    data: Some(serde_json::json!({
                        "uri": uri
                    })),
                })
            }
        }
    }
}

fn trader_positions_json(trader: &Pubkey, held: &TraderPositions) -> serde_json::Value {
    let margin = |view: &MarginView| {
        serde_json::json!({
            "collateral": view.collateral_quote_lots.to_string(),
            "effectiveCollateral": view.effective_collateral_quote_lots.to_string(),
            "maintenanceMargin": view.maintenance_margin_quote_lots.to_string(),
            "backstopMargin": view.backstop_margin_quote_lots.to_string(),
            "highRiskMargin": view.high_risk_margin_quote_lots.to_string(),
            "liquidatable": view.is_liquidatable != 0,
        })
    };
    serde_json::json!({
        "trader": trader.to_string(),
        "positions": held
            .positions
            .iter()
            .map(|position| {
                serde_json::json!({
                    "symbol": position.symbol,
                    "orderbook": position.orderbook.to_string(),
                    "side": if position.base_lots > 0 { "long" } else { "short" },
                    "baseLots": position.base_lots.unsigned_abs().to_string(),
                    "maintenanceMarginQuoteLots": position.maintenance_margin_quote_lots.to_string(),
                })
            })
            .collect::<Vec<_>>(),
        "hot": held.hot,
        "margin": margin(&held.margin),
        "marginAfterMoves": held.margin_after_moves.as_ref().map(margin),
        "refusal": held.refusal,
    })
}

fn unlisted_phoenix_symbols(
    listing: &serde_json::Value,
    symbols: &[(String, String)],
) -> Vec<String> {
    let listed: Vec<&str> = listing["symbols"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| symbol.as_str())
        .collect();
    symbols
        .iter()
        .filter(|(_, symbol)| !listed.contains(&symbol.as_str()))
        .map(|(owner, symbol)| {
            let hint = listed
                .iter()
                .find(|listed| listed.eq_ignore_ascii_case(symbol))
                .map(|listed| format!(" Did you mean '{listed}'?"))
                .unwrap_or_default();
            format!(
                "{owner}: '{symbol}' is not a listed Phoenix market (symbols are \
                 case-sensitive).{hint} Resolve it with the `list_phoenix_markets` tool."
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_takes_its_template_id_the_way_the_response_names_it() {
        let schema = serde_json::to_value(schemars::schema_for!(SearchConstantOptionsParams))
            .expect("schema serializes");
        let properties = schema["properties"]
            .as_object()
            .expect("schema has properties");
        assert!(properties.contains_key("templateId"), "{properties:?}");
        assert!(!properties.contains_key("template_id"), "{properties:?}");

        let parsed: SearchConstantOptionsParams = serde_json::from_value(
            serde_json::json!({"templateId": "pyth-price-feed-v2", "query": ""}),
        )
        .expect("the deserializer accepts what the schema advertises");
        assert_eq!(parsed.template_id, "pyth-price-feed-v2");
    }

    #[test]
    fn pump_graduation_mint_is_required() {
        assert!(
            serde_json::from_value::<CreatePumpGraduationScenarioParams>(serde_json::json!({}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn pump_graduation_rejects_invalid_inputs_before_rpc() {
        let surfpool = Surfpool::new();

        let graduation = surfpool
            .create_pump_graduation_scenario(Parameters(CreatePumpGraduationScenarioParams {
                token_mint: "not-a-mint".to_string(),
                surfnet_port: None,
            }))
            .await
            .expect("tool result");
        assert!(
            json_of(&graduation)["error"]
                .as_str()
                .expect("error")
                .contains("Invalid token mint")
        );
    }

    /// A token account update that fails is reported, never dropped; with no success at all
    /// there is no success list.
    #[tokio::test]
    async fn set_token_accounts_reports_the_updates_that_failed() {
        let surfpool = Surfpool::new();
        let result = surfpool
            .set_token_accounts(Parameters(SetTokenAccountsParams {
                // No surfnet listens here: every update fails.
                surfnet_address: "http://127.0.0.1:1".to_string(),
                token_params_with_owner: vec![CreateTokenAccountForOwnerParams {
                    owner: None,
                    params: vec![CreateTokenAccountParams {
                        token_symbol: Some("USDC".to_string()),
                        ..CreateTokenAccountParams::default()
                    }],
                }],
            }))
            .await
            .expect("tool result");
        let response = json_of(&result);
        assert!(response["error"].is_string(), "{response}");
        assert!(response["success"].is_null(), "{response}");
    }

    fn json_of(result: &CallToolResult) -> serde_json::Value {
        let text = &result.content[0].as_text().expect("text content").text;
        serde_json::from_str(text).expect("valid JSON payload")
    }

    fn search(
        template_id: &str,
        constant: Option<&str>,
        query: &str,
    ) -> Parameters<SearchConstantOptionsParams> {
        Parameters(SearchConstantOptionsParams {
            template_id: template_id.to_string(),
            constant: constant.map(str::to_string),
            query: query.to_string(),
        })
    }

    fn template_id(id: &str) -> Parameters<GetTemplateParams> {
        Parameters(GetTemplateParams {
            template_id: id.to_string(),
        })
    }

    #[tokio::test]
    async fn the_template_index_is_light_and_omits_the_heavy_fields() {
        let surfpool = Surfpool::new();
        let result = surfpool.get_override_templates().await.unwrap();
        assert_ne!(result.is_error, Some(true));

        let templates = json_of(&result);
        let pyth = templates
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "pyth-price-feed-v2")
            .expect("pyth template present");

        for heavy in ["llmContext", "properties", "address", "constants", "idl"] {
            assert!(
                pyth.get(heavy).is_none(),
                "the index must not carry {heavy}"
            );
        }
        assert_eq!(
            pyth["hasLlmContext"], true,
            "the index flags which templates have context to fetch"
        );
    }

    #[tokio::test]
    async fn get_override_template_returns_full_detail_and_summarizes_constants() {
        let surfpool = Surfpool::new();
        let result = surfpool
            .get_override_template(template_id("pyth-price-feed-v2"))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        let pyth = json_of(&result);
        assert!(
            pyth.get("properties").is_some(),
            "detail carries properties"
        );
        assert!(pyth.get("address").is_some(), "detail carries the address");
        assert_eq!(
            pyth["rawLayout"], false,
            "detail identifies the template's write path"
        );
        assert!(
            pyth.get("llmContext").is_some(),
            "detail carries llmContext"
        );

        let price_feed = &pyth["constants"]["price_feed"];
        assert!(
            price_feed["optionsCount"].as_u64().unwrap() > 0,
            "summary must report how many options exist"
        );
        assert!(
            price_feed.get("options").is_none(),
            "options must not be inlined; they blow past LLM token limits"
        );
    }

    #[tokio::test]
    async fn get_override_template_rejects_an_unknown_id_and_names_valid_ones() {
        let surfpool = Surfpool::new();
        let result = surfpool
            .get_override_template(template_id("no-such-template"))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = &result.content[0].as_text().expect("text").text;
        assert!(
            text.contains("pyth-price-feed-v2"),
            "the error must name valid ids, got: {text}"
        );
    }

    #[test]
    fn the_index_is_lighter_than_the_full_detail_of_every_template() {
        let registry = TemplateRegistry::new();
        let index: Vec<_> = registry
            .all()
            .iter()
            .map(|t| index_template_json(t))
            .collect();
        let full: Vec<_> = registry
            .all()
            .iter()
            .map(|t| compact_template_json(t))
            .collect();

        let index_len = serde_json::to_string(&index).unwrap().len();
        let full_len = serde_json::to_string(&full).unwrap().len();
        assert!(
            index_len * 3 < full_len,
            "the index ({index_len} bytes) should be far lighter than the old full payload \
             ({full_len} bytes)"
        );
    }

    #[tokio::test]
    async fn search_finds_a_feed_case_insensitively_and_returns_usable_values() {
        let surfpool = Surfpool::new();
        let result = surfpool
            .search_constant_options(search("pyth-price-feed-v2", None, "sOl/UsD"))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));

        let payload = json_of(&result);
        let results = payload["results"].as_array().unwrap();
        assert!(
            results
                .iter()
                .any(|r| r["label"].as_str().unwrap().eq_ignore_ascii_case("sol/usd")),
            "SOL/USD must match a case-insensitive query"
        );
        assert!(
            results
                .iter()
                .all(|r| !r["value"].as_str().unwrap().is_empty()),
            "every result must carry the value create_scenario expects"
        );
    }

    #[tokio::test]
    async fn search_truncates_at_max_results_and_reports_the_full_total() {
        let surfpool = Surfpool::new();
        let result = surfpool
            .search_constant_options(search("pyth-price-feed-v2", Some("price_feed"), ""))
            .await
            .unwrap();

        let payload = json_of(&result);
        let returned = payload["results"].as_array().unwrap().len();
        let total = payload["totalMatches"].as_u64().unwrap() as usize;
        assert!(returned <= MAX_SEARCH_RESULTS);
        assert_eq!(payload["truncated"].as_bool().unwrap(), total > returned);
        assert!(
            total > MAX_SEARCH_RESULTS,
            "the pyth feed list is expected to exceed one page; if this fails the \
             truncation branch is no longer covered"
        );
        assert_eq!(returned, MAX_SEARCH_RESULTS);
    }

    #[test]
    fn compact_template_json_never_inlines_constant_options() {
        let registry = TemplateRegistry::new();
        for template in registry.all() {
            let json = compact_template_json(template);
            for (name, constant) in json["constants"].as_object().unwrap() {
                assert!(
                    constant.get("options").is_none(),
                    "constant {name} of template {} leaks inlined options",
                    template.id
                );
                assert!(constant.get("optionsCount").is_some());
            }
        }
    }

    #[test]
    fn compact_template_json_exports_the_raw_layout_flag() {
        let registry = TemplateRegistry::new();
        let mut template = registry
            .get("pyth-price-feed-v2")
            .expect("template exists")
            .clone();
        template.raw_layout = true;

        assert_eq!(compact_template_json(&template)["rawLayout"], true);
    }

    #[test]
    fn index_template_json_omits_the_heavy_fields() {
        let registry = TemplateRegistry::new();
        for template in registry.all() {
            let json = index_template_json(template);
            for heavy in ["llmContext", "properties", "address", "constants", "idl"] {
                assert!(
                    json.get(heavy).is_none(),
                    "index entry for {} leaks {heavy}",
                    template.id
                );
            }
            assert_eq!(json["hasLlmContext"], template.llm_context.is_some());
        }
    }

    #[tokio::test]
    async fn search_rejects_unknown_template_and_unknown_constant() {
        let surfpool = Surfpool::new();

        let unknown_template = surfpool
            .search_constant_options(search("no-such-template", None, "x"))
            .await
            .unwrap();
        assert_eq!(unknown_template.is_error, Some(true));

        let unknown_constant = surfpool
            .search_constant_options(search("pyth-price-feed-v2", Some("no-such-constant"), "x"))
            .await
            .unwrap();
        assert_eq!(unknown_constant.is_error, Some(true));
    }

    #[tokio::test]
    async fn the_pump_templates_appear_in_the_index_and_detail_summarizes_the_catalog() {
        let surfpool = Surfpool::new();
        let result = surfpool.get_override_templates().await.unwrap();
        assert_ne!(result.is_error, Some(true));

        let templates = json_of(&result);
        let templates = templates.as_array().unwrap();
        for id in [
            "pump-bonding-curve-custom",
            "pump-global",
            "pump-amm-pool-state",
            "pump-amm-canonical-pool",
            "pump-amm-global-config",
        ] {
            let template = templates
                .iter()
                .find(|t| t["id"] == id)
                .unwrap_or_else(|| panic!("template {id} missing from the index"));
            assert!(
                template.get("idl").is_none(),
                "{id} must not inline the ~160KB IDL into the index"
            );
        }

        let detail = surfpool
            .get_override_template(template_id("pump-bonding-curve-custom"))
            .await
            .unwrap();
        let curve = json_of(&detail);
        let token_mint = &curve["constants"]["token_mint"];
        assert!(
            token_mint["optionsCount"].as_u64().unwrap() > 0,
            "the verified-tokens catalog must be visible as a summary"
        );
        assert!(token_mint.get("options").is_none());
    }

    #[tokio::test]
    async fn search_resolves_pump_coin_mints_for_both_programs() {
        let surfpool = Surfpool::new();

        let result = surfpool
            .search_constant_options(search("pump-bonding-curve-custom", None, "pump"))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let payload = json_of(&result);
        let results = payload["results"].as_array().unwrap();
        assert!(!results.is_empty(), "a pump coin must be findable");
        assert!(
            results
                .iter()
                .all(|r| !r["value"].as_str().unwrap().is_empty()),
            "every result must carry the mint create_scenario expects"
        );
        assert!(
            results
                .iter()
                .any(|r| r["value"].as_str().unwrap().ends_with("pump")),
            "pump.fun mints are recognizable by their suffix"
        );

        let result = surfpool
            .search_constant_options(search("pump-amm-canonical-pool", Some("token_mint"), ""))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let payload = json_of(&result);
        assert!(
            payload["totalMatches"].as_u64().unwrap() > 0,
            "the canonical pool template must expose base mints to search"
        );
    }

    #[tokio::test]
    async fn pump_token_catalogs_offer_only_pump_mints() {
        let registry = TemplateRegistry::new();
        for template_id in ["pump-bonding-curve-custom", "pump-amm-canonical-pool"] {
            let template = registry.get(template_id).expect("template");
            let constant = template.constants.get("token_mint").expect("constant");
            assert_eq!(
                constant.options.len(),
                976,
                "{template_id} must offer every pump-suffixed catalog mint (976 in the CSV)"
            );
            assert!(
                constant.options.iter().all(|o| o.value.ends_with("pump")),
                "{template_id} must offer only pump.fun mints"
            );
        }

        let surfpool = Surfpool::new();
        for mint in [
            "So11111111111111111111111111111111111111112",
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        ] {
            let result = surfpool
                .search_constant_options(search("pump-bonding-curve-custom", None, mint))
                .await
                .unwrap();
            let payload = json_of(&result);
            assert_eq!(
                payload["totalMatches"].as_u64().unwrap(),
                0,
                "{mint} must not be offered for a bonding curve"
            );
        }
    }

    #[test]
    fn unlisted_phoenix_symbols_names_each_miss_and_the_listed_spelling() {
        let listing = serde_json::json!({ "symbols": ["SOL", "BNB", "kBONK"] });
        let pairs = |symbols: &[&str]| -> Vec<(String, String)> {
            symbols
                .iter()
                .map(|symbol| ("Override 'o'".to_string(), symbol.to_string()))
                .collect()
        };

        assert!(unlisted_phoenix_symbols(&listing, &pairs(&["SOL", "kBONK"])).is_empty());

        let errors = unlisted_phoenix_symbols(&listing, &pairs(&["sol", "SOL-PERP", "BONK"]));
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].contains("'sol'") && errors[0].contains("Did you mean 'SOL'?"));
        assert!(errors[1].contains("'SOL-PERP'") && !errors[1].contains("Did you mean"));
        assert!(errors[2].contains("'BONK'") && errors[2].contains("list_phoenix_markets"));
    }

    #[test]
    fn trader_positions_keep_the_contract_studio_reads() {
        use surfpool_core::scenarios::protocols::phoenix_eternal::v1::trader::TraderPosition;

        let trader = Pubkey::new_unique();
        let orderbook = Pubkey::new_unique();
        let position = |symbol: &str, base_lots| TraderPosition {
            symbol: symbol.to_string(),
            orderbook,
            base_lots,
            maintenance_margin_quote_lots: 12_345_678_901,
        };
        let mut view = MarginView::default();
        view.collateral_quote_lots = -5_030_916_850;
        view.effective_collateral_quote_lots = 41_872_811;
        view.maintenance_margin_quote_lots = 11_473_811;
        view.backstop_margin_quote_lots = 4_589_524;
        view.high_risk_margin_quote_lots = 2_294_761;
        view.is_liquidatable = 1;
        let mut held = TraderPositions {
            positions: vec![position("ETH", 250), position("SOL", -1_000)],
            hot: true,
            margin: view,
            margin_after_moves: None,
            refusal: None,
        };
        let payload = trader_positions_json(&trader, &held);
        assert_eq!(
            payload,
            serde_json::json!({
                "trader": trader.to_string(),
                "positions": [
                    {
                        "symbol": "ETH",
                        "orderbook": orderbook.to_string(),
                        "side": "long",
                        "baseLots": "250",
                        "maintenanceMarginQuoteLots": "12345678901",
                    },
                    {
                        "symbol": "SOL",
                        "orderbook": orderbook.to_string(),
                        "side": "short",
                        "baseLots": "1000",
                        "maintenanceMarginQuoteLots": "12345678901",
                    },
                ],
                "hot": true,
                "margin": {
                    "collateral": "-5030916850",
                    "effectiveCollateral": "41872811",
                    "maintenanceMargin": "11473811",
                    "backstopMargin": "4589524",
                    "highRiskMargin": "2294761",
                    "liquidatable": true,
                },
                "marginAfterMoves": null,
                "refusal": null,
            })
        );
        held.margin_after_moves = Some(view);
        let payload = trader_positions_json(&trader, &held);
        assert_eq!(payload["marginAfterMoves"], payload["margin"]);
        let params: ListPhoenixTraderPositionsParams = serde_json::from_value(serde_json::json!({
            "trader": trader.to_string(),
            "moves": [{ "symbol": "SOL", "targetTicks": "10446" }, { "symbol": "BTC", "targetTicks": 78545 }],
        }))
        .unwrap();
        assert_eq!(params.moves[0].target_ticks, "10446");
        assert_eq!(params.moves[1].target_ticks, 78545);
    }

    #[tokio::test]
    async fn trader_positions_accept_surrounding_address_whitespace() {
        let surfpool = Surfpool::new();
        let trader = Pubkey::new_unique().to_string();
        let rpc_url = "not-a-url";
        let expected = surfpool
            .list_phoenix_trader_positions_at(rpc_url, &trader, &[])
            .await
            .unwrap_err();
        assert!(!expected.contains("is not a valid Phoenix Trader account address"));

        for input in [format!(" {trader} "), format!("\t{trader}\r\n")] {
            let error = surfpool
                .list_phoenix_trader_positions_at(rpc_url, &input, &[])
                .await
                .unwrap_err();
            assert_eq!(error, expected);
        }
    }

    #[tokio::test]
    async fn trader_positions_refuse_an_invalid_address_before_any_call() {
        let result = Surfpool::new()
            .list_phoenix_trader_positions(Parameters(ListPhoenixTraderPositionsParams {
                trader: "not-an-address".to_string(),
                surfnet_port: None,
                moves: Vec::new(),
            }))
            .await
            .unwrap();
        let payload = json_of(&result);
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|error| error.contains("'not-an-address' is not a valid")),
            "{payload}"
        );
    }

    #[tokio::test]
    async fn create_scenario_rejects_a_missing_dynamic_ref_value() {
        let surfpool = Surfpool::new();
        let template = TemplateRegistry::new()
            .get("phoenix-market-move")
            .expect("template")
            .clone();
        let mut scenario = surfpool_types::Scenario::new(
            "no symbol".to_string(),
            "a dynamic_ref without a value must be rejected".to_string(),
        );
        scenario.add_override(
            surfpool_types::OverrideInstance::new(template.id.clone(), 0, template.address)
                .with_values(HashMap::from([(
                    "target_ticks".to_string(),
                    serde_json::json!("1"),
                )])),
        );
        let result = surfpool
            .create_scenario(Parameters(scenario))
            .await
            .unwrap();
        let text = &result.content[0].as_text().expect("text").text;
        assert!(
            text.contains("Missing required value")
                && text.contains("symbol")
                && text.contains("list_phoenix_markets"),
            "the missing symbol and the tool that resolves it must be named, got: {text}"
        );
    }

    #[tokio::test]
    async fn create_scenario_rejects_bad_pump_overrides_before_any_http_call() {
        let surfpool = Surfpool::new();
        let registry = TemplateRegistry::new();
        let curve_address = registry
            .get("pump-bonding-curve-custom")
            .expect("template")
            .address
            .clone();

        let mut unknown = surfpool_types::Scenario::new(
            "bad template".to_string(),
            "unknown templateId must be rejected".to_string(),
        );
        unknown.add_override(surfpool_types::OverrideInstance::new(
            "pump-bonding-curve".to_string(),
            0,
            curve_address.clone(),
        ));
        let result = surfpool.create_scenario(Parameters(unknown)).await.unwrap();
        let text = &result.content[0].as_text().expect("text").text;
        assert!(
            text.contains("Invalid templateId"),
            "a misremembered id must be named, got: {text}"
        );

        let mut incomplete = surfpool_types::Scenario::new(
            "missing mint".to_string(),
            "a PDA constant without a value must be rejected".to_string(),
        );
        incomplete.add_override(
            surfpool_types::OverrideInstance::new(
                "pump-bonding-curve-custom".to_string(),
                0,
                curve_address,
            )
            .with_values(HashMap::from([(
                "complete".to_string(),
                serde_json::json!(true),
            )])),
        );
        let result = surfpool
            .create_scenario(Parameters(incomplete))
            .await
            .unwrap();
        let text = &result.content[0].as_text().expect("text").text;
        assert!(
            text.contains("Missing required value") && text.contains("token_mint"),
            "the missing PDA seed value must be named, got: {text}"
        );

        let mut wrong_type = surfpool_types::Scenario::new(
            "numeric mint".to_string(),
            "a non-string mint must be rejected, not silently skipped".to_string(),
        );
        wrong_type.add_override(
            surfpool_types::OverrideInstance::new(
                "pump-bonding-curve-custom".to_string(),
                0,
                registry
                    .get("pump-bonding-curve-custom")
                    .expect("template")
                    .address
                    .clone(),
            )
            .with_values(HashMap::from([(
                "token_mint".to_string(),
                serde_json::json!(12345),
            )])),
        );
        let result = surfpool
            .create_scenario(Parameters(wrong_type))
            .await
            .unwrap();
        let text = &result.content[0].as_text().expect("text").text;
        assert!(
            text.contains("must be a base-58 mint address string") && text.contains("token_mint"),
            "the wrong-typed mint must be named, got: {text}"
        );

        let mut tampered = surfpool_types::Scenario::new(
            "tampered mint".to_string(),
            "base58 is case-sensitive; a case-folded match targets another PDA".to_string(),
        );
        tampered.add_override(
            surfpool_types::OverrideInstance::new(
                "pump-bonding-curve-custom".to_string(),
                0,
                registry
                    .get("pump-bonding-curve-custom")
                    .expect("template")
                    .address
                    .clone(),
            )
            .with_values(HashMap::from([(
                "token_mint".to_string(),
                serde_json::json!("9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgPUMP"),
            )])),
        );
        let result = surfpool
            .create_scenario(Parameters(tampered))
            .await
            .unwrap();
        let text = &result.content[0].as_text().expect("text").text;
        assert!(
            text.contains("Invalid value"),
            "a case-flipped mint must be rejected, got: {text}"
        );
    }

    const KAMINO_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
    const USER_RESERVE_YAML: &str = r#"
protocol: My Kamino
version: v1
templates:
  - id: my-kamino-reserve
    name: Drain a reserve
    description: Zero the available liquidity
    idl_account_name: Reserve
    properties:
      - "liquidity.total_available_amount"
    address:
      type: pubkey
    llm_context: Set fetchBeforeUse true, then liquidity.total_available_amount 0.
"#;

    fn text_of(result: &CallToolResult) -> String {
        result.content[0]
            .as_text()
            .expect("text content")
            .text
            .clone()
    }

    fn kamino_idl() -> Value {
        serde_json::from_str(surfpool_core::scenarios::registry::KAMINO_V1_IDL_CONTENT).unwrap()
    }

    fn collection(yaml: &str, idl: Value) -> OverrideTemplateCollectionParams {
        OverrideTemplateCollectionParams {
            collection_yaml: yaml.to_string(),
            idl,
            overwrite: false,
            surfnet_port: None,
        }
    }

    fn refusal(surfpool: &Surfpool, yaml: &str, idl: Value) -> String {
        match surfpool.checked_collection(&collection(yaml, idl)) {
            Ok(_) => panic!("the collection was accepted"),
            Err(error) => error,
        }
    }

    #[tokio::test]
    async fn a_collection_is_checked_against_its_idl_and_the_built_in_ids() {
        let surfpool = Surfpool::new();
        let ok = surfpool
            .validate_override_template(Parameters(collection(USER_RESERVE_YAML, kamino_idl())))
            .await
            .unwrap();
        assert_ne!(ok.is_error, Some(true), "{}", text_of(&ok));
        let json = json_of(&ok);
        assert_eq!(json["templates"][0]["id"], "my-kamino-reserve");
        assert_eq!(json["templates"][0]["accountType"], "Reserve");
        assert!(json["templates"][0]["llmContext"].is_string());
        let idl_text = Value::String(kamino_idl().to_string());
        assert!(
            surfpool
                .checked_collection(&collection(USER_RESERVE_YAML, idl_text))
                .is_ok(),
            "the IDL file's text is accepted as well as its JSON"
        );
        assert!(
            !surfpool
                .template_registry
                .read()
                .unwrap()
                .contains("my-kamino-reserve"),
            "validation adds nothing"
        );

        let entry = USER_RESERVE_YAML.split("templates:\n").nth(1).unwrap();
        let failures = [
            (
                USER_RESERVE_YAML.replace("total_available_amount", "no_such_field"),
                "no_such_field",
            ),
            (
                USER_RESERVE_YAML.replace("name: Reserve", "name: ReserveLiquidity"),
                "no account named",
            ),
            (format!("raw_layout: true\n{USER_RESERVE_YAML}"), "raw_layout"),
            (
                USER_RESERVE_YAML.replace("    llm_context:", "    # llm_context:"),
                "llm_context",
            ),
            (format!("{USER_RESERVE_YAML}{entry}"), "used twice"),
            (
                USER_RESERVE_YAML.replace("my-kamino-reserve", "kamino-reserve-state"),
                "built-in",
            ),
            (
                USER_RESERVE_YAML.replace(
                    "    address:\n",
                    "    keep_alive:\n      - template: some-keep-alive\n        address: {type: override}\n        values: {}\n    address:\n",
                ),
                "keep_alive is not supported in user templates",
            ),
            (
                USER_RESERVE_YAML.replace(
                    "templates:\n",
                    "keep_alive:\n  - template: some-keep-alive\n    address: {type: override}\n    values: {}\ntemplates:\n",
                ),
                "keep_alive is not supported in user templates",
            ),
        ];
        for (yaml, expected) in failures {
            let error = refusal(&surfpool, &yaml, kamino_idl());
            assert!(error.contains(expected), "{expected}: {error}");
        }
        let empty_keep_alive =
            USER_RESERVE_YAML.replace("    address:\n", "    keep_alive: []\n    address:\n");
        assert!(
            surfpool
                .checked_collection(&collection(&empty_keep_alive, kamino_idl()))
                .is_ok(),
            "an explicit empty keep_alive list is accepted"
        );

        let mut bad_address = kamino_idl();
        bad_address["address"] = Value::String("not-a-pubkey".into());
        let error = refusal(&surfpool, USER_RESERVE_YAML, bad_address);
        assert!(error.contains("not a pubkey"), "{error}");
        let legacy = serde_json::json!({
            "version": "0.1.0",
            "name": "legacy",
            "instructions": [],
            "metadata": {"address": KAMINO_PROGRAM},
        });
        let error = refusal(&surfpool, USER_RESERVE_YAML, legacy);
        assert!(error.contains("anchor idl convert"), "{error}");
    }

    const USER_PDA_YAML: &str = r#"
protocol: My Kamino PDA
version: v1
templates:
  - id: my-kamino-pda-reserve
    name: Drain a derived reserve
    description: Zero the available liquidity of a reserve found by its market
    idl_account_name: Reserve
    properties:
      - "liquidity.total_available_amount"
      - path: market
        type: input
        value_type: pubkey
      - path: note
        type: input
        value_type: string
    address:
      type: pda
      program_id: KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD
      seeds:
        - type: string
          value: reserve
        - type: property_ref
          value: market
    llm_context: Set liquidity.total_available_amount 0.
"#;

    fn session_with_pda_template() -> Surfpool {
        let surfpool = Surfpool::new();
        let Ok(checked) = surfpool.checked_collection(&collection(USER_PDA_YAML, kamino_idl()))
        else {
            panic!("the PDA collection is valid");
        };
        surfpool.store_collection(checked, false).unwrap();
        surfpool
    }

    fn pda_scenario(values: Value) -> Scenario {
        let mut scenario = Scenario::new("pda".to_string(), "a saved PDA template".to_string());
        scenario.add_override(
            OverrideInstance::new(
                "my-kamino-pda-reserve".to_string(),
                0,
                AccountAddress::Pubkey(String::new()),
            )
            .with_values(serde_json::from_value(values).unwrap()),
        );
        scenario
    }

    #[test]
    fn a_saved_template_override_gets_a_pubkey_and_only_field_values() {
        let surfpool = session_with_pda_template();
        let market = solana_signer::Signer::pubkey(&solana_keypair::Keypair::new());
        let program = Pubkey::from_str(KAMINO_PROGRAM).unwrap();
        let (reserve, _) = Pubkey::find_program_address(&[b"reserve", market.as_ref()], &program);
        let values = serde_json::json!({
            "liquidity.total_available_amount": 0,
            "market": market.to_string(),
            "note": "an input the program never reads",
        });

        let mut scenario = pda_scenario(values.clone());
        assert!(surfpool.resolve_saved_overrides(&mut scenario).unwrap());
        assert_eq!(
            scenario.overrides[0].account,
            AccountAddress::Pubkey(reserve.to_string())
        );
        assert_eq!(
            scenario.overrides[0].values,
            HashMap::from([(
                "liquidity.total_available_amount".to_string(),
                serde_json::json!(0)
            )])
        );

        let mut sent = serde_json::to_value(pda_scenario(values)).unwrap();
        surfpool.resolve_registered_scenario(&mut sent).unwrap();
        assert_eq!(
            sent["overrides"][0]["account"],
            serde_json::json!({"pubkey": reserve.to_string()}),
            "surfnet_registerScenario through call_surfnet_rpc is resolved the same way"
        );
        assert_eq!(
            sent["overrides"][0]["values"],
            serde_json::json!({"liquidity.total_available_amount": 0})
        );
    }

    #[tokio::test]
    async fn a_saved_template_override_with_a_missing_seed_is_refused() {
        let surfpool = session_with_pda_template();
        let scenario = pda_scenario(serde_json::json!({"liquidity.total_available_amount": 0}));
        let result = surfpool
            .create_scenario(Parameters(scenario))
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(
            text.contains("needs a value") && text.contains("market"),
            "{text}"
        );
    }

    #[test]
    fn a_built_in_template_override_is_sent_unchanged() {
        let surfpool = session_with_pda_template();
        let template = TemplateRegistry::new()
            .get("pyth-price-feed-v2")
            .expect("template")
            .clone();
        let mut scenario = Scenario::new("pyth".to_string(), "a built-in template".to_string());
        scenario.add_override(
            OverrideInstance::new(template.id.clone(), 0, template.address).with_values(
                HashMap::from([
                    (
                        "feed_id".to_string(),
                        serde_json::json!(
                            "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d"
                        ),
                    ),
                    (
                        "price_message.price".to_string(),
                        serde_json::json!(8_500_000_000u64),
                    ),
                ]),
            ),
        );
        let before = scenario.clone();
        assert!(!surfpool.resolve_saved_overrides(&mut scenario).unwrap());
        assert_eq!(scenario, before);

        let mut sent = serde_json::to_value(&before).unwrap();
        let original = sent.clone();
        surfpool.resolve_registered_scenario(&mut sent).unwrap();
        assert_eq!(sent, original);
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn save(
        surfpool: &Surfpool,
        yaml: &str,
        idl: Value,
        overwrite: bool,
        port: u16,
    ) -> Result<Value, String> {
        let mut params = collection(yaml, idl);
        params.overwrite = overwrite;
        params.surfnet_port = Some(port);
        let result = surfpool
            .save_override_template(Parameters(params))
            .await
            .unwrap();
        match result.is_error {
            Some(true) => Err(text_of(&result)),
            _ => Ok(json_of(&result)),
        }
    }

    #[tokio::test]
    async fn saving_registers_the_idl_and_adds_the_templates_to_this_session() {
        let port = free_port();
        let started = start_surfnet::run_headless(1, port, free_port());
        assert!(started.error.is_none(), "{:?}", started.error);
        let surfpool = Surfpool::new();
        let contains = |id: &str| surfpool.template_registry.read().unwrap().contains(id);

        let saved = save(&surfpool, USER_RESERVE_YAML, kamino_idl(), false, port)
            .await
            .unwrap();
        assert_eq!(
            saved["templateIds"],
            serde_json::json!(["my-kamino-reserve"])
        );
        assert_eq!(
            saved["idlRegistered"], false,
            "the surfnet already has the built-in Kamino IDL"
        );
        let index = json_of(&surfpool.get_override_templates().await.unwrap());
        assert!(
            index
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == "my-kamino-reserve")
        );

        let again = save(&surfpool, USER_RESERVE_YAML, kamino_idl(), false, port)
            .await
            .unwrap_err();
        assert!(again.contains("overwrite: true"), "{again}");
        let renamed = USER_RESERVE_YAML.replace("my-kamino-reserve", "my-kamino-reserve-2");
        save(&surfpool, &renamed, kamino_idl(), true, port)
            .await
            .unwrap();
        assert!(contains("my-kamino-reserve-2"));
        assert!(
            !contains("my-kamino-reserve"),
            "the replaced collection's id leaves the session"
        );
        let elsewhere = renamed.replace("protocol: My Kamino", "protocol: Other");
        let taken = save(&surfpool, &elsewhere, kamino_idl(), true, port)
            .await
            .unwrap_err();
        assert!(
            taken.contains("My Kamino v1"),
            "overwrite reuses only the ids of the collection it replaces: {taken}"
        );

        let program = solana_signer::Signer::pubkey(&solana_keypair::Keypair::new()).to_string();
        let mut idl = kamino_idl();
        idl["address"] = Value::String(program.clone());
        let fresh = elsewhere.replace("my-kamino-reserve-2", "other-reserve");
        let saved = save(&surfpool, &fresh, idl.clone(), false, port)
            .await
            .unwrap();
        assert_eq!(saved["idlRegistered"], true);
        let active = surfnet_rpc(port, "surfnet_getActiveIdl", serde_json::json!([program]))
            .await
            .unwrap();
        assert_eq!(active["value"]["address"], program);
        let saved = save(&surfpool, &fresh, idl, true, port).await.unwrap();
        assert_eq!(
            saved["idlRegistered"], false,
            "an IDL equal to the active one is not registered again"
        );

        let down = fresh
            .replace("other-reserve", "down-reserve")
            .replace("protocol: Other", "protocol: Down");
        assert!(
            save(&surfpool, &down, kamino_idl(), false, free_port())
                .await
                .is_err()
        );
        assert!(
            !contains("down-reserve"),
            "a save with no surfnet to take the IDL changes nothing"
        );
    }

    #[tokio::test]
    async fn the_authoring_prompt_carries_the_guide_and_the_request() {
        let messages = Surfpool::new()
            .author_override_template(Parameters(AuthorTemplateArgs {
                program_id: KAMINO_PROGRAM.to_string(),
                goal: "drain a reserve".to_string(),
            }))
            .await;
        let text = &messages[0].content.as_text().expect("a text message").text;
        assert!(text.starts_with("# "), "the guide comes first");
        assert!(text.contains(KAMINO_PROGRAM) && text.contains("drain a reserve"));
    }
}
