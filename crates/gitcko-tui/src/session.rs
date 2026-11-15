// SPDX-License-Identifier: AGPL-3.0-or-later

//! The real terminal. [`Guard`] restores it on drop, including when a panic unwinds
//! the draw, and [`run`] installs the same restore on the panic hook so a panic that
//! skips a drop still hands the terminal back.

use std::collections::BTreeMap;
use std::io::{self, stdout};
use std::panic;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Receiver;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use tokio_util::sync::CancellationToken;

use crate::app::Effect;
use crate::context;
use crate::draw;
use crate::drive::{self, DriveMsg};

/// Something that can hand the terminal back. The live one calls crossterm; tests
/// record the call.
pub trait Restorer {
    fn restore(&mut self);
}

/// Runs `restore` exactly once, on drop. A panic unwinds through this, so the
/// terminal is restored on the way out.
pub struct Guard<R: Restorer> {
    restorer: Option<R>,
}

impl<R: Restorer> Guard<R> {
    pub fn new(restorer: R) -> Guard<R> {
        Guard {
            restorer: Some(restorer),
        }
    }
}

impl<R: Restorer> Drop for Guard<R> {
    fn drop(&mut self) {
        if let Some(mut restorer) = self.restorer.take() {
            restorer.restore();
        }
    }
}

struct CrosstermRestorer;

impl Restorer for CrosstermRestorer {
    fn restore(&mut self) {
        restore_terminal();
    }
}

/// Leave the alternate screen and raw mode. Safe to call when neither is active:
/// crossterm's errors are ignored so a second call cannot panic.
pub fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut out = stdout();
    let _ = execute!(out, LeaveAlternateScreen, Show);
}

fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// Side effects after a key. A plain quit leaves the cancel token alone.
pub fn apply_effect(
    effect: &Effect,
    cwd: &Path,
    cancel: &CancellationToken,
    env: &BTreeMap<String, String>,
) {
    match effect {
        Effect::CancelReview | Effect::QuitAndCancel => cancel.cancel(),
        Effect::OpenEditor { path, line } => open_editor(cwd, path, *line, env),
        Effect::CopyFix(_) => {}
        Effect::None | Effect::Quit | Effect::StartReview => {}
    }
}

fn open_editor(cwd: &Path, path: &str, line: u32, env: &BTreeMap<String, String>) {
    let editor = env
        .get("EDITOR")
        .cloned()
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".into());
    let target = format!("+{line}");
    let _ = Command::new(&editor)
        .arg(&target)
        .arg(cwd.join(path))
        .spawn();
}

/// Open the shell on this checkout and run until the user quits.
pub fn run(cwd: &Path, env: &BTreeMap<String, String>) -> io::Result<u8> {
    let mut app = context::assemble(cwd, env);
    install_panic_hook();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let _guard = Guard::new(CrosstermRestorer);
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut cancel = CancellationToken::new();
    let mut inbox: Option<Receiver<DriveMsg>> = None;
    loop {
        drive::pump(&mut app, &mut inbox);
        app.tick();
        let size = terminal.size()?;
        app.resize(size.width, size.height);
        terminal.draw(|frame| draw::draw(frame, &app))?;
        if app.quit {
            break;
        }
        if event::poll(std::time::Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
        {
            match app.on_key(key) {
                Effect::StartReview => {
                    cancel = CancellationToken::new();
                    inbox = Some(drive::start(cwd.to_path_buf(), env.clone(), cancel.clone()));
                }
                effect => apply_effect(&effect, cwd, &cancel, env),
            }
        }
    }
    Ok(0)
}
