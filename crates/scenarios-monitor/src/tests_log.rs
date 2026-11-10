//! Reads the protocol test modules and the output of `cargo test`.

use std::collections::BTreeMap;

use serde::Serialize;

/// The path every protocol test starts with, which is also the filter that runs them all.
const PREFIX: &str = "tests::protocols::";
const MAX_MESSAGE_LINES: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    /// The run reported no result, or ran none of the module's tests.
    Incomplete,
}

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub name: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleResult {
    pub status: Status,
    pub passed: usize,
    pub ignored: usize,
    pub failed: Vec<Failure>,
}

/// The modules `crates/core/src/tests/protocols/mod.rs` declares, one per protocol.
pub fn protocol_modules(protocols_mod: &str) -> Vec<String> {
    protocols_mod
        .lines()
        .filter_map(|line| {
            let declaration = line
                .trim()
                .trim_start_matches("pub(crate)")
                .trim_start_matches("pub")
                .trim_start();
            let name = declaration.strip_prefix("mod ")?.strip_suffix(';')?;
            Some(name.trim().to_owned())
        })
        .collect()
}

/// The protocol module a test belongs to.
pub fn module_of(test_name: &str) -> Option<&str> {
    let (module, _) = test_name.strip_prefix(PREFIX)?.split_once("::")?;
    Some(module)
}

/// Groups a `cargo test` log by integration-test module.
pub fn parse(log: &str, modules: &[String]) -> BTreeMap<String, ModuleResult> {
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

    modules
        .iter()
        .map(|module| {
            let names: Vec<&str> = results
                .keys()
                .copied()
                .filter(|name| module_of(name) == Some(module.as_str()))
                .collect();
            let failed: Vec<Failure> = names
                .iter()
                .filter(|name| results[*name] == "FAILED")
                .map(|name| Failure {
                    name: (*name).to_owned(),
                    message: trim(blocks.get(name).map(Vec::as_slice).unwrap_or_default())
                        .trim()
                        .to_owned(),
                })
                .collect();
            let status = if !summary_seen || names.is_empty() {
                Status::Incomplete
            } else if failed.is_empty() {
                Status::Pass
            } else {
                Status::Fail
            };
            let count = |outcome: &str| {
                names
                    .iter()
                    .filter(|name| results[*name] == outcome)
                    .count()
            };
            (
                module.clone(),
                ModuleResult {
                    status,
                    passed: count("ok"),
                    ignored: count("ignored"),
                    failed,
                },
            )
        })
        .collect()
}

/// `test NAME ... ok`, `... FAILED` or `... ignored[, reason]`.
fn result_line(line: &str) -> Option<(&str, &'static str)> {
    let (name, outcome) = line.strip_prefix("test ")?.split_once(" ... ")?;
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
running 4 tests
test tests::protocols::kamino::round_trips ... ok
test tests::protocols::kamino::reserve_price ... FAILED
test tests::protocols::pump::lifecycle ... FAILED
test tests::protocols::pump::old ... ignored, needs a validator

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


failures:
    tests::protocols::kamino::reserve_price
    tests::protocols::pump::lifecycle

test result: FAILED. 1 passed; 2 failed; 1 ignored; 0 measured; 900 filtered out; finished in 9.00s
";

    fn modules(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn lists_the_protocol_modules() {
        let source = "//! One module per protocol.\n\npub mod kamino;\nmod pump;\n";
        assert_eq!(protocol_modules(source), ["kamino", "pump"]);
        assert_eq!(module_of("tests::protocols::pump::lifecycle"), Some("pump"));
        assert_eq!(module_of("tests::integration::test_simnet_ticks"), None);
    }

    #[test]
    fn groups_results_by_module() {
        let report = parse(LOG, &modules(&["kamino", "pump", "phoenix"]));
        let kamino = &report["kamino"];
        assert_eq!(kamino.status, Status::Fail);
        assert_eq!(kamino.passed, 1);
        assert!(kamino.failed[0].message.contains("left: 1"));
        assert!(!kamino.failed[0].message.contains("RUST_BACKTRACE"));
        // A failure is a failure, whatever its output blames.
        let pump = &report["pump"];
        assert_eq!(pump.status, Status::Fail);
        assert_eq!(pump.ignored, 1);
        assert!(pump.failed[0].message.contains("timed out"));
        // Nothing of phoenix ran, so nothing says it passes.
        assert_eq!(report["phoenix"].status, Status::Incomplete);
    }

    #[test]
    fn a_run_without_a_result_is_incomplete() {
        let report = parse("error[E0425]: cannot find value", &modules(&["kamino"]));
        assert_eq!(report["kamino"].status, Status::Incomplete);
    }
}
