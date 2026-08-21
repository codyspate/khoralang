//! `std/` type checks.
//!
//! The corpus test in `khora-fmt` proves `std/` *parses*; nothing proved it
//! meant anything. Everything in `std::core` below the effect declarations is
//! ordinary phase 3 code — traits, generic impls, higher kinds, closures — so
//! it can be checked like any other program, and it is the largest single
//! piece of Khora that exists.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("std/ should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic from every file in `std/`, checked as one compilation so
/// that cross-module imports resolve.
fn errors() -> Vec<String> {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "no .kh files under std/");

    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("std/ should be readable");
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
    let found = errors();
    assert!(found.is_empty(), "std/ does not type check:\n  {}", found.join("\n  "));
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
    ] {
        assert!(text.contains(expected), "std/core.kh no longer declares `{expected}`");
    }
}
