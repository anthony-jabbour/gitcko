// SPDX-License-Identifier: AGPL-3.0-or-later

//! Files that look like secrets are held back: never read into a prompt, whether or not
//! the change touched them (CLI_SPEC §3.3, THREAT_MODEL TM-20). Holding back applies to
//! local models too, since a local binding may still be remote.

use std::borrow::Cow;

use gitcko_domain::RepoPath;

/// True for a file name that is usually a secret: `.env*`, `*.pem`, `*.key`, `*.p12`,
/// `credentials*`, and SSH keys (`id_rsa`, `id_ed25519.pub`). `id_*` alone would hold
/// back ordinary source such as `id_map.rs`, so it needs no extension or `.pub`.
pub fn is_sensitive_name(path: &RepoPath) -> bool {
    let name = path.as_str().rsplit('/').next().unwrap_or("");
    let name = name.to_ascii_lowercase();
    name.starts_with(".env")
        || name.starts_with("credentials")
        || [".pem", ".key", ".p12"]
            .iter()
            .any(|ext| name.ends_with(ext))
        || name.starts_with("id_") && (!name.contains('.') || name.ends_with(".pub"))
}

/// True when `text` holds something credential-shaped by `gitcko-redact`'s high-confidence
/// patterns (a key, a token, a private key block).
pub fn has_secret(text: &str) -> bool {
    matches!(gitcko_redact::Redactor::new().redact(text), Cow::Owned(_))
}
