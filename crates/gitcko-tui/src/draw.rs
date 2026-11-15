// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure drawing. The same [`App`] produces the same cells, which is what the frame
//! snapshots lock in.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, Screen};
use crate::code;
use crate::findings::{self, LineSpec};
use crate::header::{self, Piece};
use crate::history;
use crate::theme::{self, Role};

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    if app.too_small() {
        frame.render_widget(paragraph(small_terminal_lines(), true), area);
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    frame.render_widget(paragraph(header_line(app, rows[0].width), false), rows[0]);
    frame.render_widget(paragraph(tab_line(app), false), rows[1]);
    frame.render_widget(
        paragraph(body_lines(app, rows[2].width, rows[2].height), true),
        rows[2],
    );
    frame.render_widget(
        paragraph(vec![footer_line(app, rows[3].width)], false),
        rows[3],
    );
}

fn paragraph(lines: Vec<Line<'static>>, wrap: bool) -> Paragraph<'static> {
    let paragraph = Paragraph::new(lines);
    if wrap {
        paragraph.wrap(Wrap { trim: false })
    } else {
        paragraph
    }
}

fn header_line(app: &App, width: u16) -> Vec<Line<'static>> {
    vec![Line::from(spans(
        header::pieces(&app.header, width),
        app.color,
    ))]
}

fn spans(pieces: Vec<Piece>, color: bool) -> Vec<Span<'static>> {
    pieces
        .into_iter()
        .map(|piece| Span::styled(piece.text, theme::style(color, piece.role)))
        .collect()
}

fn tab_line(app: &App) -> Vec<Line<'static>> {
    let tabs = [
        (Screen::Review, "1 Review"),
        (Screen::Findings, "2 Findings"),
        (Screen::History, "3 History"),
    ];
    let mut spans = Vec::new();
    for (index, (screen, label)) in tabs.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let active = app.screen == screen;
        let text = if active {
            format!("[{label}]")
        } else {
            label.to_string()
        };
        let role = if active { Role::Active } else { Role::Muted };
        spans.push(Span::styled(text, theme::style(app.color, role)));
    }
    vec![Line::from(spans)]
}

fn body_lines(app: &App, width: u16, height: u16) -> Vec<Line<'static>> {
    if app.help {
        return styled_lines(help_lines(), app.color);
    }
    if let Some(path) = &app.no_repo {
        return styled_lines(no_repo_lines(path), app.color);
    }
    if let Some(view) = &app.code {
        return from_specs(
            code::lines(view, usize::from(height), usize::from(width)),
            app.color,
        );
    }
    let specs = match app.screen {
        Screen::Review => app
            .review_lines(usize::from(width))
            .into_iter()
            .map(|line| LineSpec {
                text: line,
                role: None,
            })
            .collect(),
        Screen::Findings => findings::lines(
            &app.findings,
            app.active_review(),
            usize::from(width),
            app.color,
        ),
        Screen::History => history::lines(&app.history),
    };
    from_specs(specs, app.color)
}

fn styled_lines(lines: Vec<String>, color: bool) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| Line::from(Span::styled(line, theme::style(color, Role::Normal))))
        .collect()
}

fn from_specs(specs: Vec<LineSpec>, color: bool) -> Vec<Line<'static>> {
    specs
        .into_iter()
        .map(|spec| {
            let text = fit(&spec.text, 10_000);
            let role = spec.role.unwrap_or(Role::Normal);
            Line::from(Span::styled(text, theme::style(color, role)))
        })
        .collect()
}

fn help_lines() -> Vec<String> {
    vec![
        "1 Review   2 Findings   3 History   Tab next".into(),
        "j k move    g top    G bottom    Enter open".into(),
        "Esc back or cancel    Ctrl+C cancel, again to quit".into(),
        "y copy the fix    o editor    c coverage    d file or diff".into(),
        "/review /findings /history /index /model /help /quit".into(),
        "Screen readers: gitcko review --text".into(),
    ]
}

fn no_repo_lines(path: &str) -> Vec<String> {
    vec![
        "No repository".into(),
        path.to_string(),
        "Change to a git repository and run gitcko again.".into(),
    ]
}

fn small_terminal_lines() -> Vec<Line<'static>> {
    vec![
        Line::from("Terminal too small"),
        Line::from("Use at least 60 columns and 20 rows,"),
        Line::from("or run: gitcko review --text"),
        Line::from("Press any key to exit."),
    ]
}

fn footer_line(app: &App, width: u16) -> Line<'static> {
    let text = if let Some(command) = &app.command {
        format!("/{command}")
    } else if !app.status.is_empty() {
        app.status.clone()
    } else if app.help {
        "? close   q quit".into()
    } else {
        "? help   q quit".into()
    };
    Line::from(fit(&text, usize::from(width)))
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

pub fn render_text(app: &App, cols: u16, rows: u16) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows))
        .expect("test backend");
    terminal
        .draw(|frame| draw(frame, app))
        .expect("draw the test frame");
    buffer_text(terminal.backend().buffer())
}

pub fn cell_fg(app: &App, cols: u16, rows: u16, needle: &str) -> Option<Color> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows))
        .expect("test backend");
    terminal
        .draw(|frame| draw(frame, app))
        .expect("draw the test frame");
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width;
    let height = buffer.area.height;
    for y in 0..height {
        let mut row = String::new();
        let mut fg_at: Vec<Option<Color>> = Vec::new();
        for x in 0..width {
            let cell = &buffer[(x, y)];
            row.push_str(cell.symbol());
            fg_at.push(cell.style().fg);
        }
        if let Some(start) = row.find(needle) {
            let cell_x = row[..start].chars().count();
            return fg_at.get(cell_x).copied().flatten();
        }
    }
    None
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        for x in 0..buffer.area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        let row = row.trim_end();
        if !row.is_empty() {
            out.push_str(row);
            out.push('\n');
        }
    }
    out
}
