// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Rust adapter: one walk over a tree-sitter-rust syntax tree.
//!
//! A hand-written walk rather than Tree-sitter queries: extraction needs the enclosing
//! module, `impl` type, trait and test context of every node, which queries don't carry,
//! and the walk is where node, depth and count limits are enforced. Node kinds and
//! fields follow the grammar's `node-types.json`; its `tags.scm` (the format GitHub's code
//! navigation uses) was the reference for which nodes are definitions and references.
//!
//! The walk keeps its own stack instead of recursing, so a deeply nested file can't
//! overflow the thread's stack (TM-38).

use std::ops::ControlFlow;
use std::rc::Rc;
use std::time::Instant;

use gitcko_domain::{Coverage, Position, Reason, SourceSpan};
use tree_sitter::{Node, Parser};

use crate::{
    Extension, Extraction, Implementation, Import, Language, LanguageAdapter, ModuleDecl,
    ParseLimits, RefKind, Reference, Scope, Symbol, SymbolKind, Visibility,
};

/// Syntax errors recorded per file; past this the file is partial anyway.
const MAX_SYNTAX_ERRORS: usize = 100;
/// Longest signature kept, in characters.
const MAX_SIGNATURE: usize = 200;
/// Longest name kept, in bytes; longer ones are cut. Real identifiers are far shorter,
/// and every stored name is copied into each result that mentions it.
const MAX_NAME: usize = 512;
/// Segments in a path; longer chains aren't treated as plain paths.
const MAX_PATH_SEGMENTS: usize = 64;
/// Generic parameters remembered per declaration.
const MAX_GENERICS: usize = 64;
/// How often the walk checks its deadline, in nodes.
const DEADLINE_EVERY: usize = 4_096;

/// Reads Rust with the tree-sitter-rust grammar.
pub(crate) struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn version(&self) -> &'static str {
        // Bump the first part when extraction changes, the second with the grammar.
        "rust-1+tree-sitter-rust-0.24"
    }

    fn extract(&self, source: &str, limits: &ParseLimits) -> Extraction {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("the bundled grammar's ABI is one this tree-sitter supports");
        let deadline = Instant::now() + limits.timeout;
        // Tree-sitter calls this as it goes; breaking abandons the parse (tree-sitter
        // 0.25+ replaced the old timeout setting with this callback).
        let mut over_time = |_: &tree_sitter::ParseState| {
            if Instant::now() >= deadline {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut over_time);
        let bytes = source.as_bytes();
        let mut read = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
        let Some(tree) = parser.parse_with_options(&mut read, None, Some(options)) else {
            let unavailable = Coverage::Unavailable {
                reason: reason("parse_timeout"),
            };
            return Extraction {
                coverage: unavailable.clone(),
                reference_coverage: unavailable,
                ..Extraction::default()
            };
        };
        Walker {
            src: source,
            limits,
            deadline,
            walk_timed_out: false,
            out: Extraction::default(),
            nodes: 0,
            node_limit_hit: false,
            depth_limit_hit: false,
            symbols_left_out: 0,
            references_left_out: 0,
            macro_arguments: false,
        }
        .run(tree.root_node())
    }
}

/// What a node's children inherit.
#[derive(Clone, Default)]
struct Context {
    /// Index into `Extraction::scopes`; `None` at file level.
    scope: Option<usize>,
    /// The enclosing symbol's index, for `Symbol::parent`.
    parent: Option<usize>,
    /// The innermost symbol, for `Reference::enclosing`.
    enclosing: Option<usize>,
    in_test: bool,
    /// Inside a trait or `impl` block, where functions are methods.
    in_trait_or_impl: bool,
    /// The trait an `impl` block implements, for its methods' discriminator.
    implementing: Option<String>,
    /// Generic type parameters in scope (`T` in `impl<T>`): names that look like types
    /// but refer to nothing declared anywhere, so they aren't references. Shared, so a
    /// child's context costs the same however many there are.
    generics: Rc<Vec<String>>,
    depth: usize,
}

struct Walker<'a> {
    src: &'a str,
    limits: &'a ParseLimits,
    deadline: Instant,
    walk_timed_out: bool,
    out: Extraction,
    nodes: usize,
    node_limit_hit: bool,
    depth_limit_hit: bool,
    symbols_left_out: u64,
    references_left_out: u64,
    /// A macro call had arguments, which Tree-sitter leaves unparsed.
    macro_arguments: bool,
}

impl<'a> Walker<'a> {
    fn run(mut self, root: Node<'a>) -> Extraction {
        let mut stack = vec![(root, Context::default())];
        while let Some((node, ctx)) = stack.pop() {
            self.nodes += 1;
            if self.nodes > self.limits.max_nodes {
                self.node_limit_hit = true;
                break;
            }
            // The parse and the walk share one time budget.
            if self.nodes.is_multiple_of(DEADLINE_EVERY) && Instant::now() >= self.deadline {
                self.walk_timed_out = true;
                break;
            }
            if (node.is_error() || node.is_missing())
                && self.out.syntax_errors.len() < MAX_SYNTAX_ERRORS
            {
                self.out.syntax_errors.push(span(node));
            }
            if ctx.depth > self.limits.max_depth {
                self.depth_limit_hit = true;
                continue;
            }
            self.visit(node, ctx, &mut stack);
        }
        self.finish()
    }

    fn visit(&mut self, node: Node<'a>, ctx: Context, stack: &mut Vec<(Node<'a>, Context)>) {
        use SymbolKind::*;
        let declaration = |kind, native: &'static str| (kind, native);
        let decl = match node.kind() {
            "function_item" | "function_signature_item" => {
                let native = if node.kind() == "function_item" {
                    "function"
                } else {
                    "function_signature"
                };
                Some(if self.has_attribute(node, is_test_attribute) {
                    declaration(Test, native)
                } else if ctx.in_trait_or_impl {
                    declaration(Method, native)
                } else {
                    declaration(Function, native)
                })
            }
            "struct_item" => Some(declaration(Struct, "struct")),
            "union_item" => Some(declaration(Struct, "union")),
            "enum_item" => Some(declaration(Enum, "enum")),
            "enum_variant" => Some(declaration(Constant, "enum_variant")),
            "field_declaration" => Some(declaration(Field, "field")),
            "trait_item" => Some(declaration(Trait, "trait")),
            "mod_item" => Some(declaration(Module, "module")),
            "type_item" => Some(declaration(TypeAlias, "type_alias")),
            "const_item" => Some(declaration(Constant, "const")),
            "static_item" => Some(declaration(Variable, "static")),
            "macro_definition" => Some(declaration(Macro, "macro_rules")),
            _ => None,
        };
        if let Some((kind, native)) = decl {
            return self.declaration(node, kind, native, ctx, stack);
        }
        match node.kind() {
            "impl_item" => {
                let type_path = node
                    .child_by_field_name("type")
                    .and_then(|t| self.path_of(t))
                    .unwrap_or_default();
                let trait_path = node
                    .child_by_field_name("trait")
                    .and_then(|t| self.path_of(t));
                self.out.implementations.push(Implementation {
                    trait_path: trait_path.clone(),
                    type_path: type_path.clone(),
                    scope: ctx.scope,
                    span: span(node),
                });
                let mut inner = child_context(&ctx);
                self.add_generics(&mut inner, node);
                if let Some(name) = type_path.last() {
                    inner.scope = Some(self.scope(ctx.scope, name));
                }
                inner.in_trait_or_impl = true;
                inner.implementing = trait_path.map(|t| t.join("::"));
                push_children(stack, node, &inner, &[]);
            }
            "use_declaration" => self.use_declaration(node, &ctx),
            "extern_crate_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.out.imports.push(Import {
                        path: vec![self.name(name)],
                        alias: node.child_by_field_name("alias").map(|a| self.name(a)),
                        glob: false,
                        public: has_visibility(node),
                        scope: ctx.scope,
                        span: span(node),
                    });
                }
            }
            "call_expression" => {
                let function = node.child_by_field_name("function");
                let mut target = function;
                if let Some(f) = function.filter(|f| f.kind() == "generic_function") {
                    if let Some(args) = f.child_by_field_name("type_arguments") {
                        stack.push((args, child_context(&ctx)));
                    }
                    target = f.child_by_field_name("function");
                }
                match target {
                    Some(f) if f.kind() == "field_expression" => {
                        if let Some(field) = f.child_by_field_name("field") {
                            let name = vec![self.name(field)];
                            self.reference(RefKind::MethodCall, name, field, &ctx);
                        }
                        if let Some(value) = f.child_by_field_name("value") {
                            stack.push((value, child_context(&ctx)));
                        }
                    }
                    Some(f) => match self.path_of(f) {
                        Some(path) => self.reference(RefKind::Call, path, f, &ctx),
                        // A call through an expression (`(table[i])()`): its parts may
                        // still name things.
                        None => stack.push((f, child_context(&ctx))),
                    },
                    None => {}
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    stack.push((args, child_context(&ctx)));
                }
            }
            "macro_invocation" => {
                if let Some(path) = node
                    .child_by_field_name("macro")
                    .and_then(|m| self.path_of(m))
                {
                    self.reference(RefKind::MacroCall, path, node, &ctx);
                }
                self.macro_arguments(node, &ctx);
            }
            "scoped_identifier" => {
                if let Some(path) = self.path_of(node) {
                    self.reference(RefKind::Path, path, node, &ctx);
                }
            }
            "type_identifier" => {
                let name = self.name(node);
                if name != "Self" && !ctx.generics.contains(&name) {
                    self.reference(RefKind::Type, vec![name], node, &ctx);
                }
            }
            "scoped_type_identifier" => {
                if let Some(path) = self.path_of(node) {
                    self.reference(RefKind::Type, path, node, &ctx);
                }
            }
            // Attributes and comments name nothing the graph needs.
            "attribute_item" | "inner_attribute_item" | "line_comment" | "block_comment" => {}
            _ => push_children(stack, node, &child_context(&ctx), &[]),
        }
    }

    /// Records a declaration and walks its children (all but its name) in its scope.
    fn declaration(
        &mut self,
        node: Node<'a>,
        kind: SymbolKind,
        native: &'static str,
        ctx: Context,
        stack: &mut Vec<(Node<'a>, Context)>,
    ) {
        let mut inner = child_context(&ctx);
        self.add_generics(&mut inner, node);
        let Some(name_node) = node.child_by_field_name("name") else {
            // Error recovery can leave a declaration without a name: keep walking it.
            push_children(stack, node, &inner, &[]);
            return;
        };
        let name = self.name(name_node);
        let test_module = kind == SymbolKind::Module && self.has_attribute(node, is_cfg_test);
        let index = if self.out.symbols.len() < self.limits.max_symbols {
            let body = node.child_by_field_name("body");
            let header_end = body.map_or(node.end_byte(), |b| b.start_byte());
            // Clamped before copying: a body-less item (`const X = "…1 MB…"`) would
            // otherwise be copied whole just to keep its first 200 characters.
            let header_end = header_end.min(node.start_byte() + MAX_SIGNATURE * 4);
            // A macro has no body field; its signature is just its name.
            let header = if kind == SymbolKind::Macro {
                format!("macro_rules! {name}")
            } else {
                prefix(self.src, node.start_byte(), header_end).to_string()
            };
            self.out.symbols.push(Symbol {
                kind,
                native_kind: native.to_string(),
                name: name.clone(),
                scope: ctx.scope,
                discriminator: ctx.implementing.clone(),
                parent: ctx.parent,
                span: span(node),
                name_span: span(name_node),
                signature: signature(&header),
                visibility: visibility(self.src, node),
                in_test: ctx.in_test || kind == SymbolKind::Test || test_module,
                near_syntax_error: false,
            });
            Some(self.out.symbols.len() - 1)
        } else {
            self.symbols_left_out += 1;
            None
        };
        if let Some(i) = index {
            inner.parent = Some(i);
            inner.enclosing = Some(i);
        }
        inner.scope = Some(self.scope(ctx.scope, &name));
        inner.in_test |= kind == SymbolKind::Test || test_module;
        inner.implementing = None;
        inner.in_trait_or_impl = kind == SymbolKind::Trait;
        match kind {
            SymbolKind::Module if node.child_by_field_name("body").is_none() => {
                if let Some(i) = index {
                    self.out.modules.push(ModuleDecl {
                        name,
                        path_attr: self.path_attribute(node),
                        scope: ctx.scope,
                        symbol: i,
                    });
                }
            }
            SymbolKind::Trait => {
                if let (Some(i), Some(bounds)) = (index, node.child_by_field_name("bounds")) {
                    let mut cursor = bounds.walk();
                    for bound in bounds.named_children(&mut cursor) {
                        if let Some(target) = self.path_of(bound) {
                            self.out.extensions.push(Extension {
                                symbol: i,
                                target,
                                span: span(bound),
                            });
                        }
                    }
                }
            }
            // A macro's rules are patterns over tokens, not code to index.
            SymbolKind::Macro => return,
            _ => {}
        }
        push_children(stack, node, &inner, &["name"]);
    }

    /// Flattens a `use` tree into one import per name, without recursion.
    fn use_declaration(&mut self, node: Node<'a>, ctx: &Context) {
        let public = has_visibility(node);
        let Some(argument) = node.child_by_field_name("argument") else {
            return;
        };
        let mut found: Vec<Import> = Vec::new();
        let mut pending: Vec<(Node<'a>, Vec<String>)> = vec![(argument, Vec::new())];
        while let Some((item, prefix)) = pending.pop() {
            let mut import = |path: Vec<String>, alias: Option<String>, glob: bool| {
                found.push(Import {
                    path,
                    alias,
                    glob,
                    public,
                    scope: ctx.scope,
                    span: span(item),
                });
            };
            match item.kind() {
                // `use a::{self, b}`: `self` names the list's own path.
                "self" if !prefix.is_empty() => import(prefix, None, false),
                "use_as_clause" => {
                    let path = item
                        .child_by_field_name("path")
                        .and_then(|p| self.path_of(p));
                    let alias = item.child_by_field_name("alias").map(|a| self.name(a));
                    if let Some(path) = path {
                        import([prefix, path].concat(), alias, false);
                    }
                }
                "use_wildcard" => {
                    let mut cursor = item.walk();
                    let path = item
                        .named_children(&mut cursor)
                        .next()
                        .and_then(|p| self.path_of(p))
                        .unwrap_or_default();
                    import([prefix, path].concat(), None, true);
                }
                "scoped_use_list" => {
                    let path = item
                        .child_by_field_name("path")
                        .and_then(|p| self.path_of(p))
                        .unwrap_or_default();
                    if let Some(list) = item.child_by_field_name("list") {
                        pending.push((list, [prefix, path].concat()));
                    }
                }
                "use_list" => {
                    let mut cursor = item.walk();
                    let children: Vec<_> = item.named_children(&mut cursor).collect();
                    for child in children.into_iter().rev() {
                        pending.push((child, prefix.clone()));
                    }
                }
                _ => {
                    if let Some(path) = self.path_of(item) {
                        import([prefix, path].concat(), None, false);
                    }
                }
            }
        }
        self.out.imports.extend(found);
    }

    /// Macro arguments are an unparsed token tree. A name followed by `(` there is
    /// recorded as a likely call, marked as a guess, and the file's reference coverage
    /// becomes partial.
    fn macro_arguments(&mut self, node: Node<'a>, ctx: &Context) {
        let mut tokens: Vec<Node<'a>> = Vec::new();
        let mut cursor = node.walk();
        let mut pending: Vec<Node<'a>> = node
            .children(&mut cursor)
            .filter(|c| c.kind() == "token_tree")
            .collect();
        pending.reverse();
        while let Some(n) = pending.pop() {
            self.nodes += 1;
            if self.nodes > self.limits.max_nodes {
                self.node_limit_hit = true;
                return;
            }
            if n.child_count() == 0 {
                tokens.push(n);
            } else {
                let mut cursor = n.walk();
                let children: Vec<_> = n.children(&mut cursor).collect();
                pending.extend(children.into_iter().rev());
            }
        }
        // Delimiters alone (`m!()`) hide nothing.
        if tokens.len() > 2 {
            self.macro_arguments = true;
        }
        for i in 1..tokens.len() {
            if tokens[i].kind() != "(" || tokens[i - 1].kind() != "identifier" {
                continue;
            }
            let name = tokens[i - 1];
            let after_dot = i >= 2 && tokens[i - 2].kind() == ".";
            let kind = if after_dot {
                RefKind::MethodCall
            } else {
                RefKind::MacroArgCall
            };
            self.reference(kind, vec![self.name(name)], name, ctx);
        }
    }

    fn reference(&mut self, kind: RefKind, path: Vec<String>, node: Node<'a>, ctx: &Context) {
        if path.is_empty() {
            return;
        }
        if self.out.references.len() >= self.limits.max_references {
            self.references_left_out += 1;
            return;
        }
        self.out.references.push(Reference {
            kind,
            path,
            span: span(node),
            enclosing: ctx.enclosing,
            scope: ctx.scope,
        });
    }

    /// The segments of a path-like node (`a::b::C`, `C<T>`, `f::<T>`), walking down the
    /// `path` field without recursion; `None` for anything that isn't a plain path.
    fn path_of(&self, node: Node<'a>) -> Option<Vec<String>> {
        let mut segments = Vec::new();
        let mut current = node;
        loop {
            if segments.len() >= MAX_PATH_SEGMENTS {
                return None;
            }
            match current.kind() {
                "identifier" | "type_identifier" | "field_identifier" | "crate" | "self"
                | "super" | "metavariable" => {
                    segments.push(self.name(current));
                    break;
                }
                "scoped_identifier" | "scoped_type_identifier" => {
                    segments.push(self.name(current.child_by_field_name("name")?));
                    match current.child_by_field_name("path") {
                        Some(path) => current = path,
                        // `::name`: an absolute path.
                        None => break,
                    }
                }
                "generic_type" => current = current.child_by_field_name("type")?,
                "generic_function" => current = current.child_by_field_name("function")?,
                _ => return None,
            }
        }
        segments.reverse();
        Some(segments)
    }

    /// Whether one of the attributes right before `node` satisfies `test`. Attributes are
    /// the item's preceding siblings in tree-sitter-rust, with doc comments between them.
    fn has_attribute(&self, node: Node<'a>, test: fn(&str) -> bool) -> bool {
        self.attributes(node).iter().any(|a| test(a))
    }

    /// The attribute texts before `node`, whitespace removed (`cfg(test)`, `tokio::test`).
    fn attributes(&self, node: Node<'a>) -> Vec<String> {
        let mut found = Vec::new();
        let mut sibling = node.prev_named_sibling();
        while let Some(s) = sibling {
            match s.kind() {
                "attribute_item" => {
                    let mut cursor = s.walk();
                    if let Some(attribute) = s.named_children(&mut cursor).next() {
                        let text: String = self.text(attribute).split_whitespace().collect();
                        found.push(text);
                    }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            sibling = s.prev_named_sibling();
        }
        found
    }

    /// The value of a `#[path = "…"]` attribute, which puts a module in another file.
    fn path_attribute(&self, node: Node<'a>) -> Option<String> {
        self.attributes(node).iter().find_map(|a| {
            let value = a.strip_prefix("path=")?;
            Some(value.trim_matches('"').to_string())
        })
    }

    /// Adds a declaration's `<…>` names to the context's generics (at most
    /// [`MAX_GENERICS`]), copying the shared list only when there's something to add.
    fn add_generics(&self, ctx: &mut Context, node: Node<'a>) {
        let Some(params) = node.child_by_field_name("type_parameters") else {
            return;
        };
        let mut cursor = params.walk();
        let names: Vec<String> = params
            .named_children(&mut cursor)
            .filter(|p| p.kind() == "type_parameter")
            .filter_map(|p| p.child_by_field_name("name"))
            .take(MAX_GENERICS)
            .map(|n| self.name(n))
            .collect();
        if !names.is_empty() {
            let mut all = (*ctx.generics).clone();
            all.extend(names);
            ctx.generics = Rc::new(all);
        }
    }

    /// A new scope named `name` inside `parent`.
    fn scope(&mut self, parent: Option<usize>, name: &str) -> usize {
        self.out.scopes.push(Scope {
            parent,
            name: name.to_string(),
        });
        self.out.scopes.len() - 1
    }

    /// A node's text as a stored name, cut at [`MAX_NAME`] bytes.
    fn name(&self, node: Node<'a>) -> String {
        prefix(
            self.src,
            node.start_byte(),
            node.end_byte().min(node.start_byte() + MAX_NAME),
        )
        .to_string()
    }

    fn text(&self, node: Node<'a>) -> &'a str {
        self.src.get(node.byte_range()).unwrap_or("")
    }

    fn finish(mut self) -> Extraction {
        for symbol in &mut self.out.symbols {
            symbol.near_syntax_error = self.out.syntax_errors.iter().any(|e| {
                e.start_byte() < symbol.span.end_byte() && symbol.span.start_byte() < e.end_byte()
            });
        }
        let errors = !self.out.syntax_errors.is_empty();
        let partial = |code: &str, omitted: Option<u64>| Coverage::Partial {
            reason: reason(code),
            omitted,
        };
        let walk_cut = if self.walk_timed_out {
            Some(partial("walk_timeout", None))
        } else if self.node_limit_hit {
            Some(partial("node_limit", None))
        } else if self.depth_limit_hit {
            Some(partial("depth_limit", None))
        } else {
            None
        };
        self.out.coverage = walk_cut.clone().unwrap_or_else(|| {
            if self.symbols_left_out > 0 {
                partial("symbol_limit", Some(self.symbols_left_out))
            } else if errors {
                partial("syntax_errors", None)
            } else {
                Coverage::Complete {}
            }
        });
        self.out.reference_coverage = walk_cut.unwrap_or_else(|| {
            if self.references_left_out > 0 {
                partial("reference_limit", Some(self.references_left_out))
            } else if self.macro_arguments {
                partial("macro_arguments", None)
            } else if errors {
                partial("syntax_errors", None)
            } else {
                Coverage::Complete {}
            }
        });
        self.out
    }
}

/// A child's context: the parent's, one level deeper.
fn child_context(ctx: &Context) -> Context {
    Context {
        depth: ctx.depth + 1,
        ..ctx.clone()
    }
}

/// Pushes `node`'s named children (except the given fields) so they pop in source order.
fn push_children<'a>(
    stack: &mut Vec<(Node<'a>, Context)>,
    node: Node<'a>,
    ctx: &Context,
    except: &[&str],
) {
    let mut cursor = node.walk();
    let mut children = Vec::new();
    if cursor.goto_first_child() {
        loop {
            let skip = cursor.field_name().is_some_and(|f| except.contains(&f));
            if cursor.node().is_named() && !skip {
                children.push(cursor.node());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    let mut child = child_context(ctx);
    child.depth = ctx.depth + 1;
    stack.extend(children.into_iter().rev().map(|n| (n, child.clone())));
}

fn span(node: Node<'_>) -> SourceSpan {
    let (start, end) = (node.start_position(), node.end_position());
    SourceSpan::new(
        node.start_byte(),
        node.end_byte(),
        Position {
            row: start.row,
            column: start.column,
        },
        Position {
            row: end.row,
            column: end.column,
        },
    )
    .expect("tree-sitter never gives a node that ends before it starts")
}

/// `src[start..end]`, with `end` moved back to a character boundary.
fn prefix(src: &str, start: usize, end: usize) -> &str {
    let mut end = end.min(src.len());
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    src.get(start..end).unwrap_or("")
}

fn reason(code: &str) -> Reason {
    Reason::new(code).expect("reason codes in this file are valid")
}

fn has_visibility(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|c| c.kind() == "visibility_modifier")
}

fn visibility(src: &str, node: Node<'_>) -> Visibility {
    let mut cursor = node.walk();
    let modifier = node
        .children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier");
    let Some(modifier) = modifier else {
        return Visibility::Private;
    };
    let text: String = src
        .get(modifier.byte_range())
        .unwrap_or("")
        .split_whitespace()
        .collect();
    match text.as_str() {
        "pub" => Visibility::Public,
        "pub(crate)" => Visibility::Crate,
        _ => Visibility::Restricted,
    }
}

/// `#[test]`, or a framework's own (`#[tokio::test]`, `#[rstest::test(…)]`).
fn is_test_attribute(attribute: &str) -> bool {
    let path = attribute.split('(').next().unwrap_or("");
    path == "test" || path.ends_with("::test")
}

fn is_cfg_test(attribute: &str) -> bool {
    attribute == "cfg(test)"
}

/// A declaration's header with whitespace collapsed, cut at [`MAX_SIGNATURE`] characters.
fn signature(header: &str) -> Option<String> {
    let collapsed: String = header.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed.trim_end_matches([' ', '{', ';']).to_string();
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_SIGNATURE).collect())
}
