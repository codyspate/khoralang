#![cfg(feature = "llvm")]

//! Compiling and linking a program made of several modules.
//!
//! Whole-program, not separate compilation: a generic function is compiled by
//! substituting its type arguments into its *body*, so every module's source
//! has to be present at once. The same constraint C++ templates and Rust
//! generics have, and the reason a symbol carries the module that defines it.

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

/// Compiles every `(name, source)` as one program and runs it.
fn run(test: &str, modules: &[(&str, &str)]) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = modules
        .iter()
        .map(|(name, source)| {
            SourceFile::new(&db, dir.join(format!("{name}.kh")), source.to_string())
        })
        .collect();
    let root = SourceRoot::new(&db, files);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{test}` failed:\n  {}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

const LIB: (&str, &str) = (
    "lib",
    "module demo::lib;
export type Option<A> = | Some(value: A) | None;
export type Step<S, A> = | Yield(state: S, item: A) | Done;
export type Range = | Of(from: Int, to: Int);

export fn double(x: Int) -> Int { x * 2 }

export trait Iterator {
  type Item;
  fn next(self) -> Step<Self, Self::Item>;
}

impl Iterator for Range {
  type Item = Int;
  fn next(self) -> Step<Range, Int> {
    match self {
      Range::Of(from, to) => if from >= to {
        Step::Done
      } else {
        Step::Yield(Range::Of(from + 1, to), from)
      },
    }
  }
}

impl<A> Option<A> {
  fn unwrap_or(self, fallback: A) -> A {
    match self { Option::Some(v) => v, Option::None => fallback }
  }
}
",
);

#[test]
fn a_concrete_function_is_callable_across_modules() {
    let ran = run(
        "cross_concrete",
        &[
            LIB,
            (
                "main",
                "module demo::main;
import demo::lib::{double};
fn print(value: Int);
fn main() -> Int { print(double(21)); 0 }
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// The case separate compilation cannot serve: the body has to be specialized
/// at the *use* site, in a module that never declared it.
#[test]
fn a_generic_method_is_instantiated_across_modules() {
    let ran = run(
        "cross_generic",
        &[
            LIB,
            (
                "main",
                "module demo::main;
import demo::lib::{Option};
fn print(value: Int);
fn main() -> Int {
  print(Option::Some(41).unwrap_or(0) + 1);
  print(Option::None.unwrap_or(99));
  // A third instantiation, at `Bool`, observed through an `if`.
  print(if Option::Some(true).unwrap_or(false) { 7 } else { 0 });
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "42\n99\n7\n", "three instantiations of one imported method");
    assert_eq!(ran.code, Some(0));
}

/// `for` desugars to `Iterator::next` and `Step`, which arrive by import.
#[test]
fn a_for_loop_iterates_an_imported_type() {
    let ran = run(
        "cross_for",
        &[
            LIB,
            (
                "main",
                "module demo::main;
import demo::lib::{Step, Range, Iterator};
fn print(value: Int);
fn main() -> Int {
  let mut total = 0;
  for n in Range::Of(1, 6) {
    total = total + n;
  }
  print(total);
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "15\n");
    assert_eq!(ran.code, Some(0));
}

/// Two modules may each declare a `helper`. Symbols carry the module that
/// defines them, so both are emitted and neither is silently renamed.
#[test]
fn two_modules_may_declare_the_same_name() {
    let ran = run(
        "cross_collide",
        &[
            (
                "a",
                "module demo::a;
export fn helper() -> Int { 1 }
",
            ),
            (
                "b",
                "module demo::b;
export fn helper() -> Int { 2 }
",
            ),
            (
                "main",
                "module demo::main;
import demo::a::{helper as first};
import demo::b::{helper as second};
fn print(value: Int);
fn main() -> Int { print(first()); print(second()); 0 }
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "1\n2\n", "each module's `helper` is its own function");
    assert_eq!(ran.code, Some(0));
}

/// One instantiation asked for by two modules is emitted once, not twice under
/// names LLVM would silently disambiguate.
#[test]
fn one_instantiation_shared_by_two_modules_is_emitted_once() {
    let ran = run(
        "cross_shared",
        &[
            LIB,
            (
                "one",
                "module demo::one;
import demo::lib::{Option};
export fn take(o: Option<Int>) -> Int { o.unwrap_or(1) }
",
            ),
            (
                "two",
                "module demo::two;
import demo::lib::{Option};
export fn take(o: Option<Int>) -> Int { o.unwrap_or(2) }
",
            ),
            (
                "main",
                "module demo::main;
import demo::lib::{Option};
import demo::one::{take as first};
import demo::two::{take as second};
fn print(value: Int);
fn main() -> Int {
  print(first(Option::None));
  print(second(Option::Some(7)));
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "1\n7\n");
    assert_eq!(ran.code, Some(0));
}

/// A program built against the real `std/core.kh`, not a fixture.
///
/// This is what "the standard library works" means: `for` over std's `Range`
/// through std's `Iterator` impl, std's generic methods instantiated here, a
/// closure handed to std's `fold`, and trait dispatch on a std impl — all
/// linked into one binary.
#[test]
fn a_program_runs_against_the_real_standard_library() {
    let core = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std")
            .join("core.kh"),
    )
    .expect("std/core.kh");

    let ran = run(
        "against_std",
        &[
            ("core", core.as_str()),
            (
                "main",
                "module demo::main;
import std::core::{Option, List, Range, Ordering, Iterator, Ord, Step};

fn print(value: Int);

fn main() -> Int {
  let mut total = 0;
  for n in Range::Of(1, 6) {
    total = total + n;
  }
  print(total);

  print(Option::Some(41).unwrap_or(0) + 1);
  print(Option::None.unwrap_or(99));

  let xs = List::Cons(10, List::Cons(20, List::Cons(12, List::Nil)));
  print(xs.fold(0, fn (acc, x) => acc + x));
  print(xs.length());

  print(if 3.cmp(5).is_less() { 1 } else { 0 });
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "15\n42\n99\n42\n3\n1\n");
    assert_eq!(ran.code, Some(0));
}
