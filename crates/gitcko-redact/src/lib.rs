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
#[derive(Clone, Default)]
pub struct Redactor {
    /// `(value, replacement)` pairs: secrets and local paths.
    values: Vec<(String, String)>,
}

/// Shows how many values are registered, never the values: printing a `Redactor` in a
/// debug message must not leak what it protects.
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Redactor {{ {} registered values }}", self.values.len())
    }
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
        }
        self
    }

    /// Finds every registered value and every credential shape in the **original** text,
    /// merges spans that overlap, then replaces them in one pass. Replacing one match at
    /// a time would let an earlier replacement break a later match, leaving pieces of a
    /// secret readable (an overlapping second secret, or a registered value inside a
    /// longer token).
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut spans: Vec<(usize, usize, &str)> = Vec::new();
        for (value, replacement) in &self.values {
            let mut from = 0;
            while let Some(offset) = text[from..].find(value.as_str()) {
                let start = from + offset;
                spans.push((start, start + value.len(), replacement));
                // Step one character, not one match, so overlapping repeats are found.
                from = start + value.chars().next().map_or(1, char::len_utf8);
            }
        }
        for shape in SHAPES.iter() {
            for found in shape.captures_iter(text) {
                // A shape may name the part to hide (`secret`) to keep its context,
                // e.g. the scheme and host of a URL.
                let part = found
                    .name("secret")
                    .unwrap_or_else(|| found.get(0).unwrap());
                if !part.is_empty() {
                    spans.push((part.start(), part.end(), SECRET));
                }
            }
        }
        if spans.is_empty() {
            return Cow::Borrowed(text);
        }
        spans.sort_by_key(|&(start, _, _)| start);
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        let mut i = 0;
        while i < spans.len() {
            let (start, mut end, mut replacement) = spans[i];
            i += 1;
            while i < spans.len() && spans[i].0 < end {
                end = end.max(spans[i].1);
                // A merged span that includes any secret is shown as a secret.
                if spans[i].2 == SECRET {
                    replacement = SECRET;
                }
                i += 1;
            }
            out.push_str(&text[copied..start]);
            out.push_str(replacement);
            copied = end;
        }
        out.push_str(&text[copied..]);
        Cow::Owned(out)
    }
}

/// Credential formats, from each provider's published token format. No word boundaries
/// anywhere: glued to other text, a secret must still go. Where hiding a little too
/// much and hiding too little conflict, these hide too much.
static SHAPES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // PEM private keys: the whole block, or everything base64 after BEGIN when the
        // END line is missing (a cut-off log line or error message).
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----|[A-Za-z0-9+/=\s]*)",
        // GitHub tokens: classic (ghp_, gho_, ghu_, ghs_, ghr_) and fine-grained.
        r"gh[pousr]_[A-Za-z0-9]{36,}",
        r"github_pat_[A-Za-z0-9_]{50,}",
        // `sk-` API keys: OpenAI, Anthropic (`sk-ant-`), OpenRouter (`sk-or-`).
        r"sk-[A-Za-z0-9_-]{20,}",
        // AWS access key ids.
        r"(?:AKIA|ASIA)[0-9A-Z]{16}",
        // Slack tokens.
        r"xox[abposr]-[A-Za-z0-9-]{10,}",
        // `Authorization: Bearer …`, any length: short tokens are still credentials. A
        // repeated `Bearer` must not be taken for the token.
        r"(?i)(?:bearer\s+)+(?P<secret>[A-Za-z0-9._~+/=-]+)",
        // Credentials inside a URL: keep the scheme and host, hide everything up to the
        // last `@`, so an empty username, a token with no password, or a password
        // holding `/` or `@` all go. An `@` later in the path may hide part of the path.
        r"(?i)[a-z][a-z0-9+.-]*://(?P<secret>\S*)@",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid pattern"))
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
