//! Record types, literals and field access.
//!
//! Records exist because effects need them: a capability is a record of
//! closures, and `ledger.get_history(id)` is a field read followed by a call.
//! `docs/design/effects.md` says the dependency-injection model is "a record of
//! function types", so this is that record.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

const POINT: &str = "module m;\nexport type Point = { x: Int, y: Int };\n";

#[test]
fn a_field_has_the_type_it_was_declared_with() {
    assert_clean(&format!("{POINT}fn f(p: Point) -> Int {{ p.x + p.y }}\n"));
    assert_reports(&format!("{POINT}fn f(p: Point) -> Bool {{ p.x }}\n"), "returns `Bool`");
}

#[test]
fn a_field_that_does_not_exist_is_reported() {
    assert_reports(&format!("{POINT}fn f(p: Point) -> Int {{ p.z }}\n"), "`Point` has no field `z`");
}

/// A literal is nominal, like everything else: it is *some declared record*,
/// found by its labels rather than being a structural type of its own.
#[test]
fn a_literal_is_found_by_its_labels() {
    assert_clean(&format!("{POINT}fn f() -> Point {{ {{ x: 1, y: 2 }} }}\n"));
}

/// Order written need not match order declared.
#[test]
fn the_order_written_does_not_matter() {
    assert_clean(&format!("{POINT}fn f() -> Point {{ {{ y: 2, x: 1 }} }}\n"));
}

#[test]
fn a_field_of_the_wrong_type_is_reported() {
    assert_reports(
        &format!("{POINT}fn f() -> Point {{ {{ x: true, y: 2 }} }}\n"),
        "field `x`: expected `Int`, found `Bool`",
    );
}

#[test]
fn a_missing_field_is_reported() {
    assert_reports(
        &format!("{POINT}fn f() -> Point {{ {{ x: 1 }} }}\n"),
        "this `Point` is missing `y`",
    );
}

#[test]
fn a_field_the_record_does_not_have_is_reported() {
    assert_reports(
        &format!("{POINT}fn f() -> Point {{ {{ x: 1, y: 2, z: 3 }} }}\n"),
        "no record type has exactly the fields",
    );
}

/// Two records with the same labels cannot be told apart from the literal
/// alone, and saying so beats guessing.
#[test]
fn an_ambiguous_literal_asks_which_type_it_is() {
    assert_reports(
        "module m;\n\
         export type A = { v: Int };\n\
         export type B = { v: Int };\n\
         fn f() -> A { { v: 1 } }\n",
        "say which",
    );
}

/// A record's fields are declared against its own parameters, so a literal
/// decides them.
#[test]
fn a_generic_record_takes_its_argument_from_the_literal() {
    assert_clean(
        "module m;\n\
         export type Wrapper<A> = { value: A };\n\
         fn f() -> Wrapper<Int> { { value: 1 } }\n\
         fn g(w: Wrapper<Bool>) -> Bool { w.value }\n",
    );
    assert_reports(
        "module m;\n\
         export type Wrapper<A> = { value: A };\n\
         fn f() -> Wrapper<Bool> { { value: 1 } }\n",
        "returns `Wrapper<Bool>`",
    );
}

/// A sum type's fields are not reachable without matching: which variant is
/// present is not known until it is.
#[test]
fn a_sum_types_payload_is_not_a_field() {
    assert_reports(
        "module m;\n\
         export type Shape = | Circle(radius: Int) | Square(side: Int);\n\
         fn f(s: Shape) -> Int { s.radius }\n",
        "has no field `radius`",
    );
}

/// The shape effects need: a record whose fields are functions, read and
/// called.
#[test]
fn a_record_of_functions_is_callable_through_its_fields() {
    assert_clean(
        "module m;\n\
         export type Ledger = { get: (Int) -> Int, flag: (Int) -> Bool };\n\
         fn use_it(l: Ledger) -> Int { l.get(1) }\n\
         fn make() -> Ledger { { get: fn i => i + 1, flag: fn i => i == 0 } }\n",
    );
}
