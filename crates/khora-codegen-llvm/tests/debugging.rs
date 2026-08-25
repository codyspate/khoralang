#![cfg(feature = "llvm")]

//! What a trap says about where it happened.
//!
//! `khora_bounds_fail`'s own doc comment said "the useful thing to do is say
//! where", and until the compiler emitted line tables there was no way for it
//! to. Both traps named what had happened — `Int addition overflowed`, `index 7
//! is outside an array of 3` — in a program of any size, with nothing
//! connecting the message to a line.
//!
//! These run the whole path: Khora source, DWARF or CodeView emitted by the
//! backend, kept through the link, symbolized at runtime by the executable's
//! own debug information. Asserting on the *output of a program that trapped*
//! rather than on the metadata, because every one of those steps has already
//! been a place where it silently stopped working — the object carried
//! `.debug$S` and `.debug$T` for a while before anybody noticed the linker was
//! discarding both.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Compiles `source`, runs it with backtraces on, and returns what it printed
/// to stderr.
fn trap_of(name: &str, source: &str) -> String {
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

    let output = Command::new(&exe)
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("the program should run");
    assert_eq!(output.status.code(), Some(134), "a trap exits 134");
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

/// Overflow deep in a call chain names the function and the line it happened
/// on, and the callers underneath it.
#[test]
fn an_overflow_says_which_line_overflowed() {
    let out = trap_of(
        "trap_overflow_where",
        "module t;
fn print(value: Int);

fn deep(n: Int, big: Int) -> Int {
  big + n
}

fn middle(n: Int, big: Int) -> Int {
  deep(n, big) + 1
}

fn main() -> Int {
  let big = 9223372036854775807;
  print(middle(1, big));
  0
}
",
    );

    assert!(out.contains("overflowed"), "it still says what happened: {out}");
    assert!(out.contains("main.kh:5"), "the line that overflowed: {out}");
    // The callers, so a trap in a helper can be traced back to the request
    // that reached it — which is the whole reason a backtrace beats a line.
    assert!(out.contains("main.kh:9"), "the caller: {out}");
    assert!(out.contains("main.kh:14"), "and its caller: {out}");
    // Named, not mangled. `create_function` is given both, and a reader wants
    // the one they wrote.
    assert!(out.contains("deep"), "the function's own name: {out}");
}

/// **The runtime's own frames are not the answer.** Six frames of
/// `backtrace_rs` and `force_capture` sat above the line that trapped, and the
/// top of a backtrace is the part anybody reads first.
#[test]
fn the_runtimes_frames_are_not_at_the_top() {
    let out = trap_of(
        "trap_no_runtime_frames",
        "module t;
fn print(value: Int);

fn main() -> Int {
  let big = 9223372036854775807;
  print(big + 1);
  0
}
",
    );
    assert!(!out.contains("backtrace_rs"), "the capture machinery is trimmed: {out}");
    assert!(!out.contains("khora_rt::trap"), "and so is the handler: {out}");
    assert!(out.contains("main.kh:6"), "leaving the line that trapped: {out}");
}

/// Without the switch, a trap says how to get the rest. A backtrace on every
/// trap costs every well-behaved program a page of stack on the way out, and
/// the first thing anybody does with a bug is run it again.
#[test]
fn a_trap_without_the_switch_says_how_to_get_more() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("trap_quiet");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let source = "module t;
fn print(value: Int);

fn main() -> Int {
  let big = 9223372036854775807;
  print(big + 1);
  0
}
";
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::compile(&db, root, &exe).expect("it compiles");

    let output = Command::new(&exe)
        .env_remove("RUST_BACKTRACE")
        .output()
        .expect("the program should run");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("overflowed"), "what happened is always said: {err}");
    assert!(err.contains("RUST_BACKTRACE=1"), "and how to learn where: {err}");
}

/// An index outside its array, which is the other trap and reaches the same
/// place by a different route.
#[test]
fn a_bounds_failure_says_which_line_indexed() {
    let out = trap_of(
        "trap_bounds_where",
        "module t;
fn print(value: Int);

export type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn get(self, index: Int) -> A;
}

fn main() -> Int {
  let xs = Array::new(3, 0);
  print(Array::get(xs, 7));
  0
}
",
    );
    assert!(out.contains("outside an array"), "it still says what happened: {out}");
    assert!(out.contains("main.kh:12"), "the line that indexed: {out}");
}
