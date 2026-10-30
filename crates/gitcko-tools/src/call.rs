// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tool calls as the model sends them: a name and a JSON object of arguments. Both are
//! model output, so they're parsed into closed, bounded types before anything runs
//! (REVIEW_ENGINE §10: tool strings are arguments, never commands).

use schemars::JsonSchema;
use serde::Deserialize;

/// Longest argument object accepted, in bytes. Real calls are a few hundred.
const MAX_ARGS: usize = 16 * 1024;
/// Longest search query or symbol name accepted, in characters.
pub(crate) const MAX_QUERY: usize = 200;

/// Which side of the change a path or line is on: the base (`old`) or the head (`new`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SideArg {
    Old,
    New,
}

/// `read_file`: lines of a file at the base or head commit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFile {
    /// Repository-relative path, `/`-separated.
    pub path: String,
    /// Defaults to `new` (the head commit).
    pub side: Option<SideArg>,
    /// First line, from 1. Defaults to 1.
    pub start_line: Option<u32>,
    /// Last line, inclusive. Defaults to as many lines as one read allows.
    pub end_line: Option<u32>,
}

/// `read_diff`: the hunks of one changed file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadDiff {
    /// The file's old or new path.
    pub path: String,
}

/// How `search_code` matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Exact text, read from the head commit's files.
    Literal,
    /// Ranked search over code tokens.
    Tokens,
    /// A whole identifier.
    Identifier,
}

/// `search_code`: where text appears in the head commit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchCode {
    pub query: String,
    /// Defaults to `literal`.
    pub mode: Option<SearchMode>,
    /// Only paths starting with this, e.g. `src/db/`.
    pub path_prefix: Option<String>,
    /// Page of results, from 0.
    pub page: Option<u32>,
}

/// `find_symbol`: declarations named this (plain or `a::b::c`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindSymbol {
    pub name: String,
    /// Only declarations in this file.
    pub path: Option<String>,
}

/// A symbol from an earlier `find_symbol` result, by its id (`s12`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolArg {
    pub symbol: String,
}

/// `find_importers`: files that import this file's module.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathArg {
    pub path: String,
}

/// `git_log`: recent commits, optionally only those that changed one file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitLog {
    pub path: Option<String>,
}

/// `retrieve_chunks`: ranked code chunks for a query.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrieveChunks {
    pub query: String,
}

/// `retrieve_repo_summary` takes no arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// One parsed call. The set is closed: anything else is refused before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    ReadFile(ReadFile),
    ReadDiff(ReadDiff),
    SearchCode(SearchCode),
    FindSymbol(FindSymbol),
    FindReferences(SymbolArg),
    FindCallers(SymbolArg),
    FindCallees(SymbolArg),
    FindImporters(PathArg),
    FindTests(SymbolArg),
    GitLog(GitLog),
    RetrieveChunks(RetrieveChunks),
    RetrieveRepoSummary(NoArgs),
}

/// Why a call was refused before running. Value-free: the name and arguments are
/// model output and never appear in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("unknown tool")]
    UnknownTool,
    #[error("tool arguments too large")]
    TooLarge,
    #[error("tool arguments don't match the tool's schema")]
    BadArguments,
}

/// The tool names offered in a Base review, in the order they're described to the model.
/// `git_blame` is in the REVIEW_ENGINE §10 allowlist but not offered yet: gix's blame has
/// no step or time bound, so one call could outlast the run's deadline.
pub const NAMES: [&str; 12] = [
    "read_file",
    "read_diff",
    "search_code",
    "find_symbol",
    "find_references",
    "find_callers",
    "find_callees",
    "find_importers",
    "find_tests",
    "git_log",
    "retrieve_chunks",
    "retrieve_repo_summary",
];

impl ToolCall {
    /// Parses `name` and its JSON `args`. An empty `args` counts as `{}`, which some
    /// providers send for a tool without parameters.
    pub fn parse(name: &str, args: &str) -> Result<ToolCall, CallError> {
        if args.len() > MAX_ARGS {
            return Err(CallError::TooLarge);
        }
        let args = if args.trim().is_empty() { "{}" } else { args };
        fn de<T: serde::de::DeserializeOwned>(args: &str) -> Result<T, CallError> {
            serde_json::from_str(args).map_err(|_| CallError::BadArguments)
        }
        Ok(match name {
            "read_file" => ToolCall::ReadFile(de(args)?),
            "read_diff" => ToolCall::ReadDiff(de(args)?),
            "search_code" => ToolCall::SearchCode(de(args)?),
            "find_symbol" => ToolCall::FindSymbol(de(args)?),
            "find_references" => ToolCall::FindReferences(de(args)?),
            "find_callers" => ToolCall::FindCallers(de(args)?),
            "find_callees" => ToolCall::FindCallees(de(args)?),
            "find_importers" => ToolCall::FindImporters(de(args)?),
            "find_tests" => ToolCall::FindTests(de(args)?),
            "git_log" => ToolCall::GitLog(de(args)?),
            "retrieve_chunks" => ToolCall::RetrieveChunks(de(args)?),
            "retrieve_repo_summary" => ToolCall::RetrieveRepoSummary(de(args)?),
            _ => return Err(CallError::UnknownTool),
        })
    }
}

/// The tool definitions sent with each investigator request: our own descriptions and a
/// JSON schema generated from the same types the arguments are parsed into.
pub fn definitions() -> Vec<gitcko_model::Tool> {
    fn tool<T: JsonSchema>(name: &str, description: &str) -> gitcko_model::Tool {
        gitcko_model::Tool {
            name: name.into(),
            description: description.into(),
            parameters: serde_json::to_value(schemars::schema_for!(T))
                .expect("a generated schema is JSON"),
        }
    }
    vec![
        tool::<ReadFile>(
            "read_file",
            "Read numbered lines of a file at the base (old) or head (new) commit.",
        ),
        tool::<ReadDiff>("read_diff", "Show the changed hunks of one file."),
        tool::<SearchCode>(
            "search_code",
            "Find where text or an identifier appears in the head commit.",
        ),
        tool::<FindSymbol>(
            "find_symbol",
            "Find declarations by name. Returns symbol ids for the other symbol tools.",
        ),
        tool::<SymbolArg>("find_references", "Places that refer to a symbol."),
        tool::<SymbolArg>("find_callers", "Functions that call a symbol."),
        tool::<SymbolArg>("find_callees", "Functions a symbol calls."),
        tool::<PathArg>("find_importers", "Files that import a file's module."),
        tool::<SymbolArg>(
            "find_tests",
            "Tests related to a symbol. Related is not proof that they cover it.",
        ),
        tool::<GitLog>(
            "git_log",
            "Recent commits, optionally only those that changed a file.",
        ),
        tool::<RetrieveChunks>("retrieve_chunks", "Ranked code chunks relevant to a query."),
        tool::<NoArgs>(
            "retrieve_repo_summary",
            "Counts of files, languages, crates and symbols.",
        ),
    ]
}
