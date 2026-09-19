//! Layout drift: does a real mainnet account still round-trip through the IDL we ship? A
//! synthetic account can't do this job — it's built *by* the IDL, so it can never disagree.

use std::collections::HashMap;

use anchor_lang_idl::types::{IdlArrayLen, IdlDefinedFields, IdlType, IdlTypeDefTy};
use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, OverrideTemplate, types::Idl};

use super::{
    Report,
    live::{self, diff_indices},
    slug,
};
use crate::{scenarios::TemplateRegistry, surfnet::svm::SurfnetSvm};

const CHECK: &str = "layout-round-trip";

/// Protocols whose committed IDL does not describe a Borsh account, so a round trip would prove
/// nothing. Keep the reason with the entry: a silent skip is how coverage quietly disappears.
const NOT_BORSH: &[(&str, &str)] = &[(
    "spl-token",
    "SPL Token accounts use a fixed C layout with no discriminator; the bundled IDL is a \
     description for the UI, not a Borsh schema",
)];

/// How many options of a constant-backed seed to sample. One proves the layout; a second proves
/// the layout is not specific to one market.
const SAMPLES_PER_TEMPLATE: usize = 2;

/// Where a sample account address came from, for the report.
#[derive(Debug)]
enum Sample {
    Resolved {
        label: String,
        address: Pubkey,
        /// True when the address came from a constant-list entry rather than the template itself
        /// — one bad entry is a stale option, but every entry failing means the layout moved.
        from_options: bool,
    },
    Unresolvable(String),
}

/// Picks live accounts to test against, using only what the template carries — no
/// `getProgramAccounts` fallback, since that's heavy and a same-type account may be unaddressable.
fn samples(template: &OverrideTemplate) -> Vec<Sample> {
    if let AccountAddress::Pubkey(literal) = &template.address {
        return match Pubkey::try_from(literal.as_str()) {
            Ok(address) => vec![Sample::Resolved {
                label: "template default".to_string(),
                address,
                from_options: false,
            }],
            Err(_) => vec![Sample::Unresolvable(
                "the template carries no default address, so there is no live account to test \
                 against"
                    .to_string(),
            )],
        };
    }

    let references = template.address.get_pda_seed_references();
    if references.is_empty() {
        return match template.address.resolve_simple() {
            Some(address) => vec![Sample::Resolved {
                label: "derived from constant seeds".to_string(),
                address,
                from_options: false,
            }],
            None => vec![Sample::Unresolvable(
                "the PDA seeds are constant but did not derive an address".to_string(),
            )],
        };
    }

    // More than one free variable means guessing a combination, and a combination that has no
    // pool on chain would read as drift when it is only an address we invented.
    if references.len() > 1 {
        return vec![Sample::Unresolvable(format!(
            "the address needs {} caller-supplied values ({}), so no single live account follows \
             from the template alone",
            references.len(),
            references.join(", ")
        ))];
    }

    let reference = &references[0];
    let Some(constant_name) = template
        .properties
        .iter()
        .find(|property| &property.path == reference)
        .and_then(|property| property.constant.as_ref())
    else {
        return vec![Sample::Unresolvable(format!(
            "the address needs '{reference}', which is caller-supplied and has no constant list \
             to sample from"
        ))];
    };

    let Some(definition) = template.constants.get(constant_name) else {
        return vec![Sample::Unresolvable(format!(
            "'{reference}' points at the constant '{constant_name}', which the template does not \
             define"
        ))];
    };

    definition
        .options
        .iter()
        .take(SAMPLES_PER_TEMPLATE)
        .map(|option| {
            let values = HashMap::from([(
                reference.clone(),
                serde_json::Value::String(option.value.clone()),
            )]);
            match template.address.resolve(Some(&values)) {
                Some(address) => Sample::Resolved {
                    label: format!("{constant_name}={}", option.id),
                    address,
                    from_options: true,
                },
                None => Sample::Unresolvable(format!(
                    "'{reference}' = {} did not derive an address",
                    option.id
                )),
            }
        })
        .collect()
}

/// The IDL-declared byte size of this account's body, or `None` for variable-width types —
/// catches a same-size field swap (e.g. `u128` misread as `u64`) the round trip alone would miss.
fn declared_body_size(idl: &Idl, type_name: &str) -> Option<usize> {
    fn size_of_type(idl: &Idl, ty: &IdlType, depth: usize) -> Option<usize> {
        if depth > 32 {
            return None;
        }
        Some(match ty {
            IdlType::Bool | IdlType::U8 | IdlType::I8 => 1,
            IdlType::U16 | IdlType::I16 => 2,
            IdlType::U32 | IdlType::I32 | IdlType::F32 => 4,
            IdlType::U64 | IdlType::I64 | IdlType::F64 => 8,
            IdlType::U128 | IdlType::I128 => 16,
            IdlType::Pubkey => 32,
            IdlType::Array(inner, IdlArrayLen::Value(len)) => {
                size_of_type(idl, inner, depth + 1)?.checked_mul(*len)?
            }
            IdlType::Defined { name, .. } => size_of_def(idl, name, depth + 1)?,
            // Vec, String, Option and bytes are length-prefixed, so the type declares no size.
            _ => return None,
        })
    }

    fn size_of_def(idl: &Idl, name: &str, depth: usize) -> Option<usize> {
        if depth > 32 {
            return None;
        }
        let def = idl.types.iter().find(|def| def.name == name)?;
        match &def.ty {
            IdlTypeDefTy::Struct { fields: None } => Some(0),
            IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(named)),
            } => named
                .iter()
                .map(|field| size_of_type(idl, &field.ty, depth + 1))
                .sum(),
            IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Tuple(tuple)),
            } => tuple
                .iter()
                .map(|ty| size_of_type(idl, ty, depth + 1))
                .sum(),
            // Borsh writes a one-byte tag, so a fixed width exists only when no variant carries
            // a payload.
            IdlTypeDefTy::Enum { variants } => variants
                .iter()
                .all(|variant| variant.fields.is_none())
                .then_some(1),
            IdlTypeDefTy::Type { alias } => size_of_type(idl, alias, depth + 1),
        }
    }

    size_of_def(idl, type_name, 0)
}

/// Checks one sampled account. `Ok` carries how many bytes the account holds beyond what the IDL
/// describes; `Err` carries the reason it failed.
async fn check_sample(
    surfnet_svm: &SurfnetSvm,
    template: &OverrideTemplate,
    idl: &Idl,
    address: Pubkey,
    label: &str,
) -> Result<usize, String> {
    let Some(account) = live::try_fetch(&[address]).await.remove(0) else {
        return Err(format!(
            "the account this template addresses ({label}) no longer exists on mainnet: {address}"
        ));
    };

    if let Ok(expected_owner) = Pubkey::try_from(idl.address.as_str())
        && account.owner != expected_owner
    {
        return Err(format!(
            "{address} ({label}) is owned by {} but the template's IDL describes accounts of \
             {expected_owner}",
            account.owner
        ));
    }

    // Anchor lets an account declare a discriminator of any length; read it from the IDL rather
    // than assuming eight.
    let discriminator_len = idl
        .accounts
        .iter()
        .find(|account| account.name == template.account_type)
        .map_or(8, |account| account.discriminator.len());
    let body_len = account.data.len().saturating_sub(discriminator_len);

    let mut surplus = 0;
    if let Some(declared) = declared_body_size(idl, &template.account_type) {
        if body_len < declared {
            return Err(format!(
                "{address} ({label}) holds {body_len} bytes after its discriminator, but the \
                 bundled IDL describes {} as {declared}. The IDL claims bytes the account does not \
                 have, so every field past the shortfall is read from the wrong place.",
                template.account_type
            ));
        }
        surplus = body_len - declared;
    }

    match surfnet_svm.get_forged_account_data(&address, &account.data, idl, &HashMap::new()) {
        Ok(forged) if forged == account.data => Ok(surplus),
        Ok(forged) => {
            let diffs = diff_indices(&forged, &account.data);
            Err(format!(
                "a no-op round trip altered the live account {address} ({label}): {} of {} bytes \
                 changed, first at {:?}, re-encoded length {} against {}. The bundled IDL no \
                 longer matches the deployed layout.",
                diffs.len(),
                account.data.len(),
                diffs.first(),
                forged.len(),
                account.data.len()
            ))
        }
        Err(error) => Err(format!(
            "the live account {address} ({label}) did not decode with the bundled IDL: {error}"
        )),
    }
}

#[tokio::test]
async fn every_idl_template_round_trips_over_a_live_account() {
    let registry = TemplateRegistry::new();
    let (surfnet_svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    let mut report = Report::new(CHECK);
    let mut covered = 0usize;

    for template in registry.all() {
        let protocol = slug(&template.protocol);
        if let Some((_, reason)) = NOT_BORSH.iter().find(|(name, _)| *name == protocol) {
            report.info(
                &protocol,
                &template.id,
                format!("not layout-monitored: {reason}"),
            );
            continue;
        }
        // A raw layout has no IDL to round-trip through. Its byte guards and every-market sweeps
        // live in the protocol's own suite, which this same job runs.
        let Some(idl) = template.idl.as_ref() else {
            report.info(
                &protocol,
                &template.id,
                "raw layout; covered by the protocol's own suite rather than by this check",
            );
            continue;
        };

        let mut failures: Vec<(String, bool)> = Vec::new();
        let mut passes = 0usize;
        for sample in samples(template) {
            let (label, address, from_options) = match sample {
                Sample::Resolved {
                    label,
                    address,
                    from_options,
                } => (label, address, from_options),
                Sample::Unresolvable(reason) => {
                    report.info(
                        &protocol,
                        &template.id,
                        format!("not layout-monitored by this check: {reason}"),
                    );
                    continue;
                }
            };

            match check_sample(&surfnet_svm, template, idl, address, &label).await {
                Ok(surplus) => {
                    passes += 1;
                    covered += 1;
                    // The engine copies undescribed bytes back untouched, so surplus doesn't break
                    // the override — it just means a field the protocol added there is unreachable.
                    if surplus > 0 {
                        report.info(
                            &protocol,
                            &template.id,
                            format!(
                                "{address} carries {surplus} byte(s) past what the bundled IDL \
                                 describes for {}. Overrides still work, but those bytes are \
                                 outside the model.",
                                template.account_type
                            ),
                        );
                    }
                }
                Err(reason) => failures.push((reason, from_options)),
            }
        }

        // A template's own address failing means the integration is broken; one stale entry in a
        // constant list failing while siblings pass is just a stale market, not layout drift.
        for (reason, from_options) in &failures {
            if *from_options && passes > 0 {
                report.warn(
                    &protocol,
                    &template.id,
                    format!(
                        "{reason} Other options of this template do round-trip, so this is a \
                         stale entry in the list rather than the layout having moved."
                    ),
                );
            } else {
                report.error(&protocol, &template.id, reason);
            }
        }
    }

    if covered == 0 {
        report.error(
            "all",
            "coverage",
            "no template resolved to a live account, so this run proved nothing about any layout",
        );
    } else {
        report.info(
            "all",
            "coverage",
            format!("{covered} template/account pairs round-tripped against live mainnet"),
        );
    }
    report.finish();
}

#[test]
fn samples_come_from_the_template_itself() {
    let registry = TemplateRegistry::new();

    let literal = registry
        .get("whirlpool-sol-usdc")
        .expect("the SOL/USDC whirlpool template");
    let literal_samples = samples(literal);
    assert!(
        matches!(literal_samples.as_slice(), [Sample::Resolved { address, from_options: false, .. }]
            if address.to_string() == "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ"),
        "expected the template's own pool address, got {literal_samples:?}"
    );

    let constant_backed = registry
        .get("pyth-price-feed-v2")
        .expect("the Pyth price feed template");
    let constant_samples = samples(constant_backed);
    assert_eq!(
        constant_samples.len(),
        SAMPLES_PER_TEMPLATE,
        "a feed-backed PDA should yield one address per sampled feed, got {constant_samples:?}"
    );
    assert!(
        constant_samples
            .iter()
            .all(|sample| matches!(sample, Sample::Resolved { .. })),
        "every sampled feed should derive an address, got {constant_samples:?}"
    );
}

#[test]
fn a_declared_size_adds_up_from_the_type_graph() {
    let registry = TemplateRegistry::new();
    let template = registry
        .get("whirlpool-sol-usdc")
        .expect("the SOL/USDC whirlpool template");
    let idl = template.idl.as_ref().expect("an IDL-based template");
    let size = declared_body_size(idl, "Whirlpool")
        .expect("Whirlpool is built entirely from fixed-width fields");
    assert!(
        size > 200,
        "a Whirlpool body is several hundred bytes; the walk produced {size}"
    );
}

#[test]
fn a_protocol_written_for_people_still_matches_the_exclusion_list() {
    assert_eq!(slug("SPL Token"), "spl-token");
    assert_eq!(slug("spl-token"), "spl-token");
    let registry = TemplateRegistry::new();
    let spl = registry
        .all()
        .into_iter()
        .find(|template| slug(&template.protocol) == "spl-token")
        .expect("the SPL Token templates are loaded");
    assert!(
        NOT_BORSH
            .iter()
            .any(|(name, _)| *name == slug(&spl.protocol)),
        "SPL Token must reach its exclusion, or it is reported as an uncovered gap instead of a \
         deliberate one"
    );
}
