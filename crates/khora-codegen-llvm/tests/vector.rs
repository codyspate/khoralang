#![cfg(feature = "llvm")]

//! What `Vector`'s tests cannot say from Khora.
//!
//! **The other twelve moved to `tests/std-suite/src/vector.kh`**, where they
//! are `test` blocks in a package that imports `std` the way anybody else does.
//! They were thirteen Rust tests here, each compiling its own program against
//! the whole standard library -- 193 seconds of CI on Ubuntu and 231 on macOS,
//! because the suite's price is per *program* rather than per assertion: one
//! check costs about sixteen seconds and two hundred cost eighteen. The same
//! assertions are one `khora test` and about twenty.
//!
//! This one stays because it traps. An index past the length ends the process,
//! and a `test` block that does so takes every other block in its binary with
//! it -- so it needs a program of its own, and a runner that does not treat
//! stopping as the test failing.
//!
//! That is the line generally: what a *running* Khora program cannot observe
//! stays in Rust. A program that must fail to compile, a trap, the shape of an
//! artifact, anything setting an environment variable.

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

/// An index past the length stops the program, and says which index it was.
///
/// `Vector::at` is the unchecked-looking half of the pair `get` and `at` make:
/// `get` answers an `Option` and `at` answers the element, so `at` has to trap
/// rather than invent one. What is checked here is that it does, and that the
/// message names the problem rather than the runtime's internals.
#[test]
fn indexing_a_vector_past_its_length_stops_the_program() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("vector_trap");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let main = "module main;
import std::core::{Vector, print};

pub fn main() -> () {
  let v: Vector<Int> = Vector::new();
  Vector::push(v, 1);
  print(Int::to_string(Vector::at(v, 5)))
}
";
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling the trap program failed:\n  {}", messages.join("\n  "));
    }

    let out = Command::new(&exe).output().expect("the program should run");
    let said = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    assert!(said.contains("index"), "the trap says what was wrong: {said:?}");
    assert_ne!(out.status.code(), Some(0), "an out-of-range index must stop the program");
}
