//! Which specialisations a program needs.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_types::mono::{instances, Instance};
use khora_types::Type;

fn symbols(db: &dyn Db, text: &str) -> Vec<String> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    let mut names: Vec<String> =
        instances(db, file).instances.iter().map(|(i, _)| i.symbol()).collect();
    names.sort();
    names
}

fn errors(db: &dyn Db, text: &str) -> Vec<String> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    instances(db, file).errors.iter().map(|e| e.message.clone()).collect()
}

/// A non-generic function keeps its own name, so nothing about the common case
/// changes because monomorphisation exists.
#[test]
fn concrete_functions_keep_their_names() {
    let db = KhoraDatabase::new();
    let found = symbols(&db, "module m;\nfn a() -> Int { 1 }\nfn b() -> Int { 2 }\n");
    assert_eq!(found, vec!["m$a", "m$b"]);
}

#[test]
fn a_generic_function_is_specialised_per_type_used() {
    let db = KhoraDatabase::new();
    let found = symbols(
        &db,
        "module m;\n\
         fn id<A>(x: A) -> A { x }\n\
         fn use_int() -> Int { id(1) }\n\
         fn use_bool() -> Bool { id(true) }\n",
    );
    assert_eq!(found, vec!["m$id$Bool", "m$id$Int", "m$use_bool", "m$use_int"]);
}

#[test]
fn the_same_instantiation_twice_is_emitted_once() {
    let db = KhoraDatabase::new();
    let found = symbols(
        &db,
        "module m;\n\
         fn id<A>(x: A) -> A { x }\n\
         fn f() -> Int { id(1) }\n\
         fn g() -> Int { id(2) }\n",
    );
    assert_eq!(found, vec!["m$f", "m$g", "m$id$Int"], "one instance should serve both calls");
}

/// A shape nobody asked for has nothing to emit.
#[test]
fn an_unused_generic_function_produces_no_instance() {
    let db = KhoraDatabase::new();
    let found = symbols(&db, "module m;\nfn id<A>(x: A) -> A { x }\nfn main() -> Int { 0 }\n");
    assert_eq!(found, vec!["m$main"]);
}

/// Walking a generic body must substitute the instance's own arguments first,
/// or the inner call would ask for `id@A` instead of `id@Int`.
#[test]
fn a_generic_calling_a_generic_resolves_through_the_outer_instance() {
    let db = KhoraDatabase::new();
    let found = symbols(
        &db,
        "module m;\n\
         fn id<A>(x: A) -> A { x }\n\
         fn twice<B>(x: B) -> B { id(id(x)) }\n\
         fn f() -> Int { twice(1) }\n",
    );
    assert_eq!(found, vec!["m$f", "m$id$Int", "m$twice$Int"]);
}

#[test]
fn generic_types_appear_in_the_symbol() {
    let db = KhoraDatabase::new();
    let found = symbols(
        &db,
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         fn id<A>(x: A) -> A { x }\n\
         fn f(o: Option<Int>) -> Option<Int> { id(o) }\n",
    );
    assert_eq!(found, vec!["m$f", "m$id$Option$Int"]);
}

#[test]
fn instances_are_ordered_deterministically() {
    let db = KhoraDatabase::new();
    let source = "module m;\n\
                  fn id<A>(x: A) -> A { x }\n\
                  fn a() -> Bool { id(true) }\n\
                  fn b() -> Int { id(1) }\n";
    assert_eq!(symbols(&db, source), symbols(&db, source), "output must not vary run to run");
}

/// A generic function calling itself at a *larger* type has no finite set of
/// instances. It must be reported, not looped on.
#[test]
fn polymorphic_recursion_is_reported_rather_than_hanging() {
    let db = KhoraDatabase::new();
    let found = errors(
        &db,
        "module m;\n\
         export type Wrap<A> = | Of(value: A);\n\
         fn grow<A>(x: A) -> Int { grow(Wrap::Of(x)) }\n\
         fn main() -> Int { grow(1) }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("endlessly many specialisations")),
        "expected a diagnostic, got {found:?}"
    );
}

#[test]
fn an_instance_carries_types_with_its_arguments_substituted() {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(
        &db,
        "a.kh".into(),
        "module m;\nfn id<A>(x: A) -> A { x }\nfn f() -> Int { id(1) }\n".to_string(),
    );

    let all = instances(&db, file);
    let wanted = Instance {
        module: khora_hir::ModulePath::new(vec!["m".to_string()]),
        function: "id".to_string(),
        args: vec![Type::Int],
    };
    let types = all.get(&wanted).expect("no id$Int instance");

    // Every type in the specialised body must be concrete: a rigid parameter
    // surviving to here is exactly what code generation cannot represent.
    let body = khora_hir::body::bodies(&db, file)
        .iter()
        .find(|(n, _)| n == "id")
        .map(|(_, b)| b.clone())
        .expect("no body for id");

    for (id, _) in body.exprs() {
        assert!(
            !matches!(types.of(id), Type::Param(_)),
            "a rigid parameter survived specialisation: {:?}",
            types.of(id)
        );
    }
}
