// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rust name resolution from syntax alone: modules, `use` bindings (renames, globs,
//! `pub use` chains), enum variants and `impl` blocks. Close to what rustc does for
//! paths, without types: a method call's receiver type is unknown, so method calls are
//! candidates, never exact. Visibility isn't checked, so a private item can be a
//! candidate: for review context a lead is worth more than a strict answer.

use std::collections::{BTreeSet, HashMap, HashSet};

use gitcko_syntax::{Extraction, SymbolKind};

use crate::{Endpoint, FileNode, GraphSymbol};

/// Longest chain of `use` bindings followed; longer is treated as a cycle.
const MAX_USE_DEPTH: usize = 16;
/// Most module lookups one resolution may do. Past it the name ends unresolved (and
/// counts against coverage) rather than letting crafted glob imports take unbounded
/// time (TM-38).
const MAX_LOOKUPS: usize = 20_000;

/// A module's identity: crate unit (if any root reaches it) and path.
pub(crate) type ModuleId = (Option<usize>, Vec<String>);

/// Where a path can lead.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Module(Option<usize>, Vec<String>),
    Symbol(usize),
    External,
}

/// What a path resolved to.
pub(crate) enum Resolved {
    /// Declarations or modules in the repository.
    Found(Vec<Endpoint>),
    /// Outside the repository: another crate, or the standard library.
    External,
    /// A repository path that leads nowhere.
    Missing,
}

/// One `use` binding in a module.
struct Use {
    /// The name it binds: the alias, or the path's last segment; `None` for a glob.
    name: Option<String>,
    path: Vec<String>,
}

/// Names the standard prelude and std crates put in scope everywhere.
const EXTERNAL_ROOTS: &[&str] = &[
    "std",
    "core",
    "alloc",
    "proc_macro",
    "test",
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "Vec",
    "String",
    "Box",
    "ToString",
    "ToOwned",
    "Clone",
    "Copy",
    "Default",
    "Drop",
    "Iterator",
    "IntoIterator",
    "Extend",
    "Debug",
    "Eq",
    "PartialEq",
    "Ord",
    "PartialOrd",
    "Send",
    "Sync",
    "Sized",
    "From",
    "Into",
    "TryFrom",
    "TryInto",
    "AsRef",
    "AsMut",
    "Fn",
    "FnMut",
    "FnOnce",
    "Hash",
    "Display",
    "println",
    "print",
    "eprintln",
    "format",
    "vec",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "panic",
    "write",
    "writeln",
    "todo",
    "unimplemented",
    "unreachable",
    "matches",
    "dbg",
    "include_str",
    "concat",
    "env",
];

/// One lookup's identity: module and name.
type Key = (Option<usize>, Vec<String>, String);

/// Per-resolution memory. Finished lookups are cached, so a name reached by two routes
/// is resolved once (dense glob imports stay linear); lookups still running are tracked
/// apart, so only a real cycle (a lookup needing itself) is cut.
#[derive(Default)]
struct Memo {
    /// Finished lookups, with the depth they ran at when the depth limit cut them short:
    /// a cut result is reused only by a route with no more depth left, which couldn't
    /// reach further either. Complete results (`None`) are always reused.
    done: HashMap<Key, (Vec<Place>, Option<usize>)>,
    running: HashSet<Key>,
    /// Set when the depth limit cut the current lookup short.
    cut: bool,
    lookups: usize,
}

/// Everything name lookups need, built once per graph.
pub(crate) struct Resolver<'a> {
    files: &'a [FileNode],
    symbols: &'a [GraphSymbol],
    /// Items declared directly in each module (modules themselves are in `children`).
    items: HashMap<ModuleId, HashMap<String, Vec<usize>>>,
    /// Submodules of each module.
    children: HashMap<ModuleId, BTreeSet<String>>,
    uses: HashMap<ModuleId, Vec<Use>>,
    /// Items declared inside a non-module scope (nested functions), by file and scope.
    local: HashMap<(usize, Vec<String>), HashMap<String, Vec<usize>>>,
    /// `use` written inside a function, visible only there, by file and scope.
    local_uses: HashMap<(usize, Vec<String>), Vec<Use>>,
    /// Methods, fields and variants of a type or trait, by its symbol.
    pub(crate) members: HashMap<usize, HashMap<String, Vec<usize>>>,
    /// Every method by name, for calls whose receiver type is unknown.
    pub(crate) methods: HashMap<String, Vec<usize>>,
    /// Library crates by the name code uses for them.
    libraries: HashMap<String, usize>,
    /// Per file, the scope paths that are inline modules.
    module_scopes: Vec<BTreeSet<Vec<String>>>,
    /// Symbols by file and full scope path, for attaching `impl` members.
    by_scope: HashMap<(usize, Vec<String>), Vec<usize>>,
}

impl<'a> Resolver<'a> {
    pub(crate) fn new(
        files: &'a [FileNode],
        symbols: &'a [GraphSymbol],
        extractions: &[&Extraction],
        libraries: HashMap<String, usize>,
    ) -> Resolver<'a> {
        let mut module_scopes: Vec<BTreeSet<Vec<String>>> = vec![BTreeSet::new(); files.len()];
        for s in symbols {
            if s.kind == SymbolKind::Module {
                let x = extractions[s.file];
                let mut path = x.scope_path(x.symbols[s.local].scope);
                path.push(s.name.clone());
                module_scopes[s.file].insert(path);
            }
        }
        let mut r = Resolver {
            files,
            symbols,
            items: HashMap::new(),
            children: HashMap::new(),
            uses: HashMap::new(),
            local: HashMap::new(),
            local_uses: HashMap::new(),
            members: HashMap::new(),
            methods: HashMap::new(),
            libraries,
            module_scopes,
            by_scope: HashMap::new(),
        };
        let index: HashMap<(usize, usize), usize> = symbols
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.file, s.local), i))
            .collect();
        for (i, s) in symbols.iter().enumerate() {
            let x = extractions[s.file];
            let local = &x.symbols[s.local];
            let scope = x.scope_path(local.scope);
            r.by_scope
                .entry((s.file, scope.clone()))
                .or_default()
                .push(i);
            if s.kind == SymbolKind::Method {
                r.methods.entry(s.name.clone()).or_default().push(i);
            }
            if let Some(parent) = local.parent {
                let parent_kind = x.symbols[parent].kind;
                if matches!(
                    parent_kind,
                    SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Trait
                ) {
                    if let Some(&p) = index.get(&(s.file, parent)) {
                        r.members
                            .entry(p)
                            .or_default()
                            .entry(s.name.clone())
                            .or_default()
                            .push(i);
                    }
                    continue;
                }
            }
            match r.module_of(s.file, &scope) {
                Some(module) if module.len() == scope.len() => {
                    for id in r.module_ids(s.file, &module) {
                        // A module is reached as a module, never also as a symbol, so a
                        // plain `use crate::a;` has one target.
                        if s.kind == SymbolKind::Module {
                            r.children.entry(id).or_default().insert(s.name.clone());
                        } else {
                            r.items
                                .entry(id)
                                .or_default()
                                .entry(s.name.clone())
                                .or_default()
                                .push(i);
                        }
                    }
                }
                _ => {
                    r.local
                        .entry((s.file, scope))
                        .or_default()
                        .entry(s.name.clone())
                        .or_default()
                        .push(i);
                }
            }
        }
        for (file, x) in extractions.iter().enumerate() {
            for import in &x.imports {
                let scope = x.scope_path(import.scope);
                let module = r.module_of(file, &scope).unwrap_or_default();
                let name = if import.glob {
                    None
                } else {
                    import.alias.clone().or_else(|| import.path.last().cloned())
                };
                let entry = Use {
                    name,
                    path: import.path.clone(),
                };
                if module.len() < scope.len() {
                    r.local_uses.entry((file, scope)).or_default().push(entry);
                } else {
                    for id in r.module_ids(file, &module) {
                        r.uses.entry(id).or_default().push(Use {
                            name: entry.name.clone(),
                            path: entry.path.clone(),
                        });
                    }
                }
            }
        }
        r
    }

    /// The longest leading part of `scope` that is a chain of inline modules.
    fn module_of(&self, file: usize, scope: &[String]) -> Option<Vec<String>> {
        (0..=scope.len())
            .rev()
            .map(|n| &scope[..n])
            .find(|prefix| prefix.is_empty() || self.module_scopes[file].contains(*prefix))
            .map(<[String]>::to_vec)
    }

    /// A file's module plus inline modules inside it, in its primary target.
    fn module_id(&self, file: usize, inline: &[String]) -> ModuleId {
        let f = &self.files[file];
        let mut path = f.module.clone();
        path.extend(inline.iter().cloned());
        (f.crate_unit, path)
    }

    /// The same, in every target the file belongs to.
    fn module_ids(&self, file: usize, inline: &[String]) -> Vec<ModuleId> {
        let f = &self.files[file];
        if f.memberships.is_empty() {
            return vec![self.module_id(file, inline)];
        }
        f.memberships
            .iter()
            .map(|(unit, module)| {
                let mut path = module.clone();
                path.extend(inline.iter().cloned());
                (Some(*unit), path)
            })
            .collect()
    }

    /// Records the members of a type that live in an `impl` block: the symbols whose
    /// scope is the block's (`member_scope`, the impl's scope plus its type path).
    pub(crate) fn add_impl_members(
        &mut self,
        types: &[usize],
        file: usize,
        member_scope: Vec<String>,
    ) {
        let members = self
            .by_scope
            .get(&(file, member_scope))
            .cloned()
            .unwrap_or_default();
        for i in members {
            for &t in types {
                let name = self.symbols[i].name.clone();
                let list = self.members.entry(t).or_default().entry(name).or_default();
                if !list.contains(&i) {
                    list.push(i);
                }
            }
        }
    }

    /// Resolves `path` as written in `file`, inside `scope` (its position in the file).
    pub(crate) fn resolve(&self, file: usize, scope: &[String], path: &[String]) -> Resolved {
        let inline = self.module_of(file, scope).unwrap_or_default();
        let (unit, module) = self.module_id(file, &inline);
        let mut memo = Memo::default();
        let places = self.resolve_from(unit, &module, Some((file, scope)), path, &mut memo, 0);
        let mut found: Vec<Endpoint> = places
            .iter()
            .filter_map(|p| match p {
                Place::Module(Some(u), m) => Some(Endpoint::Module(*u, m.clone())),
                Place::Symbol(s) => Some(Endpoint::Symbol(*s)),
                _ => None,
            })
            .collect();
        found.sort();
        found.dedup();
        if !found.is_empty() {
            return Resolved::Found(found);
        }
        if places.contains(&Place::External) {
            return Resolved::External;
        }
        // A path whose start is in the repository (a local module, a workspace crate) and
        // then leads nowhere is missing here, not another crate's.
        let start = self.resolve_from(
            unit,
            &module,
            Some((file, scope)),
            &path[..1.min(path.len())],
            &mut Memo::default(),
            0,
        );
        let starts_inside = start.iter().any(|p| *p != Place::External);
        if !starts_inside && looks_external(path) {
            Resolved::External
        } else {
            Resolved::Missing
        }
    }

    fn resolve_from(
        &self,
        unit: Option<usize>,
        module: &[String],
        local: Option<(usize, &[String])>,
        path: &[String],
        memo: &mut Memo,
        depth: usize,
    ) -> Vec<Place> {
        let Some((first, rest)) = path.split_first() else {
            return Vec::new();
        };
        if depth > MAX_USE_DEPTH {
            memo.cut = true;
            return Vec::new();
        }
        let mut places: Vec<Place> = match first.as_str() {
            "crate" => vec![Place::Module(unit, Vec::new())],
            "self" => vec![Place::Module(unit, module.to_vec())],
            "super" => match module.split_last() {
                Some((_, parent)) => vec![Place::Module(unit, parent.to_vec())],
                None => Vec::new(),
            },
            "Self" => self.self_type(unit, module, local, memo, depth),
            name => {
                let mut found = self.local_lookup(unit, module, local, name, memo, depth);
                if found.is_empty() {
                    found = self.lookup(unit, module, name, memo, depth);
                }
                if found.is_empty() {
                    if let Some(&lib) = self.libraries.get(name) {
                        found.push(Place::Module(Some(lib), Vec::new()));
                    } else if EXTERNAL_ROOTS.contains(&name) || !rest.is_empty() {
                        // `dep::x` with `dep` nowhere here: another crate's, and that holds
                        // through any `use` that binds it.
                        found.push(Place::External);
                    }
                }
                found
            }
        };
        for segment in rest {
            let mut next = Vec::new();
            for place in &places {
                match place {
                    // `super::super::…`: each one steps up again.
                    Place::Module(u, m) if segment == "super" => {
                        if let Some((_, parent)) = m.split_last() {
                            next.push(Place::Module(*u, parent.to_vec()));
                        }
                    }
                    Place::Module(u, m) => next.extend(self.lookup(*u, m, segment, memo, depth)),
                    Place::Symbol(s) => {
                        if let Some(members) = self.members.get(s).and_then(|m| m.get(segment)) {
                            next.extend(members.iter().map(|&m| Place::Symbol(m)));
                        }
                    }
                    Place::External => next.push(Place::External),
                }
            }
            next.sort();
            next.dedup();
            places = next;
        }
        places
    }

    /// Items and `use` bindings declared inside the enclosing functions, innermost first:
    /// they shadow the module's, and are invisible outside their function.
    fn local_lookup(
        &self,
        unit: Option<usize>,
        module: &[String],
        local: Option<(usize, &[String])>,
        name: &str,
        memo: &mut Memo,
        depth: usize,
    ) -> Vec<Place> {
        let Some((file, scope)) = local else {
            return Vec::new();
        };
        for n in (1..=scope.len()).rev() {
            let at = (file, scope[..n].to_vec());
            if let Some(syms) = self.local.get(&at).and_then(|m| m.get(name)) {
                return syms.iter().map(|&s| Place::Symbol(s)).collect();
            }
            let bound: Vec<&Use> = self
                .local_uses
                .get(&at)
                .into_iter()
                .flatten()
                .filter(|u| u.name.as_deref() == Some(name))
                .collect();
            if !bound.is_empty() {
                let mut found = Vec::new();
                for u in bound {
                    found.extend(self.resolve_from(unit, module, None, &u.path, memo, depth + 1));
                }
                return found;
            }
        }
        Vec::new()
    }

    /// `Self`: the nearest enclosing scope that names a type (an `impl`'s type, a trait,
    /// a struct), skipping the method and function names in between.
    fn self_type(
        &self,
        unit: Option<usize>,
        module: &[String],
        local: Option<(usize, &[String])>,
        memo: &mut Memo,
        depth: usize,
    ) -> Vec<Place> {
        let Some((_, scope)) = local else {
            return Vec::new();
        };
        for name in scope.iter().rev() {
            let path: Vec<String> = name.split("::").map(String::from).collect();
            let types: Vec<Place> = self
                .resolve_from(unit, module, None, &path, memo, depth + 1)
                .into_iter()
                .filter(|p| match p {
                    Place::Symbol(s) => matches!(
                        self.symbols[*s].kind,
                        SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Trait
                            | SymbolKind::TypeAlias
                    ),
                    _ => false,
                })
                .collect();
            if !types.is_empty() {
                return types;
            }
        }
        Vec::new()
    }

    /// `name` inside a module: its own items and submodules, then its `use` bindings,
    /// then its glob imports (see [`Memo`] for how repeats and cycles are handled).
    fn lookup(
        &self,
        unit: Option<usize>,
        module: &[String],
        name: &str,
        memo: &mut Memo,
        depth: usize,
    ) -> Vec<Place> {
        let key = (unit, module.to_vec(), name.to_string());
        if let Some((done, cut_at)) = memo.done.get(&key)
            && cut_at.is_none_or(|at| depth >= at)
        {
            memo.cut |= cut_at.is_some();
            return done.clone();
        }
        memo.lookups += 1;
        if memo.lookups > MAX_LOOKUPS {
            memo.cut = true;
            return Vec::new();
        }
        if !memo.running.insert(key.clone()) {
            return Vec::new();
        }
        let outer_cut = std::mem::take(&mut memo.cut);
        let id: ModuleId = (unit, module.to_vec());
        let mut found: Vec<Place> = Vec::new();
        if let Some(syms) = self.items.get(&id).and_then(|m| m.get(name)) {
            found.extend(syms.iter().map(|&s| Place::Symbol(s)));
        }
        if self.children.get(&id).is_some_and(|c| c.contains(name)) {
            let mut child = module.to_vec();
            child.push(name.to_string());
            found.push(Place::Module(unit, child));
        }
        if found.is_empty() {
            for u in self.uses.get(&id).into_iter().flatten() {
                if u.name.as_deref() == Some(name) {
                    found.extend(self.resolve_from(unit, module, None, &u.path, memo, depth + 1));
                }
            }
        }
        if found.is_empty() {
            let globs: Vec<Vec<String>> = self
                .uses
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|u| u.name.is_none())
                .map(|u| u.path.clone())
                .collect();
            for glob in globs {
                for place in self.resolve_from(unit, module, None, &glob, memo, depth + 1) {
                    match place {
                        Place::Module(gu, gm) => {
                            found.extend(self.lookup(gu, &gm, name, memo, depth + 1))
                        }
                        Place::Symbol(s) => {
                            if let Some(members) = self.members.get(&s).and_then(|m| m.get(name)) {
                                found.extend(members.iter().map(|&m| Place::Symbol(m)));
                            }
                        }
                        Place::External => {}
                    }
                }
            }
        }
        memo.running.remove(&key);
        let cut_at = memo.cut.then_some(depth);
        memo.done.insert(key, (found.clone(), cut_at));
        memo.cut |= outer_cut;
        found
    }
}

/// A path whose first segment names nothing here is another crate's, unless it starts
/// with `crate`, `self`, `super` or `Self`, which always mean this crate.
fn looks_external(path: &[String]) -> bool {
    match path.first().map(String::as_str) {
        Some("crate" | "self" | "super" | "Self") | None => false,
        Some(first) => EXTERNAL_ROOTS.contains(&first) || path.len() > 1,
    }
}
