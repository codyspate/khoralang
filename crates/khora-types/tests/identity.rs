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

/// **Naming the builtin is not defining it.** `pub type Array<A>;` with no
/// right-hand side is what `std::core` writes, and what every backend test
/// writes to reach an array without importing the standard library. It claims
/// nothing the compiler does not already provide, so it is not a collision —
/// which is why the rule needs no exemption for `std`.
#[test]
fn declaring_the_builtin_without_a_definition_is_fine() {
    for declaration in [
        "pub type Array<A>;",
        "pub type Fiber;",
        "pub type Fibers;",
        "pub type Shared<A>;",
        "pub trait Share {}",
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

/// **Two modules may each declare a `Point`, and they are two types.**
///
/// This asserted the opposite when it was written, because they were one: the
/// importer looked its fields up by spelling, found its own declaration, and
/// was told its value had no field `label`. Errata 46.
#[test]
fn two_modules_may_declare_one_name() {
    let found = errors_in_user(
        "module library;
         pub type Point = { label: String };
         pub fn make() -> Point { { label: \"theirs\" } }
",
        "module app;
         import library::{Point as Theirs, make};
         type Point = { x: Int };
         fn mine() -> Point { { x: 1 } }
         fn read() -> String { let it: Theirs = make(); it.label }
         fn count() -> Int { mine().x }
         fn main() -> Int { 0 }
",
    );
    assert!(found.is_empty(), "each `Point` should keep its own fields: {found:?}");
}

/// **Same name, same shape, different declaration — and they do not unify.**
///
/// The test above says each `Point` keeps its own fields, which is the half
/// that shows they work independently. This is the half that shows they are
/// *distinct*: give one where the other is wanted and it has to be an error.
///
/// Both directions are needed, and only having the first is how a nominal type
/// system quietly becomes a structural one. If identity were dropped tomorrow
/// the test above would still pass — two records of `{ label: String }` unify
/// perfectly well when they are secretly the same type. This one would not.
///
/// The shapes are identical on purpose. Anything else would be caught by field
/// mismatch and prove nothing about identity.
#[test]
fn two_declarations_of_one_shape_do_not_unify() {
    let found = errors_in_user(
        "module library;
         pub type Point = { label: String };
         pub fn make() -> Point { { label: \"theirs\" } }
         pub fn take(p: Point) -> String { p.label }
",
        "module app;
         import library::{make, take};
         pub type Point = { label: String };
         fn mine() -> Point { { label: \"mine\" } }
         // Theirs where mine is wanted.
         fn wrong_way() -> Point { make() }
         // Mine where theirs is wanted.
         fn other_way() -> String { take(mine()) }
         fn main() -> Int { 0 }
",
    );
    assert_eq!(
        found.len(),
        2,
        "each direction should be its own error, and neither should unify: {found:?}"
    );
    assert!(
        found.iter().all(|e| e.contains("Point")),
        "the diagnostics should name the type: {found:?}"
    );
}

/// The alias is a second spelling, not a second type.
///
/// It used to be both: the import was keyed under the local name, so `Theirs`
/// and `library::Point` would not unify and a rename invented a type.
#[test]
fn an_alias_is_the_type_it_renames() {
    let found = errors_in_user(
        "module library;
         pub type Point = { label: String };
         pub fn make() -> Point { { label: \"theirs\" } }
         pub fn take(p: Point) -> String { p.label }
",
        "module app;
         import library::{Point as Theirs, make, take};
         fn round(p: Theirs) -> String { take(p) }
         fn go() -> String { round(make()) }
         fn main() -> Int { 0 }
",
    );
    assert!(found.is_empty(), "an alias should unify with the type it names: {found:?}");
}

/// **A type has to be imported before its fields are.** Unchanged by any of
/// this, and worth pinning beside the two above so the difference is on the
/// record: a name in an annotation resolves whether or not the file imported
/// it, while its *fields* arrive only with the import. The diagnostic says so.
#[test]
fn using_a_type_without_importing_it_says_to_import_it() {
    let found = errors_in_user(
        "module library;
         pub type Point = { label: String };
         pub fn make() -> Point { { label: \"theirs\" } }
",
        "module app;
         import library::{make};
         fn read() -> String { make().label }
         fn main() -> Int { 0 }
",
    );
    assert!(
        found.iter().any(|m| m.contains("add it to an `import`")),
        "the fix should be named: {found:?}"
    );
}
