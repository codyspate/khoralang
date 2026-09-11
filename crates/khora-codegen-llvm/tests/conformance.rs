//! One program, most of the language, one compile.
//!
//! **The suite's cost is per program, not per assertion.** Measured against the
//! real `std`: a program with one check costs 16.2s, one with two hundred costs
//! 18.3s. Nearly all of it is fixed -- parsing, checking and lowering the whole
//! standard library -- and every test that compiles its own program pays it
//! again. Two hundred and thirteen tests in this crate take five seconds or
//! more each, and together they are 60% of the suite's wall clock.
//!
//! So this file compiles *one* program, and that program exercises the language
//! surface rather than one feature of it. It is the Rust-side half of a split:
//!
//!   - **here**: that the compiler still compiles Khora. Every construct the
//!     language has, in one module, checked and lowered and run. When this
//!     fails, the compiler is broken.
//!   - **`khora test`**: that `std` computes the right answers. Those live in
//!     Khora beside the code they test, cost one compile for all of them
//!     together, and say which test failed and why.
//!
//! **What cannot move to `khora test`**, and is why this file is not the only
//! Rust test left: a program that must fail to compile (`errors`, `compile`), a
//! trap that ends the process (`arithmetic`'s overflow), anything asserting on
//! the artifact rather than on a value (`reproducible`, `profiles`), and the
//! three files that set environment variables and are separate binaries for it.
//!
//! # Adding to this
//!
//! Add a `test` block to `CONFORMANCE` and a line to the expected output. A new
//! construct belongs here the day it exists; a new *behaviour* of `std` belongs
//! in `std`'s own tests, where it is cheaper and says more when it breaks.

#![cfg(feature = "llvm")]

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the conformance program.
///
/// **Compiled against the real `std`, not alone.** The program interpolates
/// values into its assertion messages, and `${n}` goes through `Show` -- which
/// lives in `std::core`. A module compiled by itself has no `Show for Int`, so
/// every message in the file is a type error, and the failure says so
/// seventeen times over.
///
/// It was written standalone and tested inside a package, where `std` comes in
/// implicitly. That is a different compile from the one this test does, which
/// is the whole reason CI caught it and the local run did not.
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
    out.push(SourceFile::new(db, dir.join("conformance.kh"), main.to_string()));
    out
}

/// The language, in one module.
///
/// Deliberately not using `std` beyond `print` and `assert_that`: this is about
/// what the *compiler* does with each construct, and a program that reaches
/// into `std` for each one is testing the library at the same time and taking
/// longer to do it. `std`'s own tests are where the library is checked.
///
/// It is nevertheless compiled *with* `std`, because `${..}` goes through
/// `Show` and `Show` lives there. See `sources`.
const CONFORMANCE: &str = r#"module conformance;

import std::core::{assert_that, print};

// --- algebraic data types, and matching over them ------------------------

pub type Shape =
  | Circle(radius: Int)
  | Rect(width: Int, height: Int)
  | Dot;

fn area(s: Shape) -> Int {
  match s {
    Shape::Circle(r) => 3 * r * r,
    Shape::Rect(w, h) => w * h,
    Shape::Dot => 0,
  }
}

// --- records, including nesting and update -------------------------------

pub type Point = { x: Int, y: Int };
pub type Line = { from: Point, to: Point };

fn shifted(p: Point, by: Int) -> Point { { x: p.x + by, y: p.y + by } }

fn span(l: Line) -> Int { (l.to.x - l.from.x) + (l.to.y - l.from.y) }

// --- generics, and a trait with an impl ----------------------------------

pub trait Doubles { fn twice(self) -> Int; }

impl Doubles for Int { fn twice(self) -> Int { self * 2 } }

fn twice_of<A: Doubles>(a: A) -> Int { a.twice() }

fn first_of<A>(a: A, _b: A) -> A { a }

// --- closures, and a function taking one ---------------------------------

fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }

fn adder(n: Int) -> (Int) -> Int { fn (x) => x + n }

// --- failure: a declared error, raised, propagated, and caught -----------

pub type Refused = | TooSmall(saw: Int);

fn at_least_ten(n: Int) -> Int raises Refused {
  if n < 10 { raise Refused::TooSmall(n) } else { n }
}

fn forgiving(n: Int) -> Int {
  at_least_ten(n)! catch { Refused::TooSmall(saw) => saw }
}

// --- capabilities: an effect, required and supplied ----------------------

pub effect Counter { bump: (Int) -> Int, }

fn counted(by: Int) -> Int with { counter: Counter } {
  counter.bump(by)
}

// --- mutation, loops, early return ---------------------------------------

fn sum_to(n: Int) -> Int {
  if n < 0 { return 0; }
  let mut total = 0;
  let mut i = 1;
  while i <= n {
    total = total + i;
    i = i + 1;
  }
  total
}

fn count_down(from: Int) -> Int {
  let mut seen = 0;
  let mut i = from;
  loop {
    if i <= 0 { break; }
    seen = seen + 1;
    i = i - 1;
  }
  seen
}

pub fn main() -> Int { 0 }

// --- what each of those is worth -----------------------------------------

test "an adt matches on every constructor" {
  assert_that(area(Shape::Circle(2)) == 12, "circle was ${area(Shape::Circle(2))}");
  assert_that(area(Shape::Rect(3, 4)) == 12, "rect was ${area(Shape::Rect(3, 4))}");
  assert_that(area(Shape::Dot) == 0, "dot was ${area(Shape::Dot)}");
}

test "a record is built, read and updated" {
  let p = shifted({ x: 1, y: 2 }, 10);
  assert_that(p.x == 11, "x was ${p.x}");
  assert_that(p.y == 12, "y was ${p.y}");
  let l = { from: { x: 0, y: 0 }, to: p };
  assert_that(span(l) == 23, "span was ${span(l)}");
}

test "a trait dispatches through a generic" {
  assert_that(twice_of(21) == 42, "twice was ${twice_of(21)}");
  assert_that(first_of(7, 9) == 7, "first was ${first_of(7, 9)}");
}

test "a closure captures and is passed as a value" {
  assert_that(apply(fn (x) => x * 3, 5) == 15, "applied to ${apply(fn (x) => x * 3, 5)}");
  let add7 = adder(7);
  assert_that(add7(1) == 8, "captured sum was ${add7(1)}");
}

test "a failure propagates, and a catch turns it into a value" {
  assert_that(forgiving(3) == 3, "caught ${forgiving(3)}");
  assert_that(forgiving(30) == 30, "passed through ${forgiving(30)}");
}

test "a capability is required in the signature and supplied at the edge" {
  with { counter: handler for Counter { bump: fn (n) => n + 100, } } {
    assert_that(counted(1) == 101, "counted ${counted(1)}");
  }
}

test "mutation, a while loop and an early return" {
  assert_that(sum_to(10) == 55, "sum was ${sum_to(10)}");
  assert_that(sum_to(0 - 1) == 0, "the guard returned ${sum_to(0 - 1)}");
  assert_that(count_down(4) == 4, "counted ${count_down(4)}");
}

test "string interpolation renders what it is given" {
  let n = 3;
  let rendered = "n is ${n}";
  assert_that(rendered == "n is 3", "rendered ${rendered}");
}
"#;

/// Compiles the conformance program's `test` blocks and runs them.
///
/// **`compile_tests` rather than `compile`**, so the `test` blocks become the
/// entry point and the runner reports each by name. A failure here names the
/// block and the assertion, which is the whole reason this is one program
/// rather than one program printing a transcript to diff.
#[test]
fn the_language_still_compiles_and_runs() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("conformance");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "conformance.exe" } else { "conformance" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, CONFORMANCE));
    if let Err(errors) = khora_codegen_llvm::compile_tests(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("the conformance program did not compile:\n  {}", messages.join("\n  "));
    }

    let out = Command::new(&exe).output().expect("running the conformance program");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");

    // The runner's own summary is the assertion. It prints one line per block
    // and a count, and a count that is not "0 failed" carries the block's name
    // and the failing assertion's message with it.
    assert!(
        stdout.contains("0 failed"),
        "a conformance test failed:\n{stdout}\n{stderr}"
    );
    assert_eq!(out.status.code(), Some(0), "the runner exited badly:\n{stdout}\n{stderr}");
}
