#![cfg(feature = "llvm")]

//! Labeled arguments, built and run.
//!
//! **A label moves nothing, so nothing after the checker knows it was
//! there.** These pin the two consequences a reader relies on: arguments run
//! in the order written, and a labeled call computes what the unlabeled one
//! does. A label that reordered would print a different sequence here.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn run(name: &str, source: &str) -> (String, Option<i32>) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    (String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"), output.status.code())
}

const PRELUDE: &str = "module t;
extern fn khora_print_int(value: Int);

fn io(tag: Int, n: Int) -> Int {
  khora_print_int(tag);
  n
}

fn sub(minuend: Int, subtrahend: Int, verbose: Bool) -> Int {
  if verbose { khora_print_int(99); }
  minuend - subtrahend
}

pub type Pt = | P(x: Int, y: Int);
";

/// The design's probe, as a test: direct call, pipe, and constructor, each
/// with labels, print their arguments' side effects left to right.
#[test]
fn labeled_arguments_evaluate_in_the_order_written() {
    let (stdout, code) = run(
        "labeled_order",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let a = sub(io(1, 10), subtrahend: io(2, 3), verbose: io(3, 1) == 1);
  khora_print_int(a);
  let b = io(4, 100) |> sub(subtrahend: io(5, 1), verbose: false);
  khora_print_int(b);
  let p = Pt::P(x: io(6, 1), y: io(7, 2));
  match p {{ Pt::P(x, y) => {{ khora_print_int(x); khora_print_int(y) }} }};
  0
}}
"
        ),
    );
    assert_eq!(stdout, "1\n2\n3\n99\n7\n4\n5\n99\n6\n7\n1\n2\n");
    assert_eq!(code, Some(0));
}

/// A method call and a trait method whose impl renamed the parameter: the
/// labeled call runs the same function with the same arguments.
#[test]
fn a_labeled_method_call_computes_what_the_unlabeled_one_does() {
    let (stdout, code) = run(
        "labeled_methods",
        "module t;
extern fn khora_print_int(value: Int);

pub type Acc = { total: Int };

impl Acc {
  pub fn add(self, amount: Int, twice: Bool) -> Int {
    if twice { self.total + amount * 2 } else { self.total + amount }
  }
}

pub trait Scale {
  fn scale(self, factor: Int, negate: Bool) -> Int;
}

impl Scale for Acc {
  fn scale(self, by: Int, flip: Bool) -> Int {
    if flip { 0 - self.total * by } else { self.total * by }
  }
}

fn main() -> Int {
  let a = { total: 5 };
  khora_print_int(a.add(1, twice: true));
  khora_print_int(Acc::add(a, amount: 1, twice: false));
  khora_print_int(a.scale(factor: 3, negate: true));
  khora_print_int(Scale::scale(a, 3, negate: false));
  0
}
",
    );
    assert_eq!(stdout, "7\n6\n-15\n15\n");
    assert_eq!(code, Some(0));
}
