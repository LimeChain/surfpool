//! Compares Anchor IDLs entry by entry.
//!
//! Every IDL, committed or published, goes through Anchor's own converter into the current
//! Anchor format first, so two documents describing the same program compare equal whichever
//! format each was published in. Codama documents do not convert and are only diffed as text.
//!
//! IDLs of one program spell its names differently (`sharedAccountsRoute` and
//! `shared_accounts_route`), so a shipped entry with no namesake is looked up without regard to
//! case and underscores. That only finds the entry: next to a copy spelled differently it still
//! compares as changed.

use std::collections::{BTreeMap, BTreeSet};

use anchor_lang_idl::{convert::convert_idl, types::Idl};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use similar::TextDiff;

pub const SECTIONS: [&str; 5] = ["accounts", "types", "instructions", "events", "errors"];

/// The format a document was published in, for the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    Anchor,
    AnchorLegacy,
    Codama,
    Unknown,
}

impl Format {
    pub fn of(document: &Value) -> Self {
        if document.get("kind").and_then(Value::as_str) == Some("rootNode") {
            Format::Codama
        } else if document.pointer("/metadata/spec").is_some() {
            Format::Anchor
        } else if document.get("instructions").is_some() {
            Format::AnchorLegacy
        } else {
            Format::Unknown
        }
    }
}

pub fn version_of(document: &Value) -> Option<String> {
    let version = match Format::of(document) {
        Format::Codama => document.pointer("/program/version"),
        _ => document
            .pointer("/metadata/version")
            .or_else(|| document.get("version")),
    };
    version.and_then(Value::as_str).map(str::to_owned)
}

fn strip_docs(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| key.as_str() != "docs")
                .map(|(key, item)| (key.clone(), strip_docs(item)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_docs).collect()),
        other => other.clone(),
    }
}

/// Changes whenever the published document changes at all.
pub fn content_hash(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// The document in the current Anchor format, without documentation, or an error for anything
/// that is not an Anchor IDL. Documentation is no part of a layout, and the committed IDLs keep
/// docs the published ones drop (and the other way round).
pub fn parse(document: &Value, address: &str) -> Result<Idl> {
    let mut document = strip_docs(document);
    // The converter reads an older-format IDL's program address from `metadata`, where those
    // IDLs often leave it out.
    if Format::of(&document) == Format::AnchorLegacy
        && document.pointer("/metadata/address").is_none()
        && let Some(object) = document.as_object_mut()
    {
        let metadata = object.entry("metadata").or_insert_with(|| json!({}));
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.insert("address".into(), json!(address));
        }
    }
    convert_idl(&serde_json::to_vec(&document)?)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SectionSummary {
    pub shipped: usize,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub added: usize,
    pub changed_elsewhere: usize,
}

struct ChangedEntry {
    section: &'static str,
    name: String,
    before: String,
    after: String,
}

#[derive(Default)]
pub struct Comparison {
    pub sections: BTreeMap<&'static str, SectionSummary>,
    entries: Vec<ChangedEntry>,
}

/// Compares two IDLs over the entries `scope` ships (`before` when `None`).
pub fn compare(before: &Idl, after: &Idl, scope: Option<&Idl>) -> Comparison {
    let scope = scope.unwrap_or(before);
    let mut comparison = Comparison::default();
    comparison.section(
        "accounts",
        &before.accounts,
        &after.accounts,
        &scope.accounts,
        |e| &e.name,
    );
    comparison.section("types", &before.types, &after.types, &scope.types, |e| {
        &e.name
    });
    comparison.section(
        "instructions",
        &before.instructions,
        &after.instructions,
        &scope.instructions,
        |e| &e.name,
    );
    comparison.section(
        "events",
        &before.events,
        &after.events,
        &scope.events,
        |e| &e.name,
    );
    comparison.section(
        "errors",
        &before.errors,
        &after.errors,
        &scope.errors,
        |e| &e.name,
    );
    comparison
}

/// How many of the entries `committed` ships `published` has at all.
pub fn coverage(committed: &Idl, published: &Idl) -> usize {
    fn present<T>(shipped: &[T], published: &[T], name: fn(&T) -> &String) -> usize {
        let published = by_name(published, shipped, name);
        shipped
            .iter()
            .filter(|entry| published.contains_key(name(entry).as_str()))
            .count()
    }
    present(&committed.accounts, &published.accounts, |e| &e.name)
        + present(&committed.types, &published.types, |e| &e.name)
        + present(&committed.instructions, &published.instructions, |e| {
            &e.name
        })
        + present(&committed.events, &published.events, |e| &e.name)
        + present(&committed.errors, &published.errors, |e| &e.name)
}

/// A name without its spelling: its letters and digits, in lower case.
fn unspelled(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The entries by name. An entry `scope` spells differently is filed under the scope's spelling,
/// unless the entries hold that very name as well: some IDLs publish two entries that differ in
/// case alone.
fn by_name<'a, T>(
    entries: &'a [T],
    scope: &'a [T],
    name: fn(&T) -> &String,
) -> BTreeMap<&'a str, &'a T> {
    let own: BTreeSet<&str> = entries.iter().map(|entry| name(entry).as_str()).collect();
    let spellings: BTreeMap<String, &str> = scope
        .iter()
        .map(|entry| (unspelled(name(entry)), name(entry).as_str()))
        .collect();
    entries
        .iter()
        .map(|entry| {
            let own_name = name(entry).as_str();
            match spellings.get(&unspelled(own_name)) {
                Some(spelling) if !own.contains(spelling) => (*spelling, entry),
                _ => (own_name, entry),
            }
        })
        .collect()
}

impl Comparison {
    fn section<T: PartialEq + Serialize>(
        &mut self,
        section: &'static str,
        before: &[T],
        after: &[T],
        scope: &[T],
        name: fn(&T) -> &String,
    ) {
        let (old, new, shipped) = (
            by_name(before, scope, name),
            by_name(after, scope, name),
            by_name(scope, scope, name),
        );
        let mut summary = SectionSummary {
            shipped: shipped.len(),
            ..Default::default()
        };
        for key in shipped.keys() {
            let Some(old_entry) = old.get(key) else {
                continue;
            };
            match new.get(key) {
                None => summary.removed.push((*key).to_owned()),
                Some(new_entry) if new_entry != old_entry => {
                    summary.changed.push((*key).to_owned());
                    self.entries.push(ChangedEntry {
                        section,
                        name: (*key).to_owned(),
                        before: pretty(old_entry),
                        after: pretty(new_entry),
                    });
                }
                Some(_) => {}
            }
        }
        summary.added = new.keys().filter(|key| !old.contains_key(*key)).count();
        summary.changed_elsewhere = old
            .iter()
            .filter(|(key, entry)| {
                !shipped.contains_key(*key) && new.get(*key).is_some_and(|other| other != *entry)
            })
            .count();
        self.sections.insert(section, summary);
    }

    /// In-scope entries that changed or disappeared, as `section/name`.
    pub fn breaking(&self) -> Vec<String> {
        SECTIONS
            .iter()
            .flat_map(|section| {
                let summary = &self.sections[section];
                summary
                    .changed
                    .iter()
                    .chain(&summary.removed)
                    .map(move |name| format!("{section}/{name}"))
            })
            .collect()
    }

    pub fn added(&self) -> usize {
        self.sections.values().map(|summary| summary.added).sum()
    }

    pub fn changed_elsewhere(&self) -> usize {
        self.sections
            .values()
            .map(|summary| summary.changed_elsewhere)
            .sum()
    }

    /// A unified diff of every in-scope entry that changed, one block per entry.
    pub fn render_diff(&self, before_label: &str, after_label: &str) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            let path = format!("{}/{}", entry.section, entry.name);
            out += &unified_diff(
                &entry.before,
                &entry.after,
                &format!("{before_label} {path}"),
                &format!("{after_label} {path}"),
            );
            out.push('\n');
        }
        for section in SECTIONS {
            for name in &self.sections[section].removed {
                out += &format!(
                    "--- {before_label} {section}/{name}\n+++ {after_label}: no longer published\n\n"
                );
            }
        }
        out
    }
}

/// A whole-document diff, for documents that are not Anchor IDLs.
pub fn render_text_diff(
    before: &Value,
    after: &Value,
    before_label: &str,
    after_label: &str,
) -> String {
    unified_diff(&pretty(before), &pretty(after), before_label, after_label)
}

fn pretty(value: &impl Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("an IDL serialises");
    text.push('\n');
    text
}

fn unified_diff(before: &str, after: &str, before_label: &str, after_label: &str) -> String {
    TextDiff::from_lines(before, after)
        .unified_diff()
        .header(before_label, after_label)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

    fn current(types: Value, accounts: Value) -> Value {
        json!({
            "address": ADDRESS,
            "metadata": {"name": "kamino_lending", "version": "1.27.0", "spec": "0.1.0"},
            "instructions": [],
            "accounts": accounts,
            "types": types,
        })
    }

    fn last_update(third: Value) -> Value {
        json!({"name": "LastUpdate", "type": {"kind": "struct", "fields": [
            {"name": "slot", "type": "u64"},
            {"name": "price_status", "type": "u8"},
            third,
        ]}})
    }

    fn reserve() -> Value {
        json!({"name": "Reserve", "type": {"kind": "struct", "fields": [
            {"name": "last_update", "type": {"defined": {"name": "LastUpdate"}}},
            {"name": "lending_market", "type": "pubkey"},
        ]}})
    }

    fn placeholder() -> Value {
        json!({"name": "placeholder", "type": {"array": ["u8", 6]}})
    }

    fn reserve_account() -> Value {
        // sha256("account:Reserve")[..8]
        json!([{"name": "Reserve", "discriminator": [43, 242, 204, 202, 26, 247, 59, 127]}])
    }

    fn committed() -> Idl {
        parse(
            &current(
                json!([reserve(), last_update(placeholder())]),
                reserve_account(),
            ),
            ADDRESS,
        )
        .unwrap()
    }

    /// The same program in the older Anchor format: camelCase, `publicKey`, structs in `accounts`.
    fn legacy() -> Value {
        json!({
            "version": "1.27.0",
            "name": "kamino_lending",
            "instructions": [{"name": "refreshReserve", "accounts": [{"name": "reserve", "isMut": true, "isSigner": false}], "args": []}],
            "accounts": [{"name": "Reserve", "type": {"kind": "struct", "fields": [
                {"name": "lastUpdate", "type": {"defined": "LastUpdate"}},
                {"name": "lendingMarket", "type": "publicKey"},
            ]}}],
            "types": [{"name": "LastUpdate", "type": {"kind": "struct", "fields": [
                {"name": "slot", "type": "u64"},
                {"name": "priceStatus", "type": "u8"},
                {"name": "placeholder", "type": {"array": ["u8", 6]}},
            ]}}],
        })
    }

    #[test]
    fn detects_each_format() {
        assert_eq!(Format::of(&current(json!([]), json!([]))), Format::Anchor);
        assert_eq!(Format::of(&legacy()), Format::AnchorLegacy);
        let codama =
            json!({"kind": "rootNode", "version": "1.0.0", "program": {"version": "3.3.0"}});
        assert_eq!(Format::of(&codama), Format::Codama);
        assert_eq!(version_of(&codama).as_deref(), Some("3.3.0"));
        assert!(parse(&codama, ADDRESS).is_err());
    }

    #[test]
    fn an_older_format_with_the_same_layout_matches() {
        let comparison = compare(&committed(), &parse(&legacy(), ADDRESS).unwrap(), None);
        assert!(
            comparison.breaking().is_empty(),
            "{:?}",
            comparison.breaking()
        );
        // The instruction is upstream only: counted, never breaking.
        assert_eq!(comparison.sections["instructions"].added, 1);
    }

    fn route(name: &str, amount_type: &str) -> Value {
        let mut idl = current(json!([]), json!([]));
        idl["instructions"] = json!([{
            "name": name,
            "discriminator": [193, 32, 155, 51, 65, 214, 156, 129],
            "accounts": [],
            "args": [{"name": "in_amount", "type": amount_type}],
        }]);
        idl
    }

    #[test]
    fn a_shipped_entry_is_found_however_upstream_spells_it() {
        let shipped = parse(&route("sharedAccountsRoute", "u64"), ADDRESS).unwrap();
        let upstream = |ty| parse(&route("shared_accounts_route", ty), ADDRESS).unwrap();

        // An upstream change reaches the entry; an unchanged copy raises nothing.
        let changed = compare(&upstream("u64"), &upstream("u128"), Some(&shipped));
        assert_eq!(changed.breaking(), ["instructions/sharedAccountsRoute"]);
        assert_eq!(changed.changed_elsewhere(), 0);
        let unchanged = compare(&upstream("u64"), &upstream("u64"), Some(&shipped));
        assert!(unchanged.breaking().is_empty());

        // Next to the committed IDL it differs in spelling, and is not missing.
        let gap = compare(&shipped, &upstream("u64"), None);
        assert_eq!(
            gap.sections["instructions"].changed,
            ["sharedAccountsRoute"]
        );
        assert!(gap.sections["instructions"].removed.is_empty());
        assert_eq!(gap.added(), 0);
        assert_eq!(coverage(&shipped, &upstream("u64")), 1);
    }

    #[test]
    fn entries_that_differ_in_case_alone_stay_apart() {
        let errors = |names: &[&str]| {
            let mut idl = current(json!([]), json!([]));
            idl["errors"] = names
                .iter()
                .zip(6000..)
                .map(|(name, code)| json!({"name": name, "code": code}))
                .collect();
            parse(&idl, ADDRESS).unwrap()
        };
        let shipped = errors(&["InvalidFeedID"]);
        let upstream = errors(&["InvalidFeedID", "InvalidFeedId"]);
        let comparison = compare(&shipped, &upstream, None);
        assert!(comparison.breaking().is_empty());
        assert_eq!(comparison.added(), 1);
    }

    #[test]
    fn documentation_is_no_change() {
        let mut documented = last_update(placeholder());
        documented["docs"] = json!(["Last update state"]);
        documented["type"]["fields"][0]["docs"] = json!(["Last slot when updated"]);
        let after = parse(
            &current(json!([reserve(), documented]), reserve_account()),
            ADDRESS,
        )
        .unwrap();
        assert!(compare(&committed(), &after, None).breaking().is_empty());
    }

    #[test]
    fn a_replaced_field_is_breaking() {
        let mut changed =
            last_update(json!({"name": "alignment_padding", "type": {"array": ["u8", 2]}}));
        changed["type"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "timestamp", "type": "u32"}));
        let after = parse(
            &current(json!([reserve(), changed]), reserve_account()),
            ADDRESS,
        )
        .unwrap();
        let comparison = compare(&committed(), &after, None);
        assert_eq!(comparison.breaking(), ["types/LastUpdate"]);
        assert!(
            comparison
                .render_diff("previous", "latest")
                .contains("\"timestamp\"")
        );
    }

    #[test]
    fn a_discriminator_change_is_breaking() {
        let accounts = json!([{"name": "Reserve", "discriminator": [1, 2, 3, 4, 5, 6, 7, 8]}]);
        let after = parse(
            &current(json!([reserve(), last_update(placeholder())]), accounts),
            ADDRESS,
        )
        .unwrap();
        assert_eq!(
            compare(&committed(), &after, None).breaking(),
            ["accounts/Reserve"]
        );
    }

    #[test]
    fn a_removed_entry_is_breaking() {
        let after = parse(&current(json!([reserve()]), reserve_account()), ADDRESS).unwrap();
        let comparison = compare(&committed(), &after, None);
        assert_eq!(comparison.sections["types"].removed, ["LastUpdate"]);
        assert!(
            comparison
                .render_diff("committed", "upstream")
                .contains("no longer published")
        );
    }

    #[test]
    fn scope_limits_what_counts() {
        let other = |ty: &str| json!({"name": "Other", "type": {"kind": "struct", "fields": [{"name": "a", "type": ty}]}});
        let idl = |ty| {
            parse(
                &current(
                    json!([reserve(), last_update(placeholder()), other(ty)]),
                    json!([]),
                ),
                ADDRESS,
            )
            .unwrap()
        };
        let scope = parse(&current(json!([reserve()]), json!([])), ADDRESS).unwrap();
        let scoped = compare(&idl("u8"), &idl("u16"), Some(&scope));
        assert!(scoped.breaking().is_empty());
        assert_eq!(scoped.changed_elsewhere(), 1);
        assert_eq!(
            compare(&idl("u8"), &idl("u16"), None).breaking(),
            ["types/Other"]
        );
    }

    #[test]
    fn coverage_counts_what_the_committed_idl_ships() {
        let partial = parse(&current(json!([reserve()]), json!([])), ADDRESS).unwrap();
        assert_eq!(coverage(&committed(), &committed()), 3);
        assert_eq!(coverage(&committed(), &partial), 1);
    }
}
