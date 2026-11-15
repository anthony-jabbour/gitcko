// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stored reviews for this repo, same data as `gitcko review show`.

use gitcko_protocol::review::Review;
use gitcko_render::{ListItem, list};
use gitcko_storage::{ReviewStore, ReviewSummary, StoreError};

use crate::findings::LineSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPane {
    pub items: Vec<ReviewSummary>,
    pub selected: usize,
    pub scroll: usize,
    pub error: Option<String>,
}

impl HistoryPane {
    pub fn empty() -> HistoryPane {
        HistoryPane {
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            error: None,
        }
    }

    pub fn refresh(&mut self, store: &ReviewStore, repo_id: &str) {
        match store.list(repo_id) {
            Ok(items) => {
                self.items = items;
                self.error = None;
                self.selected = self.selected.min(self.items.len().saturating_sub(1));
            }
            Err(StoreError::Io { op }) => {
                self.error = Some(format!("could not read history: {op}"));
            }
            Err(StoreError::Corrupt { check }) => {
                self.error = Some(format!("history is corrupt: {check}"));
            }
            Err(StoreError::NotFound | StoreError::InvalidRepoId | StoreError::InvalidReviewId) => {
                self.items.clear();
            }
        }
    }

    pub fn move_by(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let next = self.selected as i32 + delta;
        self.selected = next.clamp(0, self.items.len() as i32 - 1) as usize;
    }

    pub fn top(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn bottom(&mut self) {
        self.selected = self.items.len().saturating_sub(1);
    }

    pub fn load_selected(&self, store: &ReviewStore, repo_id: &str) -> Result<Review, String> {
        let Some(item) = self.items.get(self.selected) else {
            return Err("no review selected".into());
        };
        store
            .get(repo_id, &item.review_id)
            .map_err(|_| "could not load the review".into())
    }
}

pub fn lines(pane: &HistoryPane) -> Vec<LineSpec> {
    let mut out = Vec::new();
    if let Some(error) = &pane.error {
        out.push(LineSpec {
            text: error.clone(),
            role: Some(crate::theme::Role::Problem),
        });
        return out;
    }
    if pane.items.is_empty() {
        out.push(LineSpec {
            text: "No stored reviews yet.".into(),
            role: None,
        });
        return out;
    }
    let rows: Vec<ListItem> = pane
        .items
        .iter()
        .map(|item| ListItem {
            review_id: item.review_id.clone(),
            status: item.status,
            head_sha: item.head_sha.clone(),
        })
        .collect();
    let rendered = list(&rows);
    for (index, line) in rendered.lines().enumerate() {
        let marker = if index == pane.selected { ">" } else { " " };
        let role = if index == pane.selected {
            Some(crate::theme::Role::Active)
        } else {
            None
        };
        out.push(LineSpec {
            text: format!("{marker} {line}"),
            role,
        });
    }
    out.push(LineSpec {
        text: "Enter opens in Findings.".into(),
        role: Some(crate::theme::Role::Muted),
    });
    out
}
