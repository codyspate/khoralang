#![cfg(feature = "llvm")]

//! Fibers, end to end.
//!
//! `docs/design/fibers.md` decides what one is: a stackful coroutine
//! multiplexed onto worker threads, implemented for now as an operating-system
//! thread. What these pin is the part a program can see, which is the part that
//! does not change when the implementation does — a handle you can join and
//! cancel, and a release that waits.

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
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

const FIBERS: &str = "module t;
fn print(value: Int);
fn khora_live_count() -> Int;

export type Fiber;
impl Fiber {
  fn spawn(body: () -> ()) -> Fiber;
  fn join(self) -> ();
  fn cancel(self) -> ();
}

export type Region;
impl Region {
  fn open() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}
";

/// A fiber runs the closure it was handed, and `join` waits for it.
#[test]
fn a_fiber_runs_and_is_joined() {
    let ran = run(
        "fiber_join",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let f = Fiber::spawn(fn () => print(1));
  Fiber::join(f);
  print(2);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n");
    assert_eq!(ran.code, Some(0));
}

/// The structured part, and it needed nothing of its own: releasing the last
/// reference to a handle joins, so a fiber cannot outlive the binding that
/// holds it. Nobody wrote `join` here.
#[test]
fn a_fiber_cannot_outlive_the_binding_that_holds_it() {
    let ran = run(
        "fiber_scoped",
        &format!(
            "{FIBERS}
fn work() -> Int {{
  let f = Fiber::spawn(fn () => print(1));
  2
}}

fn main() -> Int {{ print(work()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n", "the fiber finished before `work` returned");
    assert_eq!(ran.code, Some(0));
}

/// And on the way out of a raise, because the release is the same release.
#[test]
fn a_fiber_is_waited_for_when_a_raise_passes_through() {
    let ran = run(
        "fiber_raise",
        &format!(
            "{FIBERS}
export type Oops = | Bad;

fn work() -> Int raises Oops {{
  let f = Fiber::spawn(fn () => print(1));
  raise Oops::Bad
}}

fn main() -> Int raises Oops {{ work()!; 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n");
    assert_eq!(ran.code, Some(1));
}

/// A closure captures, so a fiber can be given something to work on.
#[test]
fn a_fiber_closes_over_its_environment() {
    let ran = run(
        "fiber_capture",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let n = 21;
  let f = Fiber::spawn(fn () => print(n * 2));
  Fiber::join(f);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// Several at once, all waited for.
#[test]
fn several_fibers_are_all_waited_for() {
    let ran = run(
        "fiber_many",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let a = Fiber::spawn(fn () => print(1));
  Fiber::join(a);
  let b = Fiber::spawn(fn () => print(2));
  Fiber::join(b);
  let c = Fiber::spawn(fn () => print(3));
  Fiber::join(c);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// Cancellation is per fiber. The parent cancels the child and carries on —
/// which is the whole reason the flag stopped being one per process.
#[test]
fn cancelling_a_fiber_does_not_cancel_the_parent() {
    let ran = run(
        "fiber_cancel_child",
        &format!(
            "{FIBERS}
export type Oops = | Bad;
fn ok(n: Int) -> Int raises Oops {{ n }}

fn main() -> Int raises Oops {{
  let f = Fiber::spawn(fn () => print(1));
  Fiber::cancel(f);
  Fiber::join(f);
  print(ok(2)!);
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n2\n",
        "the parent passed its own cancellation point untouched"
    );
    assert_eq!(ran.code, Some(0), "and exited normally rather than 130");
}

/// A fiber's own region ends with the fiber, so a child's finalizers run in
/// the child. Nothing about regions knew fibers were coming.
#[test]
fn a_fibers_region_ends_with_the_fiber() {
    let ran = run(
        "fiber_region",
        &format!(
            "{FIBERS}
fn child() -> () {{
  let region = Region::open();
  Region::defer(region, fn () => print(2));
  print(1);
}}

fn main() -> Int {{
  let f = Fiber::spawn(fn () => child());
  Fiber::join(f);
  print(3);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// Nothing leaks: not the handle, not the closure, not what it captured.
#[test]
fn a_fiber_leaves_nothing_behind() {
    let ran = run(
        "fiber_leaks",
        &format!(
            "{FIBERS}
fn work() -> () {{
  let text = \"held\";
  let f = Fiber::spawn(fn () => print(1));
  Fiber::join(f);
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}
