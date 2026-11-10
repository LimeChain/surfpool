//! Compares a committed Anchor IDL with a published one, entry by entry.
//!
//! Every IDL, committed or published, goes through Anchor's own converter into the current
//! Anchor format first, so two documents describing the same program compare equal whichever
//! format each was published in. Codama documents do not convert, and cannot be compared.
//!
//! IDLs of one program spell its names differently (`sharedAccountsRoute` and
//! `shared_accounts_route`), so a shipped entry with no namesake is looked up without regard to
//! case and underscores. That only finds the entry: next to a copy spelled differently it still
//! compares as changed.

use std::collections::{BTreeMap, BTreeSet};

use anchor_lang_idl::{convert::convert_idl, types::Idl};
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use similar::TextDiff;

/// The format a document was published in, for the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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

    pub fn name(self) -> &'static str {
        match self {
            Format::Anchor => "anchor",
            Format::AnchorLegacy => "anchor-legacy",
            Format::Codama => "codama",
            Format::Unknown => "unknown",
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

/// A shipped entry the published IDL has differently, or not at all.
struct Difference {
    /// As `section/name`, in the committed spelling.
    path: String,
    committed: String,
    /// `None` when the published IDL has no such entry.
    published: Option<String>,
}

/// What the committed IDL ships that the published one has differently or not at all.
#[derive(Default)]
pub struct Comparison {
    differences: Vec<Difference>,
}

/// Compares the entries `committed` ships with the same entries of `published`. Entries only the
/// published IDL has are not looked at: the committed IDLs are trimmed to what the templates use.
pub fn compare(committed: &Idl, published: &Idl) -> Comparison {
    let mut comparison = Comparison::default();
    comparison.section("accounts", &committed.accounts, &published.accounts, |e| {
        &e.name
    });
    comparison.section("types", &committed.types, &published.types, |e| &e.name);
    comparison.section(
        "instructions",
        &committed.instructions,
        &published.instructions,
        |e| &e.name,
    );
    comparison.section("events", &committed.events, &published.events, |e| &e.name);
    comparison.section("errors", &committed.errors, &published.errors, |e| &e.name);
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

/// The entries by name. An entry `shipped` spells differently is filed under the shipped
/// spelling, unless the entries hold that very name as well: some IDLs publish two entries that
/// differ in case alone.
fn by_name<'a, T>(
    entries: &'a [T],
    shipped: &'a [T],
    name: fn(&T) -> &String,
) -> BTreeMap<&'a str, &'a T> {
    let own: BTreeSet<&str> = entries.iter().map(|entry| name(entry).as_str()).collect();
    let spellings: BTreeMap<String, &str> = shipped
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
        section: &str,
        committed: &[T],
        published: &[T],
        name: fn(&T) -> &String,
    ) {
        let published = by_name(published, committed, name);
        for entry in committed {
            let found = published.get(name(entry).as_str());
            if found.is_some_and(|found| *found == entry) {
                continue;
            }
            self.differences.push(Difference {
                path: format!("{section}/{}", name(entry)),
                committed: pretty(entry),
                published: found.map(|found| pretty(*found)),
            });
        }
    }

    /// The shipped entries that differ, as `section/name`.
    pub fn differing(&self) -> Vec<String> {
        self.differences
            .iter()
            .map(|difference| difference.path.clone())
            .collect()
    }

    /// A unified diff of every shipped entry that differs, one block per entry.
    pub fn render_diff(&self) -> String {
        let mut out = String::new();
        for difference in &self.differences {
            let path = &difference.path;
            match &difference.published {
                Some(published) => {
                    out += &TextDiff::from_lines(&difference.committed, published)
                        .unified_diff()
                        .header(&format!("committed {path}"), &format!("published {path}"))
                        .to_string();
                    out.push('\n');
                }
                None => out += &format!("--- committed {path}\n+++ no longer published\n\n"),
            }
        }
        out
    }
}

fn pretty(value: &impl Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("an IDL serialises");
    text.push('\n');
    text
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
        // The instruction is published only, and so not looked at.
        let comparison = compare(&committed(), &parse(&legacy(), ADDRESS).unwrap());
        assert!(
            comparison.differing().is_empty(),
            "{:?}",
            comparison.differing()
        );
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
    fn a_shipped_entry_is_found_however_the_program_spells_it() {
        let shipped = parse(&route("sharedAccountsRoute", "u64"), ADDRESS).unwrap();
        let published = parse(&route("shared_accounts_route", "u64"), ADDRESS).unwrap();

        // It differs in spelling, and is not missing.
        let comparison = compare(&shipped, &published);
        assert_eq!(comparison.differing(), ["instructions/sharedAccountsRoute"]);
        let diff = comparison.render_diff();
        assert!(diff.contains("shared_accounts_route"), "{diff}");
        assert!(!diff.contains("no longer published"), "{diff}");
        assert_eq!(coverage(&shipped, &published), 1);
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
        let published = errors(&["InvalidFeedID", "InvalidFeedId"]);
        assert!(compare(&shipped, &published).differing().is_empty());
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
        assert!(compare(&committed(), &after).differing().is_empty());
    }

    #[test]
    fn a_replaced_field_differs() {
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
        let comparison = compare(&committed(), &after);
        assert_eq!(comparison.differing(), ["types/LastUpdate"]);
        assert!(comparison.render_diff().contains("\"timestamp\""));
    }

    #[test]
    fn a_changed_discriminator_differs() {
        let accounts = json!([{"name": "Reserve", "discriminator": [1, 2, 3, 4, 5, 6, 7, 8]}]);
        let after = parse(
            &current(json!([reserve(), last_update(placeholder())]), accounts),
            ADDRESS,
        )
        .unwrap();
        assert_eq!(
            compare(&committed(), &after).differing(),
            ["accounts/Reserve"]
        );
    }

    #[test]
    fn an_entry_the_program_no_longer_publishes_differs() {
        let after = parse(&current(json!([reserve()]), reserve_account()), ADDRESS).unwrap();
        let comparison = compare(&committed(), &after);
        assert_eq!(comparison.differing(), ["types/LastUpdate"]);
        assert!(comparison.render_diff().contains("no longer published"));
    }

    #[test]
    fn entries_only_the_program_publishes_are_not_looked_at() {
        let other = json!({"name": "Other", "type": {"kind": "struct", "fields": [{"name": "a", "type": "u8"}]}});
        let published = parse(
            &current(
                json!([reserve(), last_update(placeholder()), other]),
                reserve_account(),
            ),
            ADDRESS,
        )
        .unwrap();
        assert!(compare(&committed(), &published).differing().is_empty());
    }

    #[test]
    fn coverage_counts_what_the_committed_idl_ships() {
        let partial = parse(&current(json!([reserve()]), json!([])), ADDRESS).unwrap();
        assert_eq!(coverage(&committed(), &committed()), 3);
        assert_eq!(coverage(&committed(), &partial), 1);
    }
}
