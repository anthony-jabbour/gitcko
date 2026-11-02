// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keys for a maintainer's live runs, from the git-ignored `.env` at the repo root, so
//! they needn't pass through shell history. Only named keys are taken: a line like `CI=`
//! must not be able to switch off the refusal to record under CI.
//!
//! This is `xtask` only. The product resolves keys from its real environment and never
//! reads a `.env`, because the repository being reviewed could plant one.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::Result;

const FILE: &str = ".env";
const KEYS: [&str; 2] = ["OPENROUTER_API_KEY", "OLLAMA_HOST"];

/// The named keys in `.env` text. Anything else on a line is ignored and never echoed.
fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let unquoted = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q))
            .unwrap_or(value);
        if KEYS.contains(&key) && !unquoted.is_empty() {
            out.insert(key.to_string(), unquoted.to_string());
        }
    }
    out
}

/// Refuses a `.env` that git tracks: its key is already in history and needs rotating, not
/// using. Anything but a clear "not tracked" from git is a refusal too.
fn refuse_if_tracked(root: &Path) -> Result {
    let status = Command::new("git")
        .args(["ls-files", "--error-unmatch", FILE])
        .current_dir(root)
        .output()
        .map_err(|_| "git can't be run to check .env".to_string())?
        .status;
    match status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(".env is tracked by git: remove it from history and rotate its keys".into()),
        _ => Err("git can't say whether .env is tracked".into()),
    }
}

#[cfg(unix)]
fn refuse_if_shared(file: &Path) -> Result {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(file)
        .map_err(|_| ".env can't be read".to_string())?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(".env is readable by other users: run `chmod 600 .env`".into());
    }
    Ok(())
}

/// Windows has no mode bits, and its ACLs can't be checked here without a new
/// dependency, so a key file is refused rather than trusted unchecked.
#[cfg(not(unix))]
fn refuse_if_shared(_file: &Path) -> Result {
    Err(
        ".env can't be checked for other readers on this OS: set the keys in your shell instead"
            .into(),
    )
}

/// `base` with the keys from `root/.env` added. What's already set wins, and no `.env` at
/// all is fine: the shell or a CI secret may provide everything.
pub(crate) fn with_file(
    mut base: BTreeMap<String, String>,
    root: &Path,
) -> Result<BTreeMap<String, String>> {
    let file = root.join(FILE);
    if !file.exists() {
        return Ok(base);
    }
    refuse_if_tracked(root)?;
    refuse_if_shared(&file)?;
    let text = std::fs::read_to_string(&file).map_err(|_| ".env can't be read".to_string())?;
    for (key, value) in parse(&text) {
        base.entry(key).or_insert(value);
    }
    Ok(base)
}

/// The process environment plus the repo's `.env`.
pub fn load(root: &Path) -> Result<BTreeMap<String, String>> {
    with_file(std::env::vars().collect(), root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitcko_test_support::{TempDir, empty_repo};

    fn fake_key() -> String {
        format!("{}-{}", "canary", "c4f3-dotenv")
    }

    fn write_env(dir: &TempDir, text: &str) {
        let file = dir.path().join(FILE);
        std::fs::write(&file, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn only_named_keys_are_taken_in_every_common_spelling() {
        let key = fake_key();
        for line in [
            format!("OPENROUTER_API_KEY={key}"),
            format!("export OPENROUTER_API_KEY={key}"),
            format!("OPENROUTER_API_KEY=\"{key}\""),
            format!("OPENROUTER_API_KEY='{key}'"),
            format!("  OPENROUTER_API_KEY = {key}  "),
        ] {
            assert_eq!(parse(&line)["OPENROUTER_API_KEY"], key, "{line}");
        }
        let ignored =
            parse("CI=\nHOME=/x\nPATH=/x\n# OPENROUTER_API_KEY=x\nno equals here\nOLLAMA_HOST=\n");
        assert!(ignored.is_empty(), "{ignored:?}");
    }

    // Loading a file is Unix only (see `refuse_if_shared`).
    #[cfg(unix)]
    #[test]
    fn the_shell_wins_over_the_file_and_the_file_fills_the_rest() {
        let dir = empty_repo("dotenv-merge");
        write_env(
            &dir,
            &format!("OPENROUTER_API_KEY={}\nOLLAMA_HOST=from-file\n", fake_key()),
        );
        let base = BTreeMap::from([("OLLAMA_HOST".to_string(), "from-shell".to_string())]);
        let env = with_file(base, dir.path()).unwrap();
        assert_eq!(env["OLLAMA_HOST"], "from-shell");
        assert_eq!(env["OPENROUTER_API_KEY"], fake_key());
    }

    #[test]
    fn no_file_is_fine() {
        let dir = empty_repo("dotenv-none");
        assert!(with_file(BTreeMap::new(), dir.path()).unwrap().is_empty());
    }

    // Loading a file is Unix only (see `refuse_if_shared`).
    #[cfg(unix)]
    #[test]
    fn canary_a_tracked_env_is_refused_without_echoing_it() {
        use gitcko_test_support::git;
        let dir = empty_repo("dotenv-tracked");
        write_env(&dir, &format!("OPENROUTER_API_KEY={}\n", fake_key()));
        // Positive control: untracked, it loads.
        assert!(with_file(BTreeMap::new(), dir.path()).is_ok());
        let added = git(dir.path()).args(["add", "-f", FILE]).status().unwrap();
        assert!(added.success());
        let err = with_file(BTreeMap::new(), dir.path()).unwrap_err();
        assert!(err.contains("tracked"), "{err}");
        assert!(!err.contains(&fake_key()), "{err}");
    }

    #[test]
    fn outside_a_repository_it_refuses_rather_than_assumes() {
        let dir = TempDir::new("dotenv-no-repo");
        write_env(&dir, "OPENROUTER_API_KEY=x\n");
        let err = with_file(BTreeMap::new(), dir.path()).unwrap_err();
        assert!(err.contains("can't say"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn canary_a_file_others_can_read_is_refused_without_echoing_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = empty_repo("dotenv-mode");
        write_env(&dir, &format!("OPENROUTER_API_KEY={}\n", fake_key()));
        let file = dir.path().join(FILE);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let err = with_file(BTreeMap::new(), dir.path()).unwrap_err();
        assert!(err.contains("chmod 600"), "{err}");
        assert!(!err.contains(&fake_key()), "{err}");
    }

    /// Windows has no mode bits to check, so a file is refused there rather than trusted.
    #[cfg(not(unix))]
    #[test]
    fn canary_off_unix_a_file_is_refused_without_echoing_it() {
        let dir = empty_repo("dotenv-windows");
        write_env(&dir, &format!("OPENROUTER_API_KEY={}\n", fake_key()));
        let err = with_file(BTreeMap::new(), dir.path()).unwrap_err();
        assert!(err.contains("shell"), "{err}");
        assert!(!err.contains(&fake_key()), "{err}");
        // Positive control: with no file, the shell alone is fine.
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        assert!(with_file(BTreeMap::new(), dir.path()).is_ok());
    }

    /// The real template: if it carried a value, this is what would load it.
    #[test]
    fn the_committed_template_holds_no_values() {
        let root = gitcko_test_support::workspace_root();
        let text = std::fs::read_to_string(root.join(".env.example")).unwrap();
        assert!(text.contains("OPENROUTER_API_KEY="));
        assert!(parse(&text).is_empty());
    }
}
