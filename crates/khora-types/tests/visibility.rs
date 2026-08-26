//! What an import brings with it.
//!
//! A `TypeMap` is per file, and what one file knows about another's
//! declarations is exactly what `import_types` copies across. Getting that set
//! wrong does not fail loudly — it produces a program that type checks in the
//! module that declared something and not in the module that uses it, under a
//! diagnostic about the wrong thing entirely. Both of these were found that
//! way.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The diagnostics of `main`, with `library` alongside it in the same program.
fn errors_in_user(library: &str, user: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let library = SourceFile::new(&db, "library.kh".into(), library.to_string());
    let user = SourceFile::new(&db, "user.kh".into(), user.to_string());
    SourceRoot::new(&db, vec![library, user]);
    khora_types::diagnostics(&db, user).iter().map(|e| e.message.clone()).collect()
}

const LIBRARY: &str = "module library;\n\
                       export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
                       export type Point = { x: Int };\n\
                       impl Eq for Point { fn eq(self, other: Point) -> Bool { self.x == other.x } }\n";

/// An impl written beside its type is visible wherever that type is.
///
/// It used to arrive only with its *trait*, so `impl Eq for Point` written in
/// the module declaring `Point` was invisible to a file importing `Point` —
/// even with `Eq` already in scope there, which is the ordinary shape of a
/// program. It made a derived impl useless outside the module it came from.
#[test]
fn an_impl_travels_with_its_type() {
    let found = errors_in_user(
        LIBRARY,
        "module user;\n\
         import library::{Point};\n\
         export fn same(a: Point, b: Point) -> Bool { Point::eq(a, b) }\n",
    );
    assert!(found.is_empty(), "the type was imported and its impl was not: {found:?}");
}

/// It still travels with its trait, which is how it always arrived.
#[test]
fn an_impl_travels_with_its_trait() {
    let found = errors_in_user(
        LIBRARY,
        "module user;\n\
         import library::{Eq};\n\
         export fn same<T: Eq>(a: T, b: T) -> Bool { a.eq(b) }\n",
    );
    assert!(found.is_empty(), "the trait was imported and its impls were not: {found:?}");
}

/// Arriving from both sides at once is still one impl.
///
/// Each side copied one in, and the coherence check then reported that the
/// trait was already implemented for the type — by itself.
#[test]
fn an_impl_imported_from_both_sides_is_not_a_duplicate() {
    let found = errors_in_user(
        LIBRARY,
        "module user;\n\
         import library::{Eq, Point};\n\
         export fn same(a: Point, b: Point) -> Bool { Point::eq(a, b) }\n",
    );
    assert!(found.is_empty(), "one impl reached here twice and was called two: {found:?}");
}

/// A type name is written into a signature whether or not the file imported
/// it, because reading the syntax does not check that anything answers to the
/// name. Its *fields* arrive only with the import — so the annotation was
/// accepted and the field read then reported that the type has no such field,
/// which is a sentence about the wrong thing.
#[test]
fn a_field_of_an_unimported_type_says_the_type_is_missing() {
    let found = errors_in_user(
        LIBRARY,
        "module user;\n\
         export fn get(p: Point) -> Int { p.x }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("`Point` is not in scope here")),
        "expected the message to name the real problem, got {found:?}"
    );
}

/// And with the import, the same read is fine.
#[test]
fn a_field_of_an_imported_type_reads() {
    let found = errors_in_user(
        LIBRARY,
        "module user;\n\
         import library::{Point};\n\
         export fn get(p: Point) -> Int { p.x }\n",
    );
    assert!(found.is_empty(), "expected no errors, got {found:?}");
}

// --- shareability is a fact about a type, not about the importer -------------

/// Three modules, because two are not enough to show the bug: `deep` declares
/// the type, `middle` holds one in a field, and `user` asks whether `middle`'s
/// type may cross into a fiber.
fn errors_in_three(deep: &str, middle: &str, user: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let deep = SourceFile::new(&db, "deep.kh".into(), deep.to_string());
    let middle = SourceFile::new(&db, "middle.kh".into(), middle.to_string());
    let user = SourceFile::new(&db, "user.kh".into(), user.to_string());
    SourceRoot::new(&db, vec![deep, middle, user]);
    khora_types::diagnostics(&db, user).iter().map(|e| e.message.clone()).collect()
}

const DEEP: &str = "module deep;\n\
                    export type Amount = { units: Int };\n";

const MIDDLE: &str = "module middle;\n\
                      import deep::{Amount};\n\
                      export type Cell = | Nothing | Money(Amount);\n\
                      export type Holder = { cells: List<Cell> };\n\
                      export type List<A> = | Nil | Cons(head: A, tail: List<A>);\n";

/// **The bug this exists for.** Whether two fibers may hold a `Cell` is a fact
/// about `Cell`. It was answered by looking inside, and the looking stopped at
/// the edge of what the *importing* file happened to name — so a file that
/// imported `Cell` without also importing `Amount` was told `Cell` could not
/// be shared, and one that imported both was told it could. Same type, same
/// question, two answers, decided by an unrelated line at the top of the file.
///
/// Found by `packages/postgres`: `std::db`'s `Cell` holds a `Decimal`, and a
/// `Channel<Cell>` was refused in every file that did not also import
/// `std::decimal`.
#[test]
fn a_types_shareability_does_not_depend_on_what_the_importer_imported() {
    let user = "module user;\n\
                export trait Share {}\n\
                import middle::{Cell};\n\
                fn takes<A: Share>(value: A) -> () { }\n\
                fn use_it(c: Cell) -> () { takes(c) }\n";
    let found = errors_in_three(DEEP, MIDDLE, user);
    assert!(found.is_empty(), "a `Cell` is shareable whoever is asking: {found:?}");
}

/// The same, one level deeper and through a generic: `Holder` holds a
/// `List<Cell>`, so answering needs `List`'s *parameter names* as well as its
/// body — without them the substitution was empty, `A` stayed a type the
/// caller chooses, and a list of anything was unshareable.
#[test]
fn shareability_reaches_through_a_generic_the_importer_never_named() {
    let user = "module user;\n\
                export trait Share {}\n\
                import middle::{Holder};\n\
                fn takes<A: Share>(value: A) -> () { }\n\
                fn use_it(h: Holder) -> () { takes(h) }\n";
    let found = errors_in_three(DEEP, MIDDLE, user);
    assert!(found.is_empty(), "a `Holder` is shareable whoever is asking: {found:?}");
}

/// The widening must not make an unshareable type shareable. A `mut` field is
/// still a race however far away it is written.
#[test]
fn a_mutable_field_two_modules_away_still_refuses() {
    let deep = "module deep;\n\
                export type Counter = { mut n: Int };\n";
    let middle = "module middle;\n\
                  import deep::{Counter};\n\
                  export type Wrapper = { inner: Counter };\n";
    let user = "module user;\n\
                export trait Share {}\n\
                import middle::{Wrapper};\n\
                fn takes<A: Share>(value: A) -> () { }\n\
                fn use_it(w: Wrapper) -> () { takes(w) }\n";
    let found = errors_in_three(deep, middle, user);
    assert!(
        found.iter().any(|e| e.contains("does not implement `Share`")),
        "a `mut` field two modules away is still a race: {found:?}"
    );
}

/// And the names that came along for the ride are **not in scope**. Carrying
/// bodies for the shareability walk must not quietly widen what a file can
/// name, or a record literal could infer as a type the file cannot write.
#[test]
fn a_reachable_type_is_visible_to_the_checker_and_not_to_the_program() {
    let user = "module user;\n\
                export trait Share {}\n\
                import middle::{Cell};\n\
                fn make() -> Amount { { units: 1 } }\n";
    let found = errors_in_three(DEEP, MIDDLE, user);
    assert!(
        !found.is_empty(),
        "`Amount` was never imported, so naming it should still fail: {found:?}"
    );
}
