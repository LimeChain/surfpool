#!/usr/bin/env python3
"""Render the protocol monitoring report.

Reads the JSON each check writes plus the JUnit results of the test runs, and produces one
Markdown document with a fixed set of sections. The structure never changes with the content:
these reports are committed one per run so that consecutive ones can be diffed, and a document
whose headings come and go cannot be.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import xml.etree.ElementTree as ElementTree
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ENV_FAILURE_MARKER = "SURFPOOL_MONITOR_ENV_FAILURE"

CHECK_TITLES = {
    "program-identity": "Program upgrades",
    "idl-document": "IDLs",
    "layout-round-trip": "Layouts",
    "market-deprecation": "Markets",
}

PMM_PROTOCOLS = ("bisonfi", "humidifi", "tessera", "goonfi", "solfi")


@dataclass
class Finding:
    check: str
    severity: str
    protocol: str
    subject: str
    message: str
    observed: dict | None = None

    @property
    def known(self) -> bool:
        return "[known:" in self.message


# Only a transport timeout that reqwest itself reports against loopback counts as environment. A
# plain assertion failure that merely mentions a local URL (e.g. an RPC client logging the address
# it queried) is not matched - that is the protocol under test, not the surfnet under it.
ENVIRONMENT_FAILURE = re.compile(
    r"kind:\s*Reqwest\(.*?127\.0\.0\.1.*?TimedOut|" + ENV_FAILURE_MARKER,
    re.DOTALL,
)


@dataclass
class Failure:
    name: str
    detail: str
    environment: bool


@dataclass
class SuiteResult:
    name: str
    passed: int
    failed: int
    skipped: int
    failures: list[Failure]

    @property
    def real_failures(self) -> list[Failure]:
        return [f for f in self.failures if not f.environment]

    @property
    def environment_failures(self) -> list[Failure]:
        return [f for f in self.failures if f.environment]


def load_findings(directory: Path) -> tuple[list[Finding], set[str]]:
    """Returns the findings and the set of checks that actually produced a file.

    The two are separate on purpose: a check that ran and found nothing is the good outcome, and
    it must not be reported the same way as a check that never ran.
    """
    findings: list[Finding] = []
    ran: set[str] = set()
    for path in sorted(directory.glob("*.json")):
        try:
            document = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        ran.add(document.get("check", path.stem))
        for raw in document.get("findings", []):
            findings.append(
                Finding(
                    check=raw.get("check", document.get("check", "unknown")),
                    severity=raw.get("severity", "info"),
                    protocol=raw.get("protocol", ""),
                    subject=raw.get("subject", ""),
                    message=raw.get("message", ""),
                    observed=raw.get("observed"),
                )
            )
    return findings, ran


def load_suites(paths: list[Path]) -> tuple[list[SuiteResult], bool]:
    """Groups JUnit cases by their protocol module, which is the unit anybody cares about."""
    grouped: dict[str, SuiteResult] = {}
    checks_hit_environment = False
    for path in paths:
        if not path.exists():
            continue
        try:
            root = ElementTree.parse(path).getroot()
        except ElementTree.ParseError:
            continue
        for case in root.iter("testcase"):
            name = case.get("name", "")
            # The drift checks report through their own JSON and the summary table, so they are
            # not listed as suites. But a check that could not reach mainnet panics before it
            # writes any JSON, and its JUnit failure text is then the only place the environment
            # marker survives. Read it before skipping.
            if name.startswith("tests::monitoring::"):
                failure = case.find("failure")
                if failure is None:
                    failure = case.find("error")
                if failure is not None:
                    system_err = case.find("system-err")
                    blob = "\n".join(
                        part
                        for part in (
                            failure.get("message"),
                            failure.text,
                            system_err.text if system_err is not None else None,
                        )
                        if part
                    )
                    if ENV_FAILURE_MARKER in blob:
                        checks_hit_environment = True
                continue
            module = name.split("::")[1] if name.startswith("tests::") else "other"
            suite = grouped.setdefault(module, SuiteResult(module, 0, 0, 0, []))
            # `or` is wrong here: an element with no children is falsy, so a <failure> that
            # carries only a message would be read as a pass.
            failure = case.find("failure")
            if failure is None:
                failure = case.find("error")
            if case.find("skipped") is not None:
                suite.skipped += 1
            elif failure is not None:
                suite.failed += 1
                # nextest puts only the first line of a panic in the `message` attribute; the
                # rest - including the reqwest error that says whether this was a timeout - is in
                # the element text and in <system-err>. Classify on all of it.
                system_err = case.find("system-err")
                raw = "\n".join(
                    part
                    for part in (
                        failure.get("message"),
                        failure.text,
                        system_err.text if system_err is not None else None,
                    )
                    if part
                ).strip()
                first = raw.splitlines()[0] if raw else ""
                suite.failures.append(
                    Failure(
                        name=name,
                        detail=first,
                        environment=bool(ENVIRONMENT_FAILURE.search(raw)),
                    )
                )
            else:
                suite.passed += 1
    return [grouped[key] for key in sorted(grouped)], checks_hit_environment


def bullets(findings: list[Finding]) -> list[str]:
    lines = []
    for finding in findings:
        mark = " _(known)_" if finding.known else ""
        lines.append(f"- **{finding.protocol} / {finding.subject}**{mark} — {finding.message}")
        if finding.observed:
            for key, value in finding.observed.items():
                rendered = value if isinstance(value, str) else json.dumps(value)
                lines.append(f"  - `{key}`: `{rendered}`")
    return lines


def grouped_bullets(findings: list[Finding]) -> list[str]:
    """One bullet per reason, listing what it applies to.

    Forty-eight identical sentences is not information. The reason is the information; the
    subjects are the detail, and they belong on the same line as the reason they share.
    """
    order: list[str] = []
    grouped: dict[str, list[str]] = {}
    for finding in findings:
        reason = finding.message
        if reason not in grouped:
            order.append(reason)
            grouped[reason] = []
        grouped[reason].append(f"`{finding.subject}`")
    lines = []
    for reason in order:
        subjects = grouped[reason]
        lines.append(f"- {reason} — {len(subjects)}: {', '.join(subjects)}")
    return lines


def section(title: str, lines: list[str], empty: str) -> str:
    body = "\n".join(lines) if lines else f"_{empty}_"
    return f"## {title}\n\n{body}\n"


def render(
    findings: list[Finding],
    suites: list[SuiteResult],
    *,
    ran: set[str],
    timestamp: str,
    commit: str,
    run_url: str,
    unverified: bool,
) -> str:
    by_check: dict[str, list[Finding]] = {}
    for finding in findings:
        by_check.setdefault(finding.check, []).append(finding)

    errors = [f for f in findings if f.severity == "error"]
    new_errors = [f for f in errors if not f.known]
    failed_suites = [s for s in suites if s.real_failures]
    environment_suites = [s for s in suites if s.environment_failures]
    missing_checks = [CHECK_TITLES[c] for c in CHECK_TITLES if c not in ran]

    def issue_parts() -> list[str]:
        parts = []
        if new_errors:
            parts.append(f"{len(new_errors)} finding(s) need attention")
        if failed_suites:
            total = sum(len(s.real_failures) for s in failed_suites)
            parts.append(f"{total} test(s) failed in {', '.join(s.name for s in failed_suites)}")
        return parts

    # Confirmed findings come first. A check that could not reach the endpoint says nothing about
    # the protocols it covers, but it must not silence what the other checks did establish.
    caveats = []
    if unverified:
        caveats.append("at least one check could not reach the endpoint and is unverified")
    if missing_checks:
        caveats.append(f"no results from: {', '.join(missing_checks)}")

    if new_errors or failed_suites:
        status = "drift"
        known = len(errors) - len(new_errors)
        tail = f", {known} already known" if known else ""
        verdict = f"**Drift.** {'; '.join(issue_parts())}{tail}."
    elif errors:
        status = "no-new-drift"
        verdict = (
            f"**No new drift.** {len(errors)} known finding(s) still open, nothing new since "
            "the last run."
        )
    elif unverified:
        status = "unverified"
        verdict = (
            "**Unverified.** The endpoint refused reads and no check reported drift, so this run "
            "establishes nothing. Nothing below should be acted on."
        )
    elif missing_checks:
        status = "incomplete"
        verdict = (
            f"**Incomplete.** These checks produced no results: {', '.join(missing_checks)}. "
            "Nothing they cover can be considered verified this run."
        )
    else:
        status = "clean"
        verdict = "**Clean.** Every monitored protocol matches what this repository ships."

    if caveats and status in ("drift", "no-new-drift"):
        verdict += " Coverage is partial: " + "; ".join(caveats) + "."

    out = [f"# Protocol monitoring — {timestamp}\n", verdict + "\n"]

    rows = ["| Check | Errors | Warnings | Notes |", "|---|---|---|---|"]
    for check in ("program-identity", "idl-document", "layout-round-trip", "market-deprecation"):
        counts = Counter(f.severity for f in by_check.get(check, []))
        missing = "" if check in ran else " _(did not run)_"
        rows.append(
            f"| {CHECK_TITLES[check]}{missing} | {counts['error']} | {counts['warn']} "
            f"| {counts['info']} |"
        )
    out.append("\n".join(rows) + "\n")

    identity = by_check.get("program-identity", [])
    moved = [f for f in identity if f.severity in ("error", "warn")]
    lines = bullets(moved)
    out.append(
        section(
            "Program upgrades",
            lines,
            "Every monitored program matches the deployment recorded in the baseline.",
        )
    )

    idl = by_check.get("idl-document", [])
    changed = [f for f in idl if f.severity in ("error", "warn")]
    out.append(
        section(
            "IDLs",
            bullets(changed),
            "Every committed IDL agrees with the one its protocol publishes.",
        )
    )

    pmm = [
        f
        for f in findings
        if any(p.strip() in PMM_PROTOCOLS for p in f.protocol.lower().split(","))
    ]
    pmm_suites = [s for s in suites if s.name in PMM_PROTOCOLS]
    lines = []
    if pmm or pmm_suites:
        lines.extend(bullets([f for f in pmm if f.severity in ("error", "warn")]))
        for suite in pmm_suites:
            lines.append(
                f"- **{suite.name}** — {suite.passed} passed, {suite.failed} failed, "
                f"{suite.skipped} skipped"
            )
    out.append(
        section(
            "PMMs",
            lines,
            "No PMM integration is merged on this branch yet. When one lands it appears here "
            "with its deployment fingerprint and its suite result.",
        )
    )

    layout = [f for f in by_check.get("layout-round-trip", []) if f.severity in ("error", "warn")]
    out.append(
        section(
            "Layouts",
            bullets(layout),
            "Every sampled live account still decodes, re-encodes and matches its declared size.",
        )
    )

    markets = [f for f in by_check.get("market-deprecation", []) if f.severity in ("error", "warn")]
    out.append(
        section(
            "Markets",
            bullets(markets),
            "Every address the templates point at exists and has recent activity.",
        )
    )

    lines = []
    for suite in suites:
        real = len(suite.real_failures)
        environment = len(suite.environment_failures)
        state = "ok" if real == 0 else f"**{real} failed**"
        if environment:
            state += f", {environment} unverified"
        lines.append(
            f"- **{suite.name}** — {state}, {suite.passed} passed, {suite.skipped} skipped"
        )
        for failure in suite.real_failures:
            lines.append(f"  - `{failure.name}` — {failure.detail}")
        for failure in suite.environment_failures:
            lines.append(
                f"  - _unverified_ `{failure.name}` — {failure.detail} "
                "(the endpoint, not the protocol)"
            )
    if environment_suites and not failed_suites:
        lines.append(
            "\nEvery failure above is the endpoint timing out rather than a protocol behaving "
            "differently, so none of them counts as drift."
        )
    out.append(section("Integration tests", lines, "No test results were produced by this run."))

    coverage = [f for f in findings if f.subject == "coverage"]
    info = [f for f in findings if f.severity == "info" and f.subject != "coverage"]
    out.append(
        section(
            "Not covered",
            bullets(coverage) + grouped_bullets(info),
            "Nothing was skipped.",
        )
    )

    out.append(
        section(
            "Run details",
            [
                f"- Commit monitored: `{commit or 'unknown'}`",
                f"- Workflow run: {run_url or 'not run from Actions'}",
                f"- Checks that ran: {len(ran)} of {len(CHECK_TITLES)}",
                f"- Findings: {len(findings)}",
            ],
            "",
        )
    )

    return "\n".join(out), status


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checks-dir", type=Path, default=Path("target/monitoring"))
    parser.add_argument("--junit", type=Path, action="append", default=[])
    parser.add_argument("--out", type=Path, default=Path("report.md"))
    parser.add_argument("--timestamp", default="")
    parser.add_argument("--commit", default="")
    parser.add_argument("--run-url", default="")
    parser.add_argument(
        "--status-out",
        type=Path,
        default=None,
        help="write the one-word verdict (clean, no-new-drift, drift, incomplete, unverified)",
    )
    args = parser.parse_args()

    findings, ran = load_findings(args.checks_dir)
    suites, checks_hit_environment = load_suites(args.junit)

    # The whole run is unverified only when the endpoint refused the reads the checks depend on.
    # A suite that timed out is reported as unverified on its own line without discrediting
    # everything else the run established.
    unverified = checks_hit_environment or any(ENV_FAILURE_MARKER in f.message for f in findings)

    report, status = render(
        findings,
        suites,
        ran=ran,
        timestamp=args.timestamp,
        commit=args.commit,
        run_url=args.run_url,
        unverified=unverified,
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(report)
    if args.status_out:
        args.status_out.parent.mkdir(parents=True, exist_ok=True)
        args.status_out.write_text(status + "\n")
    print(f"wrote {args.out} ({len(report)} bytes, {len(findings)} findings)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
