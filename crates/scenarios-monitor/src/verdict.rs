//! What needs attention, and the run's verdict.

use std::{collections::BTreeMap, fmt};

use serde::Serialize;

use crate::{
    check::{IdlResult, Outcome},
    tests_log::{ModuleResult, Status},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// A protocol needs attention: its committed IDL differs from what its program publishes, or
    /// its tests fail.
    Attention,
    /// Tests were due but some module reported nothing.
    Incomplete,
    /// Nothing needs attention, but what a program publishes could not be read.
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

/// One line for each thing that needs attention.
pub fn attention(
    idls: &[IdlResult],
    modules: Option<&BTreeMap<String, ModuleResult>>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for result in idls {
        if let Outcome::Differs { entries, .. } = &result.outcome {
            let entries = match entries.len() {
                1 => "1 entry".to_owned(),
                count => format!("{count} entries"),
            };
            lines.push(format!(
                "{} IDL differs from what its program publishes ({entries})",
                result.protocol
            ));
        }
    }
    for (module, result) in modules.into_iter().flatten() {
        if result.status == Status::Fail {
            lines.push(format!("{module} integration tests fail"));
        }
    }
    lines
}

pub fn decide(
    attention: &[String],
    idls: &[IdlResult],
    modules: Option<&BTreeMap<String, ModuleResult>>,
) -> Verdict {
    let incomplete = |modules: &BTreeMap<String, ModuleResult>| {
        modules
            .values()
            .any(|result| result.status == Status::Incomplete)
    };
    let unchecked = |result: &IdlResult| matches!(result.outcome, Outcome::Unchecked { .. });
    if !attention.is_empty() {
        Verdict::Attention
    } else if modules.is_some_and(incomplete) {
        Verdict::Incomplete
    } else if idls.iter().any(unchecked) {
        Verdict::Unverified
    } else {
        Verdict::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_log::Failure;

    fn module(status: Status) -> ModuleResult {
        let failed = match status {
            Status::Fail => vec![Failure {
                name: "tests::protocols::pump::a".into(),
                message: "boom".into(),
            }],
            _ => vec![],
        };
        ModuleResult {
            status,
            passed: 3,
            ignored: 0,
            failed,
        }
    }

    fn idl(protocol: &str, outcome: Outcome) -> IdlResult {
        IdlResult {
            protocol: protocol.into(),
            address: None,
            path: String::new(),
            committed_version: None,
            upstream: BTreeMap::new(),
            compared_with: None,
            outcome,
        }
    }

    #[test]
    fn a_differing_idl_and_a_failing_module_need_attention() {
        let idls = [
            idl("kamino/v1", Outcome::Matches),
            idl(
                "pump/v1",
                Outcome::Differs {
                    entries: vec!["types/BondingCurve".into()],
                    diff_file: "idls/pump-v1.diff".into(),
                },
            ),
        ];
        let modules = BTreeMap::from([
            ("pump".to_owned(), module(Status::Fail)),
            ("kamino".to_owned(), module(Status::Pass)),
        ]);
        let lines = attention(&idls, Some(&modules));
        assert_eq!(
            lines,
            [
                "pump/v1 IDL differs from what its program publishes (1 entry)",
                "pump integration tests fail",
            ]
        );
        assert_eq!(decide(&lines, &idls, Some(&modules)), Verdict::Attention);
    }

    #[test]
    fn only_a_run_that_checked_everything_and_found_nothing_is_clean() {
        let matching = [idl("kamino/v1", Outcome::Matches)];
        let unchecked = [idl(
            "kamino/v1",
            Outcome::Unchecked {
                error: "IDL service: HTTP 500".into(),
            },
        )];
        // Nothing to compare with is not a failure of the check.
        let unpublished = [idl("raydium/v4", Outcome::NotPublished)];
        let passing = BTreeMap::from([("kamino".to_owned(), module(Status::Pass))]);
        let unfinished = BTreeMap::from([("kamino".to_owned(), module(Status::Incomplete))]);
        assert_eq!(decide(&[], &matching, Some(&passing)), Verdict::Clean);
        assert_eq!(decide(&[], &unpublished, Some(&passing)), Verdict::Clean);
        assert_eq!(decide(&[], &unchecked, Some(&passing)), Verdict::Unverified);
        assert_eq!(
            decide(&[], &unchecked, Some(&unfinished)),
            Verdict::Incomplete
        );
    }
}
