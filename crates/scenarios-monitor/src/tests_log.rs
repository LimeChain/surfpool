//! Reads the output of `cargo test` for the protocol tests.

use std::collections::BTreeMap;

use serde::Serialize;

/// The path every protocol test starts with, which is also the filter that runs them all.
const PREFIX: &str = "tests::protocols::";
/// Where a test that sits in no module of `tests::protocols` is counted.
const NO_MODULE: &str = "protocols";
const MAX_MESSAGE_LINES: usize = 40;

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub name: String,
    pub message: String,
}

/// The results of one module of `tests::protocols`, which holds one protocol's tests.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ModuleResult {
    pub passed: usize,
    pub ignored: usize,
    pub failed: Vec<Failure>,
}

/// What a `cargo test` run of the protocol tests reported.
#[derive(Clone, Debug, Serialize)]
pub struct TestRun {
    /// Whether the run reported its result and ran at least one protocol test. A run that did
    /// not build, was cut short or ran nothing is not complete, and proves nothing by passing.
    pub complete: bool,
    /// By module, taken from the names of the tests that ran.
    pub modules: BTreeMap<String, ModuleResult>,
}

/// The module of `tests::protocols` a test belongs to, or `None` for any other test.
fn module_of(test_name: &str) -> Option<&str> {
    let rest = test_name.strip_prefix(PREFIX)?;
    Some(
        rest.split_once("::")
            .map_or(NO_MODULE, |(module, _)| module),
    )
}

/// Groups a `cargo test` log by protocol test module.
pub fn parse(log: &str) -> TestRun {
    let mut results: BTreeMap<&str, &str> = BTreeMap::new();
    let mut blocks: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut current: Option<&str> = None;
    let mut in_failures = false;
    let mut summary_seen = false;

    for line in log.lines() {
        if line.starts_with("test result: ") {
            summary_seen = true;
            current = None;
            continue;
        }
        if let Some((name, outcome)) = result_line(line) {
            results.insert(name, outcome);
            continue;
        }
        if line == "failures:" {
            in_failures = true;
            current = None;
            continue;
        }
        if in_failures && let Some(name) = block_header(line) {
            current = Some(name);
            blocks.entry(name).or_default();
            continue;
        }
        if let Some(name) = current {
            blocks.entry(name).or_default().push(line);
        }
    }

    let mut modules: BTreeMap<String, ModuleResult> = BTreeMap::new();
    for (name, outcome) in results {
        let Some(module) = module_of(name) else {
            continue;
        };
        let result = modules.entry(module.to_owned()).or_default();
        match outcome {
            "ok" => result.passed += 1,
            "ignored" => result.ignored += 1,
            _ => result.failed.push(Failure {
                name: name.to_owned(),
                message: trim(blocks.get(name).map(Vec::as_slice).unwrap_or_default())
                    .trim()
                    .to_owned(),
            }),
        }
    }
    TestRun {
        complete: summary_seen && !modules.is_empty(),
        modules,
    }
}

/// `test NAME ... ok`, `... FAILED` or `... ignored[, reason]`. A test that is expected to panic
/// carries ` - should panic` after its name.
fn result_line(line: &str) -> Option<(&str, &'static str)> {
    let (name, outcome) = line.strip_prefix("test ")?.split_once(" ... ")?;
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    let word = outcome.split([',', ' ']).next()?;
    let outcome = ["ok", "FAILED", "ignored"]
        .into_iter()
        .find(|known| *known == word)?;
    (!name.contains(' ')).then_some((name, outcome))
}

fn block_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix("---- ")?.strip_suffix(" ----")?;
    inner
        .strip_suffix(" stdout")
        .or_else(|| inner.strip_suffix(" stderr"))
}

fn trim(lines: &[&str]) -> String {
    let mut kept: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !line.starts_with("note: run with `RUST_BACKTRACE"))
        .collect();
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    if kept.len() > MAX_MESSAGE_LINES {
        let more = kept.len() - MAX_MESSAGE_LINES;
        kept.truncate(MAX_MESSAGE_LINES);
        return format!("{}\n... {more} more lines", kept.join("\n"));
    }
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
running 6 tests
test tests::protocols::kamino::round_trips ... ok
test tests::protocols::kamino::reserve_price ... FAILED
test tests::protocols::pump::lifecycle ... FAILED
test tests::protocols::pump::old ... ignored, needs a validator
test tests::protocols::pump::rejects_a_bad_curve - should panic ... FAILED
test tests::protocols::shared_fixture_loads ... ok

failures:

---- tests::protocols::kamino::reserve_price stdout ----
thread 'tests::protocols::kamino::reserve_price' panicked at crates/core/src/tests/protocols/kamino/mod.rs:10:5:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- tests::protocols::pump::lifecycle stdout ----
thread 'tests::protocols::pump::lifecycle' panicked at crates/core/src/tests/protocols/pump/mod.rs:20:5:
failed to fetch from mainnet: datasource request to https://rpc.example timed out

---- tests::protocols::pump::rejects_a_bad_curve stdout ----
note: test did not panic as expected


failures:
    tests::protocols::kamino::reserve_price
    tests::protocols::pump::lifecycle
    tests::protocols::pump::rejects_a_bad_curve

test result: FAILED. 2 passed; 3 failed; 1 ignored; 0 measured; 900 filtered out; finished in 9.00s
";

    #[test]
    fn groups_results_by_the_module_each_test_names() {
        let run = parse(LOG);
        assert!(run.complete);
        let modules: Vec<_> = run.modules.keys().map(String::as_str).collect();
        assert_eq!(modules, ["kamino", "protocols", "pump"]);

        let kamino = &run.modules["kamino"];
        assert_eq!(kamino.passed, 1);
        assert!(kamino.failed[0].message.contains("left: 1"));
        assert!(!kamino.failed[0].message.contains("RUST_BACKTRACE"));

        // A failure is a failure, whatever its output blames.
        let pump = &run.modules["pump"];
        assert_eq!(pump.ignored, 1);
        let failed: Vec<_> = pump.failed.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            failed,
            [
                "tests::protocols::pump::lifecycle",
                "tests::protocols::pump::rejects_a_bad_curve",
            ]
        );
        assert!(pump.failed[0].message.contains("timed out"));
        assert!(pump.failed[1].message.contains("did not panic"));

        // A test in no module of its own is still counted.
        assert_eq!(run.modules[NO_MODULE].passed, 1);
    }

    #[test]
    fn other_tests_of_the_crate_are_left_out() {
        let log = "test tests::integration::test_simnet_ticks ... FAILED\n\
                   test tests::protocols::kamino::round_trips ... ok\n\
                   test result: FAILED. 1 passed; 1 failed; 0 ignored\n";
        let run = parse(log);
        let modules: Vec<_> = run.modules.keys().map(String::as_str).collect();
        assert_eq!(modules, ["kamino"]);
        assert!(run.modules["kamino"].failed.is_empty());
    }

    #[test]
    fn a_run_that_reports_no_result_or_runs_nothing_is_not_complete() {
        // It did not build.
        assert!(!parse("error[E0425]: cannot find value").complete);
        // It was cut short: what it reported so far is kept.
        let cut_short = parse("test tests::protocols::kamino::round_trips ... ok\n");
        assert!(!cut_short.complete);
        assert_eq!(cut_short.modules["kamino"].passed, 1);
        // It ran, but no protocol test matched.
        let nothing = parse("running 0 tests\n\ntest result: ok. 0 passed; 0 failed\n");
        assert!(!nothing.complete);
    }
}
