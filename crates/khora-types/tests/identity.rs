//! What a name means, and what it does not.
//!
//! A [`khora_types::Type::Adt`] carries a bare `String`, so the compiler tells
//! `Array` from `Array` by spelling and nothing else. That was not a missing
//! check: a user type called `Array` was handed the runtime's array layout, and
//! dropping one read whatever the first field happened to contain as an element
//! width and aborted the process. Roadmap 8.5.2, errata 46.
//!
//! These pin the guard rather than the fix. The fix is for a type to carry the
//! declaration it resolved to; until then a *definition* of one of those names
//! is refused, and `two_modules_that_declare_one_name_still_collide` records
//! what is still wrong so it cannot be quietly forgotten.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "main.kh".into(), text.to_string());
    SourceRoot::new(&db, vec![file]);
    khora_types::diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

/// The diagnostics of `user`, with `library` beside it in one program.
fn errors_in_user(library: &str, user: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let library = SourceFile::new(&db, "library.kh".into(), library.to_string());
    let user = SourceFile::new(&db, "user.kh".into(), user.to_string());
    SourceRoot::new(&db, vec![library, user]);
    khora_types::diagnostics(&db, user).iter().map(|e| e.message.clone()).collect()
}

fn refuses(declaration: &str) -> Vec<String> {
    errors(&format!("module app;\n{declaration}\nfn main() -> Int {{ 0 }}\n"))
}

/// Each of these is a declaration the backend hands an intrinsic to, or a name
/// `named_type` answers before it consults the file at all.
#[test]
fn a_lookalike_declaration_is_refused_by_name() {
    for declaration in [
        "type Array<A> = | Empty;",
        "type Fiber = | Running;",
        "type SharedFn = | Certified;",
        "type Shared<A> = | Cell(A);",
        "type Fibers = | Crew;",
        "type String = | Text;",
        "type Int = | Whole;",
        "type Float = | Real;",
        "type Bool = | Yes;",
        "type Ptr = | Address;",
    ] {
        let found = refuses(declaration);
        assert!(
            found.iter().any(|m| m.contains("already means")),
            "`{declaration}` was accepted; the compiler would give it the built-in's \
             layout. Got {found:?}"
        );
    }
}

/// The specific crash this exists to prevent. A `type Array` reached the
/// backend, was laid out as the runtime's array, and aborted inside
/// `khora_drop` with "a counted element is a pointer, so it is always a whole
/// word wide" — a message about the runtime, from a program that never
/// mentioned an array.
#[test]
fn a_user_array_no_longer_reaches_the_backend() {
    let found = refuses("type Array = { label: String };");
    assert_eq!(found.len(), 1, "one diagnostic, and it is about the name: {found:?}");
    assert!(found[0].contains("`Array`"), "{}", found[0]);
    assert!(found[0].contains("Rename it"), "the message should say what to do: {}", found[0]);
}

/// **Naming the builtin is not defining it.** `export type Array<A>;` with no
/// right-hand side is what `std::core` writes, and what every backend test
/// writes to reach an array without importing the standard library. It claims
/// nothing the compiler does not already provide, so it is not a collision —
/// which is why the rule needs no exemption for `std`.
#[test]
fn declaring_the_builtin_without_a_definition_is_fine() {
    for declaration in [
        "export type Array<A>;",
        "export type Fiber;",
        "export type Fibers;",
        "export type Shared<A>;",
        "export trait Share {}",
    ] {
        let found = refuses(declaration);
        assert!(
            !found.iter().any(|m| m.contains("already means")),
            "`{declaration}` names the built-in and should be allowed: {found:?}"
        );
    }
}

/// A name that is merely *similar* is nobody's business but the author's.
#[test]
fn an_ordinary_name_is_left_alone() {
    for declaration in
        ["type ArrayList = | Empty;", "type Sharing = | Yes;", "trait Shareable {}"]
    {
        let found = refuses(declaration);
        assert!(
            !found.iter().any(|m| m.contains("already means")),
            "`{declaration}` is not a compiler name and should be allowed: {found:?}"
        );
    }
}

/// **Still broken, and recorded so it is not mistaken for fixed.**
///
/// Two modules that each declare a `Point` are one type to the checker, so the
/// importer's fields are the wrong ones. This is the same defect the guard
/// above works around for the handful of names the compiler already means, and the reason the guard is a
/// guard: identity is a name, and a name is not unique.
///
/// When a type carries the declaration it resolved to, this test starts failing
/// and should be inverted — the `label` field will be found and the program
/// will check.
#[test]
fn two_modules_that_declare_one_name_still_collide() {
    let found = errors_in_user(
        "module library;\n\
         export type Point = { label: String };\n\
         export fn make() -> Point { { label: \"theirs\" } }\n",
        "module app;\n\
         import library::{make};\n\
         type Point = { x: Int };\n\
         fn read() -> String { make().label }\n\
         fn main() -> Int { 0 }\n",
    );
    assert!(
        found.iter().any(|m| m.contains("has no field `label`")),
        "this collision appears to be fixed — invert this test and delete the guard \
         it justifies. Got {found:?}"
    );
}
