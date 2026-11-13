#![cfg(feature = "llvm")]

//! `attempt` and the retry policies built on it.
//!
//! `attempt` is the hinge: a tagged return already *is* "an error or a value",
//! and this is where a caller chooses which way to read it. Everything above it
//! — `retry`, `repeat` — is ordinary Khora written against that choice, which
//! is the claim these tests are really checking.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

/// Every `.kh` file of `std` for this target, plus the program under test.
///
/// **With `std`, because `attempt` is `std::core`'s.** A program's own
/// bodiless `attempt` is not the intrinsic: code generation builds its answer
/// to `std::core`'s type, and an own declaration of another type read one
/// value's bytes as another's.
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

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));

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

/// `retry` is written here rather than imported from `std::resilience`,
/// whose version also waits between attempts and so needs a clock and a
/// random source: this one is the loop alone, written against `attempt` --
/// which is the point: it is a library function, not a compiler feature.
const PRELUDE: &str = "module t;
import std::core::{Raised, Result, attempt};
fn print(value: Int);
extern fn khora_live_count() -> Int;
extern fn khora_tick() -> Int;

pub type Oops = | Bad;

pub type Schedule = { attempts: Int, };
impl Schedule {
  fn times(n: Int) -> Schedule { { attempts: if n < 1 { 1 } else { n } } }
}

pub fn retry<A, 'er, 'e>(schedule: Schedule, body: () -> A with 'e raises 'er) -> A
  with 'e
  raises 'er
{
  let mut left = schedule.attempts - 1;
  let mut outcome = attempt(body);
  while left > 0 {
    match outcome {
      Result::Ok(value) => left = 0,
      Result::Err(error) => {
        outcome = attempt(body);
        left = left - 1;
      }
    };
  }
  match outcome {
    Result::Ok(value) => value,
    Result::Err(error) => raise error,
  }
}
";

/// The whole hinge: a failure becomes a value a `match` can look at.
#[test]
fn attempt_turns_a_failure_into_a_value() {
    let ran = run(
        "attempt_value",
        &format!(
            "{PRELUDE}
fn halve(n: Int) -> Int raises Oops {{
  if n % 2 == 0 {{ n / 2 }} else {{ raise Oops::Bad }}
}}

fn describe(r: Result<Int, Raised<Oops>>) -> Int {{
  match r {{ Result::Ok(v) => v, Result::Err(e) => 0 - 1 }}
}}

pub fn main() -> Int {{
  print(describe(attempt(fn () => halve(8)!)));
  print(describe(attempt(fn () => halve(7)!)));
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n-1\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// A body that fails and then works is retried until it does.
#[test]
fn retry_runs_again_until_it_works() {
    let ran = run(
        "retry_works",
        &format!(
            "{PRELUDE}
/// Fails the first two times it is called, then works. Counting is the
/// runtime's job, so that the count survives being retried.
fn flaky() -> Int raises Oops {{
  let n = khora_tick();
  print(n);
  if n < 3 {{ raise Oops::Bad }}
  n
}}

pub fn main() -> Int raises Oops {{
  print(retry(Schedule::times(5), fn () => flaky()!)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n3\n", "three attempts, and the third's value");
    assert_eq!(ran.code, Some(0));
}

/// A schedule that runs out reports the last failure, because that is the one
/// that stopped the retrying.
#[test]
fn retry_gives_up_and_reports_the_last_failure() {
    let ran = run(
        "retry_gives_up",
        &format!(
            "{PRELUDE}
fn always_fails() -> Int raises Oops {{ print(1); raise Oops::Bad }}

pub fn main() -> Int raises Oops {{
  print(retry(Schedule::times(3), fn () => always_fails()!)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n1\n1\n", "three attempts, and no value after them");
    assert_eq!(ran.code, Some(1));
}

/// One attempt is the identity: a schedule of one does not retry.
#[test]
fn a_schedule_of_one_does_not_retry() {
    let ran = run(
        "retry_once",
        &format!(
            "{PRELUDE}
fn always_fails() -> Int raises Oops {{ print(1); raise Oops::Bad }}

pub fn main() -> Int raises Oops {{
  print(retry(Schedule::times(1), fn () => always_fails()!)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n");
    assert_eq!(ran.code, Some(1));
}

/// And nothing leaks along the way: not the closures, not the `Result`s built
/// for each attempt, not the error that finally came back.
#[test]
fn retrying_leaves_nothing_behind() {
    let ran = run(
        "retry_leaks",
        &format!(
            "{PRELUDE}
pub type Detail = | Of(text: String);
pub type Rich = | Bad(detail: Detail);

fn always_fails() -> Int raises Rich {{ raise Rich::Bad(Detail::Of(\"why\")) }}

fn work() -> Int {{
  let outcome = attempt(fn () => retry(Schedule::times(3), fn () => always_fails()!)!);
  match outcome {{ Result::Ok(v) => v, Result::Err(e) => 0 - 1 }}
}}

pub fn main() -> Int {{
  print(work());
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}
