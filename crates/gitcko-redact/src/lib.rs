// SPDX-License-Identifier: AGPL-3.0-or-later

//! Redaction for anything Gitcko writes where people or services can read it: logs,
//! errors, run events, telemetry (OBSERVABILITY §6). Two layers:
//!
//! 1. **Known values**: secrets Gitcko loaded itself (API keys, tokens) and local paths
//!    (home, repository root) are registered and replaced exactly.
//! 2. **Shapes**: common credential formats are replaced even when nobody registered
//!    them, e.g. a key echoed back in a provider error.
//!
//! Redaction is the safety net, not the design: sinks must still not be handed source,
//! prompts or finding text in the first place. The canaries ([`canaries`]) prove the net
//! holds in every sink's tests.

use std::borrow::Cow;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

const SECRET: &str = "<secret>";

/// Replaces registered values and credential-shaped text.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// `(value, replacement)`, longest value first so a secret is never partly replaced
    /// by a shorter one that is its prefix.
    values: Vec<(String, String)>,
}

impl Redactor {
    pub fn new() -> Redactor {
        Redactor::default()
    }

    /// A secret value to remove wherever it appears. Empty values are ignored: they would
    /// match everywhere.
    pub fn secret(self, value: impl Into<String>) -> Redactor {
        self.with(value.into(), SECRET.to_string())
    }

    /// A local path to show as `<label>`, e.g. the home folder as `<home>`. Both
    /// separator styles are registered, since Windows paths reach logs either way.
    pub fn path(self, path: &Path, label: &str) -> Redactor {
        let shown = path.display().to_string();
        let placeholder = format!("<{label}>");
        self.with(shown.replace('\\', "/"), placeholder.clone())
            .with(shown.replace('/', "\\"), placeholder.clone())
            .with(shown, placeholder)
    }

    fn with(mut self, value: String, replacement: String) -> Redactor {
        if !value.is_empty() && !self.values.iter().any(|(v, _)| *v == value) {
            self.values.push((value, replacement));
            self.values.sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
        }
        self
    }

    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        for (value, replacement) in &self.values {
            if out.contains(value.as_str()) {
                out = Cow::Owned(out.replace(value.as_str(), replacement));
            }
        }
        for (shape, replacement) in SHAPES.iter() {
            if shape.is_match(&out) {
                out = Cow::Owned(shape.replace_all(&out, *replacement).into_owned());
            }
        }
        out
    }
}

/// Credential formats, from each provider's published token format. Deliberately few
/// and specific: a pattern that also matches ordinary text would hide real output.
static SHAPES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // PEM private keys, whole block.
        (
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            SECRET,
        ),
        // GitHub tokens: classic (ghp_, gho_, ghu_, ghs_, ghr_) and fine-grained.
        (r"gh[pousr]_[A-Za-z0-9]{36,}", SECRET),
        (r"github_pat_[A-Za-z0-9_]{50,}", SECRET),
        // `sk-` API keys: OpenAI, Anthropic (`sk-ant-`), OpenRouter (`sk-or-`).
        (r"sk-[A-Za-z0-9_-]{20,}", SECRET),
        // AWS access key ids. No word boundaries: a key glued to other text must still go.
        (r"(?:AKIA|ASIA)[0-9A-Z]{16}", SECRET),
        // Slack tokens.
        (r"xox[abposr]-[A-Za-z0-9-]{10,}", SECRET),
        // `Authorization: Bearer …` and similar.
        (r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{16,}", "${1}<secret>"),
        // Credentials inside a URL: keep scheme and host, drop `user:password@`. No word
        // boundaries anywhere: glued to other text, a secret must still go.
        (
            r"(?i)([a-z][a-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@",
            "${1}<secret>@",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| (Regex::new(pattern).expect("valid pattern"), replacement))
    .collect()
});

/// Fake secrets, one per shape above. Tests push them through every sink (errors,
/// logs, events, review JSON) and assert none comes out (TESTING §10, SECURITY §41.8).
///
/// Built at run time, not written out, so this file doesn't trip the repository's own
/// secret scanner. None of them was ever a real credential.
pub fn canaries() -> Vec<String> {
    let run = |c: char, n: usize| c.to_string().repeat(n);
    vec![
        format!(
            "-----BEGIN {}PRIVATE KEY-----\nMIICanary{}\n-----END {}PRIVATE KEY-----",
            "",
            run('Q', 40),
            ""
        ),
        format!("ghp_{}{}", "Canary0", run('x', 33)),
        format!("github_pat_{}_{}", run('C', 22), run('a', 59)),
        format!("sk-{}-{}", "ant", run('C', 40)),
        format!("AKIA{}", "CANARYCANARY0000"),
        format!("xoxb-{}-{}", "000000000", run('c', 24)),
        format!("Bearer {}", run('C', 32)),
        format!(
            "postgres://gitcko:{}@db.example.invalid:5432/app",
            "CanaryPassw0rd"
        ),
    ]
}
