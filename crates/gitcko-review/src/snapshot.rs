// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reviewed snapshot and a disposition for every changed path (REVIEW_ENGINE §5–6).
//! Config and rules come from the **base** commit (ADR-35): a change can't loosen its
//! own review (THREAT_MODEL TM-02, TM-03).

use std::collections::{BTreeMap, BTreeSet};

use gitcko_config::{Ceilings, FolderConfig, RepoFiles, Resolved, RunFlags, resolve_repo};
use gitcko_diff::{ChangeSet, FileDiff, LineKind};
use gitcko_domain::{ContentHash, RepoPath};
use gitcko_git::{CommitId, Repo};
use gitcko_index::{Disposition as IndexDisposition, IndexLimits, ManifestEntry, Revision, build};
use gitcko_prompts::ChangeContext;
use gitcko_redact::Redactor;
use gitcko_syntax::Language;
use gitcko_tools::{Limits, has_secret, is_sensitive_name};

use crate::complete::{Completeness, reason};
use crate::providers::{Knowledge, KnowledgeProvider, ProviderStatus, Signal};
use crate::{MAX_DIFF_LINES, MAX_FILE_DIFF_LINES, MAX_FILES, ReviewInput};

/// What the review does with one changed path. Every path gets exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Investigated,
    /// Matches the base side's `ignore`.
    Ignored,
    Binary,
    /// A secret by name or content: never read into a prompt.
    HeldBack,
    /// No parser for its language: reviewed from the diff and text search alone.
    Unsupported,
    /// Its parse failed, or it isn't text the parser can take.
    ParseFailed,
    /// Over an index limit.
    IndexGap,
    /// Past the file cap.
    Deferred,
}

/// One changed path and its disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDisposition {
    pub path: RepoPath,
    pub disposition: Disposition,
}

/// Everything fixed before the first model call.
pub struct Snapshot<'a> {
    pub repo: &'a Repo,
    pub base: CommitId,
    pub head: CommitId,
    pub merge_base: Option<CommitId>,
    pub change: ChangeSet,
    pub index: Revision,
    pub dispositions: Vec<FileDisposition>,
    /// The base's config as it applies to each investigated path: nested `.gitcko/`
    /// folders only reach the paths under them.
    pub configs: BTreeMap<RepoPath, Resolved>,
    pub config_invalid: bool,
    pub commit_messages: Vec<String>,
    /// An unchanged file the review may lean on is over an index limit.
    pub repo_index_gap: bool,
}

fn file_path(file: &FileDiff) -> Option<&RepoPath> {
    file.new_path.as_ref().or(file.old_path.as_ref())
}

/// The `.gitcko/` files of a commit, as the config crate wants them.
fn base_config_files(repo: &Repo, commit: CommitId) -> Result<RepoFiles, gitcko_git::GitError> {
    let mut folders: BTreeMap<Option<RepoPath>, FolderConfig> = BTreeMap::new();
    for entry in repo.files(commit)? {
        // A name the config crate can't hold isn't a config file.
        let Ok(text) = std::str::from_utf8(&entry.path) else {
            continue;
        };
        let Some((folder, name)) = text.rsplit_once(".gitcko/") else {
            continue;
        };
        if !(name == "config.toml" || name == "rules.md")
            || !(folder.is_empty() || folder.ends_with('/'))
        {
            continue;
        }
        let folder = folder.trim_end_matches('/');
        let folder = if folder.is_empty() {
            None
        } else {
            RepoPath::parse(folder).ok()
        };
        let Ok(path) = RepoPath::parse(text) else {
            continue;
        };
        let Some(blob) = repo.read_blob(commit, &path)? else {
            continue;
        };
        let body = String::from_utf8_lossy(&blob.bytes).into_owned();
        let slot = folders.entry(folder.clone()).or_insert(FolderConfig {
            folder,
            config_toml: None,
            rules_md: None,
        });
        if name == "config.toml" {
            slot.config_toml = Some(body);
        } else {
            slot.rules_md = Some(body);
        }
    }
    Ok(RepoFiles {
        folders: folders.into_values().collect(),
        ..RepoFiles::default()
    })
}

/// Fixes the snapshot. `Err` is a reason code: the repository can't be read, so there's
/// nothing to review.
pub fn take<'a>(input: &ReviewInput<'a>) -> Result<Snapshot<'a>, &'static str> {
    let fail = |_| reason::SNAPSHOT_FAILED;
    let repo = input.repo;
    let change = repo.diff(input.base, input.head).map_err(fail)?;
    let merge_base = repo.merge_base(input.base, input.head).map_err(fail)?;
    let index =
        build(repo, input.head, &IndexLimits::default()).map_err(|_| reason::SNAPSHOT_FAILED)?;

    let files = base_config_files(repo, input.base).map_err(fail)?;
    let ignore = gitcko_config::ignore_rules(&files).ok();

    let entries: BTreeMap<&str, &ManifestEntry> = index
        .manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e))
        .collect();
    let mut investigated = 0;
    let dispositions = change
        .files
        .iter()
        .filter_map(|file| {
            let path = file_path(file)?.clone();
            let disposition = dispose(input, file, &path, &entries, &index, ignore.as_ref());
            let disposition = if disposition != Disposition::HeldBack
                && disposition != Disposition::Ignored
                && disposition != Disposition::Binary
            {
                investigated += 1;
                if investigated > MAX_FILES {
                    Disposition::Deferred
                } else {
                    disposition
                }
            } else {
                disposition
            };
            Some(FileDisposition { path, disposition })
        })
        .collect();

    let dispositions: Vec<FileDisposition> = dispositions;
    let mut configs = BTreeMap::new();
    // Broken config applying nowhere reviewed still means the ignore list is in doubt.
    let mut config_invalid = ignore.is_none();
    for d in dispositions.iter().filter(|d| reviewed(d.disposition)) {
        let resolved = resolve_repo(&files, &d.path, &RunFlags::default(), &Ceilings::local());
        // Broken config on the base doesn't stop a review; defaults apply, and it says so.
        // A broken nested file falls back to its parent inside `Ok`, and says so there.
        let resolved = resolved.unwrap_or_else(|_| {
            config_invalid = true;
            Resolved::default()
        });
        config_invalid |= resolved
            .reason_codes
            .iter()
            .any(|c| c == reason::CONFIG_INVALID);
        configs.insert(d.path.clone(), resolved);
    }

    let log = repo.log(input.head, None, 50, 500).map_err(fail)?;
    let commit_messages = log
        .entries
        .iter()
        // A diverged base is never reached on the way back; the merge base always is.
        .take_while(|e| e.id != input.base && Some(e.id) != merge_base)
        .map(|e| scrubbed(&e.summary))
        .collect();
    let repo_index_gap = !index.manifest.coverage.is_complete()
        || index.manifest.entries.iter().any(over_index_limit);
    Ok(Snapshot {
        repo,
        base: input.base,
        head: input.head,
        merge_base,
        change,
        index,
        dispositions,
        configs,
        config_invalid,
        commit_messages,
        repo_index_gap,
    })
}

/// Text with credential-shaped strings replaced. Commit subjects and rules go straight
/// into prompts, with no tool between them and the model.
fn scrubbed(text: &str) -> String {
    Redactor::new().redact(text).into_owned()
}

/// Whether the model may see this path's diff.
fn reviewed(disposition: Disposition) -> bool {
    matches!(
        disposition,
        Disposition::Investigated
            | Disposition::Unsupported
            | Disposition::ParseFailed
            | Disposition::IndexGap
    )
}

fn over_index_limit(entry: &ManifestEntry) -> bool {
    let reason = entry.reason.as_ref().map(|r| r.as_str());
    let parseable = entry.language.is_some_and(Language::has_adapter);
    matches!(reason, Some("file_too_large" | "file_too_large_to_parse")) && parseable
}

/// A language someone reviews as code, which has no parser yet. Config and prose don't
/// reduce a review's context: there's nothing to resolve in them.
fn unsupported_code(language: Language) -> bool {
    !language.has_adapter()
        && !matches!(
            language,
            Language::Markdown
                | Language::Toml
                | Language::Yaml
                | Language::Json
                | Language::Html
                | Language::Css
        )
}

fn dispose(
    input: &ReviewInput<'_>,
    file: &FileDiff,
    path: &RepoPath,
    entries: &BTreeMap<&str, &ManifestEntry>,
    index: &Revision,
    ignore: Option<&gitcko_config::IgnoreRules>,
) -> Disposition {
    // Both names and both sides: a rename can move a secret under an innocent name.
    let names = [file.old_path.as_ref(), file.new_path.as_ref()];
    if names.into_iter().flatten().any(is_sensitive_name) {
        return Disposition::HeldBack;
    }
    let sides = [
        (input.base, file.old_path.as_ref()),
        (input.head, file.new_path.as_ref()),
    ];
    for (commit, side_path) in sides {
        let Some(side_path) = side_path else { continue };
        if let Ok(Some(blob)) = input.repo.read_blob(commit, side_path)
            && has_secret(&String::from_utf8_lossy(&blob.bytes))
        {
            return Disposition::HeldBack;
        }
    }
    if ignore.is_some_and(|rules| rules.is_ignored(path)) {
        return Disposition::Ignored;
    }
    if file.binary {
        return Disposition::Binary;
    }
    let Some(entry) = entries.get(path.as_str()) else {
        // Deleted: only its diff remains, which is all a review needs.
        return Disposition::Investigated;
    };
    if entry.binary {
        return Disposition::Binary;
    }
    match entry.disposition {
        IndexDisposition::ExcludedResourceLimit => Disposition::IndexGap,
        IndexDisposition::LexicalOnly if over_index_limit(entry) => Disposition::IndexGap,
        IndexDisposition::LexicalOnly if entry.language.is_some_and(unsupported_code) => {
            Disposition::Unsupported
        }
        IndexDisposition::MetadataOnly
            if entry
                .reason
                .as_ref()
                .is_some_and(|r| r.as_str() == "not_utf8") =>
        {
            Disposition::ParseFailed
        }
        IndexDisposition::Parse => {
            let at = index
                .manifest
                .entries
                .iter()
                .position(|e| std::ptr::eq(e, *entry));
            let parsed = index.parsed.iter().find(|p| Some(p.entry) == at);
            if parsed.is_some_and(|p| {
                !p.extraction.syntax_errors.is_empty() || !p.extraction.coverage.is_complete()
            }) {
                Disposition::ParseFailed
            } else {
                Disposition::Investigated
            }
        }
        _ => Disposition::Investigated,
    }
}

impl Snapshot<'_> {
    /// Paths the model may see the diff of, in change order.
    pub fn investigated(&self) -> Vec<&RepoPath> {
        self.dispositions
            .iter()
            .filter(|d| reviewed(d.disposition))
            .map(|d| &d.path)
            .collect()
    }

    pub fn tool_limits(&self) -> Limits {
        Limits::default()
    }

    /// Both names of every held-back file, for the tools: a secret renamed to an innocent
    /// name is only known to be one from here.
    pub fn held_back_names(&self) -> Vec<RepoPath> {
        let held = |file: &&FileDiff| {
            file_path(file).is_some_and(|path| {
                self.dispositions
                    .iter()
                    .any(|d| d.path == *path && d.disposition == Disposition::HeldBack)
            })
        };
        self.change
            .files
            .iter()
            .filter(held)
            .flat_map(|f| [f.old_path.clone(), f.new_path.clone()])
            .flatten()
            .collect()
    }

    /// One digest for the policy this review ran under, however many folders it spans.
    pub fn policy_digest(&self) -> String {
        let digests: BTreeSet<String> =
            self.configs.values().map(Resolved::policy_digest).collect();
        match digests.len() {
            0 => Resolved::default().policy_digest(),
            1 => digests.into_iter().next().unwrap_or_default(),
            _ => {
                let joined = digests.into_iter().collect::<Vec<_>>().join("\n");
                format!("sha256:{}", ContentHash::of(joined.as_bytes()))
            }
        }
    }

    /// Every gap in what the review could see.
    pub fn report_gaps(&self, ledger: &mut Completeness) {
        for d in &self.dispositions {
            match d.disposition {
                Disposition::HeldBack => ledger.add(reason::SENSITIVE_HELD_BACK),
                Disposition::Unsupported => ledger.reduced(reason::UNSUPPORTED_LANGUAGE),
                Disposition::ParseFailed => ledger.reduced(reason::PARSE_FAILED),
                Disposition::IndexGap => ledger.reduced(reason::INDEX_GAP),
                Disposition::Deferred => ledger.reduced(reason::TRUNCATED),
                Disposition::Investigated | Disposition::Ignored | Disposition::Binary => {}
            }
        }
        if self.repo_index_gap {
            ledger.reduced(reason::INDEX_GAP);
        }
        if self.config_invalid {
            ledger.add(reason::CONFIG_INVALID);
        }
        if self.diff_text().1 {
            ledger.reduced(reason::TRUNCATED);
        }
    }

    /// The diff of one investigated file, with new-side line numbers, and whether it was
    /// cut. A file that isn't investigated has no diff to show.
    pub fn file_diff(&self, path: &RepoPath) -> (String, bool) {
        let allowed = self.investigated().contains(&path);
        let file = self
            .change
            .files
            .iter()
            .find(|f| file_path(f) == Some(path));
        match file {
            Some(file) if allowed => render(file, MAX_FILE_DIFF_LINES),
            _ => (String::new(), false),
        }
    }

    /// The whole reviewed diff, capped, and whether anything was cut.
    pub fn diff_text(&self) -> (String, bool) {
        let mut text = String::new();
        let mut cut = false;
        let mut left = MAX_DIFF_LINES;
        for path in self.investigated() {
            let (one, one_cut) = self.file_diff(path);
            // A file over an index limit is already reported as the gap it is.
            let reported = self
                .dispositions
                .iter()
                .any(|d| d.path == *path && d.disposition == Disposition::IndexGap);
            cut |= one_cut && !reported;
            let lines = one.lines().count();
            if lines > left {
                cut |= !reported;
                continue;
            }
            left -= lines;
            text.push_str(&one);
        }
        (text, cut)
    }

    /// The change as the prompts describe it.
    pub fn change_context(
        &self,
        knowledge: &dyn KnowledgeProvider,
        signals: &[Signal],
    ) -> ChangeContext {
        let rules = match knowledge.knowledge() {
            (ProviderStatus::Ok, Knowledge { rules }) => {
                rules.iter().map(|rule| scrubbed(rule)).collect()
            }
            _ => Vec::new(),
        };
        ChangeContext {
            diff: self.diff_text().0,
            commit_messages: self.commit_messages.clone(),
            rules,
            signals: signals.iter().map(|s| s.text.clone()).collect(),
        }
    }
}

/// A file's diff as text. Lines over the display limit are cut, like a tool's.
fn render(file: &FileDiff, max_lines: usize) -> (String, bool) {
    let limit = Limits::default().line_chars;
    let name = file_path(file).map_or("", RepoPath::as_str);
    let mut out = format!("--- {name}\n");
    let mut cut = false;
    let mut shown = 0;
    'hunks: for hunk in &file.hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        ));
        let mut new_line = hunk.new_start;
        for line in &hunk.lines {
            if shown == max_lines {
                cut = true;
                break 'hunks;
            }
            shown += 1;
            let (marker, number) = match line.kind {
                LineKind::Context => (' ', Some(new_line)),
                LineKind::Added => ('+', Some(new_line)),
                LineKind::Removed => ('-', None),
            };
            let kept: String = line.text.chars().take(limit).collect();
            cut |= kept.len() < line.text.len();
            match number {
                Some(n) => {
                    out.push_str(&format!("{n:>6} {marker}{kept}\n"));
                    new_line += 1;
                }
                None => out.push_str(&format!("       {marker}{kept}\n")),
            }
        }
    }
    (out, cut)
}
