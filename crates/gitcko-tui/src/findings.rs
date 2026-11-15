// SPDX-License-Identifier: AGPL-3.0-or-later

//! Findings list, detail, coverage, and parity with `gitcko-render`.

use gitcko_protocol::review::{Finding, Review, ReviewStatus, Severity};
use gitcko_render::{finding_card, status_line, text};

/// Findings screen state.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingsPane {
    pub selected: usize,
    pub show_coverage: bool,
    pub show_diff: bool,
    pub scroll: usize,
    /// A review opened from History; the live run uses [`crate::progress::ReviewPane`].
    pub open: Option<Review>,
}

impl FindingsPane {
    pub fn empty() -> FindingsPane {
        FindingsPane {
            selected: 0,
            show_coverage: false,
            show_diff: false,
            scroll: 0,
            open: None,
        }
    }

    pub fn move_by(&mut self, count: usize, delta: i32) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as i32 + delta;
        self.selected = next.clamp(0, count as i32 - 1) as usize;
    }

    pub fn top(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn bottom(&mut self, count: usize) {
        self.selected = count.saturating_sub(1);
    }
}

pub fn active_review<'a>(open: &'a Option<Review>, live: Option<&'a Review>) -> Option<&'a Review> {
    open.as_ref().or(live)
}

pub fn lines(
    pane: &FindingsPane,
    review: Option<&Review>,
    _width: usize,
    color: bool,
) -> Vec<LineSpec> {
    let Some(review) = review else {
        return vec![plain("Run a review or pick one from History.".into(), None)];
    };
    let mut out = Vec::new();
    let status = status_line(review);
    let role = if review.status == ReviewStatus::Complete {
        None
    } else {
        Some(Role::Problem)
    };
    out.push(LineSpec::styled(status, role));
    if pane.show_coverage {
        out.extend(coverage_lines(review));
        return out;
    }
    if review.findings.is_empty() {
        if review.status == ReviewStatus::Complete {
            out.push(plain("No issues found in what was reviewed".into(), None));
            out.extend(coverage_lines(review));
        } else {
            let body = text(review);
            for line in body.lines().skip(1) {
                if line.is_empty() {
                    continue;
                }
                let problem = incomplete_phrase(line);
                out.push(LineSpec::styled(line.to_string(), problem));
            }
        }
        return out;
    }
    let order = sorted_indices(&review.findings);
    out.push(plain(format!("{} finding(s)", order.len()), None));
    for (index, &finding_index) in order.iter().enumerate() {
        let finding = &review.findings[finding_index];
        let marker = if index == pane.selected { ">" } else { " " };
        let sev = severity_marker(finding.severity, color);
        out.push(plain(
            format!(
                "{marker} {sev} {}:{} {}",
                finding.path, finding.start_line, finding.title
            ),
            if index == pane.selected {
                Some(Role::Active)
            } else {
                None
            },
        ));
    }
    if let Some(finding) = selected_finding(pane, review) {
        out.push(plain(String::new(), None));
        for line in finding_card(finding).lines() {
            let problem = incomplete_phrase(line);
            out.push(LineSpec::styled(line.to_string(), problem));
        }
        if pane.show_diff {
            if let Some(hunk) = &finding.hunk {
                out.push(plain("diff".into(), Some(Role::Muted)));
                if !hunk.before.is_empty() {
                    out.push(plain(format!("- {}", hunk.before), None));
                }
                if !hunk.after.is_empty() {
                    out.push(plain(format!("+ {}", hunk.after), None));
                }
            } else {
                out.push(plain("no diff for this finding".into(), Some(Role::Muted)));
            }
        }
        if let Some(suggestion) = &finding.suggestion {
            out.push(plain(format!("fix: {suggestion}"), None));
        }
    }
    out
}

fn coverage_lines(review: &Review) -> Vec<LineSpec> {
    let c = &review.coverage;
    vec![
        plain("coverage".into(), Some(Role::Muted)),
        plain(format!("changed files: {}", c.changed_files), None),
        plain(format!("investigated: {}", c.investigated_files), None),
        plain(
            format!(
                "held back: {}",
                if c.held_back.is_empty() {
                    "none".into()
                } else {
                    c.held_back.join(", ")
                }
            ),
            None,
        ),
        plain(
            format!(
                "skipped binary: {}",
                if c.skipped_binary.is_empty() {
                    "none".into()
                } else {
                    c.skipped_binary.join(", ")
                }
            ),
            None,
        ),
        plain("verify: not_run".into(), None),
    ]
}

fn sorted_indices(findings: &[Finding]) -> Vec<usize> {
    let mut order: Vec<_> = (0..findings.len()).collect();
    order.sort_by_key(|index| findings[*index].severity);
    order
}

fn severity_marker(severity: Severity, _color: bool) -> String {
    let label = match severity {
        Severity::P0 => "P0",
        Severity::P1 => "P1",
        Severity::P2 => "P2",
    };
    format!("[{label}]")
}

fn incomplete_phrase(line: &str) -> Option<Role> {
    if line.contains("Review incomplete") || line.starts_with("status: partial") {
        Some(Role::Problem)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSpec {
    pub text: String,
    pub role: Option<Role>,
}

impl LineSpec {
    fn styled(text: String, role: Option<Role>) -> LineSpec {
        LineSpec { text, role }
    }
}

fn plain(text: String, role: Option<Role>) -> LineSpec {
    LineSpec { text, role }
}

pub use crate::theme::Role;

pub fn selected_finding<'a>(pane: &FindingsPane, review: &'a Review) -> Option<&'a Finding> {
    let order = sorted_indices(&review.findings);
    let index = order.get(pane.selected).copied()?;
    review.findings.get(index)
}

pub fn fix_text(finding: &Finding) -> Option<String> {
    finding
        .fix_package
        .as_ref()
        .and_then(|pkg| serde_json::to_string(pkg).ok())
}
