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
    idl_service::{IdlService, PublishedIdls},
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
    Withdrawn,
}

#[derive(Debug, Serialize)]
pub struct SourceResult {
    pub status: SourceStatus,
    pub version: Option<String>,
    pub format: Option<Format>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Changed,
    Withdrawn,
}

/// A source whose IDL changed since the previous run.
#[derive(Debug, Serialize)]
pub struct Event {
    pub source: String,
    pub kind: EventKind,
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
    service: &IdlService,
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
        let published = match service.latest(address) {
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
        for (source, published) in published.iter() {
            let known = before.and_then(|before| before.sources.get(source));
            let Some(published) = published else {
                if let Some(known) = known {
                    let previous_copy = read_copy(previous_dir, known)?;
                    result.events.push(event(
                        source,
                        EventKind::Withdrawn,
                        previous_copy.as_ref(),
                        None,
                    ));
                    result.sources.insert(
                        source.into(),
                        SourceResult {
                            status: SourceStatus::Withdrawn,
                            version: None,
                            format: None,
                        },
                    );
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

        if !sources.is_empty() {
            let (mut gap, diff) = gap(committed.as_ref(), &published, address);
            if !diff.is_empty() {
                let diff_file = format!("idls/{}.gap.diff", protocol.file_stem());
                write_text(&out.join("state").join(&diff_file), &diff)?;
                gap.diff_file = Some(diff_file);
            }
            result.gap = Some(gap);
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

fn event(source: &str, kind: EventKind, before: Option<&Value>, after: Option<&Value>) -> Event {
    Event {
        source: source.into(),
        kind,
        from_version: before.and_then(idl::version_of),
        to_version: after.and_then(idl::version_of),
        to_hash: None,
        comparable: false,
        breaking: Vec::new(),
        added: 0,
        changed_elsewhere: 0,
        attention: true,
        note: None,
        diff_file: None,
    }
}

/// The event and its diff for a source whose IDL changed.
fn changed(
    source: &str,
    address: &str,
    before: Option<&Value>,
    after: &Value,
    committed: Option<&Idl>,
) -> (Event, String) {
    let mut event = event(source, EventKind::Changed, before, Some(after));
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
