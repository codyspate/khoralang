//! Module graph, item collection and name resolution.
//!
//! This answers "what exists, and where", which everything downstream needs
//! before it can answer anything else. Body lowering — desugaring `|>` and
//! `for`, resolving expressions, taking patterns apart — is `body`, and builds
//! on this.
//!
//! Everything here is a salsa query (decision A3). Collecting items for one
//! file reads no other file, so editing a body invalidates that file's item map
//! and nothing else.
//!
//! # What resolution means
//!
//! `docs/design/associated-items.md` decides this, and the `::` / `.` split
//! does most of the work: a `::` path is resolved in the module graph and the
//! type namespace, with locals deliberately not participating, while `.` is
//! field-then-method on a value.
//!
//! An associated item nothing can resolve reports
//! [`Resolution::Unsupported`] rather than being guessed at.

pub mod body;
pub mod derive;

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
    Const,
    Context,
}

impl ItemKind {
    pub fn describe(self) -> &'static str {
        match self {
            ItemKind::Type => "type",
            ItemKind::Trait => "trait",
            ItemKind::Effect => "effect",
            ItemKind::Function => "function",
            ItemKind::Const => "constant",
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
    pub kind: TestKind,
    pub range: TextRange,
}

/// Whether a block is checked or timed.
///
/// They are collected together and keyed the same way because to everything
/// between here and code generation they are the same thing: a body with no
/// name, no parameters and no caller. Only the runner treats them differently,
/// and only in what it does with the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// `test "..." { .. }` — run once, and it either passed or it did not.
    Test,
    /// `bench "..." { .. }` — run many times, and what comes out is a
    /// distribution.
    Bench,
}

/// The types that exist without being declared.
///
/// `impl Int { .. }` is as legitimate as `impl User { .. }`, so a path whose
/// owner is one of these names an item the same way — the resolver only has to
/// know that the owner is a type, and for these nobody wrote it down.
///
/// `I64` is here because it is a second spelling of `Int` rather than another
/// type, so `I64::to_u8` has to resolve exactly as `Int::to_u8` does.
pub const BUILTIN_TYPES: [&str; 14] = [
    "Int", "Float", "Bool", "Char", "String", "Ptr", "U8", "U16", "U32", "U64", "I8", "I16",
    "I32", "I64",
];

/// What every test's key begins with. `#` cannot occur in a Khora identifier,
/// so this can never collide with a name a program chose.
pub const TEST_PREFIX: &str = "#test$";

/// The body of the *n*th test in a file.
pub fn test_key(index: usize) -> String {
    format!("{TEST_PREFIX}{index}")
}

/// Whether a name is a test's or a bench's, however it has been qualified.
///
/// One prefix for both, because everything this gates is "a body with no name
/// and no caller, which has to be compiled anyway". [`TestKind`] is where the
/// difference lives.
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
    /// Where the `module` declaration is, so that a second file claiming the
    /// same module has somewhere to be told about it.
    pub module_range: Option<TextRange>,
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

/// One declaration, as another file can observe it.
///
/// [`Item`] without the span, which is the whole point -- see [`module_api`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiItem {
    pub name: String,
    pub kind: ItemKind,
    pub is_public: bool,
}

/// What a module offers, with nothing in it that moves when a body is edited.
///
/// **This type exists to be compared.** Salsa stops an invalidation spreading
/// when a recomputed value equals the old one, so what decides whether this
/// compiler is incremental in practice is which values change after a
/// keystroke. [`ItemMap`] changes constantly — it carries a [`TextRange`] per
/// item, so inserting one character into the *first* function shifts the span
/// of every declaration below it, and everything downstream re-runs.
///
/// The cross-file queries depend on this projection instead. It re-executes on
/// each edit, costs a walk over the item list, and compares equal, which is
/// where the invalidation stops. `item_map` keeps its spans for the things that
/// need them: diagnostics, and go-to-definition.
///
/// Measured in `khora-hir/tests/incremental.rs`: a one-character body edit used
/// to re-run `module_graph` and the importing file's `file_scope`, and now runs
/// neither.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModuleApi {
    pub module: Option<ModulePath>,
    pub items: Vec<ApiItem>,
    pub variants: Vec<Variant>,
}

impl ModuleApi {
    pub fn item(&self, name: &str) -> Option<&ApiItem> {
        self.items.iter().find(|i| i.name == name)
    }

    /// Constructors of `type_name`.
    pub fn variants_of<'a>(&'a self, type_name: &'a str) -> impl Iterator<Item = &'a Variant> + 'a {
        self.variants.iter().filter(move |v| v.type_name == type_name)
    }
}

/// The span-free view of a file's declarations.
///
/// Every query that reads *another* file goes through this rather than through
/// [`item_map`]. See [`ModuleApi`] for why.
#[salsa::tracked(returns(ref))]
pub fn module_api(db: &dyn Db, file: SourceFile) -> ModuleApi {
    let map = item_map(db, file);
    ModuleApi {
        module: map.module.clone(),
        items: map
            .items
            .iter()
            .map(|i| ApiItem { name: i.name.clone(), kind: i.kind, is_public: i.is_public })
            .collect(),
        variants: map.variants.clone(),
    }
}

/// Collects what one file declares.
///
/// Deliberately reads only this file, so it can never be invalidated by an edit
/// to another one. Note that the converse does not follow and was wrong here
/// for a long time: this map changing does not mean another file's *meaning*
/// changed, because it carries spans that move for reasons nobody imports.
/// [`module_api`] is the barrier that makes the difference visible to salsa.
#[salsa::tracked(returns(ref))]
pub fn item_map(db: &dyn Db, file: SourceFile) -> ItemMap {
    let parse = khora_db::parse(db, file);
    let source = parse.source_file();
    let mut map = ItemMap {
        module: module_path_of(&source),
        module_range: module_range_of(&source),
        ..ItemMap::default()
    };

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
        let (kind, name, range) = match &decl {
            ast::Decl::Test(t) => {
                (TestKind::Test, t.name(), t.syntax().text_range())
            }
            // **`bench` parsed and was then dropped on the floor.** The grammar
            // has had it since phase 1 and nothing ever collected it, so a
            // `bench` block compiled to nothing and ran never -- silently,
            // which is the worst way for a promised feature not to work.
            ast::Decl::Bench(b) => {
                (TestKind::Bench, b.name(), b.syntax().text_range())
            }
            _ => continue,
        };
        map.tests.push(TestItem {
            key: test_key(index),
            name: name.unwrap_or_default(),
            kind,
            range,
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

/// The span of the `module` declaration, for a diagnostic to point at.
fn module_range_of(source: &ast::SourceFile) -> Option<TextRange> {
    Some(source.module()?.syntax().text_range())
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
                            });
                        }
                    }
                }
            }
            // **`type UserId = Int;` has one constructor, named after itself.**
            //
            // It is a type of its own — nothing accepts a `UserId` where an
            // `Int` was wanted, which is the point of writing it — and it used
            // to have no way in or out at all, so nothing could convert either
            // direction and the type was uninhabitable.
            //
            // `UserId(7)` makes one and `match id { UserId(v) => v }` takes it
            // apart, which is what a reader of Rust's tuple struct expects.
            // Recorded here so the *pattern* resolves; the constructor already
            // followed from the type's one positional field.
            //
            // A record or a variant declares its own cases, and a declaration
            // with no definition — `pub type Ptr;` — is opaque and has none.
            if let (Some(name), Some(definition)) = (t.name(), t.definition()) {
                let wraps = !matches!(definition, ast::Type::Record(_) | ast::Type::Variant(_));
                if let (true, Some(type_name)) = (wraps, name.ident()) {
                    map.variants.push(Variant {
                        type_name: type_name.clone(),
                        name: type_name,
                    });
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
        ast::Decl::Const(c) => {
            let name = match c.pat() {
                Some(ast::Pat::Ident(p)) => p.name(),
                _ => None,
            };
            (name, ItemKind::Const, c.is_exported(), c.syntax().text_range())
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
        // `module_api`, not `item_map`: this reads one field, and depending on
        // the whole map would rebuild the graph -- and everything behind it --
        // every time a span moved.
        let api = module_api(db, *file);
        let Some(path) = api.module.clone() else { continue };

        if graph.modules.iter().any(|(p, _)| p == &path) {
            // **Kept, and reported somewhere a person will see it.** This
            // error existed and nothing ever read `graph.errors`, so two files
            // declaring one module compiled quietly -- and since a name
            // defined in both resolves to whichever file was read last, adding
            // a helper whose name already existed in a sibling changed what
            // the program did with no diagnostic anywhere. The graph has no
            // span to point at, so [`file_scope`] says it again against the
            // file that lost. Found by somebody's second program.
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

/// The modules one file imports, and nothing else.
///
/// Separate from [`item_map`] so that [`import_cycles`] can be built without
/// depending on it: a map carries ranges, so every edit that moves a span
/// would rebuild the cycle check and everything behind it. A list of paths
/// changes only when an `import` line does.
#[salsa::tracked(returns(ref))]
pub fn module_imports(db: &dyn Db, file: SourceFile) -> Vec<ModulePath> {
    item_map(db, file).imports.iter().map(|i| i.path.clone()).collect()
}

/// Import cycles, as the modules on them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportCycles {
    /// One entry per module that can reach itself through imports, holding a
    /// path back to itself for the message.
    through: Vec<(ModulePath, Vec<ModulePath>)>,
}

impl ImportCycles {
    /// The cycle `module` sits on, if it sits on one.
    pub fn through(&self, module: &ModulePath) -> Option<&[ModulePath]> {
        self.through.iter().find(|(m, _)| m == module).map(|(_, path)| path.as_slice())
    }
}

/// Which modules import themselves, directly or through others.
///
/// **A cycle is an error rather than a shape to support**, and finding it here
/// is what stops it being a crash. `type_map` resolves an imported name by
/// asking for the exporting file's `type_map`, so two modules that import each
/// other ask for each other for ever -- and Salsa, which has no way to know
/// the recursion is the program's fault rather than the compiler's, panics
/// with `dependency graph cycle when querying type_map`. Seven lines of Khora
/// were enough. Errata 55.
///
/// Built from paths alone, so it costs one walk of the imports and is
/// invalidated only by an `import` line changing.
#[salsa::tracked(returns(ref))]
pub fn import_cycles(db: &dyn Db, root: khora_db::SourceRoot) -> ImportCycles {
    let graph = module_graph(db, root);
    let mut out = ImportCycles::default();

    for start in graph.paths() {
        // Depth-first from `start`, looking only for a way back to it. The
        // first one found is the one reported: a module on two cycles has one
        // problem, and naming both would not help fix either.
        let mut stack = vec![(start.clone(), vec![start.clone()])];
        let mut seen: Vec<ModulePath> = Vec::new();
        while let Some((at, path)) = stack.pop() {
            if seen.contains(&at) {
                continue;
            }
            seen.push(at.clone());
            let Some(file) = graph.file(&at) else { continue };
            for next in module_imports(db, file) {
                let mut path = path.clone();
                path.push(next.clone());
                if *next == *start {
                    out.through.push((start.clone(), path));
                    stack.clear();
                    break;
                }
                stack.push((next.clone(), path));
            }
        }
    }
    out
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
/// Reads other modules through [`module_api`], never through [`item_map`] and
/// never their bodies, so editing a function cannot invalidate another file's
/// scope.
///
/// That sentence was here before it was true. `item_map` carries a span per
/// item, so it changed whenever anything above an item was edited, and this
/// query re-ran for every importer of the edited file.
#[salsa::tracked(returns(ref))]
pub fn file_scope(db: &dyn Db, file: SourceFile) -> FileScope {
    let map = item_map(db, file);
    let mut out = FileScope::default();

    let Some(root) = khora_db::source_root(db) else { return out };
    let graph = module_graph(db, root);

    // **A module lives in one file.** The graph keeps the first file that
    // claims a path and drops the rest, so a second one is invisible to name
    // resolution and perfectly visible to everything downstream that walks the
    // file list -- which is how `fn shared_name` in two files of `module main`
    // both compiled and the later one won, with `khora check` reporting no
    // errors at all.
    //
    // Said here rather than in the graph because this is where the spans are.
    // The file that lost is the one told, because it is the one that has to
    // move, and it is named after the file that has the module so that the
    // reader does not have to search for the other half.
    if let (Some(mine), Some(range)) = (map.module.as_ref(), map.module_range) {
        if let Some(owner) = graph.file(mine) {
            // **Unless the two are variants of one file for different
            // targets.** `socket_linux.kh`, `socket_macos.kh` and
            // `socket_windows.kh` all declare `std::net::socket` and exactly
            // one of them is ever compiled, which is the whole point of the
            // suffix. The command line filters them out before a root is
            // built, so this only arises where a tool hands the checker every
            // file there is -- and there it is the correct set, not a mistake.
            let target = khora_db::host_target();
            let both_apply = khora_db::selected_for_target(file.path(db), target)
                && khora_db::selected_for_target(owner.path(db), target);
            if owner != file && both_apply {
                out.errors.push(HirError {
                    message: format!(
                        "module `{mine}` is already declared in `{}`; a module \
                         is one file, and a name defined in both resolves to \
                         whichever was read last. Give this file a module of \
                         its own",
                        owner.path(db).display()
                    ),
                    range,
                });
            }
        }
    }

    let cycles = import_cycles(db, root);

    for import in &map.imports {
        let Some(source) = graph.file(&import.path) else {
            out.errors.push(HirError {
                message: format!("cannot find module `{}`", import.path),
                range: import.range,
            });
            continue;
        };
        // **A cycle is refused here, and that is what keeps it from being a
        // crash.** Dropping the import rather than resolving it means
        // `import_types` never asks the other module for its `type_map`,
        // which is where the recursion was. Errata 55.
        if let Some(mine) = map.module.as_ref() {
            if let Some(path) = cycles.through(mine) {
                if path.contains(&import.path) {
                    let drawn: Vec<String> =
                        path.iter().map(ModulePath::to_string).collect();
                    out.errors.push(HirError {
                        message: format!(
                            "`{}` and `{}` import each other: {}. Move what they \
                             share into a module they can both import",
                            mine,
                            import.path,
                            drawn.join(" -> ")
                        ),
                        range: import.range,
                    });
                    continue;
                }
            }
        }
        let exported = module_api(db, source);

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
                        // **A builtin is already in scope, so importing it
                        // is a harmless no-op rather than a mistake.** The
                        // generated API pages carry `### String` and `### Int`
                        // sections under `std::core` and spell their functions
                        // `String::join`, `Int::of_string` -- so a reader
                        // building an import list from those headings writes
                        // exactly this, and was told the module does not
                        // declare them. It failed four files at once on a
                        // first program, and nothing anywhere said where the
                        // line between "importable" and "always there" falls.
                        None if is_builtin_type(&wanted.name) => {}
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
    // **A derived `Ord` returns an `Ordering`, and nobody wrote that down.**
    //
    // `pub type Point = { .. } impl Ord;` expands to a `cmp` whose arms are
    // `Ordering::Less` and friends, so without this the author is told that
    // `Ordering::Less` is not a constructor — about a line they did not write,
    // naming a type they never mentioned. The clause is the mention.
    //
    // Taken from wherever `Ord` itself came from rather than from `std::core`
    // by name, so a program that defines its own comparison hierarchy gets its
    // own answer type. Nothing happens if `Ordering` is already in scope, or
    // if `Ord` is not.
    if derives_ord(db, file) && !out.names.iter().any(|(n, _)| n == "Ordering") {
        if let Some(home) =
            out.origins.iter().find(|o| o.local == "Ord").map(|o| o.module.clone())
        {
            if let Some(source) = graph.file(&home) {
                let exported = module_api(db, source);
                if let Some(item) = exported.item("Ordering").filter(|i| i.is_public) {
                    out.names.push((
                        "Ordering".to_string(),
                        Resolution::Item {
                            module: home.clone(),
                            name: "Ordering".to_string(),
                            kind: item.kind,
                        },
                    ));
                    out.origins.push(Origin {
                        local: "Ordering".to_string(),
                        module: home,
                        name: "Ordering".to_string(),
                        kind: item.kind,
                    });
                    out.variants
                        .extend(exported.variants_of("Ordering").cloned().collect::<Vec<_>>());
                }
            }
        }
    }

    // A derived schema reaches everything as `Schema::..`, `Fields::..`,
    // `Raw::..`, `List::..` and `Decode::schema()`, never by a bare name: a
    // bare name resolves to the deriving file's own item before an imported
    // one, so a companion called `field` would be captured, silently, by a
    // function of that name in the file. Three type names cannot be.
    if derives_trait(db, file, "Decode") {
        bring_derive_companions(db, graph, file, &mut out, "Decode", &["Schema", "Fields", "List"]);
    }
    if derives_trait(db, file, "Encode") {
        bring_derive_companions(db, graph, file, &mut out, "Encode", &["Raw", "List"]);
    }
    // `struct({ .. })` is rewritten into `Schema::record` over `Fields`, so a
    // file that imported `struct` needs both names, under whatever it called
    // `struct`. See `body/schema.rs`.
    if let Some(home) = out
        .origins
        .iter()
        .find(|o| o.name == "struct" && o.module.segments() == ["std", "schema"])
        .map(|o| o.module.clone())
    {
        bring_companions_from(db, graph, file, &mut out, home, &["Schema", "Fields"]);
    }

    out
}

/// Brings the names a source-expanded derive writes into the deriving file.
///
/// Same reasoning as `Ordering` above, and the same rule about where to look:
/// from the module that declares the *trait*, so a program with its own
/// `Decode` gets its own helpers rather than `std::schema`'s.
///
/// Two places are searched there, because a generated body borrows its home
/// module's whole vocabulary rather than only what that module declared:
/// `Schema` and `Fields` are `std::schema`'s own, while `List` is one
/// `std::schema` itself imported and a generated `List::Cons` chain needs it
/// just as much. Looking through the home module's scope is what reaches the
/// second kind without naming `std::core` here.
///
/// Nothing is brought that the file already has, so an author who imported
/// `List` under an alias keeps their spelling and their alias.
fn bring_derive_companions(
    db: &dyn Db,
    graph: &ModuleGraph,
    file: SourceFile,
    scope: &mut FileScope,
    trait_name: &str,
    wanted: &[&str],
) {
    let Some(home) =
        scope.origins.iter().find(|o| o.local == trait_name).map(|o| o.module.clone())
    else {
        return;
    };
    bring_companions_from(db, graph, file, scope, home, wanted);
}

/// The names `wanted`, as `home` declares or imports them, brought into
/// `scope` unless the file already has them.
fn bring_companions_from(
    db: &dyn Db,
    graph: &ModuleGraph,
    file: SourceFile,
    scope: &mut FileScope,
    home: ModulePath,
    wanted: &[&str],
) {
    let Some(source) = graph.file(&home) else { return };
    // A module deriving a trait it declares itself already has every name in
    // hand, and asking for its own scope here would be a query cycle.
    if source == file {
        return;
    }

    let declared = module_api(db, source);
    // `file_scope` rather than `item_map`, so a name the home module imported
    // is as reachable as one it wrote. Terminates because the branch above
    // stops a module from asking about itself.
    let inherited = file_scope(db, source);

    for name in wanted {
        if scope.names.iter().any(|(present, _)| present == name) {
            continue;
        }

        if let Some(item) = declared.item(name).filter(|item| item.is_public) {
            let resolution = Resolution::Item {
                module: home.clone(),
                name: (*name).to_string(),
                kind: item.kind,
            };
            scope.names.push(((*name).to_string(), resolution));
            scope.origins.push(Origin {
                local: (*name).to_string(),
                module: home.clone(),
                name: (*name).to_string(),
                kind: item.kind,
            });
            if item.kind == ItemKind::Type {
                scope.variants.extend(declared.variants_of(name).cloned());
            }
            continue;
        }

        // Not declared there, so it was imported there. Carry the resolution
        // across unchanged: it already names the module that defines the item.
        if let Some((_, resolution)) =
            inherited.names.iter().find(|(present, _)| present == name)
        {
            scope.names.push(((*name).to_string(), resolution.clone()));
            if let Some(origin) = inherited.origins.iter().find(|o| &o.local == name) {
                scope.origins.push(origin.clone());
            }
            scope.variants.extend(
                inherited.variants.iter().filter(|v| &v.type_name == name).cloned(),
            );
        }
    }
}

/// Whether any type in this file asks for a derived `Ord`.
///
/// Read from the syntax rather than from `ItemMap`, which records what a file
/// declares and not what each declaration asked to have written for it. One
/// walk of the declarations, and only when a scope is being built.
fn derives_ord(db: &dyn Db, file: SourceFile) -> bool {
    derives_trait(db, file, "Ord")
}

fn derives_trait(db: &dyn Db, file: SourceFile, wanted: &str) -> bool {
    khora_db::parse(db, file).source_file().decls().any(|decl| {
        let ast::Decl::Type(t) = decl else { return false };
        t.derive_clause().is_some_and(|c| {
            c.traits().filter_map(|n| n.ident()).any(|name| name == wanted)
        })
    })
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
        // **A constructor named after its own type**, which is what
        // `type UserId = Int;` has. `UserId(7)` is one segment, because there
        // is no case to name apart from the type itself, and `UserId::UserId`
        // would be true and unwritable.
        //
        // Before the item lookup, because the item is the *type* and a type is
        // not a value: a bare `UserId` in expression position can only be the
        // constructor. Only a newtype has one of these — a record declares no
        // case, and a variant's cases have names of their own.
        if let Some(variant) = map.variants_of(first).find(|v| v.name == *first) {
            return Ok(Resolution::Variant {
                module: map.module.clone().unwrap_or_else(|| ModulePath::new(vec![])),
                type_name: variant.type_name.clone(),
                name: variant.name.clone(),
            });
        }
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
        if matches!(local.kind, ItemKind::Const | ItemKind::Function) {
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

/// Whether `name` is a type the language provides rather than a module does.
///
/// **Kept in step with `khora_types::syntax::named_type` by a test, not by
/// hope.** That function is the authority on which names mean a builtin, and
/// this crate cannot call it -- `khora-types` depends on this one, so the
/// dependency only runs the other way, for tests. `builtin_names_agree` over
/// there fails if the two ever drift.
pub fn is_builtin_type(name: &str) -> bool {
    if matches!(name, "Int" | "Float" | "Bool" | "Char" | "String" | "Ptr" | "Never") {
        return true;
    }
    // `I8` through `U64`, and `I64`, which is `Int` spelled the other way.
    // Taken a character at a time rather than `split_at(1)`, which panics on
    // the empty name -- and the empty name reaches here, because a malformed
    // import is still an import.
    let mut letters = name.chars();
    match (letters.next(), letters.as_str()) {
        (Some('I' | 'U'), bits) => matches!(bits, "8" | "16" | "32" | "64"),
        _ => false,
    }
}
