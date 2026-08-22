//! `std/` type checks.
//!
//! The corpus test in `khora-fmt` proves `std/` *parses*; nothing proved it
//! meant anything. Everything in `std::core` below the effect declarations is
//! ordinary phase 3 code — traits, generic impls, higher kinds, closures — so
//! it can be checked like any other program, and it is the largest single
//! piece of Khora that exists.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn repo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn std_dir() -> PathBuf {
    repo_dir().join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the source directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic from every file under `dirs`, checked as one compilation
/// so that cross-module imports resolve.
fn errors_under(dirs: &[PathBuf]) -> Vec<String> {
    let mut paths = Vec::new();
    for dir in dirs {
        sources(dir, &mut paths);
    }
    paths.sort();
    assert!(!paths.is_empty(), "no .kh files under {dirs:?}");

    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("the sources should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    SourceRoot::new(&db, files.clone());

    files
        .iter()
        .flat_map(|f| {
            let path = f.path(&db).display().to_string();
            khora_types::diagnostics(&db, *f)
                .iter()
                .map(|e| format!("{path}: {}", e.message))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_standard_library_type_checks() {
    let found = errors_under(&[std_dir()]);
    assert!(found.is_empty(), "std/ does not type check:\n  {}", found.join("\n  "));
}

/// The phase 4 exit criterion, minus serving a request.
///
/// `examples/risk_analyzer` is the program the whole design was written
/// against: capabilities, a fallible service, `catch` discharging half an error
/// row, a router carrying its handlers' requirements, a named context
/// installing three services at once. It type checking is the claim that the
/// pieces fit together, and it is worth a test precisely because every one of
/// those pieces has a unit test that passed while this did not.
///
/// **This reported clean for a long time while it was not.** `ai.extract` was
/// declared `forall <A: Extract> . (Prompt, A::Spec) -> A`, the checker had
/// nowhere to put the `A`, and the `Unknown` it produced agreed with everything
/// downstream — the same way entry 24's test was green. Errata 40 and 41 are
/// that story, the `Unknown` audit is what ended it, and
/// `docs/design/polymorphic-operations.md` is the decision that made the
/// program true rather than the test lenient.
#[test]
fn the_reference_application_type_checks() {
    let found = errors_under(&[std_dir(), repo_dir().join("examples")]);
    assert!(
        found.is_empty(),
        "the reference application does not type check:\n  {}",
        found.join("\n  ")
    );
}

/// The pieces that make `std::core` worth having, so that losing one is a test
/// failure rather than a quiet regression in a file nobody reads.
#[test]
fn the_standard_library_declares_what_it_promises() {
    let text = std::fs::read_to_string(std_dir().join("core.kh")).expect("std/core.kh");
    for expected in [
        // Comparison, and the three-way answer that decides all six operators.
        "export type Ordering",
        "export trait Eq",
        "export trait Ord: Eq",
        "export trait Show",
        // Optional values and failures.
        "export type Option<A>",
        "export type Result<A, E>",
        "impl<A> Option<A>",
        "impl<A, E> Result<A, E>",
        // Containers and iteration.
        "export type Array<A>",
        "export type Map<K, V>",
        "export type Chain<K, V>",
        // A map's key is any type with a `Hash`, which is what having bytes
        // was for: before them, a `String` could not be one.
        "export trait Hash: Eq",
        "impl Hash for String",
        "export type List<A>",
        "export type Step<S, A>",
        "export trait Iterator",
        "impl Iterator for Range",
        "impl<A> Iterator for List<A>",
        // The reason higher-kinded types are a non-negotiable.
        "export trait Functor",
        "export trait Applicative: Functor",
        "export trait Traversable: Functor",
        "impl Traversable for Option",
        "impl Traversable for List",
        // The scalars show, which is what a derived `Show` on a record of them
        // calls. Without these `derive(Show)` was correctly refused everywhere.
        "impl Show for Int",
        "impl Show for Bool",
        "impl Show for String",
        // Shared state, and the ordered map that can go in it.
        "export type Shared<A>",
        "export type Dict<K, V>",
        "export type Pair<K, V>",
        // The growable one. `Array::empty` is what made it possible to hold an
        // `Array<A>` rather than an `Array<Option<A>>`, and a hundred integers
        // two objects rather than a hundred and three.
        "export type Vector<A>",
        "fn empty() -> Array<A>",
    ] {
        assert!(text.contains(expected), "std/core.kh no longer declares `{expected}`");
    }
}
