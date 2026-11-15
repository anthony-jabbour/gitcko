// SPDX-License-Identifier: AGPL-3.0-or-later

//! `review show` and `review status`. They read the store only: no model, no router.
//! A live running marker outranks a stored file for the same review, and a marker whose
//! process is gone is a failure, never a review that is still running.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gitcko_config::GitckoPaths;
use gitcko_git::Repo;
use gitcko_protocol::event::RunEvent;
use gitcko_protocol::review::{Lifecycle, Review, ReviewStatus, Severity};
use gitcko_render::{ListItem, boxed, events, list, status_line, text};
use gitcko_storage::{ReviewStore, RunState, StoredMarker};
use serde_json::{Value, json};

use crate::Io;
use crate::cli::{ShowArgs, StatusArgs};
use crate::exit::{StatusView, status_exit};
use crate::review::repo_cache_id;

/// Print one stored review, or the list when no id is given.
pub fn show<O, E>(
    args: &ShowArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    tty: bool,
    io: &Io<O, E>,
) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    match show_inner(args, cwd, env, tty, io) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(lock(&io.stderr), "{}", err.message);
            err.code
        }
    }
}

/// Exit code for the newest review of a commit. Defaults to HEAD.
pub fn status<O, E>(
    args: &StatusArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    io: &Io<O, E>,
) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    match status_inner(args, cwd, env, io) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(lock(&io.stderr), "{}", err.message);
            err.code
        }
    }
}

struct CmdError {
    code: u8,
    message: String,
}

impl CmdError {
    fn new(code: u8, message: impl Into<String>) -> CmdError {
        CmdError {
            code,
            message: message.into(),
        }
    }
}

fn show_inner<O, E>(
    args: &ShowArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    tty: bool,
    io: &Io<O, E>,
) -> Result<u8, CmdError>
where
    O: Write + Send,
    E: Write + Send,
{
    let repo = Repo::open(cwd).map_err(|_| CmdError::new(2, "not a git repository"))?;
    let (repo_id, store) = open_store(cwd, env)?;
    let Some(id) = args.id.as_deref() else {
        return show_list(cwd, &repo, &store, &repo_id, args, io);
    };
    if store.get(&repo_id, id).is_err() {
        return Err(CmdError::new(2, "no such review"));
    }
    let path = review_path(&store, &repo_id, id);
    let bytes = fs::read(&path).map_err(|_| CmdError::new(2, "no such review"))?;
    if args.json {
        lock(&io.stdout)
            .write_all(&bytes)
            .map_err(|_| CmdError::new(1, "could not write the review"))?;
    } else {
        let review: Review =
            serde_json::from_slice(&bytes).map_err(|_| CmdError::new(2, "no such review"))?;
        if review.status != ReviewStatus::Complete {
            let _ = writeln!(lock(&io.stderr), "{}", status_line(&review));
        }
        let body = if args.text || args.agent || !tty {
            text(&review)
        } else {
            boxed(&review, 80)
        };
        lock(&io.stdout)
            .write_all(body.as_bytes())
            .map_err(|_| CmdError::new(1, "could not write the review"))?;
    }
    if args.events {
        write_events(&store, &repo_id, id, args.json, io)?;
    }
    Ok(0)
}

fn show_list<O, E>(
    cwd: &Path,
    repo: &Repo,
    store: &ReviewStore,
    repo_id: &str,
    args: &ShowArgs,
    io: &Io<O, E>,
) -> Result<u8, CmdError>
where
    O: Write,
    E: Write,
{
    let summaries = store
        .list(repo_id)
        .map_err(|_| CmdError::new(1, "could not read the review history"))?;
    let mut items = Vec::new();
    for summary in summaries {
        let review = match store.get(repo_id, &summary.review_id) {
            Ok(review) => review,
            Err(_) => continue,
        };
        items.push(list_item(cwd, repo, review));
    }
    let items = group_by_head(items);
    if args.json {
        let reviews: Vec<Value> = items.iter().map(list_json).collect();
        let body = json!({
            "schema_version": "gitcko.review-list/1",
            "reviews": reviews,
        });
        let text = serde_json::to_string(&body)
            .map_err(|_| CmdError::new(1, "could not encode the review list"))?;
        writeln!(lock(&io.stdout), "{text}")
            .map_err(|_| CmdError::new(1, "could not write the review list"))?;
    } else {
        let rows: Vec<ListItem> = items
            .iter()
            .map(|item| ListItem {
                review_id: item.review_id.clone(),
                status: item.status,
                head_sha: item.head_sha.clone(),
            })
            .collect();
        write!(lock(&io.stdout), "{}", list(&rows))
            .map_err(|_| CmdError::new(1, "could not write the review list"))?;
    }
    Ok(0)
}

struct Listed {
    review_id: String,
    status: ReviewStatus,
    reason_codes: Vec<String>,
    base_sha: String,
    head_sha: String,
    branch: Option<String>,
    counts: (u32, u32, u32),
}

fn list_item(cwd: &Path, repo: &Repo, review: Review) -> Listed {
    Listed {
        branch: branch_for(cwd, repo, &review.target.head_sha),
        counts: finding_counts(&review),
        review_id: review.review_id,
        status: review.status,
        reason_codes: review.reason_codes,
        base_sha: review.target.base_sha,
        head_sha: review.target.head_sha,
    }
}

fn list_json(item: &Listed) -> Value {
    json!({
        "review_id": item.review_id,
        "status": status_name(item.status),
        "reason_codes": item.reason_codes,
        "base_sha": item.base_sha,
        "head_sha": item.head_sha,
        "branch": item.branch,
        "completed_at": Value::Null,
        "counts": { "P0": item.counts.0, "P1": item.counts.1, "P2": item.counts.2 },
    })
}

fn group_by_head(items: Vec<Listed>) -> Vec<Listed> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<Listed>> = Vec::new();
    for item in items {
        if let Some(index) = order.iter().position(|head| head == &item.head_sha) {
            groups[index].push(item);
        } else {
            order.push(item.head_sha.clone());
            groups.push(vec![item]);
        }
    }
    groups.into_iter().flatten().collect()
}

fn status_inner<O, E>(
    args: &StatusArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    io: &Io<O, E>,
) -> Result<u8, CmdError>
where
    O: Write + Send,
    E: Write + Send,
{
    let repo = Repo::open(cwd).map_err(|_| CmdError::new(2, "not a git repository"))?;
    let commit_ref = args.commit.as_deref().unwrap_or("HEAD");
    let commit = repo.resolve(commit_ref).map_err(|_| {
        CmdError::new(
            2,
            "could not resolve the commit; pass --commit with a commit in this repository",
        )
    })?;
    let commit = commit.to_string();
    let (repo_id, store) = open_store(cwd, env)?;
    let markers = store
        .list_markers(&repo_id)
        .map_err(|_| CmdError::new(1, "could not read the review history"))?;
    let reviews = store
        .list(&repo_id)
        .map_err(|_| CmdError::new(1, "could not read the review history"))?;
    let Some(hit) = select(&reviews, &markers, &store, &repo_id, &commit)? else {
        present_missing(args, &commit, io)?;
        return Ok(status_exit(StatusView::Missing));
    };
    if !args.json && hit.view != StatusView::Complete && hit.view != StatusView::Missing {
        let _ = writeln!(lock(&io.stderr), "{}", hit.line);
    }
    if args.json {
        let body = json!({
            "schema_version": "gitcko.review/1",
            "commit": commit,
            "review_id": hit.review_id,
            "lifecycle": hit.name,
            "completed_at": Value::Null,
            "base_sha": hit.base_sha,
            "head_sha": hit.head_sha,
            "status": hit.name,
            "reason_codes": hit.reason_codes,
        });
        let text = serde_json::to_string(&body)
            .map_err(|_| CmdError::new(1, "could not encode the status"))?;
        writeln!(lock(&io.stdout), "{text}")
            .map_err(|_| CmdError::new(1, "could not write the status"))?;
    } else {
        writeln!(lock(&io.stdout), "{}", hit.line)
            .map_err(|_| CmdError::new(1, "could not write the status"))?;
    }
    Ok(status_exit(hit.view))
}

struct Hit {
    review_id: String,
    name: String,
    line: String,
    view: StatusView,
    reason_codes: Vec<String>,
    base_sha: Option<String>,
    head_sha: Option<String>,
}

fn select(
    reviews: &[gitcko_storage::ReviewSummary],
    markers: &[StoredMarker],
    store: &ReviewStore,
    repo_id: &str,
    commit: &str,
) -> Result<Option<Hit>, CmdError> {
    let mut ids: Vec<String> = reviews
        .iter()
        .filter(|review| review.head_sha == commit)
        .map(|review| review.review_id.clone())
        .collect();
    for marker in markers {
        let sha = marker_head(marker);
        if sha.as_deref() == Some(commit) && !ids.iter().any(|id| id == &marker.review_id) {
            ids.push(marker.review_id.clone());
        }
    }
    ids.sort();
    let Some(id) = ids.pop() else {
        return Ok(None);
    };
    let marker = markers.iter().find(|marker| marker.review_id == id);
    if let Some(StoredMarker {
        state: RunState::Running(marker),
        ..
    }) = marker
    {
        return Ok(Some(Hit {
            review_id: id,
            name: "running".into(),
            line: "status: running".into(),
            view: StatusView::Running,
            reason_codes: Vec::new(),
            base_sha: marker.base_sha.clone(),
            head_sha: marker.head_sha.clone(),
        }));
    }
    if let Ok(review) = store.get(repo_id, &id) {
        let view = view_of(review.status);
        return Ok(Some(Hit {
            line: status_line(&review),
            name: status_name(review.status).to_string(),
            review_id: review.review_id,
            reason_codes: review.reason_codes,
            base_sha: Some(review.target.base_sha),
            head_sha: Some(review.target.head_sha),
            view,
        }));
    }
    if matches!(
        marker,
        Some(StoredMarker {
            state: RunState::Stale(_),
            ..
        })
    ) {
        let stale = marker.and_then(|marker| match &marker.state {
            RunState::Stale(body) => Some(body),
            _ => None,
        });
        return Ok(Some(Hit {
            review_id: id,
            name: "failed".into(),
            line: "status: failed".into(),
            view: StatusView::Failed,
            reason_codes: Vec::new(),
            base_sha: stale.and_then(|body| body.base_sha.clone()),
            head_sha: stale.and_then(|body| body.head_sha.clone()),
        }));
    }
    Ok(None)
}

fn marker_head(marker: &StoredMarker) -> Option<String> {
    match &marker.state {
        RunState::Running(body) | RunState::Stale(body) => body.head_sha.clone(),
        RunState::Idle => None,
    }
}

fn view_of(status: ReviewStatus) -> StatusView {
    match status {
        ReviewStatus::Complete => StatusView::Complete,
        ReviewStatus::Partial => StatusView::Partial,
        ReviewStatus::Failed => StatusView::Failed,
        ReviewStatus::Canceled => StatusView::Canceled,
        ReviewStatus::Superseded => StatusView::Superseded,
    }
}

fn present_missing<O, E>(args: &StatusArgs, commit: &str, io: &Io<O, E>) -> Result<(), CmdError>
where
    O: Write,
    E: Write,
{
    if args.json {
        let body = json!({
            "schema_version": "gitcko.review/1",
            "commit": commit,
            "review_id": Value::Null,
            "lifecycle": Value::Null,
            "completed_at": Value::Null,
            "base_sha": Value::Null,
            "head_sha": Value::Null,
            "status": Value::Null,
            "reason_codes": [],
        });
        let text = serde_json::to_string(&body)
            .map_err(|_| CmdError::new(1, "could not encode the status"))?;
        writeln!(lock(&io.stdout), "{text}")
            .map_err(|_| CmdError::new(1, "could not write the status"))?;
    } else {
        let _ = writeln!(lock(&io.stderr), "no review");
    }
    Ok(())
}

fn write_events<O, E>(
    store: &ReviewStore,
    repo_id: &str,
    review_id: &str,
    json_mode: bool,
    io: &Io<O, E>,
) -> Result<(), CmdError>
where
    O: Write,
    E: Write,
{
    let path = store
        .root()
        .join("repos")
        .join(repo_id)
        .join("reviews")
        .join(format!("{review_id}.events"));
    let Ok(bytes) = fs::read(&path) else {
        return Ok(());
    };
    let parsed: Vec<RunEvent> = serde_json::from_slice(&bytes).unwrap_or_default();
    let rendered = events(&parsed);
    if json_mode {
        write!(lock(&io.stderr), "{rendered}")
            .map_err(|_| CmdError::new(1, "could not write the events"))?;
    } else {
        write!(lock(&io.stdout), "{rendered}")
            .map_err(|_| CmdError::new(1, "could not write the events"))?;
    }
    Ok(())
}

fn open_store(
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(String, ReviewStore), CmdError> {
    let paths = GitckoPaths::from_env(env).map_err(|err| CmdError::new(2, err.to_string()))?;
    let repo_id = repo_cache_id(cwd).map_err(|err| CmdError::new(err.code, err.message))?;
    Ok((repo_id, ReviewStore::new(&paths.cache_dir)))
}

fn review_path(store: &ReviewStore, repo_id: &str, review_id: &str) -> PathBuf {
    store
        .root()
        .join("repos")
        .join(repo_id)
        .join("reviews")
        .join(format!("{review_id}.json"))
}

fn finding_counts(review: &Review) -> (u32, u32, u32) {
    let mut counts = (0, 0, 0);
    for finding in &review.findings {
        if finding.lifecycle == Lifecycle::Resolved {
            continue;
        }
        match finding.severity {
            Severity::P0 => counts.0 += 1,
            Severity::P1 => counts.1 += 1,
            Severity::P2 => counts.2 += 1,
        }
    }
    counts
}

fn status_name(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Complete => "complete",
        ReviewStatus::Partial => "partial",
        ReviewStatus::Failed => "failed",
        ReviewStatus::Canceled => "canceled",
        ReviewStatus::Superseded => "superseded",
    }
}

fn branch_for(cwd: &Path, repo: &Repo, head_sha: &str) -> Option<String> {
    let git = cwd.join(".git");
    let dir = if git.is_dir() {
        git
    } else {
        let text = fs::read_to_string(&git).ok()?;
        let rel = text.trim().strip_prefix("gitdir:")?.trim();
        let path = PathBuf::from(rel);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let head = fs::read_to_string(dir.join("HEAD")).ok()?;
    let name = head
        .lines()
        .next()?
        .trim()
        .strip_prefix("ref: refs/heads/")?;
    if name.is_empty() {
        return None;
    }
    let tip = repo.resolve(name).ok()?;
    (tip.to_string() == head_sha).then(|| name.to_string())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
