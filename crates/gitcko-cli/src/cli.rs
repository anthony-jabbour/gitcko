// SPDX-License-Identifier: AGPL-3.0-or-later

//! The command tree for Phase 5 (P5_PLAN §10.1). Parsing uses clap's derive API, which
//! the `derive` feature turns on
//! (<https://docs.rs/clap/4.6.7/clap/_features/index.html>).
//!
//! Flags that this build does not run (`--plus`, `--layout diff`, …) are still real
//! flags. An unknown flag is exit 2; a known flag this build refuses is exit 6, and
//! that decision happens after parsing so the two codes stay distinct.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The `gitcko` command. No subcommand means help on stderr and exit 2 when this
/// isn't a TUI session (the TUI arrives later).
#[derive(Debug, Parser)]
#[command(
    name = "gitcko",
    about = "Review a local git repository",
    disable_version_flag = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Print the version line and exit. Same text as `gitcko version`.
    #[arg(long = "version", action = clap::ArgAction::SetTrue)]
    pub version_flag: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level commands this build ships. Later batches add config, setup, doctor,
/// update and mcp on this enum.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Review committed changes, or look at a stored review.
    Review(ReviewArgs),
    /// Build or refresh this repository's local index.
    Index(IndexArgs),
    /// Show or delete local cache data.
    Cache(CacheArgs),
    /// Print the binary version and the schema ids it writes.
    Version,
}

/// `gitcko review` and its flags (CLI_SPEC §3). `show` and `status` are subcommands.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ReviewArgs {
    #[command(subcommand)]
    pub action: Option<ReviewCmd>,
    /// Compare HEAD with the merge base of this ref.
    #[arg(short = 'b', long)]
    pub base: Option<String>,
    /// Review up to this ref instead of HEAD.
    #[arg(long)]
    pub head: Option<String>,
    /// Limit the review to these repository-relative paths.
    #[arg(long = "paths", num_args = 1..)]
    pub paths: Vec<String>,
    /// Review these held-back paths anyway.
    #[arg(long = "include", num_args = 1..)]
    pub include: Vec<String>,
    /// On a complete review, exit 7 when a finding is at least this severe.
    #[arg(long, value_enum)]
    pub fail_on: Option<FailOn>,
    /// One JSON object on stdout. Progress stays on stderr.
    #[arg(long, group = "mode")]
    pub json: bool,
    /// Plain text, even when stdout is a terminal.
    #[arg(long, group = "mode")]
    pub text: bool,
    /// Plain text for an agent: no prompts.
    #[arg(long, group = "mode")]
    pub agent: bool,
    /// Lines of context around a finding (0–60).
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=60))]
    pub context: Option<u8>,
    /// Width of the text box (40–240).
    #[arg(long, value_parser = clap::value_parser!(u16).range(40..=240))]
    pub width: Option<u16>,
    /// Never prompt. A missing answer exits 2.
    #[arg(long)]
    pub no_input: bool,
    /// Color the text output.
    #[arg(long, group = "color_flag")]
    pub color: bool,
    /// Do not color the text output.
    #[arg(long, group = "color_flag")]
    pub no_color: bool,
    /// Not in this build.
    #[arg(long)]
    pub plus: bool,
    /// Not in this build.
    #[arg(long)]
    pub max: bool,
    /// Not in this build.
    #[arg(long)]
    pub auto: bool,
    /// Not in this build.
    #[arg(long)]
    pub cloud: bool,
    /// Not in this build.
    #[arg(long)]
    pub verify: bool,
    /// Not in this build.
    #[arg(long)]
    pub instructions: Option<String>,
    /// Not in this build.
    #[arg(long)]
    pub resume: bool,
    /// `comments` is the only layout this build prints. `diff` exits 6.
    #[arg(long, value_enum)]
    pub layout: Option<LayoutArg>,
}

/// `review show` and `review status`.
#[derive(Debug, Subcommand)]
pub enum ReviewCmd {
    /// Print one stored review, or list them when no id is given.
    Show(ShowArgs),
    /// Exit code for the latest stored review of a commit.
    Status(StatusArgs),
}

/// Arguments for `review show`.
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Review id. Omit to list recent reviews.
    pub id: Option<String>,
    /// Print the run's event timeline after the review.
    #[arg(long)]
    pub events: bool,
    /// Print `gitcko.review/1`, or the list shape when no id is given.
    #[arg(long, group = "show_mode")]
    pub json: bool,
    /// Plain text.
    #[arg(long, group = "show_mode")]
    pub text: bool,
    /// Plain text, and never prompt.
    #[arg(long, group = "show_mode")]
    pub agent: bool,
}

/// Arguments for `review status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Commit to look up. Defaults to HEAD.
    #[arg(long)]
    pub commit: Option<String>,
    /// Print the status object instead of a sentence.
    #[arg(long)]
    pub json: bool,
}

/// How severe a finding must be to fail a complete review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOn {
    /// Blocker.
    #[value(name = "P0")]
    P0,
    /// Should fix.
    #[value(name = "P1")]
    P1,
    /// Nit.
    #[value(name = "P2")]
    P2,
}

/// `--layout` values. `diff` parses so it can exit 6 rather than 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LayoutArg {
    /// Finding cards, the only layout this build renders.
    Comments,
    /// Inline diff layout. Not in this build.
    Diff,
}

/// `gitcko index`.
#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Rebuild the index instead of reusing a compatible revision.
    #[arg(long)]
    pub full: bool,
}

/// `gitcko cache`.
#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: CacheCmd,
}

/// Cache subcommands. One of them is required.
#[derive(Debug, Subcommand)]
pub enum CacheCmd {
    /// Print how much cache each repository uses.
    Stats,
    /// Delete one repository's cache, or the whole cache.
    Purge(PurgeArgs),
}

/// `cache purge`. `--repo` and `--all` cannot be combined.
#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("scope")
        .required(true)
        .multiple(false)
        .args(["repo", "all"])
))]
pub struct PurgeArgs {
    /// Delete the current repository's cache.
    #[arg(long)]
    pub repo: bool,
    /// Delete every repository's cache.
    #[arg(long)]
    pub all: bool,
    /// Delete without asking. Required when stdin is not a terminal.
    #[arg(short = 'f', long)]
    pub force: bool,
}
