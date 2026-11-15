// SPDX-License-Identifier: AGPL-3.0-or-later

//! The real terminal. [`Guard`] restores it on drop, including when a panic unwinds
//! the draw, and [`run`] installs the same restore on the panic hook so a panic that
//! skips a drop still hands the terminal back.

use std::collections::BTreeMap;
use std::io::{self, stdout};
use std::panic;
use std::path::Path;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::context;
use crate::draw;

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

/// Open the shell on this checkout and run until the user quits.
pub fn run(cwd: &Path, env: &BTreeMap<String, String>) -> io::Result<u8> {
    let mut app = context::assemble(cwd, env);
    install_panic_hook();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let _guard = Guard::new(CrosstermRestorer);
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    loop {
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
            app.on_key(key);
        }
    }
    Ok(0)
}
