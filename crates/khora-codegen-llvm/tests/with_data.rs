#![cfg(feature = "llvm")]

//! `with_data` with a lambda written in place builds its closure in the
//! caller's frame.
//!
//! **What these guard: a heap allocation per call that nobody wrote.**
//! `String::join` and `String::escape_html` lend their bytes through
//! `with_data`, and each call built a closure object on the heap that lived
//! exactly as long as the call. On a page request that was 102 of 616
//! allocations. The counting tests below read `khora_alloc_count` across a
//! loop of calls, and were 100 and 200 before the closure moved into the frame.
//!
//! **And the frame closure must behave as the heap one did.** A capture is
//! released on every way out of the call: a raise from the body, a `return`
//! in it, a cancellation inside it. The live-object delta over repeated calls
//! is 0 when nothing leaks and negative when a capture is released twice, and
//! each program runs on both fiber backends.
//!
//! Allocation counts are the compiler's own instrument and not a promise to
//! anybody (`docs/design/compatibility.md`), which is why they live here and
//! not in the std-suite.

use crate::harness;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

fn build(name: &str, source: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on `backend` under a 30 s watchdog, and hands back its stdout.
/// The cancellation program is a hang if the fiber never stops.
fn run(exe: &PathBuf, backend: &str) -> String {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    while child.try_wait().expect("waiting").is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("`{backend}`: {} did not finish in 30 s", exe.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    assert_eq!(status.code(), Some(0), "`{backend}` exited badly: {stderr}");
    stdout.replace("\r\n", "\n")
}

/// Builds `PRELUDE` + `body` and checks stdout on both fiber backends.
fn check(name: &str, body: &str, expected: &str) {
    let exe = build(name, &format!("{PRELUDE}{body}"));
    for backend in ["threads", "scheduler"] {
        assert_eq!(run(&exe, backend), expected, "`{backend}`");
    }
}

const PRELUDE: &str = r#"module main;
import std::core::{Array, Channel, Fiber, List, print};

extern fn khora_alloc_count() -> Int;
extern fn khora_live_count() -> Int;

type Bad = | Bad(n: Int);
"#;

/// **The count this change is for.** A literal body, alone and nested inside
/// another the way `String::join` nests them, called a hundred times each.
/// Each figure is read before anything is printed, so it is the loop's alone.
///
/// The nested pair is the prototype's open question: each literal has its own
/// frame slot, written afresh on every turn, and the outer one captures the
/// array the inner one lends.
#[test]
fn a_literal_body_allocates_no_closure() {
    check(
        "with_data_literal",
        r#"
pub fn main() -> Int {
  let s = "khora";
  let buf: Array<U8> = Array::new(3, 0);
  let mut total = 0;
  let mut i = 0;
  let live = khora_live_count();
  let before = khora_alloc_count();
  while i < 100 { total = total + String::with_data(s, fn (_p, n) => n); i = i + 1; };
  let plain = khora_alloc_count() - before;
  i = 0;
  let again = khora_alloc_count();
  while i < 100 {
    total = total + String::with_data(s, fn (_p, n) => Array::with_data(buf, fn (_q, m) => n * m));
    i = i + 1;
  };
  let nested = khora_alloc_count() - again;
  let leaked = khora_live_count() - live;
  print("total ${total} plain ${plain} nested ${nested} leaked ${leaked}");
  0
}
"#,
        "total 2000 plain 0 nested 0 leaked 0\n",
    );
}

/// **A captured value is held for the call and released after it.** Each
/// call captures a string built for it, so a missing release keeps that
/// string alive and shows as a live delta of 100. A double release shows as a
/// negative delta, or as a crash reading `tail` afterwards. The allocation
/// count is taken over a separate loop with one shared capture, so the
/// strings built for the first loop don't count.
#[test]
fn a_captured_value_is_released_after_the_call() {
    check(
        "with_data_capture",
        r#"
pub fn main() -> Int {
  let s = "khora";
  let tail = "ab" + "cd";
  let mut total = 0;
  let mut i = 0;
  let live = khora_live_count();
  while i < 100 {
    let fresh = "t${i}";
    total = total + String::with_data(s, fn (_p, n) => n + String::byte_length(fresh));
    i = i + 1;
  };
  let leaked = khora_live_count() - live;
  i = 0;
  let before = khora_alloc_count();
  while i < 100 { total = total + String::with_data(s, fn (_p, n) => n + String::byte_length(tail)); i = i + 1; };
  let spent = khora_alloc_count() - before;
  print("total ${total} spent ${spent} leaked ${leaked} tail ${tail}");
  0
}
"#,
        "total 1690 spent 0 leaked 0 tail abcd\n",
    );
}

/// **Every way out of the body releases the captures:** a raise propagated
/// with `!`, a `return` inside the lambda, and a cancellation while the body
/// is running. Ten rounds of each, counted by the live delta; the calls that
/// finish normally are counted for allocations too.
///
/// `return` inside a lambda leaves the lambda, not the function around it,
/// which is why `early` answers 1 and not 7.
#[test]
fn a_raise_return_or_cancel_in_the_body_releases_the_captures() {
    check(
        "with_data_exits",
        r#"
fn strict(s: String, tail: String) -> Int raises Bad {
  String::with_data(s, fn (_p, n) => if n > 3 { raise Bad::Bad(n + String::byte_length(tail)) } else { n })!
}

fn early(s: String, tail: String) -> Int {
  let got = String::with_data(s, fn (_p, n) => { if n > 3 { return String::byte_length(tail) - 1; }; n });
  if got == 1 { 1 } else { 7 }
}

fn spin(s: String, tail: String, ready: Channel<Int>) -> Int {
  String::with_data(s, fn (_p, n) => {
    Channel::send(ready, String::byte_length(tail));
    let mut i = n;
    loop { i = i + 1; }
  })
}

pub fn main() -> Int {
  let tail = "ab" + "cd";
  let mut sink = 0;
  let mut j = 0;
  let live = khora_live_count();
  while j < 10 { sink = sink + (strict("khora", "t${j}")! catch { Bad::Bad(k) => k }); j = j + 1; };
  let raised = khora_live_count() - live;
  let before = khora_alloc_count();
  j = 0;
  while j < 10 { sink = sink + (strict("abc", tail)! catch { Bad::Bad(k) => k }); j = j + 1; };
  let spent = khora_alloc_count() - before;
  let quiet = khora_live_count() - live;
  j = 0;
  while j < 10 { sink = sink + early("khora", "t${j}"); j = j + 1; };
  let returned = khora_live_count() - live;
  let mut stopped = 0;
  j = 0;
  let parked = khora_live_count();
  while j < 10 {
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => spin("khora", "t${j}", ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    Fiber::wait(f);
    if Fiber::canceled(f) { stopped = stopped + 1; };
    j = j + 1;
  };
  let canceled = khora_live_count() - parked;
  print("sink ${sink} raised ${raised} spent ${spent} quiet ${quiet} returned ${returned}");
  print("stopped ${stopped} canceled ${canceled} tail ${tail}");
  0
}
"#,
        "sink 110 raised 0 spent 0 quiet 0 returned 0\nstopped 10 canceled 0 tail abcd\n",
    );
}

/// **Anything but a lambda written in place keeps the heap closure.** A named
/// function is wrapped in an adapter object, and a lambda bound to a name is
/// built where it is bound; either can outlive the call, so neither may live
/// in a frame this call does not own. One allocation per call, each freed.
///
/// This one passes with or without the frame closure: it is the other half of
/// the rule, and it fails if the frame path is taken for something it must
/// not be.
#[test]
fn a_body_that_is_not_a_literal_still_allocates_and_still_works() {
    check(
        "with_data_not_literal",
        r#"
fn measure(_p: Ptr, n: Int) -> Int { n }

pub fn main() -> Int {
  let s = "khora";
  let tail = "ab" + "cd";
  let mut total = 0;
  let mut i = 0;
  let live = khora_live_count();
  let before = khora_alloc_count();
  while i < 100 { total = total + String::with_data(s, measure); i = i + 1; };
  let named = khora_alloc_count() - before;
  i = 0;
  let again = khora_alloc_count();
  while i < 100 {
    let f = fn (_p: Ptr, n: Int) => n + String::byte_length(tail);
    total = total + String::with_data(s, f);
    i = i + 1;
  };
  let bound = khora_alloc_count() - again;
  let leaked = khora_live_count() - live;
  print("total ${total} named ${named} bound ${bound} leaked ${leaked} tail ${tail}");
  0
}
"#,
        "total 1400 named 100 bound 100 leaked 0 tail abcd\n",
    );
}
