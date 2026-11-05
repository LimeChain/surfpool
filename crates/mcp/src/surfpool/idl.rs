//! Finds a program's IDL for the template authoring tools: the IDL the surfnet has registered,
//! else the program's Anchor IDL account; a file the caller names replaces both.

use std::{io::Read, str::FromStr};

use anchor_lang_idl::types::{Idl, IdlDefinedFields, IdlTypeDefTy};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use surfpool_core::{
    solana_commitment_config::CommitmentConfig, surfnet::remote::SurfnetRemoteClient,
};

const MAX_IDL_BYTES: u64 = 8 * 1024 * 1024;

pub struct ResolvedIdl {
    pub json: String,
    pub source: &'static str,
    pub idl: Idl,
}

pub async fn surfnet_rpc(port: u16, method: &str, params: Value) -> Result<Value, String> {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response: Value = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}"))
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("{method} on port {port}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("{method} on port {port}: {e}"))?;
    if let Some(error) = response.get("error") {
        return Err(format!("{method}: {error}"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

pub fn anchor_idl_address(program: &Pubkey) -> Result<Pubkey, String> {
    let (base, _) = Pubkey::find_program_address(&[], program);
    Pubkey::create_with_seed(&base, "anchor:idl", program).map_err(|e| e.to_string())
}

/// An Anchor IDL account is a discriminator, the authority, a `u32` length and that many bytes of
/// zlib-compressed JSON.
pub fn decode_anchor_idl_account(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 44 {
        return Err(format!(
            "the IDL account holds {} bytes, fewer than its 44-byte header",
            data.len()
        ));
    }
    let len = u32::from_le_bytes([data[40], data[41], data[42], data[43]]) as usize;
    let compressed = data
        .get(44..44 + len)
        .ok_or("the IDL account is shorter than its declared length")?;
    let mut json = Vec::new();
    flate2::read::ZlibDecoder::new(compressed)
        .take(MAX_IDL_BYTES)
        .read_to_end(&mut json)
        .map_err(|e| format!("the IDL account is not zlib data: {e}"))?;
    Ok(json)
}

pub async fn resolve_idl(
    port: u16,
    program_id: &str,
    idl_path: Option<&str>,
) -> Result<ResolvedIdl, String> {
    let program =
        Pubkey::from_str(program_id).map_err(|e| format!("programId {program_id:?}: {e}"))?;
    let (json, source) = match idl_path {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            let (json, converted) = published_idl(bytes, program_id)?;
            (json, if converted { "fileConverted" } else { "file" })
        }
        None => registered_or_chain(port, &program, program_id).await?,
    };
    let idl: Idl = serde_json::from_str(&json).map_err(|e| format!("{source} IDL: {e}"))?;
    if idl.address != program_id {
        return Err(format!(
            "the IDL belongs to program {}, not {program_id}",
            idl.address
        ));
    }
    Ok(ResolvedIdl { json, source, idl })
}

/// The surfnet's registered IDL is the one its writes go through, so it comes first; the chain
/// is read only for a program the surfnet does not know yet.
async fn registered_or_chain(
    port: u16,
    program: &Pubkey,
    program_id: &str,
) -> Result<(String, &'static str), String> {
    let registered = surfnet_rpc(port, "surfnet_getActiveIdl", json!([program_id])).await?;
    if let Some(idl) = registered.get("value").filter(|idl| !idl.is_null()) {
        let json = serde_json::to_string_pretty(idl).map_err(|e| e.to_string())?;
        return Ok((json, "registered"));
    }
    let client = SurfnetRemoteClient::new(format!("http://127.0.0.1:{port}"));
    let account = client
        .get_account(&anchor_idl_address(program)?, CommitmentConfig::confirmed())
        .await
        .map_err(|e| format!("reading the Anchor IDL account: {e}"))?
        .map_account()
        .map_err(|_| {
            format!(
                "{program_id} publishes no Anchor IDL account and the surfnet has no IDL registered for it; pass the IDL file as idlPath if you have one"
            )
        })?;
    let (json, converted) = published_idl(decode_anchor_idl_account(&account.data)?, program_id)?;
    Ok((
        json,
        if converted {
            "onChainConverted"
        } else {
            "onChain"
        },
    ))
}

/// A spec-format IDL is kept byte for byte. A legacy one goes through Anchor's converter, which
/// needs the program address the legacy format rarely carries. The flag says which happened.
pub fn published_idl(bytes: Vec<u8>, program_id: &str) -> Result<(String, bool), String> {
    let mut raw: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("the IDL is not JSON: {e}"))?;
    if !raw.is_object() {
        return Err("the IDL is not a JSON object".into());
    }
    if raw["metadata"].get("spec").is_some() {
        let json = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        return Ok((json, false));
    }
    if raw["metadata"].is_null() {
        raw["metadata"] = json!({});
    }
    let Some(metadata) = raw["metadata"].as_object_mut() else {
        return Err("the IDL's metadata is not an object".into());
    };
    metadata
        .entry("address")
        .or_insert_with(|| Value::String(program_id.to_string()));
    let idl = anchor_lang_idl::convert::convert_idl(raw.to_string().as_bytes())
        .map_err(|e| format!("the IDL did not convert: {e}"))?;
    Ok((
        serde_json::to_string_pretty(&idl).map_err(|e| e.to_string())?,
        true,
    ))
}

/// Registers the IDL. Without `replace`, an IDL the surfnet already has for the program is kept,
/// so an authoring call never displaces the IDL a bundled template was checked against; an IDL
/// the caller named explicitly replaces it.
pub async fn register_idl(port: u16, idl: &Idl, replace: bool) -> Result<(), String> {
    let registered = surfnet_rpc(port, "surfnet_getActiveIdl", json!([idl.address])).await?;
    let active = registered
        .get("value")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value::<Idl>(value.clone()).ok());
    let keep = match active {
        Some(active) => active == *idl || !replace,
        None => false,
    };
    if keep {
        return Ok(());
    }
    surfnet_rpc(port, "surfnet_registerIdl", json!([idl, null]))
        .await
        .map(|_| ())
}

/// The account types with their top-level fields, and the names of the other types. Nested
/// shapes and real values come from a live account read as `jsonParsed`.
pub fn account_index(idl: &Idl) -> Value {
    let fields = |name: &str| -> Vec<Value> {
        let Some(def) = idl.types.iter().find(|t| t.name == name) else {
            return Vec::new();
        };
        match &def.ty {
            IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(fields)),
            } => fields
                .iter()
                .map(|f| json!({"name": f.name, "type": f.ty}))
                .collect(),
            _ => Vec::new(),
        }
    };
    let accounts: Vec<Value> = idl
        .accounts
        .iter()
        .map(|a| json!({"name": a.name, "fields": fields(&a.name)}))
        .collect();
    let types: Vec<&String> = idl.types.iter().map(|t| &t.name).collect();
    json!({"accounts": accounts, "types": types})
}
