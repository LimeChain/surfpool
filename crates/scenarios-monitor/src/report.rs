//! The Markdown report each run shows as its summary.

use std::{collections::BTreeMap, fmt::Write};

use crate::{
    check::IdlResult,
    findings::{Finding, Verdict, failure_details},
    tests_log::ModuleResult,
};

pub struct Report<'a> {
    pub stamp: &'a str,
    pub verdict: Verdict,
    pub run_url: Option<&'a str>,
    pub findings: &'a [Finding],
    pub idls: &'a [IdlResult],
    pub tests: Option<&'a BTreeMap<String, ModuleResult>>,
}

fn code_list(entries: &[String], limit: usize) -> String {
    let shown: Vec<_> = entries
        .iter()
        .take(limit)
        .map(|entry| format!("`{entry}`"))
        .collect();
    let more = entries.len().saturating_sub(limit);
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

impl Report<'_> {
    pub fn render(&self) -> String {
        let mut out = format!(
            "# Scenarios monitoring — {}\n\nVerdict: **{}**",
            self.stamp, self.verdict
        );
        if let Some(url) = self.run_url {
            let _ = write!(out, " · [Run]({url})");
        }
        out += "\n\n";
        if !self.findings.is_empty() {
            out += "## Needs attention\n\n";
            for finding in self.findings {
                let _ = writeln!(
                    out,
                    "- {}",
                    finding.title.trim_start_matches("Scenarios monitoring: ")
                );
            }
            out.push('\n');
        }
        self.idls_section(&mut out);
        self.tests_section(&mut out);
        out
    }

    fn idls_section(&self, out: &mut String) {
        *out += "## IDLs\n\n\
                 Compared with the IDL each program publishes on chain (Anchor IDL account or Program \
                 Metadata). *Upstream change* is a change since the previous run; *committed vs upstream* \
                 is what `idl.json` ships that the program no longer publishes the same way. The diffs are \
                 files in the run's artifact.\n\n\
                 | Protocol | Upstream | Committed | Upstream change | Committed vs upstream |\n\
                 |---|---|---|---|---|\n";
        for result in self.idls {
            let (upstream, mut change) = if result.sources.is_empty() {
                let upstream = match result.error {
                    Some(_) => "—",
                    None => "none published",
                };
                (upstream.to_owned(), String::new())
            } else {
                let upstream: Vec<_> = result
                    .sources
                    .iter()
                    .map(|(source, info)| {
                        format!("{source} {}", info.version.as_deref().unwrap_or_default())
                            .trim_end()
                            .to_owned()
                    })
                    .collect();
                let mut statuses: Vec<String> = result
                    .sources
                    .values()
                    .map(|info| format!("{:?}", info.status).to_lowercase())
                    .collect();
                statuses.sort();
                statuses.dedup();
                let mut change = statuses.join(", ");
                for event in &result.events {
                    let detail = if !event.breaking.is_empty() {
                        code_list(&event.breaking, usize::MAX)
                    } else if event.comparable {
                        "outside what Surfpool ships".into()
                    } else {
                        "not comparable".into()
                    };
                    let diff = event
                        .diff_file
                        .as_ref()
                        .map(|file| format!(" (`report/{file}`)"))
                        .unwrap_or_default();
                    let _ = write!(change, "<br>{}: {detail}{diff}", event.source);
                }
                (upstream.join(", "), change)
            };
            if let Some(error) = &result.error {
                let separator = if change.is_empty() { "" } else { "<br>" };
                let _ = write!(change, "{separator}could not check: {error}");
            }
            if change.is_empty() {
                change.push('—');
            }
            let gap = match &result.gap {
                None => "—".to_owned(),
                Some(gap) if !gap.comparable => {
                    let formats: Vec<_> = gap
                        .formats
                        .iter()
                        .map(|format| format!("{format:?}").to_lowercase())
                        .collect();
                    format!("not comparable ({})", formats.join(", "))
                }
                Some(gap) if gap.breaking.is_empty() => "matches".to_owned(),
                Some(gap) => format!(
                    "{} differ: {}",
                    gap.breaking.len(),
                    code_list(&gap.breaking, 6)
                ),
            };
            let _ = writeln!(
                out,
                "| `{}` | {upstream} | {} | {change} | {gap} |",
                result.protocol,
                result.committed_version.as_deref().unwrap_or("—")
            );
        }
        out.push('\n');
    }

    fn tests_section(&self, out: &mut String) {
        *out += "## Integration tests\n\n";
        let Some(modules) = self.tests else {
            *out += "Not run in this run.\n";
            return;
        };
        *out += "| Module | Passed | Failed | Ignored | Status |\n|---|---|---|---|---|\n";
        for (module, result) in modules {
            let status = format!("{:?}", result.status).to_lowercase();
            let _ = writeln!(
                out,
                "| `{module}` | {} | {} | {} | {status} |",
                result.passed,
                result.failed.len(),
                result.ignored
            );
        }
        out.push('\n');
        for result in modules.values() {
            *out += &failure_details(&result.failed);
        }
    }
}
