//! Module graph, item collection and name resolution.
//!
//! This is the first half of roadmap phase 2.1. It answers "what exists, and
//! where", which everything downstream needs before it can answer anything
//! else. Body lowering — desugaring `|>`, resolving expressions, compiling
//! `match` to a decision tree — comes next and builds on this.
//!
//! Everything here is a salsa query, per decision A3. Collecting items for one
//! file does not read any other file, so editing a function body invalidates
//! that file's item map and nothing else.
//!
//! # What resolution means
//!
//! `docs/design/associated-items.md` decides this, and the `::` / `.` split
//! does most of the work: a `::` path is resolved in the module graph and the
//! type namespace, with locals deliberately not participating, while `.` is
//! field-then-method on a value.
//!
//! Only the `::` half matters for phase 2 — module paths and variant
//! constructors — because the vertical slice excludes records and typeclasses.
//! Associated items report [`Resolution::Unsupported`] rather than being
//! guessed at, which keeps the rule intact for when they arrive.

pub mod body;

use khora_db::{Db, SourceFile};
use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;

/// A dotted module path, e.g. `std.core`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModulePath(Vec<String>);

impl ModulePath {
    pub fn new(segments: Vec<String>) -> ModulePath {
        ModulePath(segments)
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

impl std::fmt::Display for ModulePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.join("."))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Type,
    Trait,
    Effect,
    Function,
    Let,
    Context,
}

impl ItemKind {
    pub fn describe(self) -> &'static str {
        match self {
            ItemKind::Type => "type",
            ItemKind::Trait => "trait",
            ItemKind::Effect => "effect",
            ItemKind::Function => "function",
            ItemKind::Let => "binding",
            ItemKind::Context => "context",
        }
    }
}

/// A named thing a module declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
    /// Whether the item is exported from its module.
    pub is_public: bool,
    pub range: TextRange,
}

/// A constructor of a variant type, recorded separately because it is reached
/// through its type (`RiskLevel.Critical`) rather than by a bare name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub type_name: String,
    pub name: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    /// `import a.b.{X, Y as Z};`
    Named(Vec<ImportedName>),
    /// `import a.b.*;`
    Glob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedName {
    pub name: String,
    /// The local name, which differs from `name` under `as`.
    pub alias: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub path: ModulePath,
    pub kind: ImportKind,
    pub range: TextRange,
}

/// Anything a query wants to report without being able to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirError {
    pub message: String,
    pub range: TextRange,
}

/// One `test "name" { .. }` block.
///
/// Not an [`Item`]: a test has no name a program can refer to, only one a
/// person reads in a report. `key` is what its body is recorded under, and is
/// mangled so it cannot collide with anything a program declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestItem {
    pub key: String,
    pub name: String,
    pub range: TextRange,
}

/// The types that exist without being declared.
///
/// `impl Int { .. }` is as legitimate as `impl User { .. }`, so a path whose
/// owner is one of these names an item the same way — the resolver only has to
/// know that the owner is a type, and for these three nobody wrote it down.
pub const BUILTIN_TYPES: [&str; 3] = ["Int", "Bool", "String"];

/// What every test's key begins with. `#` cannot occur in a Khora identifier,
/// so this can never collide with a name a program chose.
pub const TEST_PREFIX: &str = "#test$";

/// The body of the *n*th test in a file.
pub fn test_key(index: usize) -> String {
    format!("{TEST_PREFIX}{index}")
}

/// Whether a name is a test's, however it has been qualified.
///
/// Monomorphization prefixes a body's key with its module — `app$main$#test$0`
/// — so a test is not recognised by what its symbol *starts* with. Searching
/// is still exact rather than loose: `#` cannot occur in a Khora identifier
/// and module segments are identifiers, so `#test$` can only have come from
/// [`test_key`].
pub fn is_test(symbol: &str) -> bool {
    symbol.contains(TEST_PREFIX)
}

/// Everything one file declares and imports.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemMap {
    pub module: Option<ModulePath>,
    pub items: Vec<Item>,
    /// The file's tests, in written order.
    pub tests: Vec<TestItem>,
    pub variants: Vec<Variant>,
    pub imports: Vec<Import>,
    pub errors: Vec<HirError>,
}

impl ItemMap {
    pub fn item(&self, name: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.name == name)
    }

    /// Constructors of `type_name`.
    pub fn variants_of<'a>(&'a self, type_name: &'a str) -> impl Iterator<Item = &'a Variant> + 'a {
        self.variants.iter().filter(move |v| v.type_name == type_name)
    }
}

/// Collects what one file declares.
///
/// Deliberately reads only this file. Cross-file questions are answered by
/// [`module_graph`], so a body edit cannot invalidate another file's items.
#[salsa::tracked(returns(ref))]
pub fn item_map(db: &dyn Db, file: SourceFile) -> ItemMap {
    let parse = khora_db::parse(db, file);
    let source = parse.source_file();
    let mut map = ItemMap { module: module_path_of(&source), ..ItemMap::default() };

    if map.module.is_none() {
        map.errors.push(HirError {
            message: "every file must begin with a `module` declaration".to_string(),
            range: TextRange::empty(0.into()),
        });
    }

    for import in source.imports() {
        if let Some(collected) = collect_import(&import) {
            map.imports.push(collected);
        }
    }

    for decl in source.decls() {
        collect_decl(&decl, &mut map);
    }

    // Numbered by position, so a test's identity is where it is written. A
    // name would be nicer to read in a symbol table and worse to rely on:
    // nothing stops two tests sharing one.
    for (index, decl) in source.decls().enumerate() {
        let ast::Decl::Test(t) = &decl else { continue };
        map.tests.push(TestItem {
            key: test_key(index),
            name: t.name().unwrap_or_default(),
            range: t.syntax().text_range(),
        });
    }

    detect_duplicates(&mut map);
    map
}

fn module_path_of(source: &ast::SourceFile) -> Option<ModulePath> {
    let path = source.module()?.path()?;
    let segments: Vec<String> = path.segments().filter_map(|s| s.ident()).collect();
    (!segments.is_empty()).then(|| ModulePath::new(segments))
}

fn collect_import(import: &ast::ImportDecl) -> Option<Import> {
    let path = import.path()?;
    let segments: Vec<String> = path.segments().filter_map(|s| s.ident()).collect();
    if segments.is_empty() {
        return None;
    }

    let kind = if import.is_glob() {
        ImportKind::Glob
    } else {
        ImportKind::Named(
            import
                .items()
                .filter_map(|item| {
                    let name = item.name()?.ident()?;
                    let alias = item.alias().and_then(|a| a.ident()).unwrap_or_else(|| name.clone());
                    Some(ImportedName { name, alias, range: item.syntax().text_range() })
                })
                .collect(),
        )
    };

    Some(Import { path: ModulePath::new(segments), kind, range: import.syntax().text_range() })
}

fn collect_decl(decl: &ast::Decl, map: &mut ItemMap) {
    let (name, kind, is_public, range) = match decl {
        ast::Decl::Type(t) => {
            // A variant type's constructors are reached through the type, so
            // they are recorded against it rather than as free items.
            if let (Some(name), Some(ast::Type::Variant(v))) = (t.name(), t.definition()) {
                if let Some(type_name) = name.ident() {
                    for case in v.cases() {
                        if let Some(case_name) = case.name().and_then(|n| n.ident()) {
                            map.variants.push(Variant {
                                type_name: type_name.clone(),
                                name: case_name,
                                range: case.syntax().text_range(),
                            });
                        }
                    }
                }
            }
            (t.name(), ItemKind::Type, t.is_exported(), t.syntax().text_range())
        }
        ast::Decl::Trait(t) => {
            (t.name(), ItemKind::Trait, t.is_exported(), t.syntax().text_range())
        }
        // An impl has no name of its own: it is found through the trait and the
        // type it is written for, never referred to directly. Recording it as
        // an item would give two impls of different traits for one type a
        // spurious duplicate-name error.
        ast::Decl::Impl(_) => return,
        ast::Decl::Effect(e) => {
            (e.name(), ItemKind::Effect, e.is_exported(), e.syntax().text_range())
        }
        ast::Decl::Context(c) => {
            (c.name(), ItemKind::Context, c.is_exported(), c.syntax().text_range())
        }
        ast::Decl::Fn(f) => (f.name(), ItemKind::Function, f.is_exported(), f.syntax().text_range()),
        ast::Decl::Let(l) => {
            let name = match l.pat() {
                Some(ast::Pat::Ident(p)) => p.name(),
                _ => None,
            };
            (name, ItemKind::Let, false, l.syntax().text_range())
        }
        // Tests and benches are not nameable, so they are not items.
        ast::Decl::Test(_) | ast::Decl::Bench(_) => return,
    };

    if let Some(name) = name.and_then(|n| n.ident()) {
        map.items.push(Item { name, kind, is_public, range });
    }
}

/// Two items with the same name in one module is an error, and the message has
/// to name both places or it is useless.
fn detect_duplicates(map: &mut ItemMap) {
    let mut seen: Vec<(String, TextRange)> = Vec::new();
    let mut errors = Vec::new();
    for item in &map.items {
        if let Some((_, first)) = seen.iter().find(|(name, _)| name == &item.name) {
            errors.push(HirError {
                message: format!(
                    "`{}` is defined twice in this module; the first is at byte {}",
                    item.name,
                    u32::from(first.start())
                ),
                range: item.range,
            });
        } else {
            seen.push((item.name.clone(), item.range));
        }
    }
    map.errors.extend(errors);
}

/// Which file defines which module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModuleGraph {
    modules: Vec<(ModulePath, SourceFile)>,
    pub errors: Vec<HirError>,
}

impl ModuleGraph {
    pub fn file(&self, path: &ModulePath) -> Option<SourceFile> {
        self.modules.iter().find(|(p, _)| p == path).map(|(_, f)| *f)
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn paths(&self) -> impl Iterator<Item = &ModulePath> {
        self.modules.iter().map(|(p, _)| p)
    }
}

/// Builds the module graph for a source root.
#[salsa::tracked(returns(ref))]
pub fn module_graph(db: &dyn Db, root: khora_db::SourceRoot) -> ModuleGraph {
    let mut graph = ModuleGraph::default();
    for file in root.files(db) {
        let map = item_map(db, *file);
        let Some(path) = map.module.clone() else { continue };

        if graph.modules.iter().any(|(p, _)| p == &path) {
            graph.errors.push(HirError {
                message: format!("module `{path}` is declared in more than one file"),
                range: TextRange::empty(0.into()),
            });
            continue;
        }
        graph.modules.push((path, *file));
    }
    graph.modules.sort_by(|a, b| a.0.cmp(&b.0));
    graph
}

/// Every name a file can use without qualifying it.
///
/// One entry per *bare* name: what this file declares, plus what it imported.
/// Built once per file so that body lowering can answer "what is `double`?"
/// without walking the module graph at every mention.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileScope {
    /// Imported names, by the spelling this file uses for them — an alias if
    /// one was written, otherwise the item's own name.
    ///
    /// The `Resolution` names the item as *this file* spells it, because every
    /// lookup downstream is keyed that way. Where the defining module spells
    /// it differently, [`FileScope::origin`] says so.
    pub names: Vec<(String, Resolution)>,
    /// Where each imported name came from and what its own module calls it.
    ///
    /// Only an alias makes the two differ, and only monomorphization cares:
    /// it has to find the body, which lives under the original name.
    pub origins: Vec<Origin>,
    /// Constructors reachable unqualified, from a glob or a named import of
    /// the type. Kept apart from `names` because a case and an item may share
    /// a spelling without conflicting.
    pub variants: Vec<Variant>,
    pub errors: Vec<HirError>,
}

/// One imported name, as written here and as written where it is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub local: String,
    pub module: ModulePath,
    pub name: String,
    pub kind: ItemKind,
}

impl FileScope {
    /// What the defining module calls the thing this file calls `local`.
    pub fn origin(&self, local: &str) -> Option<&Origin> {
        self.origins.iter().find(|o| o.local == local)
    }

    pub fn get(&self, name: &str) -> Option<&Resolution> {
        self.names.iter().find(|(n, _)| n == name).map(|(_, r)| r)
    }

    pub fn variants_of<'a>(&'a self, type_name: &'a str) -> impl Iterator<Item = &'a Variant> + 'a {
        self.variants.iter().filter(move |v| v.type_name == type_name)
    }
}

/// Resolves a file's imports into the names its bodies may use.
///
/// Reads the *other* modules' item maps, never their bodies, so editing a
/// function cannot invalidate another file's scope.
#[salsa::tracked(returns(ref))]
pub fn file_scope(db: &dyn Db, file: SourceFile) -> FileScope {
    let map = item_map(db, file);
    let mut out = FileScope::default();

    let Some(root) = khora_db::source_root(db) else { return out };
    let graph = module_graph(db, root);

    for import in &map.imports {
        let Some(source) = graph.file(&import.path) else {
            out.errors.push(HirError {
                message: format!("cannot find module `{}`", import.path),
                range: import.range,
            });
            continue;
        };
        let exported = item_map(db, source);

        match &import.kind {
            ImportKind::Glob => {
                for item in exported.items.iter().filter(|i| i.is_public) {
                    out.names.push((
                        item.name.clone(),
                        Resolution::Item {
                            module: import.path.clone(),
                            name: item.name.clone(),
                            kind: item.kind,
                        },
                    ));
                    out.origins.push(Origin {
                        local: item.name.clone(),
                        module: import.path.clone(),
                        name: item.name.clone(),
                        kind: item.kind,
                    });
                }
                out.variants.extend(exported.variants.iter().cloned());
            }
            ImportKind::Named(names) => {
                for wanted in names {
                    // `alias` already holds the local spelling, `as` or not.
                    let local = wanted.alias.clone();
                    match exported.item(&wanted.name) {
                        Some(item) if item.is_public => {
                            // The resolution carries the *local* spelling:
                            // signatures, types and every other downstream map
                            // are keyed by what this file calls the thing, and
                            // a resolution naming the original would miss them
                            // all under an alias.
                            out.names.push((
                                local.clone(),
                                Resolution::Item {
                                    module: import.path.clone(),
                                    name: local.clone(),
                                    kind: item.kind,
                                },
                            ));
                            out.origins.push(Origin {
                                local,
                                module: import.path.clone(),
                                name: item.name.clone(),
                                kind: item.kind,
                            });
                            // Importing a type brings its constructors with it.
                            // Naming each one separately would be ceremony for
                            // no decision: a type without its cases is not
                            // usable.
                            out.variants.extend(
                                exported
                                    .variants_of(&wanted.name)
                                    .cloned()
                                    .collect::<Vec<_>>(),
                            );
                        }
                        Some(_) => out.errors.push(HirError {
                            message: format!(
                                "`{}` is not exported from `{}`",
                                wanted.name, import.path
                            ),
                            range: wanted.range,
                        }),
                        None => out.errors.push(HirError {
                            message: format!(
                                "`{}` does not declare `{}`",
                                import.path, wanted.name
                            ),
                            range: wanted.range,
                        }),
                    }
                }
            }
        }
    }
    out
}

/// What a name resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// An item in a module.
    Item { module: ModulePath, name: String, kind: ItemKind },
    /// A constructor of a variant type.
    Variant { module: ModulePath, type_name: String, name: String },
    /// A trait's function reached without a receiver: `Applicative::pure(x)`
    /// or `F::pure(x)` where `F` is a bounded type parameter.
    ///
    /// `owner` is written as it appears — a trait name, or a parameter name —
    /// because which one it is depends on the type parameters in scope, and
    /// resolving that is the checker's job rather than the name resolver's.
    TraitItem { owner: String, name: String },
    /// Recognized, but the vertical slice does not handle it yet.
    Unsupported(&'static str),
}

/// Resolves a `::` path as written in `file`.
///
/// Locals do not participate: `x::foo` where `x` is a binding is an error that
/// says so, rather than a mysterious failure to find `foo`. Field access goes
/// through `.` and is a separate question.
pub fn resolve_path(
    db: &dyn Db,
    root: khora_db::SourceRoot,
    file: SourceFile,
    segments: &[String],
) -> Result<Resolution, HirError> {
    let graph = module_graph(db, root);
    let map = item_map(db, file);
    let unresolved = |what: &str| HirError {
        message: format!("cannot find `{what}` in this scope"),
        range: TextRange::empty(0.into()),
    };

    let Some((first, rest)) = segments.split_first() else {
        return Err(unresolved(""));
    };

    // A name declared in this file, or brought in by an import.
    if rest.is_empty() {
        if let Some(item) = map.item(first) {
            return Ok(Resolution::Item {
                module: map.module.clone().unwrap_or_else(|| ModulePath::new(vec![])),
                name: item.name.clone(),
                kind: item.kind,
            });
        }
        if let Some(res) = resolve_through_imports(db, graph, map, first) {
            return Ok(res);
        }
        return Err(unresolved(first));
    }

    // A local in a `::` path is a category error, and saying so beats
    // reporting that its last segment could not be found.
    if let Some(local) = map.item(first) {
        if matches!(local.kind, ItemKind::Let | ItemKind::Function) {
            return Err(HirError {
                message: format!(
                    "`{first}` is a {}, not a module or a type; use `.` to project from a value",
                    local.kind.describe()
                ),
                range: local.range,
            });
        }
    }

    // `Type::Constructor`, checked before module paths because a type in scope
    // is the more specific reading.
    if rest.len() == 1 && map.item(first).is_some_and(|i| i.kind == ItemKind::Type) {
        if map.variants_of(first).any(|v| v.name == rest[0]) {
            return Ok(Resolution::Variant {
                module: map.module.clone().unwrap_or_else(|| ModulePath::new(vec![])),
                type_name: first.clone(),
                name: rest[0].clone(),
            });
        }
        // Not a constructor, so it is a function the type declares for
        // itself. Which one is the checker's question, the same as it is for
        // `Applicative::pure` — the resolver's job ends at "the owner is this
        // name", because whether that name is a trait, a type, or a bounded
        // parameter depends on what is in scope where it was written.
        return Ok(Resolution::TraitItem { owner: first.clone(), name: rest[0].clone() });
    }

    // `a.b.c` as a module path — case 2. The longest prefix that names a module
    // wins, and the remainder must be a single item name.
    for split in (1..=segments.len().saturating_sub(1)).rev() {
        let candidate = ModulePath::new(segments[..split].to_vec());
        let Some(target) = graph.file(&candidate) else { continue };
        let target_map = item_map(db, target);
        let remainder = &segments[split..];

        if remainder.len() == 1 {
            if let Some(item) = target_map.item(&remainder[0]) {
                if !item.is_public {
                    return Err(HirError {
                        message: format!(
                            "`{}` is private to module `{candidate}`",
                            remainder[0]
                        ),
                        range: TextRange::empty(0.into()),
                    });
                }
                return Ok(Resolution::Item {
                    module: candidate,
                    name: item.name.clone(),
                    kind: item.kind,
                });
            }
        }
        if remainder.len() == 2 {
            if let Some(v) = target_map
                .variants_of(&remainder[0])
                .find(|v| v.name == remainder[1])
            {
                return Ok(Resolution::Variant {
                    module: candidate,
                    type_name: v.type_name.clone(),
                    name: v.name.clone(),
                });
            }
        }
        return Err(unresolved(&remainder.join(".")));
    }

    Err(unresolved(&segments.join(".")))
}

fn resolve_through_imports(
    db: &dyn Db,
    graph: &ModuleGraph,
    map: &ItemMap,
    name: &str,
) -> Option<Resolution> {
    for import in &map.imports {
        let target = graph.file(&import.path)?;
        let target_map = item_map(db, target);
        match &import.kind {
            ImportKind::Named(names) => {
                let imported = names.iter().find(|n| n.alias == name)?;
                let item = target_map.item(&imported.name)?;
                if item.is_public {
                    return Some(Resolution::Item {
                        module: import.path.clone(),
                        name: item.name.clone(),
                        kind: item.kind,
                    });
                }
            }
            ImportKind::Glob => {
                if let Some(item) = target_map.item(name) {
                    if item.is_public {
                        return Some(Resolution::Item {
                            module: import.path.clone(),
                            name: item.name.clone(),
                            kind: item.kind,
                        });
                    }
                }
            }
        }
    }
    None
}
