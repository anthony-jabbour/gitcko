// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::Duration;

use gitcko_domain::{Coverage, SourceSpan};

use crate::Language;

/// One language's extractor (INDEXING §10). It reports what a single file says: its
/// declarations, imports and the names it refers to. Deciding which declaration a name
/// means across files is the graph's job (`gitcko-graph`), because it needs every file.
///
/// A trait, although Rust is the only implementation for now, because the other MVP
/// languages plug in here (Phase 6) and the index must not care which one it calls.
pub trait LanguageAdapter: Sync {
    /// The language this adapter reads.
    fn language(&self) -> Language;

    /// Changes whenever the adapter or its grammar would extract something different, so
    /// a cache built by an older adapter is rebuilt rather than trusted (INDEXING §6).
    fn version(&self) -> &'static str;

    /// Reads `source`. Never fails: a file that can't be parsed in full yields what could
    /// be read, with coverage saying what's missing (§10.1–10.2).
    fn extract(&self, source: &str, limits: &ParseLimits) -> Extraction;
}

/// The adapter for `language`, if Gitcko has one.
pub fn adapter(language: Language) -> Option<&'static dyn LanguageAdapter> {
    match language {
        #[cfg(feature = "lang-rust")]
        Language::Rust => Some(&crate::rust::RustAdapter),
        _ => None,
    }
}

/// Bounds on one file's parse and extraction (INDEXING §10.1, §35.2). Hitting one marks
/// the file's coverage `partial` (or `unavailable` for a timeout), never a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLimits {
    /// Wall-clock time Tree-sitter may spend parsing; past it the parse is abandoned.
    pub timeout: Duration,
    /// Syntax-tree nodes visited during extraction.
    pub max_nodes: usize,
    /// Nesting depth followed; deeper subtrees are skipped.
    pub max_depth: usize,
    pub max_symbols: usize,
    pub max_references: usize,
}

impl Default for ParseLimits {
    fn default() -> ParseLimits {
        ParseLimits {
            timeout: Duration::from_secs(2),
            max_nodes: 2_000_000,
            max_depth: 1_024,
            max_symbols: 20_000,
            max_references: 100_000,
        }
    }
}

/// The kind of a declaration, in the cross-language vocabulary of INDEXING §11.2 (only
/// the entries some adapter produces). An adapter's own term for it is kept in
/// [`Symbol::native_kind`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Module,
    Trait,
    Struct,
    Enum,
    TypeAlias,
    Function,
    Method,
    Field,
    Constant,
    Variable,
    Macro,
    Test,
}

/// Who can see a declaration, as far as the syntax says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// `pub`.
    Public,
    /// `pub(crate)`.
    Crate,
    /// `pub(super)`, `pub(in path)`.
    Restricted,
    /// No modifier.
    Private,
}

/// One declaration in a file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    pub kind: SymbolKind,
    /// The adapter's own term, such as `union` or `enum_variant`, for kinds the common
    /// vocabulary merges.
    pub native_kind: String,
    pub name: String,
    /// Where it sits within this file (see [`Extraction::scope_path`]): inline modules,
    /// the type of an `impl` block, a trait, an enclosing function. The graph prefixes the
    /// file's own module path.
    pub scope: Option<usize>,
    /// Tells apart declarations that share a qualified name, such as the `fmt` methods of
    /// `impl Display for X` and `impl Debug for X`: the implemented trait's path.
    pub discriminator: Option<String>,
    /// Index of the enclosing symbol in [`Extraction::symbols`], if it's in this file.
    pub parent: Option<usize>,
    /// The whole declaration.
    pub span: SourceSpan,
    /// Just the name.
    pub name_span: SourceSpan,
    /// The declaration's header (up to its body), whitespace collapsed, at most 200
    /// characters.
    pub signature: Option<String>,
    pub visibility: Visibility,
    /// A test function, or anything inside a test-only module.
    pub in_test: bool,
    /// Overlaps a syntax error, so its extent or name may be wrong (§10.2).
    pub near_syntax_error: bool,
}

/// One imported name, with `use` trees flattened: `use a::{b, c as d}` gives two.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Import {
    /// Path segments as written, `crate`, `self` and `super` included.
    pub path: Vec<String>,
    /// The local name, when renamed (`as d`).
    pub alias: Option<String>,
    /// `use a::*`: brings in everything public from `path`.
    pub glob: bool,
    /// `pub use`: re-exported for other modules too.
    pub public: bool,
    pub scope: Option<usize>,
    pub span: SourceSpan,
}

/// How a name is used where it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// `f()` or `a::f()`.
    Call,
    /// `x.f()`: which `f` depends on the type of `x`, which syntax doesn't give.
    MethodCall,
    /// `m!(…)`.
    MacroCall,
    /// `f(` seen inside a macro's arguments, which Tree-sitter doesn't parse; a guess.
    MacroArgCall,
    /// A type name.
    Type,
    /// A path in an expression that isn't a call, such as `Enum::Variant`.
    Path,
}

/// One use of a name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reference {
    pub kind: RefKind,
    pub path: Vec<String>,
    pub span: SourceSpan,
    /// The innermost symbol it appears in, if any.
    pub enclosing: Option<usize>,
    pub scope: Option<usize>,
}

/// A module declared here whose body lives in another file (`mod foo;`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModuleDecl {
    pub name: String,
    /// A `#[path = "…"]` attribute, which moves the file.
    pub path_attr: Option<String>,
    pub scope: Option<usize>,
    /// The module's entry in [`Extraction::symbols`].
    pub symbol: usize,
}

/// A type implementing a trait (`impl Trait for Type`), or an inherent `impl Type`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Implementation {
    pub trait_path: Option<Vec<String>>,
    pub type_path: Vec<String>,
    pub scope: Option<usize>,
    pub span: SourceSpan,
}

/// A declaration extending another type, such as a supertrait (`trait A: B`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Extension {
    /// The extending symbol in [`Extraction::symbols`].
    pub symbol: usize,
    pub target: Vec<String>,
    pub span: SourceSpan,
}

/// One named level of nesting in a file: an inline module, an `impl` type, a trait or
/// an enclosing declaration. Scopes form a tree through `parent`, so each stores only its
/// own name however deep it sits; copying whole paths into every reference would let a
/// deeply nested or long-named file multiply its own size many times over (TM-38).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    pub parent: Option<usize>,
    pub name: String,
}

/// Everything one file says, in source order, plus how complete it is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Extraction {
    /// The nesting levels the other lists point into; `None` there means file level.
    pub scopes: Vec<Scope>,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub references: Vec<Reference>,
    pub modules: Vec<ModuleDecl>,
    pub implementations: Vec<Implementation>,
    pub extensions: Vec<Extension>,
    /// Where the parser found errors (at most 100 recorded).
    pub syntax_errors: Vec<SourceSpan>,
    /// Of the declarations: `partial` after syntax errors or a limit, `unavailable` when
    /// the parse itself didn't finish.
    pub coverage: Coverage,
    /// Of the references and calls, which can also be partial on their own, e.g. calls
    /// hidden in macro arguments.
    pub reference_coverage: Coverage,
}

/// Empty and complete: what a file with nothing in it extracts to.
impl Default for Extraction {
    fn default() -> Extraction {
        Extraction {
            scopes: Vec::new(),
            symbols: Vec::new(),
            imports: Vec::new(),
            references: Vec::new(),
            modules: Vec::new(),
            implementations: Vec::new(),
            extensions: Vec::new(),
            syntax_errors: Vec::new(),
            coverage: Coverage::Complete {},
            reference_coverage: Coverage::Complete {},
        }
    }
}

impl Extraction {
    /// The names from file level down to `scope`, outermost first.
    pub fn scope_path(&self, scope: Option<usize>) -> Vec<String> {
        let mut path = Vec::new();
        let mut current = scope;
        // Parents always come before their children, so this ends.
        while let Some(i) = current {
            path.push(self.scopes[i].name.clone());
            current = self.scopes[i].parent;
        }
        path.reverse();
        path
    }
}
