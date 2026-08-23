#![cfg(feature = "llvm")]

//! Phase 9's target, and the measurement of how far away it is.
//!
//! The exit criterion is that `map` over a uniquely-owned list allocates
//! nothing: the cell being matched is dead the moment the arm has its fields,
//! so the cell the arm builds can be the same memory. Nothing does that today,
//! and `docs/design/reuse.md` explains why it is the *analysis* rather than the
//! fusion that is missing — at the constructor, the matched cell is still held
//! by its binding and by the `dup` the read made, so a uniqueness test
//! correctly declines.
//!
//! So `reuse_is_not_implemented_yet` records what a walk costs now, and
//! `#[ignore]`s the assertion that phase 9 has to make true. Written before the
//! work rather than after it, because a criterion first evaluated once the
//! change is in is a criterion fitted to the result.
//!
//! Allocation counts are the compiler's own instrument and not a promise to
//! anybody — `docs/design/compatibility.md` decides that when memory is
//! allocated is not observable, which is what makes phase 9 legal at all.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test.
fn sources(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
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
    out.push(SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

fn run(name: &str, main: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    assert!(output.status.success(), "`{name}` exited with {:?}", output.status.code());
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// A list of ten built and then walked, incrementing each element.
///
/// The walk is the interesting half: it consumes a list nothing else holds and
/// produces one of the same shape, which is the exact case reuse exists for.
/// Counters are reset after the list is built so the number is the walk's
/// alone, and the sum is printed so nothing can be optimised away on the
/// grounds that the result is unused.
const WALK: &str = "module main;
import std::core::{List, print};

extern fn khora_alloc_count() -> Int;
extern fn khora_reset_counters();

fn build(n: Int) -> List<Int> {
  if n == 0 { List::Nil } else { List::Cons(n, build(n - 1)) }
}

fn increment(xs: List<Int>) -> List<Int> {
  match xs {
    List::Nil => List::Nil,
    List::Cons(head, tail) => List::Cons(head + 1, increment(tail)),
  }
}

fn total(xs: List<Int>) -> Int {
  match xs {
    List::Nil => 0,
    List::Cons(head, tail) => head + total(tail),
  }
}

export fn main() -> () {
  let built = build(10);
  khora_reset_counters();
  let walked = increment(built);
  let allocations = khora_alloc_count();
  print(Int::to_string(total(walked)));
  print(Int::to_string(allocations))
}
";

/// What a ten-element walk costs today: one fresh cell per `Cons`, and nothing
/// for the `Nil`.
///
/// It was eleven when this was written. The `List::Nil` at the end of the walk
/// was a heap allocation, because every field-less constructor was — a value
/// entirely described by its tag, given twenty-four bytes and a pair of atomic
/// reference-count operations. Those are static singletons now, which is the
/// first thing phase 9 took.
///
/// This is a *record*, not a requirement. When reuse lands it should fail, and
/// the right response is to delete it and un-ignore the one below.
#[test]
fn reuse_is_not_implemented_yet() {
    let out = run("reuse_walk_today", WALK);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "65", "the walk should sum 2..=11");
    assert_eq!(
        lines[1], "10",
        "a ten-element walk allocates ten cells today. If this changed, phase 9 has \
         started and `a_uniquely_owned_walk_allocates_nothing` is the test that matters"
    );
}

/// **Phase 9's exit criterion.** `docs/design/reuse.md`.
///
/// Every cell the walk consumes is uniquely held by the time its fields are
/// out, so every cell it builds can be that memory. Ten in, ten out, nothing
/// allocated.
#[test]
#[ignore = "phase 9: reuse analysis is not written; see docs/design/reuse.md"]
fn a_uniquely_owned_walk_allocates_nothing() {
    let out = run("reuse_walk_target", WALK);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "65", "the walk should sum 2..=11");
    assert_eq!(lines[1], "0", "a uniquely-owned walk should reuse every cell it consumes");
}
