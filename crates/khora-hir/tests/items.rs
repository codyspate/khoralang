//! Item collection, the module graph and name resolution.

use khora_db::{Db, KhoraDatabase, Setter, SourceFile, SourceRoot};
use khora_hir::{item_map, module_graph, resolve_path, ItemKind, ModulePath, Resolution};

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
         export type Risk = | Low | High(reason: String);\n\
         export effect Ledger { record: Int -> () }\n\
         export fn analyze() -> Int { 1 }\n\
         fn private_helper() -> Int { 2 }\n\
         let cache = 1;\n",
    );

    let map = item_map(&db, f);
    assert_eq!(map.module, Some(ModulePath::new(path(&["app", "core"]))));
    assert!(map.errors.is_empty(), "{:?}", map.errors);

    assert_eq!(map.item("Risk").unwrap().kind, ItemKind::Type);
    assert_eq!(map.item("Ledger").unwrap().kind, ItemKind::Effect);
    assert_eq!(map.item("analyze").unwrap().kind, ItemKind::Function);
    assert_eq!(map.item("cache").unwrap().kind, ItemKind::Let);

    assert!(map.item("analyze").unwrap().is_public);
    assert!(!map.item("private_helper").unwrap().is_public, "visibility not tracked");
}

/// Constructors are reached through their type, so they are recorded against it
/// rather than as free items.
#[test]
fn variant_constructors_belong_to_their_type() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\nexport type Risk = | Low | High(reason: String);\n");
    let map = item_map(&db, f);

    let names: Vec<_> = map.variants_of("Risk").map(|v| v.name.clone()).collect();
    assert_eq!(names, vec!["Low", "High"]);
    assert!(map.item("Low").is_none(), "a constructor should not be a free item");
}

#[test]
fn a_missing_module_declaration_is_an_error() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "export fn f() -> Int { 1 }\n");
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
    let core = file(&db, "core.kh", "module std::core;\nexport type Option = | Some | None;\n");
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

#[test]
fn resolves_a_qualified_path_into_another_module() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\nexport fn identity() -> Int { 1 }\n");
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
    let f = file(&db, "a.kh", "module m;\nexport type Risk = | Low | High(reason: String);\n");
    let root = SourceRoot::new(&db, vec![f]);

    let res = resolve_path(&db, root, f, &path(&["Risk", "High"])).unwrap();
    assert!(
        matches!(res, Resolution::Variant { ref type_name, ref name, .. }
            if type_name == "Risk" && name == "High"),
        "{res:?}"
    );
}

/// Case 3ii of the resolution rule. It must say so rather than guess, so the
/// rule stays whole when typeclasses arrive in phase 3.
#[test]
fn an_associated_item_reports_that_it_is_unsupported() {
    let db = KhoraDatabase::new();
    let f = file(&db, "a.kh", "module m;\nexport type Risk = | Low;\n");
    let root = SourceRoot::new(&db, vec![f]);

    let res = resolve_path(&db, root, f, &path(&["Risk", "from_str"])).unwrap();
    assert!(
        matches!(res, Resolution::Unsupported(msg) if msg.contains("typeclasses")),
        "{res:?}"
    );
}

#[test]
fn a_named_import_brings_a_public_item_into_scope() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\nexport fn identity() -> Int { 1 }\n");
    let app = file(&db, "app.kh", "module app::main;\nimport std::core::{identity};\n");
    let root = SourceRoot::new(&db, vec![core, app]);

    let res = resolve_path(&db, root, app, &path(&["identity"])).unwrap();
    assert!(matches!(res, Resolution::Item { ref name, .. } if name == "identity"), "{res:?}");
}

#[test]
fn an_aliased_import_resolves_under_its_local_name() {
    let db = KhoraDatabase::new();
    let core = file(&db, "core.kh", "module std::core;\nexport fn identity() -> Int { 1 }\n");
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
    let core = file(&db, "core.kh", "module std::core;\nexport fn identity() -> Int { 1 }\n");
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
let xs = 1;
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
    let a = file(&db, "a.kh", "module a;\nexport fn f() -> Int { 1 }\n");
    let b = file(&db, "b.kh", "module b;\nexport fn g() -> Int { 2 }\n");

    item_map(&db, a);
    item_map(&db, b);
    log.take();

    b.set_text(&mut db).to("module b;\nexport fn g() -> Int { 99 }\n".to_string());

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
