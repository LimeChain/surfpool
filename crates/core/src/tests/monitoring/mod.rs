//! Scheduled protocol-drift monitoring.
//!
//! These checks answer one question per integrated protocol: is what we committed still true of
//! what is deployed? They run on a schedule rather than per commit, because the thing they watch
//! changes on the protocol team's clock, not on ours.
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests monitoring -- --test-threads=1
//! ```
//!
//! Each check writes its findings to `target/monitoring/<check>.json` and fails only on
//! [`Severity::Error`]. Warnings and notes are reported without failing, so a protocol shipping a
//! new instruction does not page anyone.
//!
//! A failure caused by the endpoint rather than by the protocol carries
//! [`live::ENV_FAILURE_MARKER`](self::live::ENV_FAILURE_MARKER); the workflow reads a run
//! containing it as unverified rather than as drift.
//!
//! ## What the layout check can and cannot see
//!
//! It does two independent things to one live account.
//!
//! The **round trip** decodes and re-encodes it through the bundled IDL and demands the bytes come
//! back identical. That catches a decode failing outright: a pubkey with too few bytes left, a bool
//! that is neither 0 nor 1, a discriminator the IDL does not know.
//!
//! It does **not** catch every reshaping on its own, and it is worth knowing why. The engine copies
//! back any bytes the IDL did not describe, so a field that shrank shifts every later field yet
//! still reassembles to the same string of bytes. The round trip is blind to that by construction.
//!
//! The **declared size** is the answer to it, in one direction. The chain says how long the account
//! is; the IDL says how long it should be; the two are arrived at independently. When the IDL claims
//! more bytes than exist, nothing can read that account correctly and the check fails. When the
//! account carries more than the IDL describes, the overrides still work - the engine preserves the
//! surplus - so it is a warning saying we model a prefix. Pump's `Global` and `BondingCurve` and
//! PumpSwap's `GlobalConfig` sit there today.
//!
//! What is left uncovered is a same-size reshaping: two fields swapped, or one shrinking while
//! another grows. Nothing here sees that, and nothing cheap would. The `identity` check is what
//! covers it, from the other end: such a change requires a redeploy, and a redeploy is an error on
//! its own.
//!
//! ## What each severity means
//!
//! **Error** — something we depend on is wrong now. The run is red until it is fixed or the
//! deployment is re-reviewed. Known defects stay errors; they are annotated, not downgraded, so the
//! red line is also the list of what is still outstanding.
//!
//! **Warning** — something next to what we depend on moved. A published IDL can run ahead of or
//! behind the program that is deployed, and one stale entry in a list of markets is not the layout
//! having changed, so these are reported rather than failed.
//!
//! **Note** — coverage and context: which templates no protocol account could be sampled for, what
//! the protocol has added that we do not model, which baseline entries no template needs any more.
//!
//! ## When the identity check goes red
//!
//! The bytecode moved, so every offset, every IDL field order and every behavioural expectation for
//! that protocol is unverified. In order:
//!
//! 1. Read the finding. Its `observed` block carries the deployment as it is now, in the shape
//!    of a baseline entry.
//! 2. Run that protocol's own suite. If it passes, the upgrade did not touch what we rely on.
//! 3. If it fails, the integration needs re-establishing. **Never** rewrite offsets to make the
//!    suite green again — re-derive them, then prove the result with a real transaction.
//! 4. Only then copy the observed entry into `baseline.json`, filling in `reviewed_on` and a
//!    `note` saying what was checked. That edit is the claim that somebody looked.
//!
//! ## Adding a protocol
//!
//! An IDL protocol is watched automatically once its templates are registered. A protocol without
//! an IDL is watched once its program has an entry in `baseline.json` with a `label` naming the
//! protocol; until then, the identity check warns every run rather than staying silent about it.
//!
//! `KNOWN_ISSUES`, in `mod.rs`, is the one list maintained by hand: defects already scheduled into
//! a later milestone. These still fail; the entry only marks the finding as old news. An entry
//! that stops matching is reported so it cannot outlive what it described.

pub mod identity;
pub mod idl_document;
pub mod layout;
pub mod markets;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use solana_pubkey::Pubkey;
use surfpool_types::{AccountAddress, PdaSeed};

use crate::scenarios::TemplateRegistry;

/// Findings the team has already seen and placed in a later milestone.
///
/// These do not change the outcome: a known defect still fails the run, because the list of what
/// is still wrong is the point. What an entry adds is a note on the finding saying it is already
/// understood and where it is scheduled, so a run can be read at a glance as "the same two we
/// know about" or "one of these is new".
const KNOWN_ISSUES: &[KnownIssue] = &[
    KnownIssue {
        check: "idl-document",
        protocol: "jupiter",
        subject: "TokenLedger",
        reason: "the bundled Jupiter IDL is inherited and its TokenLedger discriminator is not \
                 the canonical sha256(\"account:TokenLedger\") prefix the program publishes. \
                 Scheduled with the other inherited protocol refreshes; see milestone 3.7.",
    },
    KnownIssue {
        check: "layout-round-trip",
        protocol: "raydium",
        subject: "raydium-amm-custom",
        reason: "AMM v4 is a native program whose accounts carry no Anchor discriminator - an \
                 AmmInfo opens with its u64 status field. The inherited IDL invents \
                 discriminators 0, 1 and 2, so the engine's discriminator lookup matches no live \
                 pool and every AMM v4 template fails against real state. Fixing it needs the \
                 non-discriminator path, which the AMM milestone owns; see milestone 3.4.",
    },
];

pub struct KnownIssue {
    pub check: &'static str,
    pub protocol: &'static str,
    pub subject: &'static str,
    pub reason: &'static str,
}

impl KnownIssue {
    fn matches(&self, finding: &Finding) -> bool {
        self.check == finding.check
            && self.protocol.eq_ignore_ascii_case(&finding.protocol)
            && self.subject == finding.subject
    }
}

/// Mainnet reads for these checks.
///
/// Deliberately private to this module. The protocol suites each carry their own copy of
/// this wrapper; unifying them is not this milestone's business.
pub mod live {
    use solana_account::Account;
    use solana_commitment_config::CommitmentConfig;
    use solana_pubkey::Pubkey;

    use crate::surfnet::remote::SurfnetRemoteClient;

    pub const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
    pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

    /// Printed by every panic caused by the endpoint rather than by the protocol. The monitoring
    /// workflow greps for it to report a run as unverified instead of opening a drift alert, so a
    /// throttled endpoint never reads as a protocol regression.
    pub const ENV_FAILURE_MARKER: &str = "SURFPOOL_MONITOR_ENV_FAILURE";

    pub fn client() -> SurfnetRemoteClient {
        SurfnetRemoteClient::new(endpoint())
    }

    /// The endpoint to read mainnet through, falling back to the public one.
    ///
    /// An unset GitHub secret is not absent, it is present and empty, and `env::var` answers
    /// `Ok("")` for that. Treating empty as unset is what makes the workflow work before anyone has
    /// configured a private endpoint; without it every read fails against an empty URL and the
    /// fallback here never runs.
    pub fn endpoint() -> String {
        match std::env::var(RPC_URL_ENV) {
            Ok(url) if !url.trim().is_empty() => url,
            _ => DEFAULT_RPC_URL.to_string(),
        }
    }

    /// Fetches the accounts in one request, so every account returned is from the same slot.
    ///
    /// A missing account comes back as `None` rather than panicking: for these checks that is a
    /// finding, not a crash. Panics with [`ENV_FAILURE_MARKER`] when the endpoint never answers at
    /// all, which is the workflow's cue to call the run unverified instead of reporting drift.
    pub async fn try_fetch(addresses: &[Pubkey]) -> Vec<Option<Account>> {
        // The public endpoint throttles and intermittently 503s, which has nothing to do with what
        // the callers assert. Retry a few times with backoff so a transient refusal is not read as a
        // failure.
        let mut attempt = 0;
        let mut errors = Vec::new();
        let results = loop {
            match client()
                .get_multiple_accounts(addresses, CommitmentConfig::confirmed())
                .await
            {
                Ok(results) => break results,
                Err(error) if attempt < 4 => {
                    attempt += 1;
                    errors.push(format!("attempt {attempt}: {error}"));
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
                }
                Err(error) => {
                    errors.push(format!("attempt {}: {error}", attempt + 1));
                    panic!(
                        "{ENV_FAILURE_MARKER} failed to fetch {addresses:?} from mainnet after {} \
                         attempts: {}",
                        errors.len(),
                        errors.join("; ")
                    );
                }
            }
        };

        // A short answer is the endpoint's failure, not a finding about the missing addresses.
        // Callers index into this by position, so it has to line up or fail here, with the marker.
        assert!(
            results.len() == addresses.len(),
            "{ENV_FAILURE_MARKER} asked mainnet for {} accounts and got {} back",
            addresses.len(),
            results.len()
        );

        results
            .into_iter()
            .map(|result| result.map_account().ok())
            .collect()
    }

    /// The offsets at which two buffers differ.
    pub fn diff_indices(left: &[u8], right: &[u8]) -> Vec<usize> {
        left.iter()
            .zip(right)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn an_empty_endpoint_falls_back_to_the_public_one() {
        let previous = std::env::var(RPC_URL_ENV).ok();

        unsafe { std::env::set_var(RPC_URL_ENV, "") };
        let empty = endpoint();
        unsafe { std::env::set_var(RPC_URL_ENV, "https://private.example/?api-key=k") };
        let configured = endpoint();

        match previous {
            Some(value) => unsafe { std::env::set_var(RPC_URL_ENV, value) },
            None => unsafe { std::env::remove_var(RPC_URL_ENV) },
        }

        assert_eq!(
            empty, DEFAULT_RPC_URL,
            "an unset GitHub secret arrives as an empty string, and an empty URL reads nothing"
        );
        assert_eq!(configured, "https://private.example/?api-key=k");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Something we depend on changed. The integration is unverified until a human revisits it.
    Error,
    /// Something changed next to what we depend on. Worth a look, not worth a page.
    Warn,
    /// Context: what is covered, what is not, and what the protocol added.
    Info,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    pub protocol: String,
    /// Program id, template id or account address, whichever names the thing that drifted.
    pub subject: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<serde_json::Value>,
}

impl Finding {
    pub fn new(
        check: &'static str,
        severity: Severity,
        protocol: impl Into<String>,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            check,
            severity,
            protocol: protocol.into(),
            subject: subject.into(),
            message: message.into(),
            observed: None,
        }
    }

    pub fn with_observed(mut self, observed: serde_json::Value) -> Self {
        self.observed = Some(observed);
        self
    }
}

/// Collects a check's findings, publishes them for the workflow, and fails the check when any of
/// them is an error.
#[derive(Debug)]
pub struct Report {
    check: &'static str,
    findings: Vec<Finding>,
}

impl Report {
    pub fn new(check: &'static str) -> Self {
        Self {
            check,
            findings: Vec::new(),
        }
    }

    pub fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    pub fn push_with(
        &mut self,
        severity: Severity,
        protocol: impl Into<String>,
        subject: impl Into<String>,
        message: impl Into<String>,
        observed: serde_json::Value,
    ) {
        self.push(
            Finding::new(self.check, severity, protocol, subject, message).with_observed(observed),
        );
    }

    pub fn error(
        &mut self,
        protocol: impl Into<String>,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Finding::new(
            self.check,
            Severity::Error,
            protocol,
            subject,
            message,
        ));
    }

    pub fn warn(
        &mut self,
        protocol: impl Into<String>,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Finding::new(
            self.check,
            Severity::Warn,
            protocol,
            subject,
            message,
        ));
    }

    pub fn info(
        &mut self,
        protocol: impl Into<String>,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Finding::new(
            self.check,
            Severity::Info,
            protocol,
            subject,
            message,
        ));
    }

    /// Writes the findings and panics when any of them is an error.
    ///
    /// Always writes first: a check that found drift must still leave its report behind for the
    /// workflow to render, and a panic would otherwise take the evidence with it.
    pub fn finish(mut self) {
        self.annotate_known_issues();

        let errors: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .collect();
        let summary = format!(
            "{}: {} error(s), {} warning(s), {} note(s)",
            self.check,
            errors.len(),
            self.findings
                .iter()
                .filter(|f| f.severity == Severity::Warn)
                .count(),
            self.findings
                .iter()
                .filter(|f| f.severity == Severity::Info)
                .count(),
        );

        for finding in &self.findings {
            println!(
                "[{:?}] {} / {} - {}",
                finding.severity, finding.protocol, finding.subject, finding.message
            );
        }
        println!("{summary}");

        write_report(self.check, &self.findings);

        assert!(
            errors.is_empty(),
            "{summary}\n{}",
            errors
                .iter()
                .map(|f| format!("  {} / {}: {}", f.protocol, f.subject, f.message))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// Marks findings the team already knows about.
    ///
    /// Deliberately does not change severity. A known defect is still a defect, and the run stays
    /// red until it is fixed; the note only says which of the red lines are old news.
    fn annotate_known_issues(&mut self) {
        for finding in &mut self.findings {
            if let Some(known) = KNOWN_ISSUES.iter().find(|known| known.matches(finding)) {
                finding.message = format!("{} [known: {}]", finding.message, known.reason);
            }
        }
    }
}

fn write_report(check: &str, findings: &[Finding]) {
    let dir = report_dir();
    if let Err(error) = fs::create_dir_all(&dir) {
        println!("could not create {}: {error}", dir.display());
        return;
    }
    let path = dir.join(format!("{check}.json"));
    let body = serde_json::json!({
        "check": check,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "findings": findings,
    });
    match serde_json::to_string_pretty(&body) {
        Ok(text) => {
            if let Err(error) = fs::write(&path, text) {
                println!("could not write {}: {error}", path.display());
            }
        }
        Err(error) => println!("could not serialize the {check} report: {error}"),
    }
}

pub fn report_dir() -> PathBuf {
    match std::env::var("SURFPOOL_MONITORING_REPORT_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => workspace_root().join("target").join("monitoring"),
    }
}

fn workspace_root() -> PathBuf {
    // crates/core -> crates -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two directories below the workspace root")
        .to_path_buf()
}

/// The program ids the monitoring checks watch, mapped to the protocols that depend on them.
///
/// Two sources, unioned. The registry gives what can be derived: a template's IDL names the
/// program whose layout we decode, and a PDA spec names the program we derive against (for Pyth
/// those differ, and only the second can be redeployed under our templates). The baseline gives
/// what cannot be derived: a protocol without an IDL leaves no program id in its templates, so
/// its entry in `baseline.json` - which has to exist anyway, to pin the fingerprint - is what
/// puts it on the list. Nothing in a protocol's own files has to change for it to be watched.
pub fn monitored_programs(
    registry: &TemplateRegistry,
    baseline: &identity::Baseline,
) -> BTreeMap<Pubkey, BTreeSet<String>> {
    let mut programs: BTreeMap<Pubkey, BTreeSet<String>> = BTreeMap::new();
    for template in registry.all() {
        if let Some(program_id) = template
            .idl
            .as_ref()
            .and_then(|idl| Pubkey::from_str(&idl.address).ok())
        {
            programs
                .entry(program_id)
                .or_default()
                .insert(template.protocol.clone());
        }
        for program_id in address_program_ids(&template.address) {
            programs
                .entry(program_id)
                .or_default()
                .insert(template.protocol.clone());
        }
    }
    for entry in &baseline.programs {
        if let Ok(program_id) = Pubkey::from_str(&entry.program_id) {
            let protocols = programs.entry(program_id).or_default();
            for name in entry.label.split(',') {
                protocols.insert(name.trim().to_string());
            }
        }
    }
    programs
}

/// Protocol names are written for people ("SPL Token", "PumpSwap") in templates and as labels in
/// the baseline; both are folded to one spelling before being compared.
pub fn slug(protocol: &str) -> String {
    protocol.trim().to_lowercase().replace([' ', '_'], "-")
}

fn address_program_ids(address: &AccountAddress) -> Vec<Pubkey> {
    match address {
        AccountAddress::Pubkey(_) => Vec::new(),
        AccountAddress::Pda { program_id, seeds } => {
            let mut ids = Vec::new();
            if let Ok(program_id) = Pubkey::from_str(program_id) {
                ids.push(program_id);
            }
            ids.extend(seeds.iter().flat_map(seed_program_ids));
            ids
        }
    }
}

fn seed_program_ids(seed: &PdaSeed) -> Vec<Pubkey> {
    match seed {
        PdaSeed::DerivedPda { program_id, seeds } => {
            let mut ids = Vec::new();
            if let Ok(program_id) = Pubkey::from_str(program_id) {
                ids.push(program_id);
            }
            ids.extend(seeds.iter().flat_map(seed_program_ids));
            ids
        }
        _ => Vec::new(),
    }
}

#[test]
fn monitored_programs_cover_both_the_idl_and_the_derivation_program() {
    let registry = TemplateRegistry::new();
    let programs = monitored_programs(&registry, &identity::baseline());

    // Pyth is the case the derivation source exists for: its IDL names the receiver program while
    // its templates derive price accounts against a different one. Watching only the IDL address
    // would leave the program our templates actually address unmonitored.
    let derivation_program = Pubkey::from_str("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT")
        .expect("a valid pubkey literal");
    assert!(
        programs.contains_key(&derivation_program),
        "the Pyth PDA derivation program is not in the monitored set: {:?}",
        programs.keys().map(|k| k.to_string()).collect::<Vec<_>>()
    );
}

/// A protocol without an IDL leaves no program id in its templates. Its baseline entry is what
/// puts it on the list, and the label is how the guard above knows the protocol is covered.
#[test]
fn a_baseline_entry_is_a_monitored_program() {
    let baseline = identity::Baseline {
        programs: vec![identity::BaselineEntry {
            program_id: "goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE".to_string(),
            label: "goonfi".to_string(),
            loader: String::new(),
            last_deployed_slot: None,
            upgrade_authority: None,
            elf_sha256: String::new(),
            elf_len: 0,
            reviewed_on: String::new(),
            note: String::new(),
        }],
    };
    let programs = monitored_programs(&TemplateRegistry::default(), &baseline);
    let owner = Pubkey::from_str("goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE").expect("pubkey");
    assert_eq!(
        programs
            .get(&owner)
            .map(|p| p.iter().cloned().collect::<Vec<_>>()),
        Some(vec!["goonfi".to_string()]),
        "a baseline entry must reach the identity check on its own: {programs:?}"
    );
}

#[test]
fn a_known_issue_stays_an_error_and_gains_its_note() {
    for known in KNOWN_ISSUES {
        assert!(
            known.reason.len() > 20,
            "{} / {} is listed as known without saying why or where it is scheduled, which makes \
             it indistinguishable from a defect being hidden",
            known.protocol,
            known.subject
        );
    }

    let mut report = Report::new("idl-document");
    report.error("jupiter", "TokenLedger", "the discriminator does not match");
    report.annotate_known_issues();

    let finding = report
        .findings
        .iter()
        .find(|finding| finding.subject == "TokenLedger")
        .expect("the finding survives annotation");
    assert_eq!(
        finding.severity,
        Severity::Error,
        "a known defect is still a defect; the run has to stay red until it is fixed"
    );
    assert!(finding.message.contains("[known:"));
}
