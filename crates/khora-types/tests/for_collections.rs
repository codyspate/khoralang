//! What a `for` over something it cannot walk says, and what a `for` over a
//! collection asks a program to import.
//!
//! **What these guard: a loop that compiles only once a name it never writes
//! is imported, and a refusal about a method nobody wrote.** `for` reaches
//! `Vector`, `Array`, `Map` and `Dict` through `Iterable`, which no program
//! has to name; each test below fails if that stops being true, or if what a
//! wrong loop is told stops pointing at what was written after `in`.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the std directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic for the program's files, compiled with `std`, as
/// `(message, underlined text)`, for the file named first.
fn said(program: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("std should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    let mine: Vec<SourceFile> = program
        .iter()
        .map(|(name, text)| SourceFile::new(&db, PathBuf::from(name), text.to_string()))
        .collect();
    files.extend(mine.iter().copied());
    SourceRoot::new(&db, files);
    let text = program[0].1;
    khora_types::diagnostics(&db, mine[0])
        .iter()
        .map(|e| {
            let span = text[usize::from(e.range.start())..usize::from(e.range.end())].to_string();
            (e.message.clone(), span)
        })
        .collect()
}

const NEITHER: &str = "a `for` walks an `Iterator` or an `Iterable`, and `Int` is neither. \
                       `List`, `Range`, `Vector`, `Array`, `Map` and `Dict` are, and a type of \
                       your own is once it implements `Iterator` (it is its own cursor) or \
                       `Iterable` (it hands out a cursor)";

/// **The refusal names both ways in, and points at the operand.** It used to
/// be "`Int` does not implement `Iterator`, which is where `next` comes from",
/// under the whole loop: a method the reader never wrote, and silent about
/// `Iterable`.
#[test]
fn a_for_over_something_not_iterable_is_told_what_is() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterator, Step};\n\
         fn f() -> Int { let mut t = 0; for x in 5 { t = t + x; }; t }\n",
    )]);
    assert_eq!(found, vec![(NEITHER.to_string(), "5".to_string())]);
}

/// **A loop over a `Vector` needs exactly the imports a loop over a `List`
/// does**: `Iterator` and `Step`, and never `Iterable`.
#[test]
fn a_for_over_each_collection_needs_only_iterator_and_step() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterator, Step, Vector, Array, Map, Dict};\n\
         fn f(v: Vector<Int>, a: Array<Int>, m: Map<Int, Int>, d: Dict<Int, Int>) -> Int {\n\
         let mut n = 0;\n\
         for x in v { n = n + x; };\n\
         for x in a { n = n + x; };\n\
         for _e in m { n = n + 1; };\n\
         for _e in d { n = n + 1; };\n\
         n\n}\n",
    )]);
    assert!(found.is_empty(), "{found:?}");
}

/// **Not even the collection's own name, when a call hands it over.** A file
/// that loops over what an imported function returns never writes `Vector`,
/// and asking it to import one would be advice `unused-import` then calls
/// wrong.
#[test]
fn a_collection_that_arrives_from_a_call_needs_no_import_of_its_type() {
    let found = said(&[
        (
            "main.kh",
            "module main;\nimport std::core::{Iterator, Step};\nimport other::{make};\n\
             fn f() -> Int { let mut t = 0; for x in make() { t = t + x; }; t }\n",
        ),
        (
            "other.kh",
            "module other;\nimport std::core::{Vector};\n\
             pub fn make() -> Vector<Int> { Vector::new() }\n",
        ),
    ]);
    assert!(found.is_empty(), "{found:?}");
}

/// Without `Iterator` and `Step` a loop over a `Vector` says what a loop over
/// a `List` says, once, and nothing else.
#[test]
fn a_missing_import_is_one_error_for_a_vector_too() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{Vector};\n\
         fn f(v: Vector<Int>) -> Int { let mut t = 0; for x in v { t = t + x; }; t }\n",
    )]);
    let messages: Vec<&str> = found.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        messages,
        vec!["`for` needs `Step` and `Iterator` in scope; import them from `std::core`"]
    );
}

/// **The cursor's bound is checked where the impl is written.** An
/// `Iterable` whose `Cursor` is not an `Iterator` is refused there; the files
/// that merely receive the impl with its type do not judge it again.
#[test]
fn a_cursor_that_is_not_an_iterator_is_refused_at_the_impl() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterator, Iterable, Step};\n\
         type Bag = { n: Int };\n\
         type NotACursor = { at: Int };\n\
         impl Iterable for Bag {\n  type Cursor = NotACursor;\n  \
         fn iter(self) -> NotACursor { { at: self.n } }\n}\n",
    )]);
    let messages: Vec<&str> = found.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        messages,
        vec!["`NotACursor` does not implement `Iterator`, which `Iterable` requires of its `Cursor`"]
    );
}

/// A file that imports a collection and writes no loop is told nothing about
/// cursors: the impls it received with `Array` are not its to judge.
#[test]
fn a_file_without_iterator_in_scope_is_not_told_about_std_cursors() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{Array};\n\
         fn f(a: Array<Int>) -> Int { Array::length(a) }\n",
    )]);
    assert!(found.is_empty(), "{found:?}");
}

/// **An impl's associated-type bound is judged where the impl is written.**
/// A file that imports `Iterable` and not `Iterator` holds `std`'s four
/// `Iterable` impls, which come with the trait, and none of the `Iterator`
/// impls their cursors rely on, which come only with `Iterator`. Judged again
/// there, each bound failed: "`VectorItems<A>` does not implement
/// `Iterator`", four times, about `std`'s declarations, in a file that
/// writes no loop -- the shape of a module that only implements `Iterable`.
#[test]
fn an_imported_iterable_impl_is_not_judged_again_by_its_importer() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterable};\n\
         type Bag = { n: Int };\n\
         fn size(b: Bag) -> Int { b.n }\n",
    )]);
    assert!(found.is_empty(), "{found:?}");
}
