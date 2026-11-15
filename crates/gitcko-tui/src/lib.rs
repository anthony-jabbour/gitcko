// SPDX-License-Identifier: AGPL-3.0-or-later

//! The full-screen terminal UI (TUI_SPEC, P5_PLAN §5). Phase 5 is Review, Findings,
//! History and a plain code view. Ask, graph jumps and syntax highlighting are later.
//!
//! Drawing is a pure function of [`App`]. The real terminal is only entered from
//! [`run`], and a panic while it is up restores the user's terminal.

mod app;
mod context;
mod draw;
mod header;
mod session;
mod theme;

pub use app::{App, Effect, Screen, key};
pub use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
pub use header::{Header, IndexState, Locality};
pub use ratatui::style::Color;
pub use session::{Guard, Restorer, run};

/// Cache key for this checkout: SHA-256 of the canonical working directory. The CLI
/// uses the same bytes, so a review the TUI stores is the one `review show` reads.
pub fn repo_cache_id(cwd: &std::path::Path) -> Option<String> {
    context::repo_cache_id(cwd)
}

/// The shell for this checkout: header, index and model, or the no-repository screen.
pub fn load(cwd: &std::path::Path, env: &std::collections::BTreeMap<String, String>) -> App {
    context::assemble(cwd, env)
}

/// The frame `app` would show at `cols` by `rows`, as text. Trailing spaces are
/// dropped so a snapshot is the content, not the empty cells.
pub fn render_text(app: &App, cols: u16, rows: u16) -> String {
    draw::render_text(app, cols, rows)
}

/// Foreground of the first cell of `needle`, if the frame contains it.
pub fn cell_fg(app: &App, cols: u16, rows: u16, needle: &str) -> Option<Color> {
    draw::cell_fg(app, cols, rows, needle)
}
