//! The Markdown report each run shows as its summary.

use std::fmt::Write;

use crate::{
    check::{IdlResult, Outcome},
    tests_log::{Failure, TestRun},
    verdict::Verdict,
};

/// How many differing entries a row names before it counts the rest.
const ENTRIES_SHOWN: usize = 6;

pub struct Report<'a> {
    pub stamp: &'a str,
    pub verdict: Verdict,
    pub run_url: Option<&'a str>,
    pub attention: &'a [String],
    pub idls: &'a [IdlResult],
    pub tests: Option<&'a TestRun>,
}

fn failure_details(failed: &[Failure]) -> String {
    failed
        .iter()
        .map(|failure| {
            let message = match failure.message.as_str() {
                "" => "(no output)",
                message => message,
            };
            format!(
                "<details><summary><code>{}</code></summary>\n\n````\n{message}\n````\n</details>\n\n",
                failure.name
            )
        })
        .collect()
}

fn code_list(entries: &[String]) -> String {
    let shown: Vec<_> = entries
        .iter()
        .take(ENTRIES_SHOWN)
        .map(|entry| format!("`{entry}`"))
        .collect();
    match entries.len().saturating_sub(ENTRIES_SHOWN) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
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
        if !self.attention.is_empty() {
            out += "## Needs attention\n\n";
            for line in self.attention {
                let _ = writeln!(out, "- {line}");
            }
            out.push('\n');
        }
        self.idls_section(&mut out);
        self.tests_section(&mut out);
        out
    }

    fn idls_section(&self, out: &mut String) {
        *out += "## IDLs\n\n\
                 Each committed `idl.json` against the IDL its program publishes on chain, over the \
                 entries the committed file ships. Where a program publishes two, the one compared \
                 with is in bold: its Program Metadata (`pmp`) copy, since the one in its Anchor \
                 IDL account is often left behind. For every \
                 protocol that differs, the run's artifact holds the diff and the published IDL \
                 under `idls/`.\n\n\
                 | Protocol | Published | Committed | Result |\n\
                 |---|---|---|---|\n";
        for result in self.idls {
            let published: Vec<_> = result
                .upstream
                .iter()
                .map(|(source, upstream)| {
                    let version = upstream.version.as_deref().unwrap_or_default();
                    let copy = format!("{source} {version}").trim_end().to_owned();
                    let compared = result.compared_with.as_deref() == Some(source.as_str());
                    match compared && result.upstream.len() > 1 {
                        true => format!("**{copy}**"),
                        false => copy,
                    }
                })
                .collect();
            let published = match (&result.outcome, published.is_empty()) {
                // Nothing is known of a program whose reply could not be read.
                (Outcome::Unchecked { .. }, true) => "—".to_owned(),
                (_, true) => "nothing".to_owned(),
                (_, false) => published.join(", "),
            };
            let outcome = match &result.outcome {
                Outcome::Matches => "matches".to_owned(),
                Outcome::Differs { entries, diff_file } => {
                    let count = match entries.len() {
                        1 => "1 entry differs".to_owned(),
                        count => format!("{count} entries differ"),
                    };
                    format!("**{count}**: {} (`{diff_file}`)", code_list(entries))
                }
                Outcome::NotPublished => "nothing to compare with".to_owned(),
                Outcome::NotComparable { reason } => format!("not comparable: {reason}"),
                Outcome::Unchecked { error } => format!("could not check: {error}"),
            };
            let unreadable: String = result
                .unreadable
                .iter()
                .map(|reason| format!("<br>not read: {reason}"))
                .collect();
            let _ = writeln!(
                out,
                "| `{}` | {published} | {} | {outcome}{unreadable} |",
                result.protocol,
                result.committed_version.as_deref().unwrap_or("—")
            );
        }
        out.push('\n');
    }

    fn tests_section(&self, out: &mut String) {
        *out += "## Integration tests\n\n";
        let Some(tests) = self.tests else {
            *out += "Not run in this run.\n";
            return;
        };
        if !tests.complete {
            *out += "These results are not complete: the tests did not build, were cut short, ran \
                     no protocol test, or count a different number of failures than their output \
                     lists. What could be read is below.\n\n";
        }
        *out += "| Module | Passed | Failed | Ignored | Result |\n|---|---|---|---|---|\n";
        for (module, result) in &tests.modules {
            let outcome = match result.failed.is_empty() {
                true => "pass",
                false => "fail",
            };
            let _ = writeln!(
                out,
                "| `{module}` | {} | {} | {} | {outcome} |",
                result.passed,
                result.failed.len(),
                result.ignored
            );
        }
        out.push('\n');
        for result in tests.modules.values() {
            *out += &failure_details(&result.failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{check::Upstream, idl::Format};

    /// The table row of one protocol whose Anchor IDL account publishes `version`, if any.
    fn row(version: Option<&str>, outcome: Outcome) -> String {
        let upstream = version.map(|version| Upstream {
            version: Some(version.to_owned()),
            format: Format::Anchor,
        });
        let idls = [IdlResult {
            protocol: "demo/v1".into(),
            address: None,
            path: String::new(),
            committed_version: None,
            upstream: upstream
                .map(|upstream| ("anchor".to_owned(), upstream))
                .into_iter()
                .collect(),
            compared_with: None,
            unreadable: Vec::new(),
            outcome,
        }];
        let report = Report {
            stamp: "now",
            verdict: Verdict::Unverified,
            run_url: None,
            attention: &[],
            idls: &idls,
            tests: None,
        };
        let rendered = report.render();
        let row = rendered
            .lines()
            .find(|line| line.starts_with("| `demo/v1`"));
        row.unwrap().to_owned()
    }

    #[test]
    fn a_row_that_could_not_be_checked_still_shows_what_is_published() {
        let unchecked = || Outcome::Unchecked {
            error: "boom".into(),
        };
        assert_eq!(
            row(Some("1.0.0"), unchecked()),
            "| `demo/v1` | anchor 1.0.0 | — | could not check: boom |"
        );
        assert_eq!(
            row(None, unchecked()),
            "| `demo/v1` | — | — | could not check: boom |"
        );
        assert_eq!(
            row(None, Outcome::NotPublished),
            "| `demo/v1` | nothing | — | nothing to compare with |"
        );
    }
}
