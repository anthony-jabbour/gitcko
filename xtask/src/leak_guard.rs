// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keeps secrets and private hosted-service content out of this public repo.
//! The rules live in `xtask/leak-guard.toml` (a gitleaks config); this file
//! runs gitleaks over the right commits and proves the rules work.

use crate::Result;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;

const CONFIG: &str = "xtask/leak-guard.toml";
/// gitleaks exit code when it finds a leak, distinct from its error exits.
const LEAK_EXIT: i32 = 42;
/// What git sends for a ref that doesn't exist on one side of a push.
const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Scans the commits selected by `log_opts` (git log arguments) in the current repo.
pub fn scan(log_opts: &str) -> Result {
    match gitleaks(Path::new("."), log_opts)? {
        false => {
            println!("leak-guard: clean ({log_opts})");
            Ok(())
        }
        true => Err(format!(
            "leak-guard: blocked, see the findings above ({log_opts})"
        )),
    }
}

/// Runs gitleaks with our config. Returns whether it found a leak.
fn gitleaks(repo: &Path, log_opts: &str) -> Result<bool> {
    // Absolute, because the self-test points gitleaks at repos outside the workspace.
    let config = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(CONFIG);
    let status = Command::new("gitleaks")
        .args([
            "git",
            "--no-banner",
            "--redact",
            "--exit-code",
            &LEAK_EXIT.to_string(),
        ])
        .arg("--config")
        .arg(&config)
        .arg(format!("--log-opts={log_opts}"))
        .arg(repo)
        .status()
        .map_err(|e| {
            format!("could not start gitleaks ({e}); run `cargo xtask setup` for install help")
        })?;
    match status.code() {
        Some(0) => Ok(false),
        Some(LEAK_EXIT) => Ok(true),
        _ => Err(format!("gitleaks failed ({status})")),
    }
}

/// Git pre-push hook: refuse to push anywhere but a repo named `gitcko`, and
/// scan only the commits being pushed (git sends them on stdin).
pub fn pre_push(url: &str) -> Result {
    if repo_name(url) != Some("gitcko") {
        return Err(format!(
            "leak-guard: this checkout is the public `gitcko` repo, but the push goes to {url}"
        ));
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        // <local ref> <local sha> <remote ref> <remote sha>
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [_, local_sha, _, remote_sha] = fields[..] else {
            continue;
        };
        if local_sha == ZERO_SHA {
            continue; // deleting a remote branch pushes no commits
        }
        if remote_sha == ZERO_SHA {
            // New branch: every commit not already on a remote.
            scan(&format!("{local_sha} --not --remotes"))?;
        } else {
            scan(&format!("{remote_sha}..{local_sha}"))?;
        }
    }
    Ok(())
}

/// The repo name at the end of a git URL, for both `git@host:owner/name.git`
/// and `https://host/owner/name` forms.
fn repo_name(url: &str) -> Option<&str> {
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    url.rsplit(['/', ':'])
        .next()
        .filter(|name| !name.is_empty())
}

/// Builds throwaway repos that each contain one thing the guard must block
/// (plus one clean repo it must pass), and checks gitleaks' verdict on each.
pub fn self_test() -> Result {
    // Assembled at runtime so this source file doesn't trip its own rules.
    let private_name = ["gitcko", "cloud"].join("-");
    let fake_github_token = format!("ghp_{}", "Xq7vR2mK9pL4wZ8nB3jH6tY1cF5dG0sA2eUo");

    let cases: [(&str, &str, &str, bool); 6] = [
        ("clean", "README.md", "hello", false),
        ("private repo name", "notes.txt", &private_name, true),
        ("hosted-service folder", "infra/main.tf", "x", true),
        ("planning doc", "docs/ROADMAP.md", "x", true),
        ("agent tool state", ".serena/project.yml", "x", true),
        (
            "secret",
            "app.env",
            &format!("GITHUB_PAT={fake_github_token}"),
            true,
        ),
    ];

    let mut failures = Vec::new();
    for (label, file, content, should_block) in cases {
        let repo = temp_repo(file, content)?;
        let blocked = gitleaks(&repo, "HEAD");
        let _ = std::fs::remove_dir_all(&repo);
        match blocked {
            Ok(b) if b == should_block => println!("self-test ok: {label}"),
            Ok(b) => failures.push(format!(
                "{label}: expected blocked={should_block}, got blocked={b}"
            )),
            Err(e) => failures.push(format!("{label}: {e}")),
        }
    }
    if failures.is_empty() {
        println!("leak-guard self-test passed");
        Ok(())
    } else {
        Err(format!(
            "leak-guard self-test FAILED:\n{}",
            failures.join("\n")
        ))
    }
}

/// A new git repo in the temp dir with one commit adding `file`.
fn temp_repo(file: &str, content: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("gitcko-leak-guard-{}-{nanos}", std::process::id()));
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    let git = |args: &[&str]| -> Result {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args([
                "-c",
                "user.name=leak-guard",
                "-c",
                "user.email=leak-guard@invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .map_err(|e| e.to_string())?
            .status
            .success();
        ok.then_some(())
            .ok_or_else(|| format!("git {} failed in {}", args.join(" "), dir.display()))
    };
    git(&["init", "-q"])?;
    git(&["add", "."])?;
    git(&["commit", "-q", "-m", "seed"])?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::repo_name;

    #[test]
    fn repo_name_from_ssh_and_https_urls() {
        assert_eq!(
            repo_name("git@github.com:anthony-jabbour/gitcko.git"),
            Some("gitcko")
        );
        assert_eq!(
            repo_name("https://github.com/someone/gitcko"),
            Some("gitcko")
        );
        assert_eq!(
            repo_name("https://github.com/someone/gitcko.git/"),
            Some("gitcko")
        );
    }

    #[test]
    fn other_repos_are_not_gitcko() {
        let private = ["gitcko", "cloud"].join("-");
        assert_eq!(
            repo_name(&format!("git@github.com:anthony-jabbour/{private}.git")),
            Some(private.as_str())
        );
        assert_ne!(repo_name("https://github.com/x/not-gitcko"), Some("gitcko"));
        assert_eq!(repo_name(""), None);
    }
}
