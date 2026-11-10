//! What needs an issue, what is resolved, and the run's verdict.

use std::{collections::BTreeMap, fmt};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    check::IdlResult,
    tests_log::{Failure, ModuleResult, Status},
};

/// GitHub rejects issue and comment bodies over 65536 characters.
const MAX_BODY: usize = 60_000;

/// One issue's worth of attention, keyed so later runs find the same issue.
#[derive(Debug, Serialize)]
pub struct Finding {
    pub key: String,
    /// Hidden in the issue body, so the issue can be found again.
    pub marker: String,
    pub title: String,
    /// Changes when the finding changes, so an unchanged finding is not posted twice.
    pub fingerprint: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// A protocol needs attention: a shipped IDL entry changed upstream, or tests fail.
    Attention,
    /// Tests were due but some module reported nothing.
    Incomplete,
    /// Nothing needs attention, but an IDL could not be fetched.
    Unverified,
    Clean,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Verdict::Attention => "attention",
            Verdict::Incomplete => "incomplete",
            Verdict::Unverified => "unverified",
            Verdict::Clean => "clean",
        };
        f.write_str(name)
    }
}

pub fn marker(key: &str) -> String {
    format!("<!-- scenarios-monitoring: {key} -->")
}

fn fingerprint<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut parts: Vec<&str> = parts.into_iter().collect();
    parts.sort_unstable();
    let digest = Sha256::digest(parts.join("\n").as_bytes());
    format!("{digest:x}")[..16].to_owned()
}

fn clip(mut body: String) -> String {
    if body.len() > MAX_BODY {
        let mut end = MAX_BODY;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
        body.push_str("\n\n… truncated; the full output is in the report.");
    }
    body
}

fn links(address: &str) -> String {
    format!(
        "[Solscan](https://solscan.io/account/{address}) · [Explorer](https://explorer.solana.com/address/{address})"
    )
}

pub fn failure_details(failed: &[Failure]) -> String {
    failed
        .iter()
        .map(|failure| {
            let message = if failure.message.is_empty() { "(no output)" } else { &failure.message };
            format!("<details><summary><code>{}</code></summary>\n\n````\n{message}\n````\n</details>\n\n", failure.name)
        })
        .collect()
}

/// Open findings and the keys whose issues can close.
pub fn idl_findings(
    results: &[IdlResult],
    report_url: Option<&str>,
) -> (Vec<Finding>, Vec<String>) {
    let mut open = Vec::new();
    let mut resolved = Vec::new();
    for result in results {
        let key = format!("idl:{}", result.protocol);
        let attention: Vec<_> = result
            .events
            .iter()
            .filter(|event| event.attention)
            .collect();
        if attention.is_empty() {
            if result
                .gap
                .as_ref()
                .is_some_and(|gap| gap.comparable && gap.breaking.is_empty())
            {
                resolved.push(key);
            }
            continue;
        }
        let address = result.address.as_deref().unwrap_or_default();
        let mut body = format!(
            "The upstream IDL of **{}** changed in a way that touches what Surfpool ships in `{}`.\n\n\
             Program `{address}` · {}\n\n",
            result.protocol,
            result.path,
            links(address)
        );
        for event in &attention {
            let versions = if event.from_version == event.to_version {
                let version = event.to_version.as_deref().unwrap_or("unset");
                format!("version stays {version}")
            } else {
                format!(
                    "{} → {}",
                    event.from_version.as_deref().unwrap_or("?"),
                    event.to_version.as_deref().unwrap_or("?")
                )
            };
            body += &format!("- **{}** IDL changed: {versions}\n", event.source);
            if !event.breaking.is_empty() {
                let entries: Vec<_> = event
                    .breaking
                    .iter()
                    .map(|entry| format!("`{entry}`"))
                    .collect();
                body += &format!("  - changed or removed: {}\n", entries.join(", "));
            } else if !event.comparable {
                body += "  - not an Anchor IDL Surfpool can compare entry by entry\n";
            }
            if let Some(note) = &event.note {
                body += &format!("  - {note}\n");
            }
            if let (Some(url), Some(diff)) = (report_url, &event.diff_file) {
                body += &format!("  - [diff]({url}/{diff})\n");
            }
        }
        if let Some(url) = report_url {
            body += &format!("\n[Report]({url}/report.md)\n");
        }
        let parts: Vec<String> = attention
            .iter()
            .map(|event| {
                format!(
                    "{}:{}",
                    event.source,
                    event.to_hash.as_deref().unwrap_or_default()
                )
            })
            .collect();
        open.push(Finding {
            marker: marker(&key),
            title: format!(
                "Scenarios monitoring: {} IDL changed upstream",
                result.protocol
            ),
            fingerprint: fingerprint(parts.iter().map(String::as_str)),
            body: clip(body),
            key,
        });
    }
    (open, resolved)
}

pub fn test_findings(
    modules: &BTreeMap<String, ModuleResult>,
    report_url: Option<&str>,
) -> (Vec<Finding>, Vec<String>) {
    let mut open = Vec::new();
    let mut resolved = Vec::new();
    for (module, result) in modules {
        let key = format!("tests:{module}");
        match result.status {
            Status::Pass => resolved.push(key),
            Status::Fail => {
                let mut body =
                    format!("The `{module}` integration tests fail against mainnet.\n\n");
                if let Some(url) = report_url {
                    body += &format!("[Report]({url}/report.md)\n\n");
                }
                body += &failure_details(&result.failed);
                open.push(Finding {
                    marker: marker(&key),
                    title: format!("Scenarios monitoring: {module} integration tests fail"),
                    fingerprint: fingerprint(
                        result.failed.iter().map(|failure| failure.name.as_str()),
                    ),
                    body: clip(body),
                    key,
                });
            }
            Status::Incomplete => {}
        }
    }
    (open, resolved)
}

pub fn decide(
    findings: &[Finding],
    idls: &[IdlResult],
    modules: Option<&BTreeMap<String, ModuleResult>>,
) -> Verdict {
    let incomplete = |modules: &BTreeMap<String, ModuleResult>| {
        modules
            .values()
            .any(|result| result.status == Status::Incomplete)
    };
    if !findings.is_empty() {
        Verdict::Attention
    } else if modules.is_some_and(incomplete) {
        Verdict::Incomplete
    } else if idls.iter().any(|result| result.error.is_some()) {
        Verdict::Unverified
    } else {
        Verdict::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_ignores_order() {
        assert_eq!(fingerprint(["a", "b"]), fingerprint(["b", "a"]));
        assert_ne!(fingerprint(["a"]), fingerprint(["b"]));
    }

    #[test]
    fn a_long_body_is_clipped_on_a_character_boundary() {
        let body = clip("é".repeat(MAX_BODY));
        assert!(body.len() < MAX_BODY + 100);
        assert!(body.ends_with("in the report."));
    }

    #[test]
    fn a_failing_module_needs_attention_and_a_passing_one_resolves() {
        let failing = ModuleResult {
            status: Status::Fail,
            passed: 0,
            ignored: 0,
            failed: vec![Failure {
                name: "tests::protocols::pump::a".into(),
                message: "boom".into(),
            }],
        };
        let passing = ModuleResult {
            status: Status::Pass,
            passed: 3,
            ignored: 0,
            failed: vec![],
        };
        let modules =
            BTreeMap::from([("pump".to_owned(), failing), ("kamino".to_owned(), passing)]);
        let (open, resolved) = test_findings(&modules, None);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].key, "tests:pump");
        assert!(open[0].marker.contains("tests:pump"));
        assert_eq!(resolved, ["tests:kamino"]);
        assert_eq!(decide(&open, &[], Some(&modules)), Verdict::Attention);
    }
}
