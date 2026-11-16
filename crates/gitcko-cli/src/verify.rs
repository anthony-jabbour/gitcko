// SPDX-License-Identifier: AGPL-3.0-or-later

//! Minisign and SHA-256 checks for a release (P5_PLAN §5.4, TM-34).
//!
//! The pinned public key is the TEST key in `integrations/install/minisign.pub`.
//! Anthony replaces that file, and the halves in the install scripts, before the
//! first real tag. The release workflow signs with `MINISIGN_SECRET_KEY` from the
//! GitHub `release` environment and then checks the result against this file.
//! A signature that does not verify is a hard failure: nothing is installed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};

/// The public key compiled into this binary. The file's first line is a comment.
pub fn pinned_public_key() -> &'static str {
    include_str!("../../../integrations/install/minisign.pub")
}

/// What `gitcko doctor` should report for the running binary's signature.
/// `detail` is fixed text: a receipt or a signature never gets to be part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorSignature {
    /// `pass` or `fail`.
    pub status: &'static str,
    /// Safe to print. No path, hash, or file bytes.
    pub detail: &'static str,
}

/// Why a signed checksum file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifyFailure {
    /// No signature bytes, or only whitespace.
    MissingSignature,
    /// The signature is not a minisign signature.
    MalformedSignature,
    /// The signature names a different key than the one pinned here.
    WrongKey,
    /// The key id matches and the signature does not.
    BadSignature,
    /// The checksum file is not a list of `hex  name` lines we can trust.
    Checksum,
}

impl VerifyFailure {
    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::MissingSignature => "signature missing",
            Self::MalformedSignature | Self::BadSignature => "signature rejected",
            Self::WrongKey => "signature from a different key",
            Self::Checksum => "checksum mismatch",
        }
    }
}

impl std::fmt::Display for VerifyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

/// SHA-256 of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Verify `signature` over `sums` with the pinned text (a `.pub` file or one base64 line).
pub(crate) fn verify_signature(
    sums: &[u8],
    signature: &[u8],
    public_key: &str,
) -> Result<(), VerifyFailure> {
    if signature.iter().all(u8::is_ascii_whitespace) {
        return Err(VerifyFailure::MissingSignature);
    }
    let key = decode_public_key(public_key)?;
    let text = std::str::from_utf8(signature).map_err(|_| VerifyFailure::MalformedSignature)?;
    let parsed = Signature::decode(text).map_err(map_sig_error)?;
    // `allow_legacy` is false: current minisign signs the prehash (`ED`). A legacy
    // signature is rejected rather than treated as checked.
    key.verify(sums, &parsed, false).map_err(map_sig_error)
}

fn decode_public_key(text: &str) -> Result<PublicKey, VerifyFailure> {
    if text.contains('\n') {
        PublicKey::decode(text)
    } else {
        PublicKey::from_base64(text.trim())
    }
    .map_err(|_| VerifyFailure::MalformedSignature)
}

fn map_sig_error(err: minisign_verify::Error) -> VerifyFailure {
    match err {
        minisign_verify::Error::UnexpectedKeyId => VerifyFailure::WrongKey,
        minisign_verify::Error::InvalidSignature => VerifyFailure::BadSignature,
        minisign_verify::Error::InvalidEncoding
        | minisign_verify::Error::UnexpectedAlgorithm
        | minisign_verify::Error::UnsupportedAlgorithm
        | minisign_verify::Error::UnsupportedLegacyMode
        | minisign_verify::Error::IoError(_) => VerifyFailure::MalformedSignature,
    }
}

/// `name → sha256` from a `sha256sum` file. Duplicate names and junk lines fail closed.
pub(crate) fn parse_sums(bytes: &[u8]) -> Result<BTreeMap<String, [u8; 32]>, VerifyFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| VerifyFailure::Checksum)?;
    let mut out = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(hex_hash) = parts.next() else {
            return Err(VerifyFailure::Checksum);
        };
        let Some(name) = parts.next() else {
            return Err(VerifyFailure::Checksum);
        };
        if parts.next().is_some() {
            return Err(VerifyFailure::Checksum);
        }
        let name = name.strip_prefix('*').unwrap_or(name);
        if !is_release_name(name)
            || hex_hash.len() != 64
            || !hex_hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(VerifyFailure::Checksum);
        }
        let hash = parse_hex(hex_hash).ok_or(VerifyFailure::Checksum)?;
        if out.insert(name.to_string(), hash).is_some() {
            return Err(VerifyFailure::Checksum);
        }
    }
    if out.is_empty() {
        return Err(VerifyFailure::Checksum);
    }
    Ok(out)
}

pub(crate) fn is_release_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn parse_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// A script install's receipt, or the reason it cannot be trusted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Receipt {
    /// No `gitcko-install.txt` beside the binary.
    Absent,
    /// `method=script` and an absolute `release_dir` with no `..`.
    Script(PathBuf),
    /// The file is there and is not a receipt we can use.
    Rejected,
}

const MAX_RECEIPT: u64 = 4096;

/// Read `gitcko-install.txt` next to `exe`. A missing file is [`Receipt::Absent`].
/// Anything else that is not a safe script receipt is [`Receipt::Rejected`], so a
/// broken receipt cannot fall through to "not a script install".
pub(crate) fn read_receipt(exe: &Path) -> Receipt {
    let Some(parent) = exe.parent() else {
        return Receipt::Absent;
    };
    let path = parent.join("gitcko-install.txt");
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(_) => return Receipt::Absent,
    };
    if !meta.is_file() || meta.len() > MAX_RECEIPT {
        return Receipt::Rejected;
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return Receipt::Rejected;
    };
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    let Some(method) = lines.next() else {
        return Receipt::Rejected;
    };
    let Some(dir_line) = lines.next() else {
        return Receipt::Rejected;
    };
    if lines.next().is_some() || method != "method=script" {
        return Receipt::Rejected;
    }
    let Some(dir) = dir_line.strip_prefix("release_dir=") else {
        return Receipt::Rejected;
    };
    if dir.is_empty() || dir.contains('\0') {
        return Receipt::Rejected;
    }
    let release = PathBuf::from(dir);
    if !release.is_absolute()
        || release
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Receipt::Rejected;
    }
    Receipt::Script(release)
}

/// Signature check B3's `doctor` can call. A binary without a script receipt passes
/// with "not a script install": Homebrew and cargo are not signed by this key.
pub fn doctor_signature_check(exe: &Path) -> DoctorSignature {
    if !exe.is_file() {
        return fail("binary unreadable");
    }
    match read_receipt(exe) {
        Receipt::Absent => DoctorSignature {
            status: "pass",
            detail: "not a script install",
        },
        Receipt::Rejected => fail("install receipt rejected"),
        Receipt::Script(dir) => check_release(exe, &dir),
    }
}

fn check_release(exe: &Path, dir: &Path) -> DoctorSignature {
    let Ok(sums) = fs::read(dir.join("SHA256SUMS")) else {
        return fail("signature missing");
    };
    let Ok(signature) = fs::read(dir.join("SHA256SUMS.minisig")) else {
        return fail("signature missing");
    };
    if let Err(err) = verify_signature(&sums, &signature, pinned_public_key()) {
        return fail(err.detail());
    }
    let Ok(files) = parse_sums(&sums) else {
        return fail("checksum mismatch");
    };
    let Ok(bytes) = fs::read(exe) else {
        return fail("binary unreadable");
    };
    let hash = sha256(&bytes);
    if files.values().any(|listed| listed == &hash) {
        DoctorSignature {
            status: "pass",
            detail: "signature ok",
        }
    } else {
        fail("binary hash is not in the signed checksums")
    }
}

fn fail(detail: &'static str) -> DoctorSignature {
    DoctorSignature {
        status: "fail",
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_file(rel: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop();
        path.pop();
        path.push(rel);
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
    }

    fn fixture_signature() -> (Vec<u8>, String) {
        let payload = repo_file("integrations/install/testdata/payload.txt");
        let signature = repo_file("integrations/install/testdata/payload.txt.minisig");
        (payload.into_bytes(), signature)
    }

    #[test]
    fn the_pinned_key_accepts_its_fixture_and_a_good_checksum_round_trips() {
        let (payload, signature) = fixture_signature();
        verify_signature(&payload, signature.as_bytes(), pinned_public_key())
            .expect("the test key signed this payload");
        let body = b"payload-bytes";
        let hash = sha256(body);
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        let parsed =
            parse_sums(format!("{hex}  payload.txt\n").as_bytes()).expect("one checksum line");
        assert_eq!(parsed.get("payload.txt"), Some(&hash));
    }

    #[test]
    fn a_bad_signature_a_wrong_key_and_a_missing_signature_are_refused() {
        let (payload, signature) = fixture_signature();
        verify_signature(&payload, signature.as_bytes(), pinned_public_key()).unwrap();

        let mut flipped: Vec<String> = signature.lines().map(|line| line.to_string()).collect();
        let chars: Vec<char> = flipped[3].chars().collect();
        let mut next = chars.clone();
        next[8] = if chars[8] == 'A' { 'B' } else { 'A' };
        flipped[3] = next.into_iter().collect();
        let bad = flipped.join("\n") + "\n";
        assert_eq!(
            verify_signature(&payload, bad.as_bytes(), pinned_public_key()),
            Err(VerifyFailure::BadSignature)
        );

        let other = sign_with_other_key(&payload);
        assert_eq!(
            verify_signature(&payload, other.as_bytes(), pinned_public_key()),
            Err(VerifyFailure::WrongKey),
            "a signature that belongs to another key is not a bad blob of the pinned key"
        );

        assert_eq!(
            verify_signature(&payload, b"   \n", pinned_public_key()),
            Err(VerifyFailure::MissingSignature)
        );
        assert_eq!(
            verify_signature(&payload, b"", pinned_public_key()),
            Err(VerifyFailure::MissingSignature)
        );
    }

    #[test]
    fn canary_a_bad_checksum_file_does_not_appear_in_the_error() {
        let sums = b"canary_update_secret this is not a checksum\n";
        let err = parse_sums(sums).expect_err("junk is not a checksum file");
        assert_eq!(err, VerifyFailure::Checksum);
        assert!(!err.detail().contains("canary_"));
        let mismatch = VerifyFailure::BadSignature;
        assert!(!mismatch.detail().contains("canary_"));
    }

    #[test]
    fn the_install_scripts_pin_the_same_public_key_as_the_binary() {
        let key_line = pinned_public_key()
            .lines()
            .find(|line| !line.is_empty() && !line.starts_with("untrusted"))
            .expect("key line");
        for script in [
            "integrations/install/install.sh",
            "integrations/install/install.ps1",
        ] {
            let text = repo_file(script);
            let joined = halves(&text);
            assert_eq!(joined, key_line, "{script} drifted from minisign.pub");
        }
    }

    #[test]
    fn the_release_workflow_names_the_environment_secret_and_not_the_test_key() {
        let workflow = repo_file(".github/workflows/release.yml");
        let config = repo_file("dist-workspace.toml");
        assert!(workflow.contains("environment: release"));
        assert!(workflow.contains("secrets.MINISIGN_SECRET_KEY"));
        assert!(!workflow.contains("testdata/minisign.secret"));
        let secret = repo_file("integrations/install/testdata/minisign.secret");
        let secret_line = secret
            .lines()
            .find(|line| !line.starts_with("untrusted"))
            .expect("secret body");
        assert!(!workflow.contains(secret_line));
        for target in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "x86_64-pc-windows-msvc",
        ] {
            assert!(config.contains(target), "{target} missing from dist config");
            assert!(
                workflow.contains(target),
                "{target} missing from release.yml"
            );
        }
        assert!(config.contains("github-attestations = false"));
        assert!(config.contains("cargo-dist-version = \"0.34.0\""));
    }

    fn halves(script: &str) -> String {
        let mut a = None;
        let mut b = None;
        for line in script.lines() {
            if let Some(rest) = line.trim().strip_prefix("pinned_a=") {
                a = Some(unquote(rest));
            }
            if let Some(rest) = line.trim().strip_prefix("pinned_b=") {
                b = Some(unquote(rest));
            }
            if let Some(rest) = line.trim().strip_prefix("$pinnedA = ") {
                a = Some(unquote(rest));
            }
            if let Some(rest) = line.trim().strip_prefix("$pinnedB = ") {
                b = Some(unquote(rest));
            }
        }
        format!("{}{}", a.expect("pinned_a"), b.expect("pinned_b"))
    }

    fn unquote(text: &str) -> String {
        text.trim().trim_matches('\'').trim_matches('"').to_string()
    }

    fn sign_with_other_key(message: &[u8]) -> String {
        let pair = minisign::KeyPair::generate_encrypted_keypair(Some("other-key".to_string()))
            .expect("keypair");
        let signature = minisign::sign(
            None,
            &pair.sk,
            std::io::Cursor::new(message),
            Some("other"),
            None,
        )
        .expect("sign");
        signature.into_string()
    }
}
