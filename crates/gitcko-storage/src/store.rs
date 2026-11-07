// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use gitcko_protocol::review::{Review, ReviewStatus};

use crate::StoreError;

/// Crockford Base32 as used by ULIDs (no I, L, O, U).
const ULID: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// On-disk review history for one cache root (`GITCKO_CACHE_DIR` or the XDG cache).
pub struct ReviewStore {
    root: PathBuf,
}

/// Pid and start time written while a review is in flight.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunningMarker {
    /// OS process id of the `gitcko review` that created the marker.
    pub pid: u32,
    /// RFC 3339 start, for humans; liveness is the pid.
    pub started: String,
}

/// What `review status` should say about a review id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    /// No marker; look at the stored JSON if any.
    Idle,
    /// Marker present and its pid is still alive.
    Running(RunningMarker),
    /// Marker present but the pid is gone: never report this as running.
    Stale(RunningMarker),
}

/// Enough of a stored review for `review show` with no id and `review status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSummary {
    /// `rv_` plus the ULID from the file name, after the on-disk id check.
    pub review_id: String,
    /// End state of that run.
    pub status: ReviewStatus,
    /// Why a non-complete run stopped; empty when complete.
    pub reason_codes: Vec<String>,
    /// Bound base commit.
    pub base_sha: String,
    /// Bound head commit.
    pub head_sha: String,
}

impl ReviewStore {
    /// `root` is the cache directory (`~/.cache/gitcko`), not a repo path.
    pub fn new(root: impl Into<PathBuf>) -> ReviewStore {
        ReviewStore { root: root.into() }
    }

    /// Cache root this store writes under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write `review` as `<review_id>.json`. The file appears only after a rename.
    pub fn put(&self, repo_id: &str, review: &Review) -> Result<(), StoreError> {
        check_review_id(&review.review_id)?;
        let dir = self.ensure_reviews(repo_id)?;
        let bytes = serde_json::to_vec(review).map_err(|_| StoreError::Io {
            op: "write the store",
        })?;
        write_atomic(&dir.join(format!("{}.json", review.review_id)), &bytes)
    }

    /// Load one stored review. A missing file is [`StoreError::NotFound`].
    pub fn get(&self, repo_id: &str, review_id: &str) -> Result<Review, StoreError> {
        check_review_id(review_id)?;
        let path = self.reviews_dir(repo_id)?.join(format!("{review_id}.json"));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound);
            }
            Err(_) => {
                return Err(StoreError::Io {
                    op: "read the store",
                });
            }
        };
        let review: Review =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt { check: "json" })?;
        if review.review_id != review_id {
            return Err(StoreError::Corrupt { check: "id" });
        }
        Ok(review)
    }

    /// Newest review id first (ULID order). Skips `.tmp` and `.running` files.
    pub fn list(&self, repo_id: &str) -> Result<Vec<ReviewSummary>, StoreError> {
        let dir = self.reviews_dir(repo_id)?;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => {
                return Err(StoreError::Io {
                    op: "read the store",
                });
            }
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| StoreError::Io {
                op: "read the store",
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.ends_with(".json") || name.contains(".tmp") {
                continue;
            }
            let id = &name[..name.len() - ".json".len()];
            if check_review_id(id).is_err() {
                continue;
            }
            match self.get(repo_id, id) {
                Ok(review) => out.push(ReviewSummary {
                    review_id: review.review_id,
                    status: review.status,
                    reason_codes: review.reason_codes,
                    base_sha: review.target.base_sha,
                    head_sha: review.target.head_sha,
                }),
                Err(StoreError::Corrupt { .. } | StoreError::NotFound) => continue,
                Err(err) => return Err(err),
            }
        }
        out.sort_by(|a, b| b.review_id.cmp(&a.review_id));
        Ok(out)
    }

    /// Record that `review_id` is in flight. Overwrites a leftover marker.
    pub fn mark_running(
        &self,
        repo_id: &str,
        review_id: &str,
        marker: &RunningMarker,
    ) -> Result<(), StoreError> {
        check_review_id(review_id)?;
        let dir = self.ensure_reviews(repo_id)?;
        let bytes = serde_json::to_vec(marker).map_err(|_| StoreError::Io {
            op: "write the store",
        })?;
        write_atomic(&dir.join(format!("{review_id}.running")), &bytes)
    }

    /// Remove the running marker. Missing is success: the run is already idle.
    pub fn clear_running(&self, repo_id: &str, review_id: &str) -> Result<(), StoreError> {
        check_review_id(review_id)?;
        let path = self
            .reviews_dir(repo_id)?
            .join(format!("{review_id}.running"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(StoreError::Io {
                op: "remove the marker",
            }),
        }
    }

    /// Idle, live, or stale. A gone pid is never reported as running.
    pub fn run_state(&self, repo_id: &str, review_id: &str) -> Result<RunState, StoreError> {
        check_review_id(review_id)?;
        let path = self
            .reviews_dir(repo_id)?
            .join(format!("{review_id}.running"));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RunState::Idle);
            }
            Err(_) => {
                return Err(StoreError::Io {
                    op: "read the store",
                });
            }
        };
        let marker: RunningMarker =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt { check: "json" })?;
        if pid_alive(marker.pid) {
            Ok(RunState::Running(marker))
        } else {
            Ok(RunState::Stale(marker))
        }
    }

    fn reviews_dir(&self, repo_id: &str) -> Result<PathBuf, StoreError> {
        check_repo_id(repo_id)?;
        Ok(self.root.join("repos").join(repo_id).join("reviews"))
    }

    fn ensure_reviews(&self, repo_id: &str) -> Result<PathBuf, StoreError> {
        let dir = self.reviews_dir(repo_id)?;
        make_dir(&dir).map_err(|_| StoreError::Io {
            op: "create the store",
        })?;
        Ok(dir)
    }
}

/// Repository ids name a folder, so they're held to a set no OS treats specially.
fn check_repo_id(id: &str) -> Result<(), StoreError> {
    let ok = (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidRepoId)
    }
}

fn check_review_id(id: &str) -> Result<(), StoreError> {
    let rest = id.strip_prefix("rv_").ok_or(StoreError::InvalidReviewId)?;
    let ok = rest.len() == 26 && rest.bytes().all(|b| ULID.contains(&b));
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidReviewId)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = {
        let name = path.file_name().ok_or(StoreError::Io {
            op: "write the store",
        })?;
        path.with_file_name(format!("{}.tmp", name.to_string_lossy()))
    };
    {
        let mut file = create_file(&tmp).map_err(|_| StoreError::Io {
            op: "write the store",
        })?;
        file.write_all(bytes).map_err(|_| StoreError::Io {
            op: "write the store",
        })?;
        file.sync_all().map_err(|_| StoreError::Io {
            op: "write the store",
        })?;
    }
    replace_file(&tmp, path).map_err(|_| StoreError::Io {
        op: "write the store",
    })
}

fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    match fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(dest)?;
            fs::rename(tmp, dest)
        }
        Err(err) => Err(err),
    }
}

fn make_dir(path: &Path) -> std::io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(path)
}

fn create_file(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return false;
        };
        rustix::process::test_kill_process(pid).is_ok()
    }
    #[cfg(windows)]
    {
        pid_alive_windows(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// `tasklist` is the Windows check: workspace `unsafe_code` is forbid, so we don't
/// call `OpenProcess`. A missing or failed spawn is treated as dead (fail closed).
#[cfg(windows)]
fn pid_alive_windows(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split(',')
            .nth(1)
            .is_some_and(|field| field.trim_matches('"') == pid.to_string())
    })
}
