// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain code view with line numbers. Syntax highlighting is P6.

use std::path::{Component, Path, PathBuf};

use crate::findings::LineSpec;
use crate::theme::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeView {
    pub path: String,
    pub line: u32,
    pub scroll: usize,
    numbered: Vec<String>,
}

pub fn open(root: &Path, path: &str, line: u32) -> Result<CodeView, String> {
    let safe = safe_path(root, path)?;
    let content =
        std::fs::read_to_string(&safe).map_err(|_| "could not read the file".to_string())?;
    let numbered = number_lines(&content);
    let scroll = (line.saturating_sub(1) as usize).min(numbered.len().saturating_sub(1));
    Ok(CodeView {
        path: path.to_string(),
        line,
        scroll,
        numbered,
    })
}

pub fn lines(view: &CodeView, height: usize, width: usize) -> Vec<LineSpec> {
    let mut out = vec![LineSpec {
        text: format!("{}:{}", view.path, view.line),
        role: Some(Role::Muted),
    }];
    let start = view.scroll;
    let end = (start + height.saturating_sub(2)).min(view.numbered.len());
    for line in &view.numbered[start..end] {
        out.push(LineSpec {
            text: fit(line, width),
            role: None,
        });
    }
    out
}

impl CodeView {
    pub fn scroll_by(&mut self, delta: i32) {
        let next = self.scroll as i32 + delta;
        self.scroll = next.clamp(0, self.numbered.len().saturating_sub(1) as i32) as usize;
    }

    pub fn top(&mut self) {
        self.scroll = 0;
    }

    pub fn bottom(&mut self, height: usize) {
        self.scroll = self.numbered.len().saturating_sub(height.saturating_sub(2));
    }
}

fn number_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{:>4} | {line}", index + 1))
        .collect()
}

fn safe_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    if path.contains('\\') {
        return Err("path is not safe".into());
    }
    let joined = root.join(path);
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => return Err("path escapes the repository".into()),
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    let canonical = normalized
        .canonicalize()
        .map_err(|_| "could not resolve the file".to_string())?;
    let root = root
        .canonicalize()
        .map_err(|_| "could not resolve the repository".to_string())?;
    if !canonical.starts_with(&root) {
        return Err("path escapes the repository".into());
    }
    Ok(canonical)
}

fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    if width > 0 {
        out.push('…');
    }
    out
}
