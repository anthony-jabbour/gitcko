// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repository paths and the cache key shared with the CLI.
//!
//! Every tool path is repository-relative. Absolute paths, `..`, and any symlink
//! component are rejected (MCP_SPEC §2, §10). The cache key is SHA-256 of the
//! canonical working directory, the same input `gitcko review` stores under.

use std::path::{Path, PathBuf};

use gitcko_domain::RepoPath;
use sha2::{Digest, Sha256};

/// The path is not a canonical checkout, or it leaves the checkout.
#[derive(Debug)]
pub struct PathReject;

/// SHA-256 hex of the canonical working directory. Matches the CLI's repo cache id.
pub fn root_id(cwd: &Path) -> Result<String, PathReject> {
    let canonical = cwd.canonicalize().map_err(|_| PathReject)?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    Ok(hex(&hasher.finalize()))
}

/// Accept a repository-relative path. `root` is the canonical checkout; `None` skips
/// the filesystem walk (the fuzzer has no checkout).
pub fn check_path(root: Option<&Path>, raw: &str) -> Result<RepoPath, PathReject> {
    if raw.is_empty() || raw.chars().count() > 1024 {
        return Err(PathReject);
    }
    if raw.starts_with('/') || raw.starts_with('\\') || raw.starts_with('~') || raw.contains('\\') {
        return Err(PathReject);
    }
    if raw.contains(':') {
        return Err(PathReject);
    }
    let path = RepoPath::parse(raw).map_err(|_| PathReject)?;
    if let Some(root) = root
        && escapes(root, raw)
    {
        return Err(PathReject);
    }
    Ok(path)
}

/// True when a component is a symlink or the resolved path leaves `root`.
fn escapes(root: &Path, raw: &str) -> bool {
    let Ok(root) = root.canonicalize() else {
        return true;
    };
    let mut cur = root.clone();
    for part in raw.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return true;
        }
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        }
    }
    match cur.canonicalize() {
        Ok(canon) => !canon.starts_with(&root),
        Err(_) => true,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Canonicalize once at startup. A missing directory is left as given so `tools/list`
/// still answers; tool calls then fail closed.
pub fn startup_root(cwd: &Path) -> PathBuf {
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
}
