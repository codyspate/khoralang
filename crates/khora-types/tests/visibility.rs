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
