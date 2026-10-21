// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keeps secrets and private hosted-service content out of this public repo.
//!
//! Three checks run over the commits being pushed or reviewed:
//! 1. **Paths**: every file a commit adds, renames or copies is matched against
//!    the deny-list below. Done here rather than as gitleaks path rules, because
//!    those only fire on added lines, so renames, empty and binary files slip by.
//! 2. **Secrets**: gitleaks with its default rules (`xtask/gitleaks-secrets.toml`),
//!    copied without their path allowlist so lockfiles and assets are scanned too.
//! 3. **Private content**: gitleaks with our deny-list rules only
//!    (`xtask/leak-guard.toml`), without gitleaks' default allowlist, so
//!    lockfiles and assets are scanned too.
//!
//! Inline `gitleaks:allow` comments are ignored, a committed `.gitleaksignore`
//! is a denied path, and an untracked one at the repo root stops the scan
//! (gitleaks would read it), so nothing in the repo can switch the guard off.
//! Diffs are scanned as text, so binary or `-diff` files aren't skipped.

use crate::Result;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;

const SECRETS_CONFIG: &str = "xtask/gitleaks-secrets.toml";
const CONTENT_CONFIG: &str = "xtask/leak-guard.toml";
/// gitleaks exit code when it finds a leak, distinct from its error exits.
const LEAK_EXIT: i32 = 42;
/// git sends an all-zero sha (40 or 64 digits) for a ref missing on one side of a push.
fn is_zero_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|b| b == b'0')
}

/// Folders that only exist in the private hosted-service repo, from the repo root
/// (all deny-list comparisons are case-insensitive).
const DENIED_ROOT_DIRS: &[&str] = &[
    "services/",
    "apps/web-cloud/",
    "apps/site/",
    "infra/",
    "migrations/cloud/",
    "vendor/",
];
/// Folders that are never public, at any depth: private research notes and
/// local state of agent tools.
const DENIED_ANY_DIRS: &[&str] = &["research/", ".serena/"];
/// Never allowed in any folder: personal agent rules, and gitleaks ignore files.
const DENIED_NAMES_ANYWHERE: &[&str] = &["claude.local.md", ".gitleaksignore"];
/// Templates that document `.env` keys without real values; every other `.env`
/// or `.env.*` file is denied by name, because gitleaks' rules miss plain
/// passwords and key formats it doesn't know.
const ALLOWED_ENV_TEMPLATES: &[&str] = &[".env.example", ".env.sample", ".env.template"];
/// Private planning-doc names, denied in every folder except the public files and
/// test-data folders below. Public docs use other names (e.g. `docs/architecture-overview.md`).
const DENIED_DOC_NAMES: &[&str] = &[
    "Gitcko_PRD.md",
    "MVP.md",
    "ROADMAP.md",
    "ARCHITECTURE.md",
    "CLOUD_ARCHITECTURE.md",
    "DATABASE.md",
    "REVIEW_ENGINE.md",
    "REVIEW_RULES.md",
    "INDEXING.md",
    "SECURITY.md",
    "THREAT_MODEL.md",
    "PRIVACY.md",
    "INCIDENT_RESPONSE.md",
    "MODEL_ROUTING.md",
    "EVALS.md",
    "SCORING.md",
    "TESTING.md",
    "DEVELOPMENT.md",
    "AI_DEVELOPMENT.md",
    "CODEBASE.md",
    "CONFIGURATION.md",
    "CONTRIBUTING.md",
    "LICENSE_STRATEGY.md",
    "LOCAL_DEPLOYMENT.md",
    "SELF_HOSTING.md",
    "RUNTIME_VERIFICATION.md",
    "SCM_INTEGRATION.md",
    "MCP_SPEC.md",
    "CLI_SPEC.md",
    "API_SPEC.md",
    "TUI_SPEC.md",
    "DASHBOARD.md",
    "ANALYTICS.md",
    "OBSERVABILITY.md",
    "BRANDING.md",
    "design.md",
];
/// Public files that share a name with a private planning doc.
const ALLOWED_DOC_FILES: &[&str] = &[
    "contributing.md",
    "security.md",
    ".github/contributing.md",
    ".github/security.md",
];
/// Folders holding test data, not docs, so planning-doc names are fine there.
const ALLOWED_DOC_PLACES: &[&str] = &["tests/fixtures/", "evals/"];

/// Why `path` may not be committed here, or `None` if it may.
fn denied_path(path: &str) -> Option<&'static str> {
    // Case-insensitive, because macOS and Windows treat `Infra/` and `infra/` as one folder.
    let path = path.to_ascii_lowercase();
    // A bare `infra` (no slash) is a symlink or submodule with a denied name.
    let starts = |d: &str| {
        let d = d.to_ascii_lowercase();
        path.starts_with(&d) || path == d.trim_end_matches('/')
    };
    if DENIED_ROOT_DIRS.iter().any(|d| starts(d)) {
        return Some("hosted-service folder");
    }
    if DENIED_ANY_DIRS.iter().any(|d| {
        let bare = d.trim_end_matches('/');
        starts(d) || path.contains(&format!("/{d}")) || path.ends_with(&format!("/{bare}"))
    }) {
        return Some("private or tool-state folder");
    }
    let name = path.rsplit('/').next().unwrap_or(&path);
    if DENIED_NAMES_ANYWHERE.contains(&name) {
        return Some("personal agent rules or gitleaks ignore file");
    }
    if (name == ".env" || name.starts_with(".env.")) && !ALLOWED_ENV_TEMPLATES.contains(&name) {
        return Some("environment file (may hold secrets)");
    }
    let is_doc_name = DENIED_DOC_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name));
    let allowed = ALLOWED_DOC_FILES.contains(&path.as_str())
        || ALLOWED_DOC_PLACES.iter().any(|d| path.starts_with(d));
    (is_doc_name && !allowed).then_some("private planning doc")
}

/// Scans the commits selected by `log_opts` (git log arguments) in the current repo.
pub fn scan(log_opts: &str) -> Result {
    match check_commits(Path::new("."), log_opts)? {
        false => {
            println!("leak-guard: clean ({log_opts})");
            Ok(())
        }
        true => Err(format!("leak-guard: blocked, see above ({log_opts})")),
    }
}

/// Runs all three checks. Returns whether anything must be blocked.
fn check_commits(repo: &Path, log_opts: &str) -> Result<bool> {
    let args: Vec<&str> = log_opts.split_whitespace().collect();
    // gitleaks scans nothing and reports "clean" for a range git can't resolve,
    // so resolve it ourselves first and fail if git can't.
    git_output(repo, &[&["rev-list", "--count"], &args[..]].concat())?;
    if repo.join(".gitleaksignore").exists() {
        return Err(
            "leak-guard: remove .gitleaksignore from the repo root; it would hide findings".into(),
        );
    }

    // -z: unquoted, NUL-separated names (git quotes unusual names otherwise).
    // --diff-merges=first-parent: also list what a merge commit itself changed.
    let files = git_output(
        repo,
        &[
            &[
                "log",
                "--format=",
                "--name-only",
                "-z",
                "--no-renames",
                "--diff-merges=first-parent",
                "--diff-filter=d",
            ],
            &args[..],
        ]
        .concat(),
    )?;
    let mut blocked = false;
    // Split on NUL only: a name may itself contain a newline.
    for path in files.split('\0').filter(|p| !p.is_empty()) {
        if let Some(why) = denied_path(path) {
            eprintln!("leak-guard: {path}: {why}");
            blocked = true;
        }
    }
    for config in [SECRETS_CONFIG, CONTENT_CONFIG] {
        blocked |= gitleaks(repo, config, log_opts)?;
    }
    Ok(blocked)
}

/// Runs gitleaks with one config. Returns whether it found a leak.
fn gitleaks(repo: &Path, config: &str, log_opts: &str) -> Result<bool> {
    // Absolute paths, because the self-test points gitleaks at repos outside the workspace.
    let here = std::env::current_dir().map_err(|e| e.to_string())?;
    let status = Command::new("gitleaks")
        .args([
            "git",
            "--no-banner",
            "--redact",
            // List each finding (file, line, rule), so "blocked" says why.
            "--verbose",
            "--exit-code",
            &LEAK_EXIT.to_string(),
        ])
        .arg("--ignore-gitleaks-allow")
        // A folder we control for ignore files; the repo root is checked above.
        .arg("--gitleaks-ignore-path")
        .arg(here.join("xtask"))
        .arg("--config")
        .arg(here.join(config))
        // Scan what merge commits themselves change, and binary or `-diff` files as text.
        .arg(format!(
            "--log-opts=--diff-merges=first-parent --text {log_opts}"
        ))
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

fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "leak-guard: `git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Git pre-push hook: refuse to push anywhere but a repo named `gitcko`, and
/// scan the commits being pushed (git sends them on stdin).
pub fn pre_push(remote: &str, url: &str) -> Result {
    if repo_name(url) != Some("gitcko") {
        return Err(format!(
            "leak-guard: this checkout is the public `gitcko` repo, but the push goes to {url}"
        ));
    }
    let lines: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    for range in push_ranges(remote, &lines) {
        scan(&range)?;
    }
    Ok(())
}

/// The commits to scan for each ref git is pushing. Each stdin line is
/// `<local ref> <local sha> <remote ref> <remote sha>`.
fn push_ranges(remote: &str, lines: &[String]) -> Vec<String> {
    let mut ranges = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [_, local_sha, _, remote_sha] = fields[..] else {
            continue;
        };
        if is_zero_sha(local_sha) {
            continue; // deleting a remote branch pushes no commits
        }
        if is_zero_sha(remote_sha) {
            // New branch: every commit the destination remote doesn't have yet.
            // Only that remote counts; history from other remotes is still scanned.
            ranges.push(format!("{local_sha} --not --remotes={remote}"));
        } else {
            ranges.push(format!("{remote_sha}..{local_sha}"));
        }
    }
    ranges
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
/// (plus one clean repo it must pass), and checks the verdict on each.
pub fn self_test() -> Result {
    // Assembled at runtime so this source file doesn't trip its own rules.
    let private_name = ["gitcko", "cloud"].join("-");
    let fake_github_token = format!("ghp_{}", "Xq7vR2mK9pL4wZ8nB3jH6tY1cF5dG0sA2eUo");
    let allow_comment = format!("{private_name} # gitleaks:allow");
    let secret_line = format!("GITHUB_PAT={fake_github_token}");
    let env_line = format!("export MY_{}_KEY=1", "STRIPE");
    let binary_blob = format!("AB\0\0{secret_line}\0");

    let cases: &[SelfTestCase] = &[
        (
            "clean",
            &[("README.md", "hello"), ("CONTRIBUTING.md", "x")],
            false,
        ),
        ("private repo name", &[("notes.txt", &private_name)], true),
        (
            "private name in a lockfile",
            &[("web/package-lock.json", &private_name)],
            true,
        ),
        (
            "inline allow comment ignored",
            &[("notes.txt", &allow_comment)],
            true,
        ),
        (
            "hosted-service folder, empty file",
            &[("infra/main.tf", "")],
            true,
        ),
        (
            "hosted-service folder, image",
            &[("services/diagram.png", "x")],
            true,
        ),
        (
            "planning doc in any folder",
            &[("notes/roadmap.md", "x")],
            true,
        ),
        (
            "planning-doc name in a fixture is fine",
            &[("tests/fixtures/repo/SECURITY.md", "x")],
            false,
        ),
        (
            "non-ASCII name in a hosted-service folder",
            &[("Infra/caf\u{e9}.tf", "")],
            true,
        ),
        (
            "quote in a planning-doc path",
            &[("docs/a\"b/ROADMAP.md", "")],
            true,
        ),
        (
            "newline in a planning-doc path",
            &[("notes/a\n/ROADMAP.md", "")],
            true,
        ),
        (
            "secret in a binary file",
            &[("blob.bin", &binary_blob)],
            true,
        ),
        (
            "private name in a -diff file",
            &[
                (".gitattributes", "x.txt -diff\n"),
                ("x.txt", &private_name),
            ],
            true,
        ),
        (
            "symlink-style bare hosted-service name",
            &[("infra", "")],
            true,
        ),
        ("agent tool state", &[(".serena/project.yml", "x")], true),
        ("env file", &[(".env", "ADMIN_PASSWORD=x")], true),
        (
            "env file variant in a subfolder",
            &[("config/.env.production", "x")],
            true,
        ),
        (
            "env template",
            &[(".env.example", "ADMIN_PASSWORD=")],
            false,
        ),
        (
            "gitleaks ignore file",
            &[("sub/.gitleaksignore", "x")],
            true,
        ),
        ("secret", &[("app.env", &secret_line)], true),
        (
            "secret in a lockfile",
            &[("web/package-lock.json", &secret_line)],
            true,
        ),
        ("secret in an image", &[("logo.png", &secret_line)], true),
        (
            "secret in a lookalike of the secrets config",
            &[("docs/xtask/gitleaks-secrets.toml", &secret_line)],
            true,
        ),
        (
            "provider env var with a prefix",
            &[("app.env", &env_line)],
            true,
        ),
    ];

    let mut failures = Vec::new();
    for (label, files, should_block) in cases {
        // `"` and newlines can't be in Windows file names; unit tests cover that logic there.
        if cfg!(windows) && files.iter().any(|(f, _)| f.contains(['"', '\n'])) {
            continue;
        }
        let result = temp_repo(files).and_then(|repo| {
            let verdict = check_commits(&repo, "HEAD");
            let _ = std::fs::remove_dir_all(&repo);
            verdict
        });
        record(&mut failures, label, result, *should_block);
    }

    // A private doc renamed into place adds no lines; the path check must still see it.
    let result = temp_repo(&[("plan.md", "x")]).and_then(|repo| {
        let git = |args: &[&str]| git_output(&repo, &[&IDENTITY[..], args].concat());
        let verdict = git(&["mv", "plan.md", "ROADMAP.md"])
            .and_then(|_| git(&["commit", "-q", "-m", "rename"]))
            .and_then(|_| check_commits(&repo, "HEAD~1..HEAD"));
        let _ = std::fs::remove_dir_all(&repo);
        verdict
    });
    record(
        &mut failures,
        "rename into a planning-doc name",
        result,
        true,
    );

    // A merge commit can add files of its own (e.g. in a conflict resolution);
    // both the path check and the content check must see them.
    let result = temp_repo(&[("README.md", "x")]).and_then(|repo| {
        let git = |args: &[&str]| git_output(&repo, &[&IDENTITY[..], args].concat());
        let base = git(&["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
        let verdict = base.and_then(|base| {
            git(&["checkout", "-q", "-b", "side"])?;
            std::fs::write(repo.join("side.txt"), "x").map_err(|e| e.to_string())?;
            git(&["add", "side.txt"])?;
            git(&["commit", "-q", "-m", "side"])?;
            git(&["checkout", "-q", "-"])?;
            git(&["merge", "-q", "--no-ff", "--no-commit", "side"])?;
            std::fs::create_dir_all(repo.join("infra")).map_err(|e| e.to_string())?;
            std::fs::write(repo.join("infra/main.tf"), "").map_err(|e| e.to_string())?;
            std::fs::write(repo.join("evil.txt"), &private_name).map_err(|e| e.to_string())?;
            git(&["add", "."])?;
            git(&["commit", "-q", "-m", "merge"])?;
            check_commits(&repo, &format!("{base}..HEAD"))
        });
        let _ = std::fs::remove_dir_all(&repo);
        verdict
    });
    record(
        &mut failures,
        "files added inside a merge commit",
        result,
        true,
    );

    // A range git can't resolve must fail, not pass as "0 commits, clean".
    let bad_range = temp_repo(&[("README.md", "x")]).and_then(|repo| {
        let verdict = check_commits(&repo, &format!("{}..HEAD", "0".repeat(40)));
        let _ = std::fs::remove_dir_all(&repo);
        verdict
    });
    match bad_range {
        Err(_) => println!("self-test ok: unresolvable range fails"),
        Ok(_) => failures.push("unresolvable range: expected an error, got a verdict".into()),
    }

    // gitleaks reads a .gitleaksignore at the repo root, so its presence must stop the scan.
    let root_ignore = temp_repo(&[(".gitleaksignore", "x")]).and_then(|repo| {
        let verdict = check_commits(&repo, "HEAD");
        let _ = std::fs::remove_dir_all(&repo);
        verdict
    });
    match root_ignore {
        Err(_) => println!("self-test ok: root .gitleaksignore stops the scan"),
        Ok(_) => failures.push("root .gitleaksignore: expected an error, got a verdict".into()),
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

/// (label, files to commit as (path, content), should the guard block it)
type SelfTestCase<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);

fn record(failures: &mut Vec<String>, label: &str, result: Result<bool>, should_block: bool) {
    match result {
        Ok(b) if b == should_block => println!("self-test ok: {label}"),
        Ok(b) => failures.push(format!(
            "{label}: expected blocked={should_block}, got blocked={b}"
        )),
        Err(e) => failures.push(format!("{label}: {e}")),
    }
}

const IDENTITY: [&str; 6] = [
    "-c",
    "user.name=leak-guard",
    "-c",
    "user.email=leak-guard@invalid",
    "-c",
    "commit.gpgsign=false",
];

/// A new git repo in the temp dir with one commit adding `files`.
fn temp_repo(files: &[(&str, &str)]) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("gitcko-leak-guard-{}-{nanos}", std::process::id()));
    for (file, content) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
    }
    let git = |args: &[&str]| git_output(&dir, &[&IDENTITY[..], args].concat());
    git(&["init", "-q"])?;
    git(&["add", "."])?;
    git(&["commit", "-q", "-m", "seed"])?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::{denied_path, push_ranges, repo_name};

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

    #[test]
    fn denied_paths() {
        for path in [
            "infra/main.tf",
            "apps/site/page.tsx",
            "docs/research/x.md",
            "research/x.md",
            "crates/a/.serena/project.yml",
            "ROADMAP.md",
            "docs/roadmap.md",
            "docs/CONTRIBUTING.md",
            "Infra/main.tf",
            "Research/x.md",
            "services",
            "docs/research",
            "crates/x/.serena",
            "notes/ROADMAP.md",
            "crates/x/ARCHITECTURE.md",
            "docs/SECURITY.md",
            "CLAUDE.local.md",
            "sub/.gitleaksignore",
            ".env",
            ".ENV",
            "web/.env.local",
            "config/.env.production",
        ] {
            assert!(denied_path(path).is_some(), "{path} should be denied");
        }
    }

    #[test]
    fn allowed_paths() {
        for path in [
            "CONTRIBUTING.md",
            "SECURITY.md",
            "README.md",
            "docs/install.md",
            "crates/gitcko-git/src/infra.rs",
            "crates/x/infra/mod.rs",
            "infrastructure.md",
            "web/apps/dashboard/page.tsx",
            ".github/SECURITY.md",
            "tests/fixtures/repo/ARCHITECTURE.md",
            "docs/architecture-overview.md",
            ".env.example",
            "web/.env.sample",
            ".env.template",
            "src/environment.rs",
            "app.env.rs",
        ] {
            assert_eq!(denied_path(path), None, "{path} should be allowed");
        }
    }

    #[test]
    fn push_ranges_cover_update_new_and_delete() {
        let zero40 = "0".repeat(40);
        let zero64 = "0".repeat(64);
        let lines = [
            "refs/heads/a aaa refs/heads/a bbb".to_string(),
            format!("refs/heads/new ccc refs/heads/new {zero40}"),
            format!("refs/heads/new256 ddd refs/heads/new256 {zero64}"),
            format!("(delete) {zero40} refs/heads/gone eee"),
            "garbage".to_string(),
        ];
        assert_eq!(
            push_ranges("origin", &lines),
            [
                "bbb..aaa",
                "ccc --not --remotes=origin",
                "ddd --not --remotes=origin"
            ]
        );
    }
}
