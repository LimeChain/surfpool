//! The IDL check, and the state each run hands to the next.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anchor_lang_idl::types::Idl;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    idl::{self, Format},
    idl_service::PublishedIdls,
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

pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid state", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
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

/// Copies a previous state file into the next state, so a failed read loses nothing.
fn carry(from: &Path, to: &Path) -> Result<()> {
    if from.exists() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IdlState {
    pub address: String,
    pub sources: BTreeMap<String, SourceState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceState {
    /// Of the text exactly as published.
    pub hash: String,
    pub version: Option<String>,
    pub format: Format,
    /// The last copy of the published IDL, relative to the state folder.
    pub file: String,
    pub first_seen: String,
    pub changed_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceStatus {
    Baseline,
    Unchanged,
    Changed,
    /// Published before and absent from this run's answer; its last copy stays the baseline.
    Missing,
}

#[derive(Debug, Serialize)]
pub struct SourceResult {
    pub status: SourceStatus,
    pub version: Option<String>,
    pub format: Option<Format>,
}

/// A source whose IDL changed since the previous run.
#[derive(Debug, Serialize)]
pub struct Event {
    pub source: String,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub to_hash: Option<String>,
    /// Whether both copies are Anchor IDLs, compared entry by entry.
    pub comparable: bool,
    /// Shipped entries that changed or disappeared, as `section/name`.
    pub breaking: Vec<String>,
    pub added: usize,
    pub changed_elsewhere: usize,
    pub attention: bool,
    pub note: Option<String>,
    pub diff_file: Option<String>,
}

/// What the committed IDL ships that the program no longer publishes the same way.
#[derive(Debug, Serialize)]
pub struct Gap {
    pub comparable: bool,
    pub formats: Vec<Format>,
    pub source: Option<String>,
    pub version: Option<String>,
    pub breaking: Vec<String>,
    pub added: usize,
    pub diff_file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IdlResult {
    pub protocol: String,
    pub address: Option<String>,
    pub path: String,
    pub committed_version: Option<String>,
    pub sources: BTreeMap<String, SourceResult>,
    pub events: Vec<Event>,
    pub gap: Option<Gap>,
    pub error: Option<String>,
}

pub struct IdlCheck {
    pub results: Vec<IdlResult>,
    pub state: BTreeMap<String, IdlState>,
}

/// Compares each protocol's published IDLs with the previous run and with the committed IDL.
/// Writes the next state's IDL copies under `out/state` and changed IDLs under `out/report`, each
/// exactly as published.
pub fn check_idls(
    latest: &dyn Fn(&str) -> Result<PublishedIdls>,
    protocols: &[Protocol],
    previous: &BTreeMap<String, IdlState>,
    previous_dir: &Path,
    out: &Path,
    now: &str,
) -> Result<IdlCheck> {
    let mut check = IdlCheck {
        results: Vec::new(),
        state: BTreeMap::new(),
    };
    for protocol in protocols {
        let before = previous.get(&protocol.name);
        let mut result = IdlResult {
            protocol: protocol.name.clone(),
            address: protocol.address.clone(),
            path: protocol.path.clone(),
            committed_version: idl::version_of(&protocol.idl),
            sources: BTreeMap::new(),
            events: Vec::new(),
            gap: None,
            error: None,
        };
        let Some(address) = &protocol.address else {
            result.error = Some("idl.json names no program address".into());
            check.results.push(result);
            continue;
        };
        let published = match latest(address) {
            Ok(published) => published,
            Err(error) => {
                result.error = Some(error.to_string());
                if let Some(before) = before {
                    check.state.insert(protocol.name.clone(), before.clone());
                    for source in before.sources.values() {
                        carry(
                            &previous_dir.join(&source.file),
                            &out.join("state").join(&source.file),
                        )?;
                    }
                    let gap_file = format!("idls/{}.gap.diff", protocol.file_stem());
                    carry(
                        &previous_dir.join(&gap_file),
                        &out.join("state").join(&gap_file),
                    )?;
                }
                check.results.push(result);
                continue;
            }
        };
        let committed = idl::parse(&protocol.idl, address).ok();

        let mut sources = BTreeMap::new();
        let mut missing = Vec::new();
        for (source, published) in published.iter() {
            let known = before.and_then(|before| before.sources.get(source));
            let Some(published) = published else {
                if let Some(known) = known {
                    // An IDL that was withdrawn and one the service missed for a run look the
                    // same, so the last copy stays the baseline and the run counts as unverified.
                    carry(
                        &previous_dir.join(&known.file),
                        &out.join("state").join(&known.file),
                    )?;
                    result.sources.insert(
                        source.into(),
                        SourceResult {
                            status: SourceStatus::Missing,
                            version: known.version.clone(),
                            format: Some(known.format),
                        },
                    );
                    sources.insert(source.to_owned(), known.clone());
                    missing.push(source);
                }
                continue;
            };
            let hash = idl::content_hash(&published.raw);
            let file = format!("idls/{}.{source}.json", protocol.file_stem());
            write_text(&out.join("state").join(&file), &published.raw)?;
            let status = match known {
                None => SourceStatus::Baseline,
                Some(known) if known.hash == hash => SourceStatus::Unchanged,
                Some(known) => {
                    let previous_copy = read_copy(previous_dir, known)?;
                    let (mut event, diff) = changed(
                        source,
                        address,
                        previous_copy.as_ref(),
                        &published.document,
                        committed.as_ref(),
                    );
                    event.to_hash = Some(hash.clone());
                    write_text(&out.join("report").join(&file), &published.raw)?;
                    if !diff.is_empty() {
                        let diff_file = format!("idls/{}.{source}.diff", protocol.file_stem());
                        write_text(&out.join("report").join(&diff_file), &diff)?;
                        event.diff_file = Some(diff_file);
                    }
                    result.events.push(event);
                    SourceStatus::Changed
                }
            };
            let state = SourceState {
                hash,
                version: idl::version_of(&published.document),
                format: Format::of(&published.document),
                file,
                first_seen: known.map_or_else(|| now.to_owned(), |known| known.first_seen.clone()),
                changed_at: match (status, known) {
                    (SourceStatus::Unchanged, Some(known)) => known.changed_at.clone(),
                    _ => now.to_owned(),
                },
            };
            result.sources.insert(
                source.into(),
                SourceResult {
                    status,
                    version: state.version.clone(),
                    format: Some(state.format),
                },
            );
            sources.insert(source.to_owned(), state);
        }

        if !missing.is_empty() {
            result.error = Some(format!(
                "the service returned no {} IDL, which it did before",
                missing.join(" or ")
            ));
        }
        let gap_file = format!("idls/{}.gap.diff", protocol.file_stem());
        if published.iter().any(|(_, published)| published.is_some()) {
            let (mut gap, diff) = gap(committed.as_ref(), &published, address);
            if !diff.is_empty() {
                write_text(&out.join("state").join(&gap_file), &diff)?;
                gap.diff_file = Some(gap_file);
            }
            result.gap = Some(gap);
        } else {
            carry(
                &previous_dir.join(&gap_file),
                &out.join("state").join(&gap_file),
            )?;
        }
        check.state.insert(
            protocol.name.clone(),
            IdlState {
                address: address.clone(),
                sources,
            },
        );
        check.results.push(result);
    }
    Ok(check)
}

fn read_copy(previous_dir: &Path, known: &SourceState) -> Result<Option<Value>> {
    let path = previous_dir.join(&known.file);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&text).with_context(|| {
        format!("{} is not JSON", path.display())
    })?))
}

/// The event and its diff for a source whose IDL changed.
fn changed(
    source: &str,
    address: &str,
    before: Option<&Value>,
    after: &Value,
    committed: Option<&Idl>,
) -> (Event, String) {
    let mut event = Event {
        source: source.into(),
        from_version: before.and_then(idl::version_of),
        to_version: idl::version_of(after),
        to_hash: None,
        comparable: false,
        breaking: Vec::new(),
        added: 0,
        changed_elsewhere: 0,
        attention: true,
        note: None,
        diff_file: None,
    };
    let Some(before) = before else {
        event.note = Some("the previous copy is missing from the state".into());
        return (event, String::new());
    };
    let (Ok(old), Ok(new), Some(committed)) = (
        idl::parse(before, address),
        idl::parse(after, address),
        committed,
    ) else {
        return (
            event,
            idl::render_text_diff(before, after, "previous", "latest"),
        );
    };
    let comparison = idl::compare(&old, &new, Some(committed));
    event.comparable = true;
    event.breaking = comparison.breaking();
    event.added = comparison.added();
    event.changed_elsewhere = comparison.changed_elsewhere();
    // A change outside what Surfpool ships is reported, not raised.
    event.attention = !event.breaking.is_empty();
    (event, comparison.render_diff("previous", "latest"))
}

/// The committed IDL against the published source that carries most of what it ships.
fn gap(committed: Option<&Idl>, published: &PublishedIdls, address: &str) -> (Gap, String) {
    let present: Vec<_> = published
        .iter()
        .filter_map(|(source, published)| Some((source, published?)))
        .collect();
    let mut formats: Vec<Format> = present
        .iter()
        .map(|(_, published)| Format::of(&published.document))
        .collect();
    formats.dedup();
    let not_comparable = Gap {
        comparable: false,
        formats,
        source: None,
        version: None,
        breaking: Vec::new(),
        added: 0,
        diff_file: None,
    };
    let Some(committed) = committed else {
        return (not_comparable, String::new());
    };
    let best = present
        .into_iter()
        .filter_map(|(source, published)| {
            Some((
                source,
                published,
                idl::parse(&published.document, address).ok()?,
            ))
        })
        .max_by_key(|(_, published, parsed)| {
            (
                idl::coverage(committed, parsed),
                Format::of(&published.document) == Format::Anchor,
            )
        });
    let Some((source, published, parsed)) = best else {
        return (not_comparable, String::new());
    };
    let comparison = idl::compare(committed, &parsed, None);
    let gap = Gap {
        comparable: true,
        formats: vec![Format::of(&published.document)],
        source: Some(source.into()),
        version: idl::version_of(&published.document),
        breaking: comparison.breaking(),
        added: comparison.added(),
        diff_file: None,
    };
    (
        gap,
        comparison.render_diff("committed", &format!("upstream {source}")),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;
    use crate::idl_service::Published;

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
            pmp: None,
        })
    }

    /// Runs the check once per answer, each run starting from the state the one before left.
    fn runs(answers: Vec<Result<PublishedIdls>>) -> Vec<IdlResult> {
        let dir = tempfile::tempdir().unwrap();
        let protocols = [Protocol {
            name: "demo/v1".into(),
            address: Some(ADDRESS.into()),
            path: "protocols/demo/v1/idl.json".into(),
            idl: idl("u64"),
        }];
        let mut state = BTreeMap::new();
        let mut previous_dir = dir.path().join("none");
        let mut results = Vec::new();
        for (run, answer) in answers.into_iter().enumerate() {
            let answer = RefCell::new(Some(answer));
            let latest = |_: &str| answer.borrow_mut().take().unwrap();
            let out = dir.path().join(run.to_string());
            let check =
                check_idls(&latest, &protocols, &state, &previous_dir, &out, "now").unwrap();
            state = check.state;
            previous_dir = out.join("state");
            results.extend(check.results);
        }
        results
    }

    #[test]
    fn an_upstream_change_to_a_shipped_entry_is_raised_once() {
        let results = runs(vec![
            anchor(idl("u64")),
            anchor(idl("u128")),
            anchor(idl("u128")),
        ]);
        assert_eq!(results[0].sources["anchor"].status, SourceStatus::Baseline);
        assert!(results[0].events.is_empty());
        assert_eq!(results[1].events[0].breaking, ["types/LastUpdate"]);
        assert!(results[1].events[0].attention);
        assert_eq!(results[2].sources["anchor"].status, SourceStatus::Unchanged);
        assert!(results[2].events.is_empty());
    }

    #[test]
    fn a_source_missing_for_one_run_keeps_its_baseline() {
        let results = runs(vec![
            anchor(idl("u64")),
            Ok(PublishedIdls::default()),
            anchor(idl("u128")),
        ]);
        // Nothing is raised for the run without an answer; it only counts as unverified.
        assert_eq!(results[1].sources["anchor"].status, SourceStatus::Missing);
        assert!(results[1].events.is_empty());
        assert!(results[1].error.is_some());
        // The change that happened meanwhile is still found against the last copy.
        assert_eq!(results[2].events[0].breaking, ["types/LastUpdate"]);
    }

    #[test]
    fn a_failed_fetch_keeps_the_baseline() {
        let results = runs(vec![
            anchor(idl("u64")),
            Err(anyhow::anyhow!("IDL service: HTTP 500")),
            anchor(idl("u128")),
        ]);
        assert_eq!(results[1].error.as_deref(), Some("IDL service: HTTP 500"));
        assert_eq!(results[2].events[0].breaking, ["types/LastUpdate"]);
    }
}
