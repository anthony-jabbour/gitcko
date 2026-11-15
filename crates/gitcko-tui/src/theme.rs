// SPDX-License-Identifier: AGPL-3.0-or-later

//! Colour from the brand tokens. `NO_COLOR` drops every one of them: a frame must
//! still be readable as text (TUI_SPEC §8).

use std::collections::BTreeMap;

use ratatui::style::{Color, Modifier, Style};

/// Gitcko sky, the focus colour (BRANDING §3).
const SKY: Color = Color::Rgb(0xA9, 0xDD, 0xF7);

/// What a span is for. The monochrome path ignores this and draws plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The mark or the word Gitcko.
    Brand,
    /// The selected tab or row.
    Active,
    /// A problem badge: missing index, failed index.
    Problem,
    /// An inactive tab.
    Muted,
    /// Body text.
    Normal,
}

pub fn style(color: bool, role: Role) -> Style {
    if !color {
        return Style::default();
    }
    match role {
        Role::Brand | Role::Active => Style::default().fg(SKY).add_modifier(Modifier::BOLD),
        Role::Problem => Style::default().fg(Color::Red),
        Role::Muted => Style::default().fg(Color::Gray),
        Role::Normal => Style::default(),
    }
}

/// `FORCE_COLOR` wins, then `NO_COLOR`, then the user's `color` key. An absent
/// preference is colour on, because a terminal UI is how this screen is read.
pub fn color_enabled(env: &BTreeMap<String, String>, preference: Option<bool>) -> bool {
    if env
        .get("FORCE_COLOR")
        .is_some_and(|value| !value.is_empty() && value != "0")
    {
        return true;
    }
    if env.contains_key("NO_COLOR") {
        return false;
    }
    preference.unwrap_or(true)
}

pub fn reduced_motion(env: &BTreeMap<String, String>) -> bool {
    env.get("GITCKO_REDUCED_MOTION")
        .is_some_and(|value| value == "1")
}
