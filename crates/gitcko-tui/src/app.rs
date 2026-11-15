// SPDX-License-Identifier: AGPL-3.0-or-later

//! The screen state machine. Input becomes an [`Effect`]; nothing here touches the
//! terminal, the filesystem or a process.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::header::Header;

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
}

/// Everything the frame is a function of.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    /// Apply one key. Release events are ignored so a terminal that reports them
    /// does not run the action twice.
    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if self.command.is_some() {
            return self.on_command_key(key);
        }
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.request_quit()
            }
            KeyCode::Char('?') => {
                self.help = !self.help;
                Effect::None
            }
            KeyCode::Esc if self.help => {
                self.help = false;
                Effect::None
            }
            KeyCode::Char('1') => self.go(Screen::Review),
            KeyCode::Char('2') => self.go(Screen::Findings),
            KeyCode::Char('3') => self.go(Screen::History),
            KeyCode::Tab => self.cycle(),
            KeyCode::Char('/') => {
                self.command = Some(String::new());
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn go(&mut self, screen: Screen) -> Effect {
        self.help = false;
        self.screen = screen;
        Effect::None
    }

    fn cycle(&mut self) -> Effect {
        self.help = false;
        self.screen = match self.screen {
            Screen::Review => Screen::Findings,
            Screen::Findings => Screen::History,
            Screen::History => Screen::Review,
        };
        Effect::None
    }

    fn request_quit(&mut self) -> Effect {
        self.quit = true;
        Effect::Quit
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

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
