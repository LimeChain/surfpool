//! Compares each committed IDL with what its program publishes.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::{
    idl::{self, Format},
    idl_service::{ANCHOR, PROGRAM_METADATA, Published, PublishedIdls},
};

pub const PROTOCOLS_DIR: &str = "crates/core/src/scenarios/protocols";

/// A committed IDL and the program it describes.
pub struct Protocol {
    /// The IDL's folder under `protocols/`, such as `kamino/v1`.
    pub name: String,
    pub address: Option<String>,
    /// The committed `idl.json`, relative to the repository root.
    pub path: String,
    pub idl: Value,
}

impl Protocol {
    pub fn file_stem(&self) -> String {
        self.name.replace('/', "-")
    }
}

pub fn discover(root: &Path) -> Result<Vec<Protocol>> {
    let base = root.join(PROTOCOLS_DIR);
    let mut files = Vec::new();
    collect_idls(&base, &mut files)?;
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let idl: Value = serde_json::from_str(&fs::read_to_string(&file)?)
                .with_context(|| format!("{} is not JSON", file.display()))?;
            let address = idl
                .get("address")
                .or_else(|| idl.pointer("/metadata/address"))
                .and_then(Value::as_str);
            let folder = file
                .parent()
                .expect("a file has a folder")
                .strip_prefix(&base)?;
            Ok(Protocol {
                name: folder.to_string_lossy().replace('\\', "/"),
                address: address.map(str::to_owned),
                path: file
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                idl,
            })
        })
        .collect()
}

fn collect_idls(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_idls(&path, files)?;
        } else if path.file_name().is_some_and(|name| name == "idl.json") {
            files.push(path);
        }
    }
    Ok(())
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_text(path, &text)
}

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// One IDL a program publishes.
#[derive(Debug, Serialize)]
pub struct Upstream {
    pub version: Option<String>,
    pub format: Format,
}

/// How a committed IDL compares with what its program publishes.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    /// Everything the committed IDL ships is published the same way.
    Matches,
    /// The program publishes some shipped entries differently, or not at all.
    Differs {
        /// As `section/name`.
        entries: Vec<String>,
        /// The diff, relative to the output folder. The published IDL lies next to it.
        diff_file: String,
    },
    /// The program publishes no IDL to compare with.
    NotPublished,
    /// What the program publishes is not an Anchor IDL, so the two cannot be compared entry by
    /// entry.
    NotComparable { reason: String },
    /// The comparison could not be made: what the program publishes could not be read, or the
    /// committed IDL is not one Anchor accepts.
    Unchecked { error: String },
}

#[derive(Debug, Serialize)]
pub struct IdlResult {
    pub protocol: String,
    pub address: Option<String>,
    pub path: String,
    pub committed_version: Option<String>,
    /// What the program publishes, by source (`anchor` or `pmp`).
    pub upstream: BTreeMap<String, Upstream>,
    /// The source the committed IDL was compared with.
    pub compared_with: Option<String>,
    /// Why a source that publishes something could not be read, when another source could.
    pub unreadable: Vec<String>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Compares each protocol's committed IDL with what `latest` says its program publishes. For
/// every protocol that differs, writes the diff and the published IDL under `out/idls`.
pub fn check_idls(
    latest: &dyn Fn(&str) -> Result<PublishedIdls>,
    protocols: &[Protocol],
    out: &Path,
) -> Result<Vec<IdlResult>> {
    protocols
        .iter()
        .map(|protocol| {
            let mut result = IdlResult {
                protocol: protocol.name.clone(),
                address: protocol.address.clone(),
                path: protocol.path.clone(),
                committed_version: idl::version_of(&protocol.idl),
                upstream: BTreeMap::new(),
                compared_with: None,
                unreadable: Vec::new(),
                outcome: Outcome::NotPublished,
            };
            let Some(address) = &protocol.address else {
                result.outcome = Outcome::Unchecked {
                    error: "idl.json names no program address".into(),
                };
                return Ok(result);
            };
            let published = match latest(address) {
                Ok(published) => published,
                Err(error) => {
                    result.outcome = Outcome::Unchecked {
                        error: format!("{error:#}"),
                    };
                    return Ok(result);
                }
            };
            result.unreadable = published.unreadable.clone();
            let published: Vec<(&str, &Published)> = published
                .iter()
                .filter_map(|(source, published)| Some((source, published?)))
                .collect();
            for (source, published) in &published {
                let upstream = Upstream {
                    version: idl::version_of(&published.document),
                    format: Format::of(&published.document),
                };
                result.upstream.insert((*source).to_owned(), upstream);
            }
            if !published.is_empty() {
                let (source, outcome) = compare(protocol, address, &published, out)?;
                result.compared_with = source.map(str::to_owned);
                result.outcome = outcome;
            }
            Ok(result)
        })
        .collect()
}

/// The committed IDL against the one its program publishes, and which source that is.
///
/// Program Metadata is where a program publishes its IDL today, and the copy in its Anchor IDL
/// account is often left behind. So the Program Metadata copy is the one compared with, and the
/// Anchor one only where there is no other.
fn compare<'a>(
    protocol: &Protocol,
    address: &str,
    published: &[(&'a str, &Published)],
    out: &Path,
) -> Result<(Option<&'a str>, Outcome)> {
    let committed = match idl::parse(&protocol.idl, address) {
        Ok(committed) => committed,
        Err(error) => {
            let error = format!("the committed IDL is not one Anchor accepts: {error:#}");
            return Ok((None, Outcome::Unchecked { error }));
        }
    };
    let parsed: Vec<_> = published
        .iter()
        .filter_map(|(source, published)| {
            let parsed = idl::parse(&published.document, address).ok()?;
            Some((*source, *published, parsed))
        })
        .collect();
    let best = parsed
        .iter()
        .find(|(source, ..)| *source == PROGRAM_METADATA)
        .or_else(|| parsed.iter().find(|(source, ..)| *source == ANCHOR));
    let Some((source, published, parsed)) = best else {
        let formats: Vec<_> = published
            .iter()
            .map(|(_, published)| Format::of(&published.document).name())
            .collect();
        let reason = format!("published as {}", formats.join(" and "));
        return Ok((None, Outcome::NotComparable { reason }));
    };
    let comparison = idl::compare(&committed, parsed);
    let entries = comparison.differing();
    if entries.is_empty() {
        return Ok((Some(*source), Outcome::Matches));
    }
    let diff_file = format!("idls/{}.diff", protocol.file_stem());
    write_text(&out.join(&diff_file), &comparison.render_diff())?;
    // Exactly as published, to update the committed IDL from.
    let published_file = format!("idls/{}.{source}.json", protocol.file_stem());
    write_text(&out.join(published_file), &published.raw)?;
    Ok((Some(*source), Outcome::Differs { entries, diff_file }))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;

    const ADDRESS: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

    fn idl(slot_type: &str) -> Value {
        json!({
            "address": ADDRESS,
            "metadata": {"name": "demo", "version": "1.0.0", "spec": "0.1.0"},
            "instructions": [],
            "types": [{"name": "LastUpdate", "type": {"kind": "struct", "fields": [
                {"name": "slot", "type": slot_type},
            ]}}],
        })
    }

    fn anchor(document: Value) -> Result<PublishedIdls> {
        let published = Published {
            raw: document.to_string(),
            document,
        };
        Ok(PublishedIdls {
            anchor: Some(published),
            ..Default::default()
        })
    }

    fn published(document: Value) -> Option<Published> {
        Some(Published {
            raw: document.to_string(),
            document,
        })
    }

    /// A program that publishes one IDL in its Anchor IDL account and another in Program Metadata.
    fn both(anchor: Value, program_metadata: Value) -> Result<PublishedIdls> {
        Ok(PublishedIdls {
            anchor: published(anchor),
            pmp: published(program_metadata),
            ..Default::default()
        })
    }

    /// Checks one protocol that commits `committed` against what the program is said to publish.
    fn check_committed(committed: Value, answer: Result<PublishedIdls>, out: &Path) -> IdlResult {
        let protocols = [Protocol {
            name: "demo/v1".into(),
            address: Some(ADDRESS.into()),
            path: "protocols/demo/v1/idl.json".into(),
            idl: committed,
        }];
        let answer = RefCell::new(Some(answer));
        let latest = |_: &str| answer.borrow_mut().take().unwrap();
        check_idls(&latest, &protocols, out).unwrap().remove(0)
    }

    fn check(answer: Result<PublishedIdls>, out: &Path) -> IdlResult {
        check_committed(idl("u64"), answer, out)
    }

    #[test]
    fn the_same_idl_matches_and_writes_nothing() {
        let out = tempfile::tempdir().unwrap();
        let result = check(anchor(idl("u64")), out.path());
        assert_eq!(result.outcome, Outcome::Matches);
        assert_eq!(result.compared_with.as_deref(), Some("anchor"));
        assert!(!out.path().join("idls").exists());
    }

    #[test]
    fn a_different_idl_is_reported_with_its_diff_and_the_published_copy() {
        let out = tempfile::tempdir().unwrap();
        let published = idl("u128");
        let result = check(anchor(published.clone()), out.path());
        let Outcome::Differs { entries, diff_file } = result.outcome else {
            panic!("{:?}", result.outcome);
        };
        assert_eq!(entries, ["types/LastUpdate"]);
        let diff = fs::read_to_string(out.path().join(diff_file)).unwrap();
        assert!(diff.contains("\"u64\""), "{diff}");
        assert!(diff.contains("\"u128\""), "{diff}");
        let copy = fs::read_to_string(out.path().join("idls/demo-v1.anchor.json")).unwrap();
        assert_eq!(copy, published.to_string());
    }

    #[test]
    fn program_metadata_is_compared_with_before_the_anchor_idl_account() {
        // The Anchor IDL account still holds what the committed IDL was made from.
        let out = tempfile::tempdir().unwrap();
        let result = check(both(idl("u64"), idl("u128")), out.path());
        assert_eq!(result.compared_with.as_deref(), Some("pmp"));
        assert!(matches!(result.outcome, Outcome::Differs { .. }));
        assert!(out.path().join("idls/demo-v1.pmp.json").exists());
    }

    #[test]
    fn a_program_replaced_by_another_no_longer_publishes_what_is_committed() {
        // The Anchor IDL account still describes the program that used to live at the address.
        let out = tempfile::tempdir().unwrap();
        let mut replacement = idl("u64");
        replacement["types"] = json!([]);
        let result = check(both(idl("u64"), replacement), out.path());
        assert_eq!(result.compared_with.as_deref(), Some("pmp"));
        let Outcome::Differs { entries, diff_file } = result.outcome else {
            panic!("{:?}", result.outcome);
        };
        assert_eq!(entries, ["types/LastUpdate"]);
        let diff = fs::read_to_string(out.path().join(diff_file)).unwrap();
        assert!(diff.contains("no longer published"), "{diff}");
    }

    #[test]
    fn a_program_that_publishes_nothing_has_nothing_to_compare_with() {
        let out = tempfile::tempdir().unwrap();
        let result = check(Ok(PublishedIdls::default()), out.path());
        assert_eq!(result.outcome, Outcome::NotPublished);
        assert!(result.upstream.is_empty());
    }

    #[test]
    fn a_codama_document_is_not_comparable() {
        let out = tempfile::tempdir().unwrap();
        let codama = json!({"kind": "rootNode", "program": {"version": "3.3.0"}});
        let result = check(anchor(codama), out.path());
        let reason = "published as codama".to_owned();
        assert_eq!(result.outcome, Outcome::NotComparable { reason });
        assert_eq!(result.upstream["anchor"].version.as_deref(), Some("3.3.0"));
    }

    #[test]
    fn a_failed_fetch_is_unchecked_and_keeps_its_cause() {
        let out = tempfile::tempdir().unwrap();
        let failure = anyhow::anyhow!("expected value at line 1").context("IDL service: not JSON");
        let result = check(Err(failure), out.path());
        let error = "IDL service: not JSON: expected value at line 1".to_owned();
        assert_eq!(result.outcome, Outcome::Unchecked { error });
    }

    #[test]
    fn a_committed_idl_anchor_does_not_accept_is_unchecked() {
        let out = tempfile::tempdir().unwrap();
        let committed = json!({"metadata": {"spec": "9.9.9"}, "instructions": []});
        let result = check_committed(committed, anchor(idl("u64")), out.path());
        let Outcome::Unchecked { error } = result.outcome else {
            panic!("{:?}", result.outcome);
        };
        assert!(
            error.starts_with("the committed IDL is not one Anchor accepts"),
            "{error}"
        );
    }

    #[test]
    fn a_source_that_could_not_be_read_is_noted_next_to_the_comparison() {
        let out = tempfile::tempdir().unwrap();
        let mut published = anchor(idl("u64")).unwrap();
        published.unreadable = vec!["the pmp IDL does not decode".into()];
        let result = check(Ok(published), out.path());
        assert_eq!(result.outcome, Outcome::Matches);
        assert_eq!(result.unreadable, ["the pmp IDL does not decode"]);
    }
}
