//! What needs attention, and the run's verdict.

use std::fmt;

use serde::Serialize;

use crate::{
    check::{IdlResult, Outcome},
    tests_log::TestRun,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// A protocol needs attention: its committed IDL differs from what its program publishes, or
    /// its tests fail.
    Attention,
    /// The tests were due and their results are not complete: see [`TestRun::complete`].
    Incomplete,
    /// Nothing needs attention, but a comparison could not be made.
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
pub fn attention(idls: &[IdlResult], tests: Option<&TestRun>) -> Vec<String> {
    // In the order the report shows its sections: the tests, then the IDLs.
    let mut lines = Vec::new();
    for (module, result) in tests.into_iter().flat_map(|tests| &tests.modules) {
        if !result.failed.is_empty() {
            lines.push(format!("{module} integration tests fail"));
        }
    }
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
    lines
}

pub fn decide(attention: &[String], idls: &[IdlResult], tests: Option<&TestRun>) -> Verdict {
    let unchecked = |result: &IdlResult| matches!(result.outcome, Outcome::Unchecked { .. });
    if !attention.is_empty() {
        Verdict::Attention
    } else if tests.is_some_and(|tests| !tests.complete) {
        Verdict::Incomplete
    } else if idls.iter().any(unchecked) {
        Verdict::Unverified
    } else {
        Verdict::Clean
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tests_log::{Failure, ModuleResult};

    fn module(failing: bool) -> ModuleResult {
        let failed = match failing {
            true => vec![Failure {
                name: "tests::protocols::pump::a".into(),
                message: "boom".into(),
            }],
            false => vec![],
        };
        ModuleResult {
            passed: 3,
            ignored: 0,
            failed,
        }
    }

    fn run(complete: bool, modules: &[(&str, bool)]) -> TestRun {
        TestRun {
            complete,
            modules: modules
                .iter()
                .map(|(name, failing)| ((*name).to_owned(), module(*failing)))
                .collect(),
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
            unreadable: Vec::new(),
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
        let tests = run(true, &[("pump", true), ("kamino", false)]);
        let lines = attention(&idls, Some(&tests));
        assert_eq!(
            lines,
            [
                "pump integration tests fail",
                "pump/v1 IDL differs from what its program publishes (1 entry)",
            ]
        );
        assert_eq!(decide(&lines, &idls, Some(&tests)), Verdict::Attention);
    }

    #[test]
    fn a_failure_counts_even_when_the_run_was_cut_short() {
        let tests = run(false, &[("pump", true)]);
        let lines = attention(&[], Some(&tests));
        assert_eq!(decide(&lines, &[], Some(&tests)), Verdict::Attention);
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
        let passing = run(true, &[("kamino", false)]);
        let unfinished = run(false, &[("kamino", false)]);
        let nothing_ran = run(false, &[]);
        assert_eq!(decide(&[], &matching, Some(&passing)), Verdict::Clean);
        assert_eq!(decide(&[], &unpublished, Some(&passing)), Verdict::Clean);
        assert_eq!(decide(&[], &unchecked, Some(&passing)), Verdict::Unverified);
        assert_eq!(
            decide(&[], &matching, Some(&unfinished)),
            Verdict::Incomplete
        );
        assert_eq!(
            decide(&[], &matching, Some(&nothing_ran)),
            Verdict::Incomplete
        );
    }
}
