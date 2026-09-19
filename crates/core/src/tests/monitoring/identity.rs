//! Upgrade detection: is the deployed bytecode still the one we reverse-engineered against?
//!
//! This is the only check that covers every integration, IDL or not. A raw layout's offsets, a
//! bundled IDL's field order and a behavioural test's expected swap output are all claims about
//! one specific build of one specific program. When that build changes, all of them are
//! unverified until someone re-establishes them, which is why any difference here is an error
//! rather than a warning.

use std::{collections::BTreeMap, str::FromStr};

use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_loader_v3_interface::{get_program_data_address, state::UpgradeableLoaderState};
use solana_pubkey::Pubkey;

use super::{Report, live, monitored_programs};
use crate::scenarios::TemplateRegistry;

const CHECK: &str = "program-identity";

const BASELINE: &str = include_str!("baseline.json");

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Baseline {
    pub programs: Vec<BaselineEntry>,
}

/// One accepted deployment. `reviewed_on` and `note` are the audit trail: they say who last
/// looked at this program and why the current bytecode is considered integrated.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct BaselineEntry {
    pub program_id: String,
    pub label: String,
    #[serde(default)]
    pub loader: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_deployed_slot: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_authority: Option<String>,
    /// Empty until a run has observed the program and a human has copied the result back. An
    /// entry with only `program_id` and `label` is how a protocol without an IDL is put on the
    /// list; the first run reports the fingerprint to pin.
    #[serde(default)]
    pub elf_sha256: String,
    #[serde(default)]
    pub elf_len: usize,
    #[serde(default)]
    pub reviewed_on: String,
    #[serde(default)]
    pub note: String,
}

pub fn baseline() -> Baseline {
    serde_json::from_str(BASELINE).expect("the committed baseline is valid JSON")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observed {
    pub loader: String,
    pub last_deployed_slot: Option<u64>,
    pub upgrade_authority: Option<String>,
    pub elf_sha256: String,
    pub elf_len: usize,
}

impl Observed {
    fn into_entry(self, program_id: &Pubkey, label: String) -> BaselineEntry {
        BaselineEntry {
            program_id: program_id.to_string(),
            label,
            loader: self.loader,
            last_deployed_slot: self.last_deployed_slot,
            upgrade_authority: self.upgrade_authority,
            elf_sha256: self.elf_sha256,
            elf_len: self.elf_len,
            reviewed_on: "UNREVIEWED".to_string(),
            note: String::new(),
        }
    }
}

/// Hashes the executable bytes with the trailing zero padding removed, so the digest tracks the
/// code rather than how much room the deployer allocated for it.
fn hash_executable(bytes: &[u8]) -> (String, usize) {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    let executable = &bytes[..end];
    let digest = Sha256::digest(executable);
    (hex::encode(digest), executable.len())
}

/// Reads a program's current deployment. Returns `Err` with an explanation when the program is
/// missing or held by a loader this check does not understand.
pub async fn observe(program_id: &Pubkey) -> Result<Observed, String> {
    let Some(program) = live::try_fetch(&[*program_id]).await.remove(0) else {
        return Err("the program account does not exist on mainnet".to_string());
    };

    if !program.executable {
        return Err(format!(
            "the account is owned by {} and is not executable, so it is not a program",
            program.owner
        ));
    }

    match bincode::deserialize::<UpgradeableLoaderState>(&program.data) {
        Ok(UpgradeableLoaderState::Program { .. }) => observe_upgradeable(program_id).await,
        _ => Ok(observe_fixed(&program)),
    }
}

async fn observe_upgradeable(program_id: &Pubkey) -> Result<Observed, String> {
    let address = get_program_data_address(program_id);
    let Some(program_data) = live::try_fetch(&[address]).await.remove(0) else {
        return Err(format!("the program data account {address} does not exist"));
    };

    let header = UpgradeableLoaderState::size_of_programdata_metadata();
    if program_data.data.len() < header {
        return Err(format!(
            "the program data account {address} is {} bytes, shorter than its {header}-byte header",
            program_data.data.len()
        ));
    }

    let (slot, upgrade_authority) =
        match bincode::deserialize::<UpgradeableLoaderState>(&program_data.data[..header]) {
            Ok(UpgradeableLoaderState::ProgramData {
                slot,
                upgrade_authority_address,
            }) => (slot, upgrade_authority_address),
            other => {
                return Err(format!(
                    "the program data account {address} did not decode as ProgramData: {other:?}"
                ));
            }
        };

    let (elf_sha256, elf_len) = hash_executable(&program_data.data[header..]);
    Ok(Observed {
        loader: "v3".to_string(),
        last_deployed_slot: Some(slot),
        upgrade_authority: upgrade_authority.map(|key| key.to_string()),
        elf_sha256,
        elf_len,
    })
}

/// A program under a loader with no separate program data account: the executable is the account.
fn observe_fixed(program: &Account) -> Observed {
    let (elf_sha256, elf_len) = hash_executable(&program.data);
    Observed {
        loader: format!("fixed:{}", program.owner),
        last_deployed_slot: None,
        upgrade_authority: None,
        elf_sha256,
        elf_len,
    }
}

#[tokio::test]
async fn programs_match_the_pinned_deployment_baseline() {
    let registry = TemplateRegistry::new();
    let baseline = baseline();
    let monitored = monitored_programs(&registry, &baseline);
    let pinned: BTreeMap<String, &BaselineEntry> = baseline
        .programs
        .iter()
        .map(|entry| (entry.program_id.clone(), entry))
        .collect();

    let mut report = Report::new(CHECK);

    for (program_id, protocols) in &monitored {
        let protocol = protocols
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
            .to_lowercase();
        let key = program_id.to_string();

        let observed = match observe(program_id).await {
            Ok(observed) => observed,
            Err(reason) => {
                report.error(&protocol, &key, reason);
                continue;
            }
        };
        // Every finding below carries the deployment as it is now, in the shape of a baseline
        // entry, so accepting it is copying that block into baseline.json and filling in the
        // review fields. Nothing here writes the file: that edit is the claim that somebody looked.
        let as_entry =
            serde_json::to_value(observed.clone().into_entry(program_id, protocol.clone()))
                .unwrap_or_default();

        let Some(entry) = pinned
            .get(&key)
            .filter(|entry| !entry.elf_sha256.is_empty())
        else {
            report.push_with(
                super::Severity::Error,
                &protocol,
                &key,
                format!(
                    "no fingerprint pinned; the deployment at slot {:?} has never been \
                     reviewed. Copy the observed entry below into baseline.json.",
                    observed.last_deployed_slot
                ),
                as_entry,
            );
            continue;
        };

        if entry.elf_sha256 != observed.elf_sha256 {
            report.push_with(
                super::Severity::Error,
                &protocol,
                &key,
                format!(
                    "the deployed bytecode changed: {} -> {} (slot {:?} -> {:?}, baseline \
                     reviewed {}). Every offset, IDL field order and behavioural expectation \
                     for this protocol is unverified until it is re-established.",
                    short(&entry.elf_sha256),
                    short(&observed.elf_sha256),
                    entry.last_deployed_slot,
                    observed.last_deployed_slot,
                    entry.reviewed_on
                ),
                as_entry.clone(),
            );
        } else if entry.last_deployed_slot != observed.last_deployed_slot {
            // Identical code at a new slot: a redeploy of the same build, or a program extend.
            report.push_with(
                super::Severity::Warn,
                &protocol,
                &key,
                format!(
                    "redeployed at slot {:?} (was {:?}) with identical bytecode",
                    observed.last_deployed_slot, entry.last_deployed_slot
                ),
                as_entry.clone(),
            );
        }

        if entry.upgrade_authority != observed.upgrade_authority {
            report.push_with(
                super::Severity::Warn,
                &protocol,
                &key,
                format!(
                    "the upgrade authority changed: {:?} -> {:?}",
                    entry.upgrade_authority, observed.upgrade_authority
                ),
                as_entry,
            );
        }
    }

    let loaded: std::collections::BTreeSet<String> = registry
        .all()
        .iter()
        .map(|template| super::slug(&template.protocol))
        .collect();

    // A protocol whose templates carry no program id - every raw layout - is watched only once
    // someone pins its program. Until then, say so every run rather than letting it sit unwatched.
    let watched: std::collections::BTreeSet<String> = monitored
        .values()
        .flatten()
        .map(|protocol| super::slug(protocol))
        .collect();
    for protocol in &loaded {
        if !watched.contains(protocol) {
            report.warn(
                protocol,
                "coverage",
                "templates are loaded but no program is pinned for this protocol, so an upgrade to \
                 it would go unnoticed; add its program to baseline.json with a `label` naming the \
                 protocol",
            );
        }
    }

    for entry in &baseline.programs {
        let referenced = entry
            .label
            .split(',')
            .any(|name| loaded.contains(&super::slug(name)));
        if !referenced {
            report.info(
                &entry.label,
                &entry.program_id,
                "pinned in the baseline but no loaded template belongs to this protocol; either \
                 the protocol has not landed yet or the entry can be dropped",
            );
        }
    }

    report.finish();
}

fn short(digest: &str) -> String {
    digest.chars().take(12).collect()
}

#[test]
fn hashing_ignores_the_allocation_padding() {
    let (bare, bare_len) = hash_executable(&[1, 2, 3]);
    let (padded, padded_len) = hash_executable(&[1, 2, 3, 0, 0, 0, 0]);
    assert_eq!(bare, padded, "trailing zeros must not change the digest");
    assert_eq!((bare_len, padded_len), (3, 3));
}

#[test]
fn the_committed_baseline_parses_and_is_unique() {
    let baseline = baseline();
    let mut seen = BTreeMap::new();
    for entry in &baseline.programs {
        Pubkey::from_str(&entry.program_id)
            .unwrap_or_else(|_| panic!("{} is not a pubkey", entry.program_id));
        assert!(
            seen.insert(entry.program_id.clone(), &entry.label)
                .is_none(),
            "{} appears twice in the baseline",
            entry.program_id
        );
        // An entry without a fingerprint is a request to be watched, not a claim about a
        // deployment, so it needs no review date. Once pinned, it does.
        assert!(
            entry.elf_sha256.is_empty()
                || (!entry.reviewed_on.is_empty() && entry.reviewed_on != "UNREVIEWED"),
            "{} ({}) is pinned in the baseline without a review date; a pinned deployment nobody \
             reviewed is not a baseline",
            entry.program_id,
            entry.label
        );
    }
}
