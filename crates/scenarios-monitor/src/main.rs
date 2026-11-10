//! Checks the protocols Surfpool's scenarios integrate against what is live on mainnet.
//!
//! Reads the previous state from `--previous` (empty on the first run) and writes `report/` (this
//! run's folder), `state/` (the next run's baseline) and `verdict` to `--out`. It never writes
//! outside `--out`; publishing the report and the issues is the workflow's job.

mod check;
mod findings;
mod idl;
mod idl_service;
mod report;
mod tests_log;

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use surfpool_types::sanitized_datasource_url;

use crate::{
    check::{IdlResult, IdlState},
    findings::{Finding, Verdict},
    idl_service::IdlService,
    report::Report,
    tests_log::ModuleResult,
};

/// The endpoint the integration tests use. Read only to keep its credentials out of the report.
const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
/// Declares one test module per protocol.
const PROTOCOL_TESTS_MOD: &str = "crates/core/src/tests/protocols/mod.rs";

#[derive(Parser)]
#[command(about = "Checks the protocols Surfpool's scenarios integrate against mainnet")]
struct Cli {
    /// The repository root.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))]
    root: PathBuf,
    /// The run's timestamp, which names its report folder.
    #[arg(long)]
    stamp: String,
    /// A folder holding the previous run's `state/`.
    #[arg(long)]
    previous: PathBuf,
    #[arg(long)]
    out: PathBuf,
    /// The `cargo test` output of the integration tests, when they ran.
    #[arg(long)]
    tests_log: Option<PathBuf>,
    /// Where the report folder will be published, for links in issues.
    #[arg(long)]
    report_url: Option<String>,
    #[arg(long)]
    run_url: Option<String>,
}

#[derive(Serialize)]
struct FindingsFile<'a> {
    stamp: &'a str,
    verdict: Verdict,
    open: &'a [Finding],
    /// Findings whose issues can close.
    resolved: Vec<Resolved>,
    idls: &'a [IdlResult],
    tests: Option<&'a BTreeMap<String, ModuleResult>>,
}

#[derive(Serialize)]
struct Resolved {
    key: String,
    marker: String,
}

fn main() -> Result<()> {
    let args = Cli::parse();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let service = IdlService::new()?;
    let previous = args.previous.join("state");
    let out = &args.out;
    let protocols = check::discover(&args.root)?;

    let previous_idls: BTreeMap<String, IdlState> = check::load_json(&previous.join("idls.json"))?;
    let idls = check::check_idls(&service, &protocols, &previous_idls, &previous, out, &now)?;

    let tests = match &args.tests_log {
        Some(path) => {
            let log = fs::read(path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            // Failure output is published, and the tests' endpoint usually carries an API key.
            let log = match std::env::var(RPC_URL_ENV) {
                Ok(rpc_url) if !rpc_url.is_empty() => without_credentials(&log, &rpc_url),
                _ => log,
            };
            let declared = fs::read_to_string(args.root.join(PROTOCOL_TESTS_MOD))?;
            Some(tests_log::parse(
                &log,
                &tests_log::protocol_modules(&declared),
            ))
        }
        None => None,
    };

    let report_url = args.report_url.as_deref();
    let (mut open, mut resolved) = findings::idl_findings(&idls.results, report_url);
    if let Some(tests) = &tests {
        let (test_open, test_resolved) = findings::test_findings(tests, report_url);
        open.extend(test_open);
        resolved.extend(test_resolved);
    }
    let verdict = findings::decide(&open, &idls.results, tests.as_ref());

    check::write_json(&out.join("state/idls.json"), &idls.state)?;
    // The workflow owns the issue state; it only passes through here.
    let issues = previous.join("issues.json");
    if issues.exists() {
        fs::copy(&issues, out.join("state/issues.json")).context("carrying the issue state")?;
    }
    check::write_json(
        &out.join("report/findings.json"),
        &FindingsFile {
            stamp: &args.stamp,
            verdict,
            open: &open,
            resolved: resolved
                .iter()
                .map(|key| Resolved {
                    key: key.clone(),
                    marker: findings::marker(key),
                })
                .collect(),
            idls: &idls.results,
            tests: tests.as_ref(),
        },
    )?;
    let report = Report {
        stamp: &args.stamp,
        verdict,
        run_url: args.run_url.as_deref(),
        findings: &open,
        idls: &idls.results,
        tests: tests.as_ref(),
    };
    check::write_text(&out.join("report/report.md"), &report.render())?;
    check::write_text(&out.join("verdict"), &format!("{verdict}\n"))?;
    println!("verdict: {verdict}; {} need attention", open.len());
    Ok(())
}

/// The text with every mention of the RPC URL reduced to scheme and host, the form surfpool
/// itself prints a datasource in.
fn without_credentials(text: &str, rpc_url: &str) -> String {
    let shown = sanitized_datasource_url(rpc_url).unwrap_or_else(|| "the datasource".to_owned());
    let text = text.replace(rpc_url, &shown);
    // An HTTP client prints the URL normalised, which can differ from how it was configured.
    match reqwest::Url::parse(rpc_url) {
        Ok(normalized) => text.replace(normalized.as_str(), &shown),
        Err(_) => text,
    }
}

#[cfg(test)]
mod tests {
    use super::without_credentials;

    #[test]
    fn a_key_in_the_query_is_hidden_however_the_url_is_printed() {
        let text = "configured as https://rpc.example?api-key=SECRET, \
                    sent to url (https://rpc.example/?api-key=SECRET), \
                    not ours: https://other.example/?api-key=KEPT";
        assert_eq!(
            without_credentials(text, "https://rpc.example?api-key=SECRET"),
            "configured as https://rpc.example, sent to url (https://rpc.example), \
             not ours: https://other.example/?api-key=KEPT"
        );
    }

    #[test]
    fn a_key_in_the_path_is_hidden() {
        let text = "error sending request for url (https://rpc.example/v2/SECRET)";
        assert_eq!(
            without_credentials(text, "https://rpc.example/v2/SECRET"),
            "error sending request for url (https://rpc.example)"
        );
    }
}
