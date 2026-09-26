#![cfg(feature = "llvm")]

//! Patterns matched against a value of another type, end to end.
//!
//! **A constructor pattern names a type, and the value has to be that type.**
//! Nothing checked it: `match 3 { Option::Some(v) => v, _ => 0 }` passed the
//! checker and panicked the code generator, and where both types were heap
//! objects -- `Option<Big>` matched with `Result::Ok(v)` -- the program
//! built, ran, and read `v` out of the other constructor's layout, printing a
//! number with nothing to say it was wrong. These pin the refusal at the
//! depths it was missing, and that the programs that were right still run.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// What one run of a compiled program printed and how it ended.
pub(crate) struct Ran {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) code: Option<i32>,
}

fn compile(name: &str, source: &str) -> Result<PathBuf, Vec<String>> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    let parse = khora_db::parse(&db, file);
    if !parse.errors().is_empty() {
        return Err(parse.errors().iter().map(|e| e.message.clone()).collect());
    }
    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => Ok(exe),
        Err(errors) => Err(errors.into_iter().map(|e| e.message).collect()),
    }
}

/// Compiles `source` expecting it to be refused, and hands back the messages.
///
/// A panic in the code generator is a failure of this test, not a refusal:
/// that is the shape one of these programs had.
pub(crate) fn refused(name: &str, source: &str) -> Vec<String> {
    match compile(name, source) {
        Ok(_) => panic!("`{name}` should have been refused:\n\n{source}"),
        Err(messages) => messages,
    }
}

/// Compiles once and runs on both fiber backends, which must agree.
///
/// Both, because the backends unwind a raise differently, and a suite that
/// sets neither only ever runs the thread backend.
pub(crate) fn run_both(name: &str, source: &str) -> Ran {
    let exe = match compile(name, source) {
        Ok(exe) => exe,
        Err(messages) => {
            panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "))
        }
    };
    let mut seen: Vec<Ran> = Vec::new();
    for backend in ["threads", "scheduler"] {
        let output = Command::new(&exe)
            .env("KHORA_FIBERS", backend)
            .output()
            .expect("the program should run");
        seen.push(Ran {
            stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            stderr: String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n"),
            code: output.status.code(),
        });
    }
    let scheduler = seen.pop().expect("two runs");
    let threads = seen.pop().expect("two runs");
    assert_eq!(
        (&threads.stdout, threads.code),
        (&scheduler.stdout, scheduler.code),
        "the two backends disagree on `{name}`"
    );
    threads
}

const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

impl String {
  fn byte_length(self) -> Int;
}

pub type Option<A> = | Some(v: A) | None;
pub type Result<A, B> = | Ok(v: A) | Err(e: B);
pub type Big = { a: String, b: String, c: String, d: Int, e: Int };
pub type Gx<A> = | X(s: String, v: A) | Y(n: Int);
pub type Ng = | A(v: Option<Big>);

fn big() -> Big { { a: \"aa\" + \"aa\", b: \"b\", c: \"c\", d: 1, e: 2 } }
fn fi() -> Int raises Gx<Int> { raise Gx::X(\"i\" + \"1\", 3) }
fn fbo() -> Int raises Gx<Option<Big>> { raise Gx::X(\"s\", Option::Some(big())) }
fn fng() -> Int raises Ng { raise Ng::A(Option::Some(big())) }
";

fn assert_wrong_type(found: &[String], owner: &str, value: &str) {
    let needle = format!("this pattern is a `{owner}` case, and the value here is a `{value}`");
    assert!(found.iter().any(|e| e.contains(&needle)), "expected {needle:?}, got {found:?}");
}

/// Panicked the code generator ("expected PointerValue").
#[test]
fn a_constructor_pattern_over_an_int_is_refused() {
    let found = refused(
        "wrong_ctor_int",
        &format!("{PRELUDE}fn main() -> Int {{ let x = 3; print(match x {{ Option::Some(v) => v, _ => 0 }}); 0 }}\n"),
    );
    assert_wrong_type(&found, "Option", "Int");
}

/// Built, ran, and printed 5: the length read from the wrong layout.
#[test]
fn a_boxed_value_matched_with_another_types_constructor_is_refused() {
    let found = refused(
        "wrong_ctor_boxed_match",
        &format!(
            "{PRELUDE}fn main() -> Int {{ let o: Option<Big> = Option::Some(big()); \
             print(match o {{ Option::Some(Result::Ok(v)) => String::byte_length(v), _ => 0 }}); 0 }}\n"
        ),
    );
    assert_wrong_type(&found, "Result", "Big");
}

/// The same, one level down inside a generic error's `catch` arm.
#[test]
fn a_boxed_catch_arm_matched_with_another_types_constructor_is_refused() {
    let found = refused(
        "wrong_ctor_boxed_catch",
        &format!(
            "{PRELUDE}fn main() -> Int {{ \
             print(fbo()! catch {{ Gx::X(s, Option::Some(Result::Ok(v))) => String::byte_length(v), _ => 0 }}); 0 }}\n"
        ),
    );
    assert_wrong_type(&found, "Result", "Big");
}

/// And inside a non-generic error's.
#[test]
fn a_boxed_catch_arm_of_a_plain_error_is_checked_too() {
    let found = refused(
        "wrong_ctor_boxed_catch_plain",
        &format!(
            "{PRELUDE}fn main() -> Int {{ \
             print(fng()! catch {{ Ng::A(Option::Some(Result::Ok(v))) => String::byte_length(v), _ => 0 }}); 0 }}\n"
        ),
    );
    assert_wrong_type(&found, "Result", "Big");
}

/// Panicked the code generator: an `Option` pattern over an `Int` field.
#[test]
fn a_nested_catch_pattern_over_an_int_field_is_refused() {
    let found = refused(
        "wrong_ctor_nested_catch",
        &format!(
            "{PRELUDE}fn main() -> Int {{ print(fi()! catch {{ Gx::X(s, Option::Some(v)) => v, _ => 0 }}); 0 }}\n"
        ),
    );
    assert_wrong_type(&found, "Option", "Int");
}

/// The same shapes at the right types run and give the right answers, with
/// nothing left alive.
#[test]
fn patterns_of_the_right_type_still_run() {
    let ran = run_both(
        "right_ctor_every_depth",
        &format!(
            "{PRELUDE}fn main() -> Int {{
  let o: Option<Result<Big, Int>> = Option::Some(Result::Ok(big()));
  print(match o {{ Option::Some(Result::Ok(v)) => String::byte_length(v.a), Option::Some(Result::Err(n)) => n, Option::None => 0 }});
  print(fbo()! catch {{ Gx::X(s, Option::Some(v)) => String::byte_length(v.a), _ => 0 }});
  print(fi()! catch {{ Gx::X(s, v) => v * 10, Gx::Y(n) => n }});
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n4\n30\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}
