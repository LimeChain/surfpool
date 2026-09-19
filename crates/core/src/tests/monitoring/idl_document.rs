//! IDL drift: does the published on-chain IDL still match ours? Informational only, since a
//! published IDL can run ahead of deployment; it errors only when no live account exists instead.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    str::FromStr,
};

use anchor_lang_idl::types::{IdlArrayLen, IdlDefinedFields, IdlType, IdlTypeDefTy};
use solana_pubkey::Pubkey;
use surfpool_types::types::Idl;

use super::{Report, live};
use crate::scenarios::TemplateRegistry;

const CHECK: &str = "idl-document";

/// Rewrites a pre-0.30 Anchor IDL into the current shape. The returned flag says whether
/// discriminators are original or invented here (Pyth's casing means ours would falsely diff).
fn modernise(mut value: serde_json::Value, program_id: &str) -> (serde_json::Value, bool) {
    let Some(root) = value.as_object_mut() else {
        return (value, false);
    };
    if root.contains_key("address") {
        return (value, false);
    }

    root.insert("address".into(), serde_json::json!(program_id));
    let name = root
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let version = root
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    root.insert(
        "metadata".into(),
        serde_json::json!({ "name": name, "version": version, "spec": "0.1.0" }),
    );

    let mut types = match root.remove("types") {
        Some(serde_json::Value::Array(types)) => types,
        _ => Vec::new(),
    };
    if let Some(serde_json::Value::Array(accounts)) = root.get_mut("accounts") {
        for account in accounts {
            let Some(account) = account.as_object_mut() else {
                continue;
            };
            let Some(name) = account
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from)
            else {
                continue;
            };
            if let Some(ty) = account.remove("type") {
                types.push(serde_json::json!({ "name": name, "type": ty }));
            }
            account
                .entry("discriminator")
                .or_insert_with(|| serde_json::json!(anchor_discriminator("account", &name)));
        }
    }
    root.insert("types".into(), serde_json::Value::Array(types));

    if let Some(serde_json::Value::Array(instructions)) = root.get_mut("instructions") {
        for instruction in instructions {
            let Some(instruction) = instruction.as_object_mut() else {
                continue;
            };
            let Some(name) = instruction
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from)
            else {
                continue;
            };
            instruction.entry("discriminator").or_insert_with(|| {
                serde_json::json!(anchor_discriminator("global", &snake(&name)))
            });
        }
    }

    // Events carry their own required shape in the new schema and nothing here reads them.
    root.remove("events");

    rewrite_types(&mut value);
    (value, true)
}

/// Anchor's discriminator: the first eight bytes of sha256 over a namespaced name.
fn anchor_discriminator(namespace: &str, name: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("{namespace}:{name}").as_bytes())[..8].to_vec()
}

/// Renames old-schema type spellings: `{"defined": "Foo"}` → `{"defined": {"name": "Foo"}}`,
/// `publicKey` → `pubkey`, rewritten recursively since either can appear anywhere in the tree.
fn rewrite_types(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) if text == "publicKey" => {
            *text = "pubkey".to_string();
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(name)) = map.get("defined") {
                let name = name.clone();
                map.insert("defined".into(), serde_json::json!({ "name": name }));
            }
            for entry in map.values_mut() {
                rewrite_types(entry);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(rewrite_types),
        _ => {}
    }
}

/// Parses an IDL document, converting a pre-0.30 one on the way in. The flag says whether the
/// discriminators came from the document or were invented here.
fn parse_idl(json: &str, program_id: &str) -> Result<(Idl, bool), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("it is not JSON: {error}"))?;
    let (value, converted) = modernise(value, program_id);
    serde_json::from_value(value)
        .map(|idl| (idl, converted))
        .map_err(|error| format!("it did not parse: {error}"))
}

/// Anchor stores a program's IDL at a seeded address off the program's own signer PDA.
fn published_idl_address(program_id: &Pubkey) -> Option<Pubkey> {
    let base = Pubkey::find_program_address(&[], program_id).0;
    Pubkey::create_with_seed(&base, "anchor:idl", program_id).ok()
}

/// `IdlAccount` is an 8-byte discriminator, a 32-byte authority and a Borsh `Vec<u8>` holding the
/// zlib-compressed JSON.
fn decode_published_idl(data: &[u8], program_id: &str) -> Result<(Idl, bool), String> {
    const HEADER: usize = 8 + 32;
    if data.len() < HEADER + 4 {
        return Err(format!("the IDL account holds only {} bytes", data.len()));
    }
    let length = u32::from_le_bytes(
        data[HEADER..HEADER + 4]
            .try_into()
            .expect("a four byte slice"),
    ) as usize;
    let body = data
        .get(HEADER + 4..HEADER + 4 + length)
        .ok_or_else(|| format!("the IDL account claims {length} bytes it does not hold"))?;

    // The account is controlled by whoever holds the program's IDL authority. A bounded read
    // keeps a hostile or broken payload from taking the whole run down with it.
    const MAX_INFLATED: u64 = 16 * 1024 * 1024;
    let mut json = String::new();
    flate2::read::ZlibDecoder::new(body)
        .take(MAX_INFLATED)
        .read_to_string(&mut json)
        .map_err(|error| format!("the IDL payload did not inflate: {error}"))?;
    if json.len() as u64 >= MAX_INFLATED {
        return Err(format!(
            "the IDL payload inflates past {MAX_INFLATED} bytes"
        ));
    }
    parse_idl(&json, program_id)
}

/// Folds camelCase/snake_case to one spelling before comparing — casing has no effect on the
/// bytes, but an unfolded mismatch would read every converted field as renamed.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// Follows a template property path through the type graph. A bare numeric segment
/// (`prices.0.price.value`) steps into the array element type rather than a named field.
fn resolve_path(idl: &Idl, root: &str, path: &str) -> Option<IdlType> {
    let mut resolved: Option<IdlType> = None;
    for segment in path.split('.') {
        if !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()) {
            let container = resolved.as_ref()?;
            resolved = Some(element_type(container).clone());
            continue;
        }

        let owner = match resolved.as_ref() {
            None => root.to_string(),
            Some(ty) => match element_type(ty) {
                IdlType::Defined { name, .. } => name.clone(),
                _ => return None,
            },
        };
        let def = idl
            .types
            .iter()
            .find(|def| snake(&def.name) == snake(&owner))?;
        let IdlTypeDefTy::Struct {
            fields: Some(IdlDefinedFields::Named(named)),
        } = &def.ty
        else {
            return None;
        };
        let wanted = snake(segment);
        resolved = Some(
            named
                .iter()
                .find(|field| snake(&field.name) == wanted)?
                .ty
                .clone(),
        );
    }
    resolved
}

fn element_type(ty: &IdlType) -> &IdlType {
    match ty {
        IdlType::Option(inner) | IdlType::Vec(inner) | IdlType::Array(inner, _) => {
            element_type(inner)
        }
        other => other,
    }
}

fn describe(ty: &IdlType) -> String {
    match ty {
        IdlType::Defined { name, .. } => name.clone(),
        IdlType::Option(inner) => format!("Option<{}>", describe(inner)),
        IdlType::Vec(inner) => format!("Vec<{}>", describe(inner)),
        IdlType::Array(inner, len) => match len {
            IdlArrayLen::Value(size) => format!("[{}; {size}]", describe(inner)),
            IdlArrayLen::Generic(name) => format!("[{}; {name}]", describe(inner)),
        },
        other => format!("{other:?}"),
    }
}

struct ProgramIdls {
    protocols: BTreeSet<String>,
    committed: Idl,
    /// Account type name to the property paths our templates write.
    used: BTreeMap<String, BTreeSet<String>>,
}

fn committed_by_program(registry: &TemplateRegistry) -> BTreeMap<String, ProgramIdls> {
    let mut by_program: BTreeMap<String, ProgramIdls> = BTreeMap::new();
    for template in registry.all() {
        let Some(idl) = template.idl.as_ref() else {
            continue;
        };
        let entry = by_program
            .entry(idl.address.clone())
            .or_insert_with(|| ProgramIdls {
                protocols: BTreeSet::new(),
                committed: idl.clone(),
                used: BTreeMap::new(),
            });
        entry.protocols.insert(template.protocol.to_lowercase());
        let used = entry.used.entry(template.account_type.clone()).or_default();
        for property in &template.properties {
            used.insert(property.path.clone());
        }
    }
    by_program
}

#[tokio::test]
async fn committed_idls_match_the_published_idls() {
    let registry = TemplateRegistry::new();
    let mut report = Report::new(CHECK);

    for (program_id, program) in committed_by_program(&registry) {
        let protocol = program
            .protocols
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");

        let Ok(key) = Pubkey::from_str(&program_id) else {
            report.warn(
                &protocol,
                &program_id,
                "the committed IDL's address is not a pubkey, so its publication cannot be found",
            );
            continue;
        };
        let Some(address) = published_idl_address(&key) else {
            report.warn(
                &protocol,
                &program_id,
                "no IDL address derives from this program",
            );
            continue;
        };

        let Some(account) = live::try_fetch(&[address]).await.remove(0) else {
            report.info(
                &protocol,
                &program_id,
                "publishes no on-chain IDL and no other source is recorded for it; covered by \
                 the deployment baseline and the live round trip only",
            );
            continue;
        };

        let (published, converted) = match decode_published_idl(&account.data, &program_id) {
            Ok(parsed) => parsed,
            Err(reason) => {
                report.info(
                    &protocol,
                    &program_id,
                    format!("the on-chain IDL account {address} could not be read: {reason}"),
                );
                continue;
            }
        };

        compare(&mut report, &protocol, &program, &published, !converted);
    }

    report.finish();
}

fn compare(
    report: &mut Report,
    protocol: &str,
    program: &ProgramIdls,
    published: &Idl,
    // False when the published document carried no discriminators and this check supplied them,
    // in which case comparing them would only compare us against ourselves.
    trust_discriminators: bool,
) {
    let committed = &program.committed;

    if committed.metadata.version != published.metadata.version {
        report.info(
            protocol,
            &committed.address,
            format!(
                "the published IDL moved from version {} to {}",
                committed.metadata.version, published.metadata.version
            ),
        );
    }

    for (account_type, used_paths) in &program.used {
        // A discriminator is derived from the account's name alone, so a mismatch means ours is
        // simply wrong. Everything else can legitimately differ by version, so only this is an error.
        let committed_disc = committed
            .accounts
            .iter()
            .find(|account| &account.name == account_type)
            .map(|account| account.discriminator.clone());
        let published_disc = published
            .accounts
            .iter()
            .find(|account| snake(&account.name) == snake(account_type))
            .map(|account| account.discriminator.clone());

        match (&committed_disc, &published_disc) {
            (Some(ours), Some(theirs)) if ours != theirs && trust_discriminators => {
                report.push(super::Finding::new(
                    CHECK,
                    super::Severity::Error,
                    protocol,
                    account_type,
                    format!(
                        "the account discriminator we ship does not match the on-chain IDL: \
                         {ours:?} against {theirs:?}. A discriminator follows from the account's \
                         name, not from its version, so one of the two is wrong and every \
                         template addressing this account resolves by the value we ship.",
                    ),
                ))
            }
            (Some(_), None) => {
                report.warn(
                    protocol,
                    account_type,
                    "the published IDL does not declare this account at all, although our \
                     templates address it",
                );
                continue;
            }
            _ => {}
        }

        for path in used_paths {
            let before = resolve_path(committed, account_type, path);
            let after = resolve_path(published, account_type, path);
            match (before, after) {
                (Some(before), Some(after)) if describe(&before) != describe(&after) => {
                    report.warn(
                        protocol,
                        format!("{account_type}.{path}"),
                        format!(
                            "a field our templates write has a different type in the published \
                             IDL: {} against {}",
                            describe(&before),
                            describe(&after)
                        ),
                    );
                }
                (Some(_), None) => {
                    report.warn(
                        protocol,
                        format!("{account_type}.{path}"),
                        "a field our templates write is absent from the published IDL; either it \
                         is newer than what the protocol published, or the field is gone",
                    );
                }
                _ => {}
            }
        }
    }

    let new_accounts = published
        .accounts
        .iter()
        .filter(|account| {
            !committed
                .accounts
                .iter()
                .any(|existing| snake(&existing.name) == snake(&account.name))
        })
        .count();
    let new_instructions = published
        .instructions
        .iter()
        .filter(|instruction| {
            !committed
                .instructions
                .iter()
                .any(|existing| snake(&existing.name) == snake(&instruction.name))
        })
        .count();
    if new_accounts > 0 || new_instructions > 0 {
        report.info(
            protocol,
            &committed.address,
            format!(
                "the published IDL carries {new_accounts} account type(s) and {new_instructions} \
                 instruction(s) we do not model. Our IDLs are trimmed on purpose, so this is a \
                 menu of what could be supported, not a defect."
            ),
        );
    }
}

/// Every property path a template writes must exist in its own bundled IDL. Runs offline, so
/// it catches what the live round trip can't — that check never writes anything.
#[test]
fn every_template_property_resolves_in_its_own_idl() {
    let registry = TemplateRegistry::new();
    let mut unresolved = Vec::new();
    let mut resolved = 0usize;
    for template in registry.all() {
        let Some(idl) = template.idl.as_ref() else {
            continue;
        };
        for property in &template.properties {
            // Seed references name a value used to derive the address, not a field in the
            // account, so they are not expected to resolve.
            if template
                .address
                .get_pda_seed_references()
                .contains(&property.path)
            {
                continue;
            }
            match resolve_path(idl, &template.account_type, &property.path) {
                Some(_) => resolved += 1,
                None => unresolved.push(format!(
                    "{} writes {}.{}",
                    template.id, template.account_type, property.path
                )),
            }
        }
    }
    println!("{resolved} template property paths resolve");
    assert!(
        unresolved.is_empty(),
        "these templates write property paths their own bundled IDL does not declare: {unresolved:#?}"
    );
}

/// A pre-0.30 document must come out the other side describing the same account, with the
/// discriminator its name implies.
#[test]
fn a_legacy_document_converts_into_something_comparable() {
    let legacy = serde_json::json!({
        "version": "1.25.0",
        "name": "kamino_lending",
        "instructions": [{ "name": "initReserve", "accounts": [], "args": [] }],
        "accounts": [{
            "name": "Reserve",
            "type": { "kind": "struct", "fields": [
                { "name": "version", "type": "u64" },
                { "name": "liquidity", "type": { "defined": "ReserveLiquidity" } },
            ]},
        }],
        "types": [{
            "name": "ReserveLiquidity",
            "type": { "kind": "struct", "fields": [
                { "name": "totalAvailableAmount", "type": "u64" },
            ]},
        }],
    });

    let (converted, was_converted) =
        modernise(legacy, "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");
    assert!(
        was_converted,
        "an old-schema document must report that it was converted"
    );
    let idl: Idl = serde_json::from_value(converted).expect("a converted document must parse");

    assert_eq!(idl.address, "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");
    let reserve = idl
        .accounts
        .iter()
        .find(|account| account.name == "Reserve")
        .expect("the account survives the move into types");
    assert_eq!(
        reserve.discriminator,
        anchor_discriminator("account", "Reserve"),
        "the type requires a discriminator, so one is supplied - and the flag above is what stops \
         it from being compared against the one we ship"
    );
    let resolved = resolve_path(&idl, "Reserve", "liquidity.total_available_amount")
        .expect("a bare-string defined type must become a navigable one");
    assert_eq!(describe(&resolved), "U64");
}
