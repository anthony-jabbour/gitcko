// SPDX-License-Identifier: AGPL-3.0-or-later

//! The top line: repository, branch, base, index and model, with a local or remote
//! badge (TUI_SPEC §4). Wide terminals lead with the half-block mark; narrower ones
//! lead with the word (TUI_SPEC §3).

use crate::theme::Role;

/// What the header says about this checkout. The session fills it; tests set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// `owner/name` when the origin remote says so, otherwise the directory name.
    pub repo: String,
    /// Current branch, or a short commit when HEAD is detached.
    pub branch: String,
    /// The ref a review will compare against.
    pub base: String,
    pub index: IndexState,
    /// The review binding's model, or `none` when setup has not been run.
    pub model: String,
    pub locality: Locality,
}

impl Header {
    /// A stable header for frame snapshots.
    pub fn preview() -> Header {
        Header {
            repo: "acme/api".into(),
            branch: "feat/auth".into(),
            base: "main".into(),
            index: IndexState::Ready,
            model: "qwen3-coder".into(),
            locality: Locality::Local,
        }
    }
}

/// Index badge. Building is shown when a caller knows a build is in progress; the
/// store itself reports missing, ready, stale or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexState {
    Missing,
    Building,
    Stale,
    Failed {
        /// Safe to show. Names the failure, never a path or a source line.
        reason: String,
    },
    Ready,
}

impl IndexState {
    pub fn label(&self) -> &'static str {
        match self {
            IndexState::Missing => "missing",
            IndexState::Building => "building",
            IndexState::Stale => "stale",
            IndexState::Failed { .. } => "failed",
            IndexState::Ready => "ready",
        }
    }

    /// The line under the header when the index is not ready to review against.
    pub fn banner(&self) -> Option<String> {
        match self {
            IndexState::Missing => Some("index missing — run `gitcko index`".into()),
            IndexState::Building => Some("index building".into()),
            IndexState::Stale => Some("index stale — run `gitcko index`".into()),
            IndexState::Failed { reason } => Some(format!("index failed: {reason}")),
            IndexState::Ready => None,
        }
    }

    fn role(&self) -> Role {
        match self {
            IndexState::Ready => Role::Normal,
            IndexState::Building => Role::Active,
            IndexState::Missing | IndexState::Stale | IndexState::Failed { .. } => Role::Problem,
        }
    }
}

/// Where the selected review model sends code (PRIVACY §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locality {
    /// A loopback model. Code stays on this machine.
    Local,
    /// A remote provider. The header names it.
    Remote { provider: String },
}

impl Locality {
    pub fn label(&self) -> String {
        match self {
            Locality::Local => "local".into(),
            Locality::Remote { provider } => format!("remote: {provider}"),
        }
    }
}

/// One styled piece of the header. Joined, they are the whole line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub role: Role,
}

/// Header pieces fitted to `width`. The mark is only on wide terminals.
pub fn pieces(header: &Header, width: u16) -> Vec<Piece> {
    let wide = width >= 100;
    let brand = if wide { "▄▀▄ Gitcko" } else { "Gitcko" };
    let pieces = if wide {
        vec![
            Piece {
                text: brand.into(),
                role: Role::Brand,
            },
            Piece {
                text: format!(
                    "  repo: {}  branch: {} → {}  index: ",
                    header.repo, header.branch, header.base
                ),
                role: Role::Normal,
            },
            Piece {
                text: header.index.label().into(),
                role: header.index.role(),
            },
            Piece {
                text: format!("  model: {} ({})", header.model, header.locality.label()),
                role: Role::Normal,
            },
        ]
    } else {
        vec![
            Piece {
                text: format!(
                    "{brand} {} {}→{} idx:",
                    header.repo, header.branch, header.base
                ),
                role: Role::Brand,
            },
            Piece {
                text: header.index.label().into(),
                role: header.index.role(),
            },
            Piece {
                text: format!(" {} {}", header.model, header.locality.label()),
                role: Role::Normal,
            },
        ]
    };
    fit(pieces, usize::from(width))
}

fn fit(pieces: Vec<Piece>, width: usize) -> Vec<Piece> {
    let mut left = width;
    let mut out = Vec::new();
    for piece in pieces {
        if left == 0 {
            break;
        }
        let count = piece.text.chars().count();
        if count <= left {
            left -= count;
            out.push(piece);
            continue;
        }
        let keep = left.saturating_sub(1);
        let mut text: String = piece.text.chars().take(keep).collect();
        if left > 0 {
            text.push('…');
        }
        out.push(Piece {
            text,
            role: piece.role,
        });
        break;
    }
    out
}
