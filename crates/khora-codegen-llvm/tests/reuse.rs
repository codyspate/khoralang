#![cfg(feature = "llvm")]

//! Phase 9's target: `map` over a uniquely-owned list allocates nothing.
//!
//! The cell being matched is dead the moment the arm has its fields, so the
//! cell the arm builds can be the same memory. The assertion below was written
//! before the work rather than after it — a criterion first evaluated once the
//! change is in is a criterion fitted to the result — and sat `#[ignore]`d
//! beside a `reuse_is_not_implemented_yet` recording the ten allocations a walk
//! cost instead. Deleting that one was its own instruction for the day it
//! failed.
//!
//! Allocation counts are the compiler's own instrument and not a promise to
//! anybody — `docs/design/compatibility.md` decides that when memory is
//! allocated is not observable, which is what makes phase 9 legal at all.

use crate::harness;

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

pub fn main() -> () {
  let built = build(10);
  khora_reset_counters();
  let walked = increment(built);
  let allocations = khora_alloc_count();
  print(Int::to_string(total(walked)));
  print(Int::to_string(allocations))
}
";

/// **Phase 9's exit criterion.** `docs/design/reuse.md`.
///
/// Every cell the walk consumes is uniquely held by the time its fields are
/// out, so every cell it builds can be that memory. Ten in, ten out, nothing
/// allocated.
///
/// The number was eleven when this file was written and ten when reuse landed:
/// the `List::Nil` closing the walk stopped being an allocation once a
/// field-less constructor became one static object for the whole program, which
/// was phase 9.0.
#[test]
fn a_uniquely_owned_walk_allocates_nothing() {
    let out = run("reuse_walk_target", WALK);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "65", "the walk should sum 2..=11");
    assert_eq!(lines[1], "0", "a uniquely-owned walk should reuse every cell it consumes");
}
