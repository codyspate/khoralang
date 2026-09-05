#![cfg(feature = "llvm")]

//! `type UserId = Int;` — a type of its own, with a way in and out.
//!
//! **It was already distinct and had no way to be anything.** Nothing accepted
//! a `UserId` where an `Int` was wanted, which is the point of writing one —
//! but there was no constructor, no pattern and no field, so nothing could
//! convert in either direction. `let a: UserId = 7` was refused, `UserId::of`
//! did not exist, and `match b { UserId(v) => v }` said "cannot find
//! constructor". The type was uninhabitable and the guide described it as
//! transparent.
//!
//! Somebody reached for `type Books = Dict<Currency, Bucket>;`, got nine
//! errors, and wrote the two-parameter type out longhand at eight call sites.
//!
//! The shape is Rust's tuple struct: `UserId(1)` in, `UserId(v)` out. Both come
//! from machinery a variant case already had — one case, named after the type,
//! carrying one positional field — so construction, patterns, `derive`, drop
//! glue and code generation are all the paths that existed.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
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
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

/// Compiles `source` expecting it to be refused, and hands back the messages.
fn refused(name: &str, source: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    match khora_codegen_llvm::compile(&db, root, &dir.join("unused")) {
        Ok(()) => panic!("`{name}` compiled and should not have:\n{source}"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
}

const IDS: &str = "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);

pub type UserId = Int;
pub type OrderId = Int;

fn number(id: UserId) -> Int { match id { UserId(value) => value } }
";

/// **In and out.**
#[test]
fn a_wrapper_can_be_built_and_taken_apart() {
    let ran = run(
        "newtype_round_trip",
        &format!(
            "{IDS}
fn main() -> Int {{
  let id = UserId(41);
  print(number(id));
  print(number(UserId(1)));
  0
}}
"
        ),
    );

    assert_eq!(ran.stdout, "41\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// **The point of writing one**: three ways of confusing it with what it wraps,
/// all refused.
#[test]
fn a_wrapper_is_not_the_type_it_wraps() {
    let found = refused(
        "newtype_distinct",
        &format!(
            "{IDS}
fn main() -> Int {{
  let bad: UserId = 7;
  let worse = number(OrderId(1));
  let plain: Int = UserId(1);
  print(0);
  0
}}
"
        ),
    );

    assert!(
        found.iter().any(|e| e.contains("expected `UserId`, found `Int`")),
        "an `Int` is not a `UserId`: {found:?}"
    );
    assert!(
        found.iter().any(|e| e.contains("expected `UserId`, found `OrderId`")),
        "and two wrappers over the same type are not each other: {found:?}"
    );
    assert!(
        found.iter().any(|e| e.contains("expected `Int`, found `UserId`")),
        "nor is a `UserId` an `Int`: {found:?}"
    );
}

/// A wrapper over something boxed, which is the case reference counting has to
/// get right — the wrapper owns the string and releases it.
#[test]
fn a_wrapper_may_hold_a_boxed_value() {
    let ran = run(
        "newtype_boxed",
        "module t;
fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

pub type Label = String;

fn text(l: Label) -> String { match l { Label(t) => t } }

fn churn(rounds: Int) -> Int {
  let mut i = 0;
  while i < rounds {
    let held = Label(\"ada\");
    let _ = text(held);
    i = i + 1;
  };
  i
}

fn main() -> Int {
  print(text(Label(\"ada\")));
  let _ = churn(500);
  khora_print_int(khora_live_count());
  0
}
",
    );

    assert_eq!(ran.stdout, "ada\n0\n", "five hundred wrappers, nothing left over");
}
