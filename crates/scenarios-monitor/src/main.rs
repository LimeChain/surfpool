//! Checks the protocols Surfpool's scenarios integrate against what is live on mainnet.
//!
//! Compares every committed IDL with what its program publishes, reads the integration tests'
//! output, and writes `report.md`, `findings.json`, `verdict` and, for each protocol that differs,
//! its diff and published IDL under `idls/`. It writes to `--out` alone; publishing the report is
//! the workflow's job.

mod check;
mod idl;
mod idl_service;
mod report;
mod tests_log;
mod verdict;

use std::{
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Serialize;
use surfpool_types::sanitized_datasource_url;

use crate::{
    check::IdlResult, idl_service::IdlService, report::Report, tests_log::TestRun, verdict::Verdict,
};

/// The endpoint the integration tests use. Read only to keep its credentials out of the report.
const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
/// How long the IDL service may take for all protocols together. It answers each in a second or
/// two; past this the rest are reported as unchecked, so that a slow service cannot cost the run
/// its report.
const IDL_SERVICE_BUDGET: Duration = Duration::from_secs(5 * 60);

#[derive(Parser)]
#[command(about = "Checks the protocols Surfpool's scenarios integrate against mainnet")]
struct Cli {
    /// The repository root.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))]
    root: PathBuf,
    /// The run's timestamp, which titles its report.
    #[arg(long)]
    stamp: String,
    #[arg(long)]
    out: PathBuf,
    /// The `cargo test` output of the integration tests, when they ran.
    #[arg(long)]
    tests_log: Option<PathBuf>,
    /// The run that publishes the report, for links to it.
    #[arg(long)]
    run_url: Option<String>,
}

#[derive(Serialize)]
struct FindingsFile<'a> {
    stamp: &'a str,
    verdict: Verdict,
    /// One line for each thing that needs attention.
    attention: &'a [String],
    idls: &'a [IdlResult],
    tests: Option<&'a TestRun>,
}

fn main() -> Result<()> {
    let args = Cli::parse();
    let service = IdlService::new()?;
    let out = &args.out;
    // What an earlier run left here would sit beside a report that no longer mentions it.
    match fs::remove_dir_all(out.join("idls")) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(error).context("clearing the output folder's idls/");
        }
        _ => {}
    }
    let protocols = check::discover(&args.root)?;
    let deadline = Instant::now() + IDL_SERVICE_BUDGET;
    let latest = |address: &str| {
        if Instant::now() > deadline {
            bail!("skipped: the IDL service took too long for the protocols before this one");
        }
        service.latest(address)
    };
    let idls = check::check_idls(&latest, &protocols, out)?;

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
            Some(tests_log::parse(&log))
        }
        None => None,
    };

    let attention = verdict::attention(&idls, tests.as_ref());
    let verdict = verdict::decide(&attention, &idls, tests.as_ref());

    check::write_json(
        &out.join("findings.json"),
        &FindingsFile {
            stamp: &args.stamp,
            verdict,
            attention: &attention,
            idls: &idls,
            tests: tests.as_ref(),
        },
    )?;
    let report = Report {
        stamp: &args.stamp,
        verdict,
        run_url: args.run_url.as_deref(),
        attention: &attention,
        idls: &idls,
        tests: tests.as_ref(),
    };
    check::write_text(&out.join("report.md"), &report.render())?;
    check::write_text(&out.join("verdict"), &format!("{verdict}\n"))?;
    println!("verdict: {verdict}; {} need attention", attention.len());
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
