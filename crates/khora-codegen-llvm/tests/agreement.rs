#![cfg(feature = "llvm")]

//! Two implementations of one contract, made to answer the same questions.
//!
//! The repository is good at implementing an abstraction and bad at proving
//! that two of them agree. Seven divergences were found in one day, none by a
//! test and every one by somebody reading both sides by hand -- so this file
//! is the place where two sides are made to meet mechanically.
//!
//! The pair here is the permission matchers. `std/permissions.kh` decides what
//! a running program may touch; `khora_manifest` decides the same thing for
//! the compiler, and `std/permissions.kh`'s own doc comment says of the two
//! that "the two have to agree". They did not, twice: once over a `..`
//! segment, once over a `.` one.
//!
//! What makes this a gate rather than another pair of test suites is that the
//! cases live in one file, `crates/khora-manifest/tests/agreement/permission_cases.rs`,
//! and both sides are driven from it -- so a case cannot be added to one side
//! only, which is exactly how the two matchers drifted.
//!
//! # Adding another pair
//!
//! One test that runs both implementations over one shared list of cases, and
//! a line in `scripts/check-agreement.sh`. The list is the part that must not
//! be duplicated; the two runners may live wherever each implementation is
//! reachable from. `crates/khora-types/tests/keys_agree.rs` is the second one
//! and does not need a Khora program at all.

use crate::harness;

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

// The shared table, reached across the workspace rather than copied. If this
// path stops resolving the build fails, which is the point: a copy would have
// gone on passing while saying nothing.
//
// `allow(dead_code)`: this side needs the source-generating helpers and the
// `khora-manifest` side needs none of them, so each consumer leaves part of
// the module unused.
#[path = "../../khora-manifest/tests/agreement/permission_cases.rs"]
#[allow(dead_code)]
mod permission_cases;

use permission_cases::{CASES, khora_function, khora_list, khora_literal, rust_transcript};

fn std_source(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The Khora program that asks `std::permissions` every case in the table.
///
/// Generated rather than written out, because a hand-written program is a
/// second copy of the table: it would be the file somebody forgets when they
/// add a row, and forgetting one side is the defect this test exists to catch.
fn program() -> String {
    let mut calls = String::new();
    for (at, case) in CASES.iter().enumerate() {
        calls.push_str(&format!(
            "  say({}, {}({}, {}));\n",
            khora_literal(&at.to_string()),
            khora_function(case.kind),
            khora_list(case.grants),
            khora_literal(case.subject),
        ));
    }
    format!(
        "module demo::main;
import std::core::{{List}};
import std::permissions::{{granted, granted_host, granted_name}};

fn print(value: String);

// Numbered, so a transcript that has gone out of step by one row reads as one
// disagreement rather than as everything after it also breaking.
fn say(at: String, ok: Bool) -> () {{
  print(if ok {{ \"${{at}} granted\" }} else {{ \"${{at}} refused\" }})
}}

pub fn main() -> Int {{
{calls}  0
}}
"
    )
}

/// **The two permission matchers answer the shared table identically.**
///
/// The comparison is against what `khora_manifest` says, not against the
/// table's own column -- so this fails on a divergence even in a row where
/// both sides are arguably right and the table is wrong. The table is checked
/// separately, by `khora-manifest`'s `permissions_agree.rs`; between them the
/// three answers are pinned to each other.
#[test]
fn the_two_permission_matchers_agree() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("permission_agreement");
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");

    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("permissions.kh"), std_source("permissions.kh")),
        SourceFile::new(&db, dir.join("grants.kh"), std_source("grants.kh")),
        SourceFile::new(&db, dir.join("main.kh"), program()),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling the permission cases failed:\n  {}", messages.join("\n  "));
    }

    let output = std::process::Command::new(&exe).output().expect("the program should run");
    let said = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    let expected = rust_transcript();
    if said == expected {
        return;
    }

    // Line by line, because the useful thing to print is *which* case diverged
    // and why it is in the table -- a diff of two 36-line transcripts is not
    // that.
    let mine: Vec<&str> = said.lines().collect();
    let theirs: Vec<&str> = expected.lines().collect();
    let mut report = Vec::new();
    for (at, case) in CASES.iter().enumerate() {
        let (khora, rust) = (mine.get(at).copied(), theirs.get(at).copied());
        if khora != rust {
            report.push(format!(
                "  case {at}: {:?} {:?} against {:?}\n    \
                 std::permissions said {:?}, khora_manifest said {:?}\n    {}",
                case.kind, case.subject, case.grants, khora, rust, case.why
            ));
        }
    }
    if report.is_empty() {
        // Same answers, different shape: a truncated run, or a trailing line
        // nobody expected. Worth its own message, because the loop above would
        // have printed nothing and left the reader with a passing-looking
        // failure.
        report.push(format!("  the transcripts differ in length: {said:?} against {expected:?}"));
    }
    panic!(
        "`std::permissions` and `khora_manifest` disagree about {} case(s):\n{}",
        report.len(),
        report.join("\n")
    );
}
