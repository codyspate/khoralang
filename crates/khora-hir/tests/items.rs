//! Item collection, the module graph and name resolution.

use khora_db::{Db, KhoraDatabase, Setter, SourceFile, SourceRoot};
use khora_hir::{
    file_scope, item_map, module_graph, resolve_path, ItemKind, ModulePath, Resolution,
};

fn file(db: &dyn Db, name: &str, text: &str) -> SourceFile {
    SourceFile::new(db, name.into(), text.to_string())
}

fn path(segments: &[&str]) -> Vec<String> {
    segments.iter().map(|s| s.to_string()).collect()
}

#[test]
fn collects_every_kind_of_declaration() {
    let db = KhoraDatabase::new();
    let f = file(
        &db,
        "a.kh",
        "module app::core;\n\
         pub type Risk = | Low | High(reason: String);\n\
         pub effect Ledger { record: Int -> () }\n\
         pub fn analyze() -> Int { 1 }\n\
         fn private_helper() -> Int { 2 }\n\
         const cache = 1;\n\
         pub const shared_cache = 2;\n",
    );

    let map = item_map(&db, f);
    assert_eq!(map.module, Some(ModulePath::new(path(&["app", "core"]))));
    assert!(map.errors.is_empty(), "{:?}", map.errors);

    assert_eq!(map.item("Risk").unwrap().kind, ItemKind::Type);
    assert_eq!(map.item("Ledger").unwrap().kind, ItemKind::Effect);
    assert_eq!(map.item("analyze").unwrap().kind, ItemKind::Function);
    assert_eq!(map.item("cache").unwrap().kind, ItemKind::Const);
    assert_eq!(map.item("shared_cache").unwrap().kind, ItemKind::Const);

    assert!(map.item("analyze").unwrap().is_public);
    assert!(!map.item("private_helper").unwrap().is_public, "visibility not tracked");
    // A constant is exported like anything else. This used to be hard-coded to
    // private, so `pub const` parsed and then did not export.
    assert!(map.item("shared_cache").unwrap().is_public);
    assert!(!map.item("cache").unwrap().is_public);
}

/// Constructors are reached through their type, so they are recorded against it
/// rather than as free items.
#[test]
fn variant_constructors_belong_to_their_type() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\npub type Risk = | Low | High(reason: String);\n");
    let map = item_map(&db, f);

    let names: Vec<_> = map.variants_of("Risk").map(|v| v.name.clone()).collect();
    assert_eq!(names, vec!["Low", "High"]);
    assert!(map.item("Low").is_none(), "a constructor should not be a free item");
}

#[test]
fn a_missing_module_declaration_is_an_error() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "pub fn f() -> Int { 1 }\n");
    let map = item_map(&db, f);
    assert!(
        map.errors.iter().any(|e| e.message.contains("must begin with a `module`")),
        "{:?}",
        map.errors
    );
}

#[test]
fn a_duplicate_definition_names_both_places() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\nfn dup() -> Int { 1 }\nfn dup() -> Int { 2 }\n");
    let map = item_map(&db, f);

    let msg = map
        .errors
        .iter()
        .find(|e| e.message.contains("defined twice"))
        .map(|e| e.message.clone())
        .unwrap_or_default();
    assert!(msg.contains("dup"), "duplicate not reported: {:?}", map.errors);
    assert!(msg.contains("first is at"), "message does not locate the original: {msg}");
}

#[test]
fn the_module_graph_maps_paths_to_files() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\npub type Option = | Some | None;\n");
    let app = file(&db, "app.kh", "module app::main;\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    let graph = module_graph(&db, root);
    assert_eq!(graph.len(), 2);
    assert_eq!(graph.file(&ModulePath::new(path(&["std", "core"]))), Some(core));
    assert!(graph.errors.is_empty(), "{:?}", graph.errors);
}

#[test]
fn declaring_one_module_in_two_files_is_an_error() {
    let db = KhoraDatabase::new();
    let a = file(&db, "a.kh", "module app::main;\n");
    let b = file(&db, "b.kh", "module app::main;\n");
    let root = SourceRoot::new(&db, vec![a, b]);

    let graph = module_graph(&db, root);
    assert!(
        graph.errors.iter().any(|e| e.message.contains("more than one file")),
        "{:?}",
        graph.errors
    );
}

/// **And somebody is told about it**, which is the half that was missing.
///
/// The test above passed for as long as the check has existed, and nothing
/// ever read `graph.errors` -- so two files declaring one module compiled with
/// `khora check` reporting nothing at all. Since the graph keeps the first
/// file to claim a path and drops the rest, while everything downstream walks
/// the whole file list, `fn shared_name` in both files of `module main` both
/// compiled and the later one won. Adding a helper whose name already existed
/// in a sibling silently changed what the program did.
///
/// A test that asserts a diagnostic is *produced* is not the same as one that
/// asserts it is *reported*, and this pair is the difference.
#[test]
fn the_second_file_to_claim_a_module_is_told_where_the_first_is() {
    let db = KhoraDatabase::new();
    let a = file(&db, "a.kh", "module app::main;\n");
    let b = file(&db, "b.kh", "module app::main;\nfn helper() -> Int { 2 }\n");
    // A singleton: creating it is how the compilation learns its file set.
    let _root = SourceRoot::new(&db, vec![a, b]);

    // The file that lost is the one told, because it is the one that moves.
    let scope = file_scope(&db, b);
    let found = scope
        .errors
        .iter()
        .find(|e| e.message.contains("already declared"))
        .unwrap_or_else(|| panic!("{:?}", scope.errors));

    // Named after the other half, so the reader does not have to go looking.
    assert!(found.message.contains("a.kh"), "{}", found.message);
    // And pointed at the `module` line rather than at byte zero of nothing.
    assert!(!found.range.is_empty(), "the span must be the declaration");

    // The file that got there first has nothing to answer for.
    let winner = file_scope(&db, a);
    assert!(
        !winner.errors.iter().any(|e| e.message.contains("already declared")),
        "{:?}",
        winner.errors
    );
}

#[test]
fn resolves_a_qualified_path_into_another_module() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\npub fn identity() -> Int { 1 }\n");
    let app = file(&db, "app.kh", "module app::main;\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    let res = resolve_path(&db, root, app, &path(&["std", "core", "identity"])).unwrap();
    assert_eq!(
        res,
        Resolution::Item {
            module: ModulePath::new(path(&["std", "core"])),
            name: "identity".to_string(),
            kind: ItemKind::Function,
        }
    );
}

#[test]
fn a_private_item_is_not_reachable_from_another_module() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\nfn secret() -> Int { 1 }\n");
    let app = file(&db, "app.kh", "module app::main;\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    let err = resolve_path(&db, root, app, &path(&["std", "core", "secret"])).unwrap_err();
    assert!(err.message.contains("private"), "{}", err.message);
}

#[test]
fn resolves_a_constructor_through_its_type() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\npub type Risk = | Low | High(reason: String);\n");
    let root = SourceRoot::new(&db, vec![f]);

    let res = resolve_path(&db, root, f, &path(&["Risk", "High"])).unwrap();
    assert!(
        matches!(res, Resolution::Variant { ref type_name, ref name, .. }
            if type_name == "Risk" && name == "High"),
        "{res:?}"
    );
}

/// Case 3ii of the resolution rule. Naming a type and then something that is
/// not one of its constructors is a function it declares for itself, and
/// *which* function is the checker's question — the resolver only has to say
/// what the owner was.
#[test]
fn a_type_s_own_function_resolves_against_the_type() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\npub type Risk = | Low;\n");
    let root = SourceRoot::new(&db, vec![f]);

    let res = resolve_path(&db, root, f, &path(&["Risk", "from_str"])).unwrap();
    assert!(
        matches!(res, Resolution::TraitItem { ref owner, ref name }
            if owner == "Risk" && name == "from_str"),
        "{res:?}"
    );
}

#[test]
fn a_named_import_brings_a_public_item_into_scope() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\npub fn identity() -> Int { 1 }\n");
    let app = file(&db, "app.kh", "module app::main;\nimport std::core::{identity};\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    let res = resolve_path(&db, root, app, &path(&["identity"])).unwrap();
    assert!(matches!(res, Resolution::Item { ref name, .. } if name == "identity"), "{res:?}");
}

#[test]
fn an_aliased_import_resolves_under_its_local_name() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\npub fn identity() -> Int { 1 }\n");
    let app = file(&db, "app.kh", "module app::main;\nimport std::core::{identity as id};\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    assert!(resolve_path(&db, root, app, &path(&["id"])).is_ok(), "alias not honoured");
    assert!(
        resolve_path(&db, root, app, &path(&["identity"])).is_err(),
        "the original name should not also be in scope"
    );
}

#[test]
fn a_glob_import_brings_public_items_into_scope() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\npub fn identity() -> Int { 1 }\n");
    let app = file(&db, "app.kh", "module app::main;\nimport std::core::*;\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    assert!(resolve_path(&db, root, app, &path(&["identity"])).is_ok());
}

/// `docs/design/associated-items.md`: locals do not participate in a `::` path,
/// and the diagnostic should say that rather than report a missing item.
#[test]
fn a_local_in_a_path_is_a_category_error() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;
const xs = 1;
");
    let root = SourceRoot::new(&db, vec![f]);

    let err = resolve_path(&db, root, f, &path(&["xs", "map"])).unwrap_err();
    assert!(err.message.contains("not a module or a type"), "{}", err.message);
    assert!(err.message.contains("use `.`"), "should point at the right operator: {}", err.message);
}

#[test]
fn an_unknown_name_is_reported() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\n");
    let root = SourceRoot::new(&db, vec![f]);

    let err = resolve_path(&db, root, f, &path(&["nonexistent"])).unwrap_err();
    assert!(err.message.contains("cannot find `nonexistent`"), "{}", err.message);
}

/// Collecting items for one file must not read any other, or an edit anywhere
/// would invalidate everything.
#[test]
fn editing_one_file_does_not_recollect_another() {
    let (mut db, log) = KhoraDatabase::logged();
    let a = file(&db, "a.kh", "module a;\npub fn f() -> Int { 1 }\n");
    let b = file(&db, "b.kh", "module b;\npub fn g() -> Int { 2 }\n");

    item_map(&db, a);
    item_map(&db, b);
    log.take();

    b.set_text(&mut db).to("module b;\npub fn g() -> Int { 99 }\n".to_string());

    item_map(&db, a);
    item_map(&db, b);

    let executed = log.take();
    // b reparses and recollects; a does neither.
    assert!(
        executed.len() <= 2,
        "editing b recomputed more than b's own queries: {executed:?}"
    );
    assert!(
        !executed.iter().any(|e| e.contains("a.kh")),
        "a was recomputed: {executed:?}"
    );
}
