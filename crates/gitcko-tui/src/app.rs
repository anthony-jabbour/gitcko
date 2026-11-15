// SPDX-License-Identifier: AGPL-3.0-or-later

//! The screen state machine. Input becomes an [`Effect`]; nothing here touches the
//! terminal, the filesystem or a process.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use gitcko_protocol::event::RunEvent;
use gitcko_protocol::review::Review;
use gitcko_storage::{ReviewStore, StoreError};

use crate::code::{self, CodeView};
use crate::findings::{self, FindingsPane};
use crate::header::Header;
use crate::history::HistoryPane;
use crate::progress::{self, Phase, ReviewPane};

/// The three Phase 5 screens. Ask is not one of them (P5_PLAN C4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Review,
    Findings,
    History,
}

/// What the session should do after a key. The state machine never spawns a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    /// Start the review the session's thread runs.
    StartReview,
    /// Cancel the in-flight review and stay on this screen.
    CancelReview,
    /// Quit, and cancel the in-flight review first.
    QuitAndCancel,
    /// Open a file at a line in `$EDITOR`.
    OpenEditor {
        path: String,
        line: u32,
    },
    /// Copy text to the system clipboard.
    CopyFix(String),
}

/// Checkout paths the history and code screens need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoContext {
    pub root: PathBuf,
    pub cache_id: String,
    pub cache_dir: PathBuf,
}

/// Everything the frame is a function of.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub screen: Screen,
    pub header: Header,
    pub help: bool,
    pub quit: bool,
    /// Set while `/` is collecting a command. `None` means the keys are screen keys.
    pub command: Option<String>,
    /// Footer note: a command result, or why a review cannot start.
    pub status: String,
    /// Set when this directory is not a git repository. The body says how to fix it.
    pub no_repo: Option<String>,
    pub color: bool,
    pub reduced_motion: bool,
    /// Last known size, so a key can see the small-terminal screen.
    pub cols: u16,
    pub rows: u16,
    /// False until a model is configured and this directory is a repository.
    pub can_review: bool,
    pub repo: Option<RepoContext>,
    pub findings: FindingsPane,
    pub history: HistoryPane,
    pub code: Option<CodeView>,
    confirm_quit: bool,
    review: ReviewPane,
}

impl App {
    /// The snapshot app: a ready local model on a feature branch.
    pub fn preview() -> App {
        App {
            screen: Screen::Review,
            header: Header::preview(),
            help: false,
            quit: false,
            command: None,
            status: String::new(),
            no_repo: None,
            color: true,
            reduced_motion: false,
            cols: 80,
            rows: 24,
            can_review: true,
            repo: None,
            findings: FindingsPane::empty(),
            history: HistoryPane::empty(),
            code: None,
            confirm_quit: false,
            review: ReviewPane::idle(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.review.phase
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    pub fn too_small(&self) -> bool {
        self.cols < 60 || self.rows < 20
    }

    pub fn active_review(&self) -> Option<&Review> {
        findings::active_review(&self.findings.open, self.review.review())
    }

    pub fn refresh_history(&mut self) {
        let Some(repo) = &self.repo else {
            return;
        };
        let store = ReviewStore::new(&repo.cache_dir);
        self.history.refresh(&store, &repo.cache_id);
    }

    /// Apply one key. Release events are ignored so a terminal that reports them
    /// does not run the action twice.
    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if self.too_small() {
            self.quit = true;
            return Effect::Quit;
        }
        if self.command.is_some() {
            return self.on_command_key(key);
        }
        if self.code.is_some() {
            return self.on_code_key(key);
        }
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.review.phase == Phase::Running {
                    self.cancel_review()
                } else {
                    self.request_quit()
                }
            }
            KeyCode::Char('?') => {
                self.help = !self.help;
                Effect::None
            }
            KeyCode::Esc if self.help => {
                self.help = false;
                Effect::None
            }
            KeyCode::Esc => self.cancel_review(),
            KeyCode::Char('1') => self.go(Screen::Review),
            KeyCode::Char('2') => self.go(Screen::Findings),
            KeyCode::Char('3') => self.go(Screen::History),
            KeyCode::Tab => self.cycle(),
            KeyCode::Char('/') => {
                self.command = Some(String::new());
                Effect::None
            }
            KeyCode::Enter => self.screen_enter(),
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('g') => self.move_top(),
            KeyCode::Char('G') => self.move_bottom(),
            KeyCode::Char('o') => self.open_editor(),
            KeyCode::Char('y') => self.copy_fix(),
            KeyCode::Char('c') => self.toggle_coverage(),
            KeyCode::Char('d') => self.toggle_diff(),
            _ => Effect::None,
        }
    }

    fn screen_enter(&mut self) -> Effect {
        match self.screen {
            Screen::Review => self.start_review(),
            Screen::Findings => self.open_code_from_finding(),
            Screen::History => self.open_history_review(),
        }
    }

    fn open_history_review(&mut self) -> Effect {
        let Some(repo) = self.repo.clone() else {
            self.status = "no repository".into();
            return Effect::None;
        };
        let store = ReviewStore::new(&repo.cache_dir);
        match self.history.load_selected(&store, &repo.cache_id) {
            Ok(review) => {
                self.findings.open = Some(review);
                self.findings.selected = 0;
                self.screen = Screen::Findings;
                self.status.clear();
                Effect::None
            }
            Err(message) => {
                self.status = message;
                Effect::None
            }
        }
    }

    fn open_code_from_finding(&mut self) -> Effect {
        let Some(review) = self.active_review() else {
            return Effect::None;
        };
        let Some(finding) = review.findings.get(self.findings.selected) else {
            return Effect::None;
        };
        let Some(repo) = &self.repo else {
            return Effect::None;
        };
        match code::open(&repo.root, &finding.path, finding.start_line) {
            Ok(view) => {
                self.code = Some(view);
                Effect::None
            }
            Err(message) => {
                self.status = message;
                Effect::None
            }
        }
    }

    fn open_editor(&mut self) -> Effect {
        if self.screen != Screen::Findings {
            return Effect::None;
        }
        let Some(review) = self.active_review() else {
            return Effect::None;
        };
        let Some(finding) = review.findings.get(self.findings.selected) else {
            return Effect::None;
        };
        Effect::OpenEditor {
            path: finding.path.clone(),
            line: finding.start_line,
        }
    }

    fn copy_fix(&mut self) -> Effect {
        if self.screen != Screen::Findings {
            return Effect::None;
        }
        let Some(review) = self.active_review() else {
            return Effect::None;
        };
        let Some(finding) = review.findings.get(self.findings.selected) else {
            return Effect::None;
        };
        let Some(text) = findings::fix_text(finding) else {
            self.status = "no fix to copy".into();
            return Effect::None;
        };
        self.status = "copied fix".into();
        Effect::CopyFix(text)
    }

    fn toggle_coverage(&mut self) -> Effect {
        if self.screen == Screen::Findings {
            self.findings.show_coverage = !self.findings.show_coverage;
        }
        Effect::None
    }

    fn toggle_diff(&mut self) -> Effect {
        if self.screen == Screen::Findings {
            self.findings.show_diff = !self.findings.show_diff;
        }
        Effect::None
    }

    fn finding_count(&self) -> usize {
        self.active_review()
            .map(|review| review.findings.len())
            .unwrap_or(0)
    }

    fn move_up(&mut self) -> Effect {
        match self.screen {
            Screen::Findings => {
                self.findings.move_by(self.finding_count(), -1);
            }
            Screen::History => self.history.move_by(-1),
            Screen::Review => {}
        }
        Effect::None
    }

    fn move_down(&mut self) -> Effect {
        match self.screen {
            Screen::Findings => self.findings.move_by(self.finding_count(), 1),
            Screen::History => self.history.move_by(1),
            Screen::Review => {}
        }
        Effect::None
    }

    fn move_top(&mut self) -> Effect {
        match self.screen {
            Screen::Findings => self.findings.top(),
            Screen::History => self.history.top(),
            Screen::Review => {}
        }
        Effect::None
    }

    fn move_bottom(&mut self) -> Effect {
        match self.screen {
            Screen::Findings => self.findings.bottom(self.finding_count()),
            Screen::History => self.history.bottom(),
            Screen::Review => {}
        }
        Effect::None
    }

    fn on_code_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Esc => {
                self.code = None;
                Effect::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(view) = self.code.as_mut() {
                    view.scroll_by(1);
                }
                Effect::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(view) = self.code.as_mut() {
                    view.scroll_by(-1);
                }
                Effect::None
            }
            KeyCode::Char('g') => {
                if let Some(view) = self.code.as_mut() {
                    view.top();
                }
                Effect::None
            }
            KeyCode::Char('G') => {
                if let Some(view) = self.code.as_mut() {
                    view.bottom(usize::from(self.rows));
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn go(&mut self, screen: Screen) -> Effect {
        self.help = false;
        self.screen = screen;
        if screen == Screen::History {
            self.refresh_history();
        }
        Effect::None
    }

    fn cycle(&mut self) -> Effect {
        self.help = false;
        self.screen = match self.screen {
            Screen::Review => Screen::Findings,
            Screen::Findings => Screen::History,
            Screen::History => Screen::Review,
        };
        if self.screen == Screen::History {
            self.refresh_history();
        }
        Effect::None
    }

    fn request_quit(&mut self) -> Effect {
        if self.review.phase == Phase::Running {
            if !self.confirm_quit {
                self.confirm_quit = true;
                self.status = "A review is running. Press q again to quit.".into();
                return Effect::None;
            }
            self.review.phase = Phase::Canceled;
            self.confirm_quit = false;
            self.status = "canceled".into();
            self.quit = true;
            return Effect::QuitAndCancel;
        }
        self.quit = true;
        Effect::Quit
    }

    fn cancel_review(&mut self) -> Effect {
        if self.review.phase != Phase::Running {
            return Effect::None;
        }
        self.review.phase = Phase::Canceled;
        self.confirm_quit = false;
        self.status = "canceled".into();
        Effect::CancelReview
    }

    fn start_review(&mut self) -> Effect {
        if self.help || self.screen != Screen::Review || self.no_repo.is_some() || !self.can_review
        {
            return Effect::None;
        }
        if self.review.phase == Phase::Running {
            return Effect::None;
        }
        self.findings.open = None;
        self.review = ReviewPane::idle();
        self.review.phase = Phase::Running;
        self.confirm_quit = false;
        self.status.clear();
        Effect::StartReview
    }

    pub fn tick(&mut self) {
        if self.review.phase == Phase::Running && !self.reduced_motion {
            self.review.tick();
        }
    }

    pub fn apply_event(&mut self, event: &RunEvent) {
        if matches!(self.review.phase, Phase::Running | Phase::Canceled) {
            self.review.apply(event);
        }
    }

    pub fn finish(&mut self, review: Review, stored: bool) {
        let user_canceled = self.review.phase == Phase::Canceled;
        let outcome_canceled = review.status == gitcko_protocol::review::ReviewStatus::Canceled;
        self.review.result = Some(review);
        self.confirm_quit = false;
        if user_canceled || outcome_canceled {
            self.review.phase = Phase::Canceled;
            self.status = "canceled".into();
            return;
        }
        self.review.phase = Phase::Finished;
        self.screen = Screen::Findings;
        if stored {
            self.status.clear();
            self.refresh_history();
        } else {
            self.status = "could not store the review".into();
        }
    }

    pub fn fail_review(&mut self, message: &str) {
        if self.review.phase == Phase::Canceled {
            return;
        }
        self.review.phase = Phase::Idle;
        self.status = message.to_string();
    }

    pub(crate) fn review_lines(&self, width: usize) -> Vec<String> {
        progress::lines(&self.review, &self.header.base, width, self.reduced_motion)
    }

    fn on_command_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Esc => {
                self.command = None;
                Effect::None
            }
            KeyCode::Enter => self.run_command(),
            KeyCode::Backspace => {
                if let Some(buffer) = self.command.as_mut() {
                    buffer.pop();
                }
                Effect::None
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.command = None;
                self.request_quit()
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(buffer) = self.command.as_mut() {
                    buffer.push(ch);
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn run_command(&mut self) -> Effect {
        let raw = self.command.take().unwrap_or_default();
        let mut parts = raw.split_whitespace();
        let command = parts.next().unwrap_or("");
        match command {
            "review" | "1" => self.go(Screen::Review),
            "findings" | "2" => self.go(Screen::Findings),
            "history" | "3" => self.go(Screen::History),
            "help" | "?" => {
                self.help = true;
                Effect::None
            }
            "quit" | "q" => self.request_quit(),
            "index" => {
                self.status = format!("index: {}", self.header.index.label());
                Effect::None
            }
            "model" => {
                self.status = format!(
                    "model: {} ({})",
                    self.header.model,
                    self.header.locality.label()
                );
                Effect::None
            }
            "ask" | "export" | "config" => {
                self.status = format!("{command} is not in this build");
                Effect::None
            }
            "" => Effect::None,
            other => {
                let shown: String = other.chars().take(40).collect();
                self.status = format!("unknown command {shown}");
                Effect::None
            }
        }
    }
}

/// Preview helper: attach a stored review fixture for snapshots.
pub fn preview_with_review(review: Review) -> App {
    let mut app = App::preview();
    app.screen = Screen::Findings;
    app.review.result = Some(review);
    app
}

/// Preview helper: partial incomplete review on Findings.
pub fn preview_partial() -> App {
    let review: Review = serde_json::from_str(include_str!(
        "../../gitcko-protocol/tests/data/review-full.json"
    ))
    .expect("fixture");
    let mut review = review;
    review.status = gitcko_protocol::review::ReviewStatus::Partial;
    review.reason_codes = vec!["budget_exhausted".into()];
    review.score = None;
    review.findings.clear();
    preview_with_review(review)
}

pub fn load_history_fixture(
    app: &mut App,
    cache_dir: &std::path::Path,
    repo_id: &str,
    review: &Review,
) -> Result<(), StoreError> {
    let store = ReviewStore::new(cache_dir);
    store.put(repo_id, review)?;
    app.repo = Some(RepoContext {
        root: std::env::current_dir().unwrap_or_default(),
        cache_id: repo_id.into(),
        cache_dir: cache_dir.to_path_buf(),
    });
    app.refresh_history();
    Ok(())
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
