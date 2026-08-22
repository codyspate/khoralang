#![cfg(feature = "llvm")]

//! Mutable fields, end to end.
//!
//! A `mut` field is shared *by reference*: two bindings to one record see each
//! other's writes, which is what a hash map needs and what a Go, Rust or
//! TypeScript reader expects a struct field to mean. That is also what makes a
//! cycle constructible for the first time, so Perceus stops being provably
//! complete here — see `docs/design/memory.md` §2 and §5a.

mod harness;

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

const MUT: &str = "module t;
fn print(value: Int);
fn khora_live_count() -> Int;

export type Counter = { mut count: Int };
export type Slot = { mut held: String };
";

/// The point of the whole feature: a field written after the record was built.
#[test]
fn a_mut_field_can_be_written() {
    let ran = run(
        "mut_write",
        &format!(
            "{MUT}
fn work() -> Int {{
  let c = {{ count: 0 }};
  c.count = 5;
  c.count = c.count + 1;
  c.count
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "6\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// **Reference semantics.** Two bindings to one record, and a function that
/// takes it, all see each other's writes. This is the decision D11 turned on.
#[test]
fn two_bindings_to_one_record_share_its_writes() {
    let ran = run(
        "mut_shared",
        &format!(
            "{MUT}
fn bump(c: Counter) -> () {{ c.count = c.count + 1; }}

fn work() -> Int {{
  let a = {{ count: 0 }};
  let b = a;
  bump(a);
  bump(b);
  bump(a);
  b.count
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n0\n", "every write is visible through every binding");
    assert_eq!(ran.code, Some(0));
}

/// Overwriting a boxed field releases what was there. Getting this backwards
/// is a leak on every write, which no other test would notice.
#[test]
fn overwriting_a_boxed_field_releases_the_old_value() {
    let ran = run(
        "mut_boxed",
        &format!(
            "{MUT}
fn work() -> Int {{
  let s = {{ held: \"first\" }};
  s.held = \"second\";
  s.held = \"third\";
  0
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n");
    assert_eq!(ran.code, Some(0));
}

/// `s.x = s.x` reads before it writes, and the read already duplicated the
/// reference. Storing before releasing is what stops it freeing what it just
/// wrote.
#[test]
fn assigning_a_field_to_itself_is_not_a_free() {
    let ran = run(
        "mut_self",
        &format!(
            "{MUT}
fn work() -> Int {{
  let s = {{ held: \"kept\" }};
  s.held = s.held;
  s.held = s.held;
  0
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n");
    assert_eq!(ran.code, Some(0));
}

/// A record reached through another record's field is still the same record.
#[test]
fn a_nested_record_is_written_through() {
    let ran = run(
        "mut_nested",
        &format!(
            "{MUT}
export type Pair = {{ left: Counter, right: Counter }};

fn work() -> Int {{
  let shared = {{ count: 10 }};
  let p = {{ left: shared, right: shared }};
  p.left.count = 20;
  p.right.count + shared.count
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n0\n", "one record behind three names");
    assert_eq!(ran.code, Some(0));
}
