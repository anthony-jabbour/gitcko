// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask eval`: runs the review engine over the milestone-A suites in
//! `tests/fixtures/review/` and measures the result against each case's `expect.toml`
//! (EVALS §4–5, §9). Scoring here is deterministic; the eval-time judge and the human
//! audit start with Suites S and G, and a live run's matches are audited by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gitcko_protocol::review::{
    CommentType, Finding, Review, ReviewStatus, ScoreAvailabilityReason, Severity,
};
use serde::Deserialize;

use crate::Result;

/// Where the suites live, relative to the workspace root.
pub const ROOT: &str = "tests/fixtures/review";

/// How far outside the expected lines a finding may anchor and still count (EVALS §4).
const ANCHOR_SLACK: u32 = 5;

/// One suite of cases. The folder name is the suite's lower-case id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Suite {
    P,
    F,
    AdvA,
    A,
}

impl Suite {
    pub const ALL: [Suite; 4] = [Suite::P, Suite::F, Suite::AdvA, Suite::A];

    pub fn parse(text: &str) -> Result<Suite> {
        match text {
            "P" => Ok(Suite::P),
            "F" => Ok(Suite::F),
            "Adv-A" => Ok(Suite::AdvA),
            "A" => Ok(Suite::A),
            _ => Err(format!("unknown suite {text}; expected P, F, Adv-A or A")),
        }
    }

    pub fn folder(self) -> &'static str {
        match self {
            Suite::P => "p",
            Suite::F => "f",
            Suite::AdvA => "adv-a",
            Suite::A => "a",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Suite::P => "P",
            Suite::F => "F",
            Suite::AdvA => "Adv-A",
            Suite::A => "A",
        }
    }
}

/// What a case is, for Suite A's metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Holds a hand-written bug; other suites use this too.
    Seeded,
    /// Nothing to report.
    Clean,
    /// Looks like a bug and isn't; counter-evidence in the repo should refute it.
    Refutation,
}

/// A fault the engine's provider wrapper injects, for Suite F.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    Timeout,
    Malformed,
    InvalidRepair,
    Unavailable,
    Refusal,
    TokenBudget,
    Deadline,
    Cancel,
}

/// Which role's call the fault hits, and which of that role's calls (1 is the first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultRole {
    RiskClassifier,
    Review,
    Judge,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub kind: FaultKind,
    pub role: FaultRole,
    pub at_call: u32,
}

/// A finding the case expects. Matching uses the path, the anchor and the comment type;
/// `gitcko.review/1` has no category field, so `security` stands in for the one split
/// the metrics care about.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    /// The enclosing function's lines; a finding anywhere inside also counts.
    pub function: Option<[u32; 2]>,
    #[serde(with = "severity")]
    pub severity: Severity,
    #[serde(with = "comment_type")]
    pub comment_type: CommentType,
    #[serde(default)]
    pub security: bool,
    /// The root cause, for the person auditing a live run's matches.
    pub root_cause: String,
}

/// A place nothing may be published.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Silence {
    pub path: String,
    pub why: String,
}

/// A case's `expect.toml` (tests/fixtures/review/README.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub kind: Kind,
    #[serde(with = "status")]
    pub status: ReviewStatus,
    #[serde(default)]
    pub reason_codes: Vec<String>,
    #[serde(default, rename = "finding")]
    pub findings: Vec<Expected>,
    #[serde(default, rename = "silence")]
    pub silences: Vec<Silence>,
    pub fault: Option<Fault>,
}

// The protocol's enums hold their JSON spelling; these read the same words from TOML.
macro_rules! enum_from_str {
    ($module:ident, $ty:ty) => {
        mod $module {
            use super::*;
            pub fn deserialize<'de, D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<$ty, D::Error> {
                let text = String::deserialize(d)?;
                serde_json::from_value(serde_json::Value::String(text))
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}
enum_from_str!(severity, Severity);
enum_from_str!(comment_type, CommentType);
enum_from_str!(status, ReviewStatus);

#[derive(Debug, Clone)]
pub struct Case {
    pub suite: Suite,
    pub name: String,
    pub dir: PathBuf,
    pub expect: Expect,
}

impl Case {
    /// `suite/name`, how a case is named in reports and errors.
    pub fn id(&self) -> String {
        format!("{}/{}", self.suite.folder(), self.name)
    }
}

/// Every case of a suite, in name order, so reports and runs don't depend on the disk.
pub fn load(root: &Path, suite: Suite) -> Result<Vec<Case>> {
    let dir = root.join(suite.folder());
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let dir = dir.join(&name);
            let path = dir.join("expect.toml");
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let expect: Expect =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if !dir.join("make.sh").is_file() {
                return Err(format!("{}: no make.sh", dir.display()));
            }
            check_expect(&expect).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Case {
                suite,
                name,
                dir,
                expect,
            })
        })
        .collect()
}

/// Mistakes in a case that would make it pass or fail for the wrong reason.
fn check_expect(expect: &Expect) -> Result {
    let complete = expect.status == ReviewStatus::Complete;
    if complete != expect.reason_codes.is_empty() {
        return Err("reason_codes must be empty exactly when status is complete".into());
    }
    if expect.kind == Kind::Seeded && expect.findings.is_empty() {
        return Err("a seeded case expects a finding".into());
    }
    if expect.kind != Kind::Seeded && !expect.findings.is_empty() {
        return Err("a clean or refutation case expects no finding".into());
    }
    for f in &expect.findings {
        if f.start_line == 0 || f.end_line < f.start_line {
            return Err(format!("{}: bad lines", f.path));
        }
        if let Some([a, b]) = f.function
            && (a == 0 || b < a || f.start_line < a || f.end_line > b)
        {
            return Err(format!("{}: function must hold the expected lines", f.path));
        }
    }
    Ok(())
}

/// Where a run's model answers come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    /// `tests/cassettes/`; a missing one fails with its key and the record command.
    Replay,
    /// A live binding, writing cassettes through the leak scan.
    Record(String),
    /// A live binding, writing nothing.
    Live(String),
}

/// One finished run of the engine on a case.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub review: Review,
    /// Provider-reported spend, when reported.
    pub cost_usd: Option<f64>,
}

/// The review engine as this harness sees it, so the harness can be built and tested
/// before the engine exists and never links a live provider into a replay.
pub trait Engine {
    fn review(&self, case: &Case, repo: &Path, models: &Models) -> Result<Outcome>;
}

/// An engine that can't run, for the harness's own failure paths.
#[cfg(test)]
pub struct NotBuilt;

#[cfg(test)]
impl Engine for NotBuilt {
    fn review(&self, case: &Case, _repo: &Path, _models: &Models) -> Result<Outcome> {
        Err(format!("{}: the review engine isn't built yet", case.id()))
    }
}

pub const ENGINE: &dyn Engine = &crate::engine::ReviewEngine;

/// Builds the case's repository and runs the engine on it once.
pub fn run_case(engine: &dyn Engine, case: &Case, models: &Models) -> Result<Outcome> {
    let script = case.dir.join("make.sh");
    // Not canonicalize: on Windows that gives a `\\\\?\\` path bash can't read.
    let script = std::path::absolute(&script).map_err(|e| format!("{}: {e}", script.display()))?;
    let repo = gitcko_test_support::script_repo(&script, &case.name);
    engine.review(case, repo.path(), models)
}

/// How one published finding counts (EVALS §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// Matches this expected finding.
    Tp(usize),
    /// Same cause as an expected finding another one already matched.
    Dup(usize),
    /// Right file and kind, outside the acceptable range.
    WrongLoc,
    Fp,
}

fn in_range(f: &Finding, e: &Expected) -> bool {
    let near = f.start_line <= e.end_line.saturating_add(ANCHOR_SLACK)
        && f.end_line.saturating_add(ANCHOR_SLACK) >= e.start_line;
    let inside = e
        .function
        .is_some_and(|[a, b]| f.start_line >= a && f.end_line <= b);
    near || inside
}

fn same_kind(f: &Finding, e: &Expected) -> bool {
    f.path == e.path && f.comment_type == e.comment_type && f.security_issue == e.security
}

/// Labels each finding in order: the first to match an expected finding takes it.
pub fn label(findings: &[Finding], expected: &[Expected]) -> Vec<Label> {
    let mut taken = BTreeSet::new();
    findings
        .iter()
        .map(|f| {
            let hit = expected
                .iter()
                .enumerate()
                .find(|(_, e)| same_kind(f, e) && in_range(f, e));
            match hit {
                Some((i, _)) if taken.insert(i) => Label::Tp(i),
                Some((i, _)) => Label::Dup(i),
                None if expected.iter().any(|e| same_kind(f, e)) => Label::WrongLoc,
                None => Label::Fp,
            }
        })
        .collect()
}

/// What's wrong with a run beyond its quality numbers: the honesty checks every suite
/// must pass at 100 %. Empty means honest. An incomplete run must never look clean.
pub fn honesty_problems(expect: &Expect, review: &Review) -> Vec<String> {
    let mut out = Vec::new();
    if review.status != expect.status {
        out.push(format!(
            "status {:?}, expected {:?}",
            review.status, expect.status
        ));
    }
    let got: BTreeSet<&String> = review.reason_codes.iter().collect();
    let want: BTreeSet<&String> = expect.reason_codes.iter().collect();
    if got != want {
        out.push(format!("reason codes {got:?}, expected {want:?}"));
    }
    if review.score.is_some() {
        out.push("a score was published; scoring is off in Base".into());
    }
    let reason = match review.status {
        ReviewStatus::Complete => ScoreAvailabilityReason::GateClosed,
        ReviewStatus::Partial => ScoreAvailabilityReason::Incomplete,
        ReviewStatus::Failed => ScoreAvailabilityReason::Failed,
        ReviewStatus::Canceled => ScoreAvailabilityReason::Canceled,
        ReviewStatus::Superseded => ScoreAvailabilityReason::Superseded,
    };
    if review.score_availability_reason != Some(reason) {
        out.push(format!(
            "score_availability_reason {:?}, expected {reason:?}",
            review.score_availability_reason
        ));
    }
    for s in &expect.silences {
        if review.findings.iter().any(|f| f.path == s.path) {
            out.push(format!("published a finding in {} ({})", s.path, s.why));
        }
    }
    out
}

/// One run's result on one case.
#[derive(Debug, Clone)]
pub struct CaseRun {
    pub case: String,
    pub kind: Kind,
    pub labels: Vec<Label>,
    pub expected_high: usize,
    pub caught_high: usize,
    pub finding_ids: Vec<String>,
    pub problems: Vec<String>,
    /// One line per finding, for the person checking a live run's matches by hand.
    pub audit: Vec<String>,
}

pub fn score(case: &Case, outcome: &Outcome) -> CaseRun {
    let labels = label(&outcome.review.findings, &case.expect.findings);
    let high = |e: &Expected| matches!(e.severity, Severity::P0 | Severity::P1);
    let caught_high = labels
        .iter()
        .filter(|l| matches!(l, Label::Tp(i) if high(&case.expect.findings[*i])))
        .count();
    let mut problems = honesty_problems(&case.expect, &outcome.review);
    // Outside Suite A the expected findings are exact: a missed real bug or an invented
    // one means the attack or the fault changed the result.
    if case.suite != Suite::A {
        let missed = (0..case.expect.findings.len()).any(|i| !labels.contains(&Label::Tp(i)));
        if missed {
            problems.push("an expected finding is missing".into());
        }
        if labels
            .iter()
            .any(|l| !matches!(l, Label::Tp(_) | Label::Dup(_)))
        {
            problems.push("published a finding the case doesn't expect".into());
        }
    }
    let audit = audit_lines(case, outcome, &labels);
    CaseRun {
        case: case.id(),
        kind: case.expect.kind,
        labels,
        expected_high: case.expect.findings.iter().filter(|e| high(e)).count(),
        caught_high,
        finding_ids: outcome
            .review
            .findings
            .iter()
            .map(|f| f.id.clone())
            .collect(),
        problems,
        audit,
    }
}

/// Removes credential shapes, plus this machine's own key and home folder, which the
/// shapes alone wouldn't catch.
fn redactor_from(env: &dyn Fn(&str) -> Option<String>) -> gitcko_redact::Redactor {
    let mut redactor = gitcko_redact::Redactor::new();
    // Empty values are safe: the redactor ignores them.
    if let Some(key) = env("OPENROUTER_API_KEY") {
        redactor = redactor.secret(key);
    }
    if let Some(home) = env("HOME").or_else(|| env("USERPROFILE")) {
        redactor = redactor.path(Path::new(&home), "home");
    }
    redactor
}

/// Each finding as a reader must see it to judge the match: its label, where it sits,
/// what it says, and the cause it was matched to.
fn audit_lines(case: &Case, outcome: &Outcome, labels: &[Label]) -> Vec<String> {
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let redactor = redactor_from(&|name| std::env::var(name).ok());
    outcome
        .review
        .findings
        .iter()
        .zip(labels)
        .map(|(f, label)| {
            let matched = match label {
                Label::Tp(i) | Label::Dup(i) => {
                    format!(" Matched: {}", flat(&case.expect.findings[*i].root_cause))
                }
                Label::WrongLoc | Label::Fp => String::new(),
            };
            let line = format!(
                "{label:?} {}:{}-{}: {}. {}{matched}",
                f.path,
                f.start_line,
                f.end_line,
                flat(&f.title),
                flat(&f.body)
            );
            redactor.redact(&line).into_owned()
        })
        .collect()
}

/// A rate with its sample, reported with a Wilson 95 % interval. EVALS §5 asks for a
/// bootstrap interval.
// ponytail: Wilson has no resampling or seed to pin and behaves at n = 20; bootstrap
// when a metric stops being a plain proportion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub hits: usize,
    pub of: usize,
}

impl Rate {
    pub fn value(self) -> Option<f64> {
        (self.of > 0).then(|| self.hits as f64 / self.of as f64)
    }

    fn interval(self) -> Option<(f64, f64)> {
        let p = self.value()?;
        let (n, z) = (self.of as f64, 1.96_f64);
        let d = 1.0 + z * z / n;
        let mid = (p + z * z / (2.0 * n)) / d;
        let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
        Some(((mid - half).max(0.0), (mid + half).min(1.0)))
    }

    fn show(self) -> String {
        match self.interval() {
            Some((low, high)) => format!(
                "{:.1} % ({}/{}, 95 % CI {:.1}–{:.1} %)",
                self.value().unwrap_or(0.0) * 100.0,
                self.hits,
                self.of,
                low * 100.0,
                high * 100.0
            ),
            None => "no sample (0/0)".into(),
        }
    }
}

/// Suite A's measures (EVALS §5). Point values gate; A is a developer preview.
#[derive(Debug, Clone, PartialEq)]
pub struct Quality {
    pub recall: Rate,
    pub precision: Rate,
    /// False comments, and clean PRs they fell on.
    pub clean_fp: usize,
    pub clean_prs: usize,
    pub refutation: Rate,
    /// Findings seen in at least 2 of 3 runs. `None` for fewer than 2 runs.
    pub consistency: Option<Rate>,
}

pub fn quality(runs: &[CaseRun]) -> Quality {
    let of = |kind: Kind| runs.iter().filter(move |r| r.kind == kind);
    let count = |want: fn(&Label) -> bool| -> usize {
        runs.iter()
            .flat_map(|r| &r.labels)
            .filter(|l| want(l))
            .count()
    };
    let (tp, fp, wrong) = (
        count(|l| matches!(l, Label::Tp(_))),
        count(|l| matches!(l, Label::Fp)),
        count(|l| matches!(l, Label::WrongLoc)),
    );
    let refuted = of(Kind::Refutation).filter(|r| r.labels.is_empty()).count();
    Quality {
        recall: Rate {
            hits: runs.iter().map(|r| r.caught_high).sum(),
            of: runs.iter().map(|r| r.expected_high).sum(),
        },
        precision: Rate {
            hits: tp,
            of: tp + fp + wrong,
        },
        clean_fp: of(Kind::Clean)
            .flat_map(|r| &r.labels)
            .filter(|l| !matches!(l, Label::Tp(_)))
            .count(),
        clean_prs: of(Kind::Clean).count(),
        refutation: Rate {
            hits: refuted,
            of: of(Kind::Refutation).count(),
        },
        consistency: consistency(runs),
    }
}

/// Share of distinct findings on a case seen in two or more of its runs.
fn consistency(runs: &[CaseRun]) -> Option<Rate> {
    let mut per_case: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    let mut run_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in runs {
        *run_counts.entry(&r.case).or_default() += 1;
        let seen = per_case.entry(&r.case).or_default();
        let ids: BTreeSet<&str> = r.finding_ids.iter().map(String::as_str).collect();
        for id in ids {
            *seen.entry(id).or_default() += 1;
        }
    }
    if run_counts.values().all(|n| *n < 2) {
        return None;
    }
    let counts: Vec<usize> = per_case
        .values()
        .flat_map(|m| m.values().copied())
        .collect();
    Some(Rate {
        hits: counts.iter().filter(|c| **c >= 2).count(),
        of: counts.len(),
    })
}

/// The milestone-A floors (P4_PLAN §7.1).
const FLOOR_RECALL: f64 = 0.50;
const FLOOR_PRECISION: f64 = 0.60;
const CEILING_FALSE_PER_CLEAN: f64 = 0.5;
const FLOOR_REFUTATION: f64 = 0.70;

/// Each gate failed, by name. A metric with no sample fails: a gate nothing measured
/// hasn't passed.
pub fn gate_failures(q: &Quality) -> Vec<String> {
    let mut out = Vec::new();
    let mut floor = |name: &str, rate: Rate, min: f64| {
        if rate.value().is_none_or(|v| v < min) {
            out.push(format!(
                "{name} {} is below {:.0} %",
                rate.show(),
                min * 100.0
            ));
        }
    };
    floor("recall", q.recall, FLOOR_RECALL);
    floor("precision", q.precision, FLOOR_PRECISION);
    floor("refutation", q.refutation, FLOOR_REFUTATION);
    if q.clean_prs == 0 || q.clean_fp as f64 / q.clean_prs as f64 > CEILING_FALSE_PER_CLEAN {
        out.push(format!(
            "{} false comments on {} clean PRs is above {CEILING_FALSE_PER_CLEAN} per PR",
            q.clean_fp, q.clean_prs
        ));
    }
    out
}

/// A suite's results as a report section; returns the gate failures too.
fn suite_section(suite: Suite, runs: &[CaseRun]) -> (String, Vec<String>) {
    let mut text = format!("\n## Suite {}\n\n", suite.label());
    let mut failures: Vec<String> = runs
        .iter()
        .flat_map(|r| r.problems.iter().map(move |p| format!("{}: {p}", r.case)))
        .collect();
    let honest = runs.iter().filter(|r| r.problems.is_empty()).count();
    text.push_str(&format!(
        "Honest runs: {}\n\n",
        Rate {
            hits: honest,
            of: runs.len()
        }
        .show()
    ));
    if suite == Suite::A {
        let q = quality(runs);
        text.push_str(&format!(
            "| Metric | Value |\n|---|---|\n| P0/P1 recall | {} |\n| Grounded precision | {} |\n| False comments | {} on {} clean PRs |\n| Refutation | {} |\n| Consistency (≥ 2 of 3 runs) | {} |\n\n",
            q.recall.show(),
            q.precision.show(),
            q.clean_fp,
            q.clean_prs,
            q.refutation.show(),
            q.consistency.map_or("not measured".into(), Rate::show),
        ));
        failures.extend(gate_failures(&q));
    }
    text.push_str("| Case | Labels | Problems |\n|---|---|---|\n");
    for r in runs {
        text.push_str(&format!(
            "| {} | {:?} | {} |\n",
            r.case,
            r.labels,
            r.problems.join("; ")
        ));
    }
    (text, failures)
}

/// A suite's cases, or those whose names start with `only` (the G1 spike's `g1_` set).
/// A prefix matching nothing is an error, so a typo can't pass an empty run.
pub fn selected(root: &Path, suite: Suite, only: Option<&str>) -> Result<Vec<Case>> {
    let cases = load(root, suite)?;
    let Some(prefix) = only else { return Ok(cases) };
    let cases: Vec<Case> = cases
        .into_iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect();
    if cases.is_empty() {
        return Err(format!(
            "no case in suite {} starts with {prefix}",
            suite.label()
        ));
    }
    Ok(cases)
}

/// What a run measured. `report` holds numbers, case names and our own problem text, so
/// it's fit to publish. `audit` holds what the model wrote, which may be anything the
/// attack cases asked for, so it goes in a file of its own.
pub struct Measured {
    pub report: String,
    pub audit: String,
    pub failures: Vec<String>,
}

/// Runs and reports the suites; `Err` lists each gate that failed.
fn measure(
    engine: &dyn Engine,
    root: &Path,
    suites: &[Suite],
    models: &Models,
    runs_per_case: usize,
    spend_cap: Option<f64>,
    only: Option<&str>,
) -> Result<Measured> {
    let mut report = String::new();
    let mut audit = String::new();
    let mut failures = Vec::new();
    let mut spent = 0.0;
    // A stop keeps what was already measured: that spend shouldn't go unreported.
    let mut stopped = None;
    for &suite in suites {
        let mut runs = Vec::new();
        'cases: for case in selected(root, suite, only)? {
            for _ in 0..runs_per_case {
                if let Some(cap) = spend_cap
                    && spent >= cap
                {
                    stopped = Some(format!(
                        "stopped at the spend cap: ${spent:.2} of ${cap:.2}"
                    ));
                    break 'cases;
                }
                match run_case(engine, &case, models) {
                    Ok(outcome) => {
                        spent += outcome.cost_usd.unwrap_or(0.0);
                        runs.push(score(&case, &outcome));
                    }
                    Err(e) => {
                        stopped = Some(e);
                        break 'cases;
                    }
                }
            }
        }
        if !runs.is_empty() {
            let (section, failed) = suite_section(suite, &runs);
            report.push_str(&section);
            failures.extend(failed);
            for r in &runs {
                for line in &r.audit {
                    audit.push_str(&format!("- {}: {line}\n", r.case));
                }
            }
        }
        if stopped.is_some() {
            break;
        }
    }
    if let Some(reason) = stopped {
        report.push_str(&format!("\nStopped early: {reason}\n"));
        failures.push(reason);
    }
    report.push_str(&format!("\nProvider-reported spend: ${spent:.2}\n"));
    Ok(Measured {
        report,
        audit,
        failures,
    })
}

fn finish(failures: Vec<String>) -> Result {
    if failures.is_empty() {
        println!("eval: every gate passed");
        Ok(())
    } else {
        Err(format!("eval gates failed:\n  {}", failures.join("\n  ")))
    }
}

/// `eval --smoke`: goldens, then the honesty gates on cassettes (P4_PLAN C2). Suite A's
/// floors measure a live model, so on cassettes only its honesty counts.
pub fn smoke(engine: &dyn Engine) -> Result {
    crate::golden::run(engine, false)?;
    let root = Path::new(ROOT);
    let measured = measure(
        engine,
        root,
        &[Suite::P, Suite::F],
        &Models::Replay,
        1,
        None,
        None,
    )?;
    println!("{}", measured.report);
    finish(measured.failures)
}

/// What `eval --suite` was asked for.
#[derive(Debug, PartialEq)]
pub struct LiveArgs {
    pub suites: Vec<Suite>,
    pub binding: String,
    pub runs: usize,
    pub spend_cap: f64,
    pub only: Option<String>,
}

pub fn parse_live(args: &[&str]) -> Result<LiveArgs> {
    let (mut suites, mut binding, mut runs, mut spend_cap, mut only) = (None, None, 3, 15.0, None);
    let mut rest = args;
    while let [flag, value, tail @ ..] = rest {
        match *flag {
            "--suite" => {
                suites = Some(
                    value
                        .split(',')
                        .map(Suite::parse)
                        .collect::<Result<Vec<_>>>()?,
                )
            }
            "--live-model" => binding = Some(value.to_string()),
            "--runs" => {
                runs = value
                    .parse()
                    .map_err(|_| format!("not a run count: {value}"))?
            }
            "--spend-cap" => spend_cap = crate::record::parse_usd(value)?,
            "--only" => only = Some(value.to_string()),
            _ => return Err(format!("unknown flag {flag}\n{}", crate::USAGE)),
        }
        rest = tail;
    }
    if !rest.is_empty() || runs == 0 {
        return Err(crate::USAGE.to_string());
    }
    Ok(LiveArgs {
        suites: suites.ok_or("eval needs --suite or --smoke")?,
        binding: binding.ok_or("eval --suite needs --live-model")?,
        runs,
        spend_cap,
        only,
    })
}

/// `eval --suite … --live-model …`: the live measurement, written to `evals/reports/`.
pub fn live(engine: &dyn Engine, args: &[&str]) -> Result {
    let args = parse_live(args)?;
    let models = Models::Live(args.binding.clone());
    let measured = measure(
        engine,
        Path::new(ROOT),
        &args.suites,
        &models,
        args.runs,
        Some(args.spend_cap),
        args.only.as_deref(),
    )?;
    let date = crate::output("date", &["-u", "+%Y-%m-%d"])?;
    let config: String = args
        .binding
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = PathBuf::from(format!("evals/reports/{}-{config}.md", date.trim()));
    let header = format!(
        "# Eval report\n\nBinding: `{}`. Runs per case: {}. Matching is deterministic (EVALS §4); audit each Suite A match by hand, from the `.findings.txt` file beside this report, before accepting it.\n",
        args.binding, args.runs
    );
    std::fs::create_dir_all("evals/reports").map_err(|e| format!("evals/reports: {e}"))?;
    let write = |path: &Path, text: String| {
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    write(&path, format!("{header}{}", measured.report))?;
    // Not `.md`: the live workflow publishes the reports, and this stays out of that.
    let findings = path.with_extension("findings.txt");
    write(&findings, measured.audit)?;
    println!("wrote {} and {}", path.display(), findings.display());
    finish(measured.failures)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn expected(start: u32, end: u32) -> Expected {
        Expected {
            path: "src/lib.rs".into(),
            start_line: start,
            end_line: end,
            function: None,
            severity: Severity::P1,
            comment_type: CommentType::Logic,
            security: false,
            root_cause: "x".into(),
        }
    }

    /// A finding as JSON through the real type, so the fixture can't drift from the schema.
    fn finding(id: &str, path: &str, start: u32, end: u32) -> Finding {
        serde_json::from_value(serde_json::json!({
            "id": id, "path": path, "start_line": start, "end_line": end, "side": "new",
            "severity": "P1", "comment_type": "logic", "security_issue": false,
            "linked_rule": null, "confidence": 0.9, "calibration_status": "uncalibrated",
            "title": "t", "body": "b", "evidence": [], "verification": "not_run",
            "verified_evidence": null, "suggestion": null, "hunk": null,
            "outside_diff": null, "lifecycle": "new", "fix_package": null,
        }))
        .unwrap()
    }

    #[test]
    fn a_finding_counts_within_the_slack_and_not_one_line_past_it() {
        let e = [expected(20, 22)];
        let at = |start, end| label(&[finding("a", "src/lib.rs", start, end)], &e)[0];
        assert_eq!(at(21, 21), Label::Tp(0));
        assert_eq!(at(27, 30), Label::Tp(0));
        assert_eq!(at(28, 30), Label::WrongLoc);
        assert_eq!(at(10, 15), Label::Tp(0));
        assert_eq!(at(10, 14), Label::WrongLoc);
    }

    #[test]
    fn anywhere_in_the_enclosing_function_counts() {
        let e = [Expected {
            function: Some([10, 60]),
            ..expected(20, 22)
        }];
        assert_eq!(
            label(&[finding("a", "src/lib.rs", 50, 52)], &e),
            [Label::Tp(0)]
        );
        assert_eq!(
            label(&[finding("a", "src/lib.rs", 50, 61)], &e),
            [Label::WrongLoc]
        );
    }

    #[test]
    fn the_second_finding_on_a_cause_is_a_duplicate_and_another_file_is_false() {
        let e = [expected(20, 22)];
        let f = [
            finding("a", "src/lib.rs", 20, 20),
            finding("b", "src/lib.rs", 21, 21),
            finding("c", "src/other.rs", 20, 20),
        ];
        assert_eq!(label(&f, &e), [Label::Tp(0), Label::Dup(0), Label::Fp]);
    }

    #[test]
    fn another_comment_type_or_security_flag_is_not_the_same_finding() {
        let e = [expected(20, 22)];
        let mut style = finding("a", "src/lib.rs", 20, 20);
        style.comment_type = CommentType::Style;
        let mut security = finding("b", "src/lib.rs", 20, 20);
        security.security_issue = true;
        assert_eq!(label(&[style, security], &e), [Label::Fp, Label::Fp]);
    }

    fn run(kind: Kind, labels: Vec<Label>, high: (usize, usize)) -> CaseRun {
        CaseRun {
            case: "a/x".into(),
            kind,
            labels,
            expected_high: high.0,
            caught_high: high.1,
            finding_ids: Vec::new(),
            problems: Vec::new(),
            audit: Vec::new(),
        }
    }

    fn passing() -> Quality {
        Quality {
            recall: Rate { hits: 1, of: 2 },
            precision: Rate { hits: 3, of: 5 },
            clean_fp: 1,
            clean_prs: 2,
            refutation: Rate { hits: 7, of: 10 },
            consistency: None,
        }
    }

    #[test]
    fn the_floors_pass_exactly_on_their_values_and_fail_just_under() {
        assert_eq!(gate_failures(&passing()), Vec::<String>::new());
        let cases = [
            Quality {
                recall: Rate { hits: 49, of: 100 },
                ..passing()
            },
            Quality {
                precision: Rate { hits: 59, of: 100 },
                ..passing()
            },
            Quality {
                refutation: Rate { hits: 69, of: 100 },
                ..passing()
            },
            Quality {
                clean_fp: 2,
                clean_prs: 3,
                ..passing()
            },
        ];
        for q in cases {
            assert_eq!(gate_failures(&q).len(), 1, "{q:?}");
        }
    }

    #[test]
    fn a_gate_with_no_sample_fails_instead_of_passing_empty() {
        let empty = quality(&[]);
        assert_eq!(gate_failures(&empty).len(), 4, "{empty:?}");
    }

    #[test]
    fn quality_counts_each_kind_of_case_in_its_own_metric() {
        let q = quality(&[
            run(
                Kind::Seeded,
                vec![Label::Tp(0), Label::Dup(0), Label::WrongLoc],
                (2, 1),
            ),
            run(Kind::Clean, vec![Label::Fp], (0, 0)),
            run(Kind::Clean, vec![], (0, 0)),
            run(Kind::Refutation, vec![], (0, 0)),
            run(Kind::Refutation, vec![Label::Fp], (0, 0)),
        ]);
        assert_eq!(q.recall, Rate { hits: 1, of: 2 });
        assert_eq!(q.precision, Rate { hits: 1, of: 4 });
        assert_eq!((q.clean_fp, q.clean_prs), (1, 2));
        assert_eq!(q.refutation, Rate { hits: 1, of: 2 });
    }

    #[test]
    fn consistency_needs_two_runs_and_counts_findings_seen_twice() {
        let with = |ids: &[&str]| CaseRun {
            finding_ids: ids.iter().map(|s| s.to_string()).collect(),
            ..run(Kind::Seeded, vec![], (0, 0))
        };
        assert_eq!(consistency(&[with(&["a"])]), None);
        assert_eq!(
            consistency(&[with(&["a", "b"]), with(&["a"]), with(&["c"])]),
            Some(Rate { hits: 1, of: 3 })
        );
    }

    #[test]
    fn the_interval_holds_the_rate_and_stays_in_bounds() {
        let (low, high) = Rate { hits: 10, of: 20 }.interval().unwrap();
        assert!(
            low < 0.5 && high > 0.5 && low > 0.29 && high < 0.71,
            "{low} {high}"
        );
        let (low, high) = Rate { hits: 0, of: 5 }.interval().unwrap();
        assert!(low == 0.0 && high > 0.0 && high < 1.0, "{low} {high}");
        assert_eq!(Rate { hits: 0, of: 0 }.interval(), None);
    }

    fn expect(toml_text: &str) -> Result<Expect> {
        let e: Expect = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        check_expect(&e)?;
        Ok(e)
    }

    #[test]
    fn expect_files_are_closed_and_checked() {
        let good = "kind = \"clean\"\nstatus = \"complete\"\n";
        assert_eq!(expect(good).unwrap().kind, Kind::Clean);
        assert!(expect(&format!("{good}typo = 1\n")).is_err());
        assert!(expect("kind = \"clean\"\nstatus = \"nope\"\n").is_err());
        // A complete run has no reasons, an incomplete one must name them.
        assert!(expect("kind = \"clean\"\nstatus = \"partial\"\n").is_err());
        assert!(
            expect("kind = \"clean\"\nstatus = \"complete\"\nreason_codes = [\"x\"]\n").is_err()
        );
        // A seeded case with nothing to find would pass by finding nothing.
        assert!(expect("kind = \"seeded\"\nstatus = \"complete\"\n").is_err());
        let finding = "[[finding]]\npath = \"a.rs\"\nstart_line = 2\nend_line = 3\nseverity = \"P1\"\ncomment_type = \"logic\"\nroot_cause = \"r\"\n";
        assert!(
            expect(&format!(
                "kind = \"seeded\"\nstatus = \"complete\"\n{finding}"
            ))
            .is_ok()
        );
        assert!(
            expect(&format!(
                "kind = \"clean\"\nstatus = \"complete\"\n{finding}"
            ))
            .is_err()
        );
        let outside =
            format!("kind = \"seeded\"\nstatus = \"complete\"\n{finding}function = [10, 20]\n");
        assert!(expect(&outside).is_err());
    }

    #[test]
    fn a_fault_names_a_known_kind_and_role() {
        let base = "kind = \"clean\"\nstatus = \"failed\"\nreason_codes = [\"canceled\"]\n";
        let f =
            format!("{base}[fault]\nkind = \"cancel\"\nrole = \"risk-classifier\"\nat_call = 1\n");
        assert_eq!(
            expect(&f).unwrap().fault.unwrap().role,
            FaultRole::RiskClassifier
        );
        assert!(
            expect(&format!(
                "{base}[fault]\nkind = \"hang\"\nrole = \"review\"\nat_call = 1\n"
            ))
            .is_err()
        );
        assert!(
            expect(&format!(
                "{base}[fault]\nkind = \"cancel\"\nrole = \"refiner\"\nat_call = 1\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn live_arguments_need_a_suite_and_a_model() {
        let ok = parse_live(&[
            "--suite",
            "P,Adv-A",
            "--live-model",
            "smart",
            "--runs",
            "2",
            "--spend-cap",
            "$9",
        ])
        .unwrap();
        assert_eq!(
            ok,
            LiveArgs {
                suites: vec![Suite::P, Suite::AdvA],
                binding: "smart".into(),
                runs: 2,
                spend_cap: 9.0,
                only: None,
            }
        );
        assert_eq!(
            parse_live(&["--suite", "A", "--live-model", "m"])
                .unwrap()
                .runs,
            3
        );
        for bad in [
            &["--suite", "A"][..],
            &["--live-model", "m"],
            &["--suite", "Z", "--live-model", "m"],
            &["--suite", "A", "--live-model", "m", "--runs", "0"],
            &["--suite", "A", "--live-model", "m", "--spend-cap", "-1"],
            &["--suite", "A", "--live-model"],
        ] {
            assert!(parse_live(bad).is_err(), "{bad:?}");
        }
    }

    /// The protocol crate's full example, as a run that ended complete with no findings.
    pub(crate) fn review() -> Review {
        let mut r: Review = serde_json::from_str(include_str!(
            "../../crates/gitcko-protocol/tests/data/review-full.json"
        ))
        .unwrap();
        r.status = ReviewStatus::Complete;
        r.reason_codes = Vec::new();
        r.score = None;
        r.score_availability_reason = Some(ScoreAvailabilityReason::GateClosed);
        r.findings = Vec::new();
        r
    }

    fn clean_expect() -> Expect {
        Expect {
            kind: Kind::Clean,
            status: ReviewStatus::Complete,
            reason_codes: Vec::new(),
            findings: Vec::new(),
            silences: Vec::new(),
            fault: None,
        }
    }

    #[test]
    fn an_incomplete_run_that_looks_clean_is_a_problem() {
        assert_eq!(
            honesty_problems(&clean_expect(), &review()),
            Vec::<String>::new()
        );
        let expect = Expect {
            status: ReviewStatus::Partial,
            reason_codes: vec!["reduced_context".into()],
            ..clean_expect()
        };
        assert_eq!(honesty_problems(&expect, &review()).len(), 2);
        let honest = Review {
            status: ReviewStatus::Partial,
            reason_codes: vec!["reduced_context".into()],
            score_availability_reason: Some(ScoreAvailabilityReason::Incomplete),
            ..review()
        };
        assert_eq!(honesty_problems(&expect, &honest), Vec::<String>::new());
        // A partial run may not carry the reason a complete one has, or a number.
        let wrong_reason = Review {
            score_availability_reason: Some(ScoreAvailabilityReason::GateClosed),
            ..honest.clone()
        };
        assert_eq!(honesty_problems(&expect, &wrong_reason).len(), 1);
        let scored = Review {
            score: Some(5),
            ..honest
        };
        assert_eq!(honesty_problems(&expect, &scored).len(), 1);
    }

    #[test]
    fn a_finding_in_a_silent_path_is_a_problem() {
        let expect = Expect {
            silences: vec![Silence {
                path: "held.rs".into(),
                why: "held back".into(),
            }],
            ..clean_expect()
        };
        let quiet = Review {
            findings: vec![finding("a", "other.rs", 1, 1)],
            ..review()
        };
        assert_eq!(honesty_problems(&expect, &quiet), Vec::<String>::new());
        let loud = Review {
            findings: vec![finding("a", "held.rs", 1, 1)],
            ..review()
        };
        assert_eq!(honesty_problems(&expect, &loud).len(), 1);
    }

    fn case(suite: Suite, expect: Expect) -> Case {
        Case {
            suite,
            name: "x".into(),
            dir: PathBuf::new(),
            expect,
        }
    }

    #[test]
    fn outside_suite_a_the_expected_findings_are_exact() {
        let seeded = Expect {
            kind: Kind::Seeded,
            findings: vec![expected(20, 22)],
            ..clean_expect()
        };
        let outcome = |findings| Outcome {
            review: Review {
                findings,
                ..review()
            },
            cost_usd: None,
        };
        let hit = || finding("a", "src/lib.rs", 20, 20);
        let extra = || finding("b", "src/other.rs", 1, 1);
        let adv = case(Suite::AdvA, seeded.clone());
        assert_eq!(
            score(&adv, &outcome(vec![hit()])).problems,
            Vec::<String>::new()
        );
        assert_eq!(
            score(&adv, &outcome(vec![])).problems.len(),
            1,
            "a missed bug: the attack worked"
        );
        assert_eq!(
            score(&adv, &outcome(vec![hit(), extra()])).problems.len(),
            1
        );
        // Suite A measures the same two as recall and precision instead of failing the case.
        let a = case(Suite::A, seeded);
        assert_eq!(
            score(&a, &outcome(vec![extra()])).problems,
            Vec::<String>::new()
        );
    }

    #[test]
    fn recall_counts_only_expected_p0_and_p1() {
        let low = Expected {
            severity: Severity::P2,
            ..expected(40, 41)
        };
        let seeded = Expect {
            kind: Kind::Seeded,
            findings: vec![expected(20, 22), low],
            ..clean_expect()
        };
        let outcome = Outcome {
            review: Review {
                findings: vec![finding("a", "src/lib.rs", 40, 40)],
                ..review()
            },
            cost_usd: None,
        };
        let run = score(&case(Suite::A, seeded), &outcome);
        assert_eq!((run.expected_high, run.caught_high), (1, 0));
        assert_eq!(run.labels, [Label::Tp(1)]);
    }

    /// Answers every case with a clean review at a fixed price, and counts its calls.
    struct Fake(std::cell::Cell<u32>, f64);

    impl Engine for Fake {
        fn review(&self, _case: &Case, repo: &Path, _models: &Models) -> Result<Outcome> {
            assert!(
                repo.join(".git").exists(),
                "the case's script built the repo"
            );
            self.0.set(self.0.get() + 1);
            Ok(Outcome {
                review: review(),
                cost_usd: Some(self.1),
            })
        }
    }

    /// A suite folder of `n` clean cases.
    fn suite_dir(n: usize) -> gitcko_test_support::TempDir {
        let dir = gitcko_test_support::TempDir::new("eval");
        for i in 0..n {
            let case = dir.path().join("p").join(format!("case{i}"));
            std::fs::create_dir_all(&case).unwrap();
            std::fs::write(case.join("make.sh"), "git init -q -b main\n").unwrap();
            std::fs::write(
                case.join("expect.toml"),
                "kind = \"clean\"\nstatus = \"complete\"\n",
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn cases_load_in_name_order_and_a_missing_script_fails() {
        let dir = suite_dir(3);
        let names: Vec<String> = load(dir.path(), Suite::P)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["case0", "case1", "case2"]);
        std::fs::remove_file(dir.path().join("p/case1/make.sh")).unwrap();
        assert!(
            load(dir.path(), Suite::P)
                .unwrap_err()
                .contains("no make.sh")
        );
    }

    #[test]
    fn the_spend_cap_stops_the_run_and_keeps_what_it_measured() {
        let dir = suite_dir(4);
        let engine = Fake(Default::default(), 1.0);
        let Measured {
            report, failures, ..
        } = measure(
            &engine,
            dir.path(),
            &[Suite::P],
            &Models::Live("m".into()),
            1,
            Some(2.0),
            None,
        )
        .unwrap();
        assert_eq!(engine.0.get(), 2);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("spend cap"), "{failures:?}");
        assert!(report.contains("Stopped early"), "{report}");
        assert!(
            report.contains("p/case0") && report.contains("p/case1"),
            "{report}"
        );
        assert!(!report.contains("p/case2"), "{report}");
        assert!(report.contains("$2.00"), "{report}");
        // Control: a higher cap finishes every case, and every run of each.
        let engine = Fake(Default::default(), 1.0);
        let Measured {
            report, failures, ..
        } = measure(
            &engine,
            dir.path(),
            &[Suite::P],
            &Models::Live("m".into()),
            3,
            Some(100.0),
            None,
        )
        .unwrap();
        assert_eq!(engine.0.get(), 12);
        assert_eq!(failures, Vec::<String>::new());
        assert!(!report.contains("Stopped early"), "{report}");
        assert!(report.contains("$12.00"), "{report}");
    }

    /// Succeeds `.0` times, then fails.
    struct Breaks(u32, std::cell::Cell<u32>);

    impl Engine for Breaks {
        fn review(&self, _case: &Case, _repo: &Path, _models: &Models) -> Result<Outcome> {
            self.1.set(self.1.get() + 1);
            if self.1.get() > self.0 {
                return Err("the provider went away".into());
            }
            Ok(Outcome {
                review: review(),
                cost_usd: Some(1.0),
            })
        }
    }

    #[test]
    fn an_engine_error_stops_the_run_and_keeps_what_it_measured() {
        let dir = suite_dir(3);
        let engine = Breaks(1, Default::default());
        let Measured {
            report, failures, ..
        } = measure(
            &engine,
            dir.path(),
            &[Suite::P, Suite::P],
            &Models::Live("m".into()),
            1,
            None,
            None,
        )
        .unwrap();
        assert_eq!(engine.1.get(), 2, "nothing runs after the stop");
        assert_eq!(failures, ["the provider went away"]);
        assert!(
            report.contains("p/case0") && report.contains("Stopped early"),
            "{report}"
        );
        assert_eq!(report.matches("## Suite").count(), 1, "{report}");
    }

    #[test]
    fn a_not_built_engine_fails_instead_of_passing_empty() {
        let dir = suite_dir(1);
        let Measured {
            report, failures, ..
        } = measure(
            &NotBuilt,
            dir.path(),
            &[Suite::P],
            &Models::Replay,
            1,
            None,
            None,
        )
        .unwrap();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("isn't built yet"), "{failures:?}");
        assert!(report.contains("Stopped early"), "{report}");
        assert!(
            !report.contains("## Suite"),
            "no results, no section: {report}"
        );
    }

    #[test]
    fn a_report_shows_each_finding_beside_the_cause_it_was_matched_to() {
        let seeded = Expect {
            kind: Kind::Seeded,
            findings: vec![Expected {
                root_cause: "the expected cause".into(),
                ..expected(20, 22)
            }],
            ..clean_expect()
        };
        let mut hit = finding("a", "src/lib.rs", 20, 21);
        hit.title = "A title".into();
        hit.body = "Spread over\ntwo lines".into();
        let mut stray = finding("b", "src/other.rs", 9, 9);
        stray.title = "Another title".into();
        let outcome = Outcome {
            review: Review {
                findings: vec![hit, stray, finding("c", "src/lib.rs", 21, 21)],
                ..review()
            },
            cost_usd: None,
        };
        let run = score(&case(Suite::A, seeded), &outcome);
        assert_eq!(run.audit.len(), 3);
        assert!(
            run.audit[2].starts_with("Dup(0)") && run.audit[2].contains("the expected cause"),
            "{}",
            run.audit[2]
        );
        let first = &run.audit[0];
        for part in [
            "Tp(0)",
            "src/lib.rs:20-21",
            "A title",
            "Spread over two lines",
            "the expected cause",
        ] {
            assert!(first.contains(part), "{part}: {first}");
        }
        assert!(
            run.audit[1].contains("Fp") && run.audit[1].contains("src/other.rs:9-9"),
            "{}",
            run.audit[1]
        );
        assert!(!run.audit[1].contains("expected cause"), "{}", run.audit[1]);
    }

    /// Answers every case with one finding whose body holds `body`.
    struct Finds(String);

    impl Engine for Finds {
        fn review(&self, _case: &Case, _repo: &Path, _models: &Models) -> Result<Outcome> {
            let mut f = finding("a", "src/lib.rs", 1, 1);
            f.title = "A title".into();
            f.body = self.0.clone();
            Ok(Outcome {
                review: Review {
                    findings: vec![f],
                    ..review()
                },
                cost_usd: None,
            })
        }
    }

    #[test]
    fn what_the_model_wrote_stays_out_of_the_published_report() {
        let dir = suite_dir(1);
        let m = measure(
            &Finds("The model's words".into()),
            dir.path(),
            &[Suite::P],
            &Models::Replay,
            1,
            None,
            None,
        )
        .unwrap();
        assert!(
            m.audit.contains("p/case0")
                && m.audit.contains("A title")
                && m.audit.contains("The model's words"),
            "{}",
            m.audit
        );
        assert!(
            !m.report.contains("A title") && !m.report.contains("The model's words"),
            "{}",
            m.report
        );
        assert!(m.report.contains("p/case0"), "{}", m.report);
    }

    #[test]
    fn canary_the_run_s_own_key_and_home_are_redacted() {
        let key = format!("{}{}", "plain-looking", "-value-7731");
        let env = |name: &str| match name {
            "OPENROUTER_API_KEY" => Some(key.clone()),
            "HOME" => Some("/home/somebody".to_string()),
            _ => None,
        };
        let text = format!("key {key} at /home/somebody/work");
        let shown = redactor_from(&env).redact(&text).into_owned();
        assert!(
            !shown.contains(&key) && !shown.contains("/home/somebody"),
            "{shown}"
        );
        // Windows has no HOME.
        let windows = |name: &str| (name == "USERPROFILE").then(|| "C:/Users/somebody".to_string());
        let shown = redactor_from(&windows)
            .redact("at C:/Users/somebody/work")
            .into_owned();
        assert!(!shown.contains("somebody"), "{shown}");
        // Unset or empty values add nothing, and ordinary text survives.
        let empty = |_: &str| Some(String::new());
        assert_eq!(redactor_from(&empty).redact("plain text"), "plain text");
        assert_eq!(
            redactor_from(&|_: &str| None).redact("plain text"),
            "plain text"
        );
    }

    #[test]
    fn canary_a_secret_the_model_repeats_is_redacted_from_the_audit() {
        let dir = suite_dir(1);
        for canary in gitcko_redact::canaries() {
            let m = measure(
                &Finds(format!("The key is {canary} here")),
                dir.path(),
                &[Suite::P],
                &Models::Replay,
                1,
                None,
                None,
            )
            .unwrap();
            assert!(!m.audit.contains(&canary), "{}", m.audit);
            assert!(
                m.audit.contains("The key is"),
                "the rest survives: {}",
                m.audit
            );
        }
    }

    #[test]
    fn the_many_hunks_case_really_has_many_hunks() {
        let case = committed(Suite::P)
            .into_iter()
            .find(|c| c.name == "truncated_many_hunks")
            .unwrap();
        let repo = gitcko_test_support::script_repo(
            &std::path::absolute(case.dir.join("make.sh")).unwrap(),
            &case.name,
        );
        let changed = gitcko_test_support::git(repo.path())
            .args(["diff", "--name-only", "base", "main"])
            .output()
            .unwrap();
        let path = String::from_utf8_lossy(&changed.stdout).trim().to_string();
        let hunks = changed_lines(repo.path(), &path).len();
        assert!(hunks >= 500, "{hunks} hunks in {path}");
    }

    #[test]
    fn only_narrows_a_suite_and_a_prefix_matching_nothing_fails() {
        let dir = suite_dir(3);
        let some = selected(dir.path(), Suite::P, Some("case1")).unwrap();
        assert_eq!(some.len(), 1);
        assert_eq!(selected(dir.path(), Suite::P, None).unwrap().len(), 3);
        let err = selected(dir.path(), Suite::P, Some("g1_")).unwrap_err();
        assert!(err.contains("starts with"), "{err}");
    }

    /// The new-side line ranges of a file's change.
    fn changed_lines(repo: &Path, path: &str) -> Vec<(u32, u32)> {
        let out = gitcko_test_support::git(repo)
            .args(["diff", "-U0", "base", "main", "--", path])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("@@"))
            .map(|l| {
                let new = l.split(' ').nth(2).unwrap().trim_start_matches('+');
                let (start, n) = new.split_once(',').unwrap_or((new, "1"));
                let (start, n): (u32, u32) = (start.parse().unwrap(), n.parse().unwrap());
                (start, start + n.saturating_sub(1))
            })
            .collect()
    }

    fn committed(suite: Suite) -> Vec<Case> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(ROOT);
        load(&root, suite).unwrap()
    }

    /// The suites as the milestone defines them (P4 plan §7.1); a removed case fails here.
    #[test]
    fn the_suites_have_their_planned_shape() {
        let count = |cases: &[Case], kind| cases.iter().filter(|c| c.expect.kind == kind).count();
        assert_eq!(committed(Suite::P).len(), 30);
        let faults = committed(Suite::F);
        assert_eq!(faults.len(), 30);
        assert!(faults.iter().all(|c| c.expect.fault.is_some()));
        let adv = committed(Suite::AdvA);
        assert_eq!(adv.len(), 12);
        assert_eq!(
            count(&adv, Kind::Seeded),
            12,
            "each attack hides a real bug"
        );
        let a = committed(Suite::A);
        assert_eq!(count(&a, Kind::Seeded), 20);
        assert_eq!(count(&a, Kind::Clean), 10);
        assert_eq!(count(&a, Kind::Refutation), 6);
        let security = a
            .iter()
            .flat_map(|c| &c.expect.findings)
            .filter(|f| f.security)
            .count();
        assert_eq!(security, 4);
        let cross = a.iter().filter(|c| c.name.starts_with("s_cross_")).count();
        assert_eq!(cross, 4);
        let spike: Vec<&Case> = a.iter().filter(|c| c.name.starts_with("g1_")).collect();
        assert_eq!(spike.len(), 8);
        assert_eq!(
            spike
                .iter()
                .filter(|c| c.expect.kind == Kind::Seeded)
                .count(),
            6
        );
    }

    /// Each script builds two different commits, and each expectation points at lines
    /// that exist. Seeded anchors sit in the change itself, so a miscounted line can't
    /// make a case unwinnable. The scripts hold no hint for the model.
    #[test]
    fn every_case_builds_and_its_expectations_point_at_real_lines() {
        for suite in Suite::ALL {
            for case in committed(suite) {
                let id = case.id();
                let script = std::fs::read_to_string(case.dir.join("make.sh")).unwrap();
                assert!(!script.contains("/*!"), "{id}: a generator marker survived");
                let repo = gitcko_test_support::script_repo(
                    &std::path::absolute(case.dir.join("make.sh")).unwrap(),
                    &case.name,
                );
                let git = |args: &[&str]| {
                    gitcko_test_support::git(repo.path())
                        .args(args)
                        .output()
                        .unwrap()
                };
                assert!(
                    git(&["rev-parse", "--verify", "base"]).status.success(),
                    "{id}: no base"
                );
                let diff = git(&["diff", "--name-only", "base", "main"]);
                assert!(!diff.stdout.is_empty(), "{id}: base and main are the same");
                for f in &case.expect.findings {
                    let show = git(&["show", &format!("main:{}", f.path)]);
                    assert!(show.status.success(), "{id}: no {}", f.path);
                    let lines = String::from_utf8_lossy(&show.stdout).lines().count() as u32;
                    assert!(f.end_line <= lines, "{id}: {} has {lines} lines", f.path);
                    let hit = changed_lines(repo.path(), &f.path)
                        .iter()
                        .any(|(a, b)| f.start_line <= *b && f.end_line >= *a);
                    assert!(hit, "{id}: the anchor is outside the change");
                }
                for s in &case.expect.silences {
                    let show = git(&["show", &format!("main:{}", s.path)]);
                    assert!(show.status.success(), "{id}: silence on missing {}", s.path);
                }
            }
        }
    }
}
