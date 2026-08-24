#![cfg(feature = "llvm")]

//! `test` and `bench`, and what selects between them.
//!
//! `bench` had been in the grammar since phase 1 and nothing collected it, so a
//! `bench` block compiled to nothing and ran never — silently, which is the
//! worst way for a promised feature not to work. These are the tests that stop
//! that happening again, and the one worth reading is
//! `a_bench_build_contains_only_benches`: the failure it catches is a build
//! that succeeds and reports nothing.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

const PROGRAM: &str = "module demo::main;

import std::core::{assert};

export fn main() -> () {}

test \"one plus one\" {
  assert(1 + 1 == 2);
}

test \"two plus two\" {
  assert(2 + 2 == 4);
}

bench \"adding\" {
  let mut total = 0;
  let mut at = 0;
  while at < 10 {
    total = total + at;
    at = at + 1;
  }
  assert(total == 45);
}
";

/// Every `.kh` file of `std` this target selects.
fn std_sources(db: &KhoraDatabase) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out
}

/// Builds the program with one of the two harnesses and runs it.
fn run(name: &str, tests: bool, args: &[&str]) -> String {
    harness::ensure_runtime();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "harness.exe" } else { "harness" });

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(&db, dir.join("main.kh"), PROGRAM.to_string()));
    let root = SourceRoot::new(&db, files);

    let built = if tests {
        khora_codegen_llvm::compile_tests(&db, root, &exe)
    } else {
        khora_codegen_llvm::compile_benches(&db, root, &exe)
    };
    if let Err(errors) = built {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = Command::new(&exe).args(args).output().expect("running the harness");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_test_build_runs_the_tests_and_not_the_bench() {
    let output = run("harness_tests", true, &[]);
    assert!(output.contains("test one plus one ... ok"), "{output}");
    assert!(output.contains("test two plus two ... ok"), "{output}");
    assert!(!output.contains("adding"), "a bench is not a test: {output}");
    assert!(output.contains("2 passed, 0 failed"), "{output}");
}

/// The one that would have caught `bench` being dropped on the floor. It was
/// not a crash or an error — the build succeeded and printed `no benchmarks`.
#[test]
fn a_bench_build_contains_only_benches() {
    let output = run("harness_benches", false, &[]);
    assert!(output.contains("bench adding"), "{output}");
    assert!(!output.contains("no benchmarks"), "the bench was not registered: {output}");
    assert!(!output.contains("one plus one"), "a test is not a bench: {output}");
}

/// Percentiles, in that order, with a sample count. No mean, deliberately.
#[test]
fn a_bench_reports_a_distribution() {
    let output = run("harness_distribution", false, &[]);
    for wanted in ["P50", "P95", "P99", "samples"] {
        assert!(output.contains(wanted), "expected `{wanted}` in: {output}");
    }
    assert!(!output.contains("mean"), "offering a mean invites somebody to quote it: {output}");
}

#[test]
fn a_filter_selects_a_test_by_substring() {
    let output = run("harness_filter", true, &["--filter", "two plus"]);
    assert!(output.contains("two plus two ... ok"), "{output}");
    assert!(!output.contains("one plus one"), "{output}");
    assert!(output.contains("1 filtered out"), "the count should say so: {output}");
}

/// `--filter=x` and a bare argument both work, because `cargo test name`
/// trained everyone to expect the third.
#[test]
fn the_filter_accepts_every_spelling() {
    for args in [vec!["--filter=one plus"], vec!["one plus"]] {
        let output = run("harness_spelling", true, &args);
        assert!(output.contains("one plus one ... ok"), "{args:?}: {output}");
        assert!(!output.contains("two plus two"), "{args:?}: {output}");
    }
}

/// A filter matching nothing looks exactly like a file with no tests, and one
/// of those is a typo. The count is what tells them apart.
#[test]
fn a_filter_matching_nothing_says_how_many_it_skipped() {
    let output = run("harness_nomatch", true, &["--filter", "nonexistent"]);
    assert!(output.contains("no tests matching `nonexistent`"), "{output}");
    // Two, not three: a test build does not contain the bench at all, so the
    // count is what this harness registered rather than what the file holds.
    assert!(output.contains("2 declared"), "{output}");
}

#[test]
fn a_filter_selects_a_bench_too() {
    let output = run("harness_benchfilter", false, &["--filter", "nonexistent"]);
    assert!(output.contains("no benchmarks"), "{output}");
}
