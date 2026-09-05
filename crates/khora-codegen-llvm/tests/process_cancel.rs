#![cfg(feature = "llvm")]

//! What a cancelled fiber does to a child process it started.
//!
//! **The claim is that it does nothing to it.** `khora_spawn_status` calls
//! `Command::status`, which waits on the child and reaps it before returning,
//! and that wait is not interruptible. So a fiber cancelled while a child is
//! running does not abandon the child: the child runs to completion, its exit
//! status is collected, and only then does the fiber notice it was cancelled
//! and unwind at its next cancellation point.
//!
//! That is the *right* behaviour and it is worth pinning, because the obvious
//! alternative — tearing the wait down so the cancellation lands promptly —
//! would leave a running program with nobody waiting on it. On Unix that is a
//! zombie until the Khora process exits; on Windows it is a live process with
//! a leaked handle. Neither is something a cancellation should produce.
//!
//! # Three answers, not two
//!
//! The child writes one file when it starts and another when it finishes, with
//! a sleep between them, so the pair says how far it got: neither means it
//! never ran, the first alone means it was killed part way, both mean it
//! finished. A single marker cannot tell the first two apart, and they are
//! wrong in completely different ways — one is a cancellation that landed
//! before the call, the other is a wait that was torn down.
//!
//! The elapsed time is the fourth check, and it is the one a file cannot make:
//! a wait cut short returns visibly early even when the child later finishes
//! on its own.
//!
//! # The control is not optional
//!
//! Everything here turns on a file appearing. A suite where the shell line was
//! simply wrong would fail exactly the same way as one where the child was
//! abandoned, so the first test runs the same program without cancelling
//! anything.
//!
//! # Cancelling from outside, which needed a nursery
//!
//! There is no `Fiber::cancel` — `khora_cancel` sets the flag on the *running*
//! fiber, which `net_cancel.rs` explains at more length. A fiber that cancels
//! itself and then calls something is a different situation from one cancelled
//! while already inside a call, and both are here because they turn out to
//! behave differently:
//!
//! - Cancelled first, the child is **never started**. The flag is noticed at
//!   the `!` the call needs, before the call rather than after it.
//! - Cancelled during the wait, the child **runs to completion**.
//!
//! The second needs a cancellation from outside, and a nursery is what
//! delivers one: the first child's failure cancels its siblings. Adoption
//! order matters — see the test — and the sibling waits before failing so the
//! holder is certainly inside the wait rather than still being spawned.

use crate::harness;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// How long the child sleeps between its two marks, in seconds.
///
/// Long enough that a wait which returned early returns *visibly* early on a
/// loaded machine, and short enough that a test suite does not notice.
const SLEEP: u64 = 2;

fn std_sources(db: &KhoraDatabase) -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
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
    out
}

/// How far the child got.
#[derive(Debug)]
struct Ran {
    started: bool,
    finished: bool,
}

/// A shell line that marks its start, sleeps, and marks its finish.
///
/// Forward slashes on both platforms: `cmd` accepts them in a redirection
/// target, and a backslash would have to survive being written into a Khora
/// string literal on the way here.
fn slow_child(started: &Path, finished: &Path) -> String {
    let began = started.display().to_string().replace('\\', "/");
    let ended = finished.display().to_string().replace('\\', "/");
    if cfg!(windows) {
        format!(
            "echo yes > {began} & ping -n {n} 127.0.0.1 > nul & echo yes > {ended}",
            n = SLEEP + 1
        )
    } else {
        format!("echo yes > {began}; sleep {SLEEP}; echo yes > {ended}")
    }
}

/// Builds and runs a program that starts `slow_child`, cancelling first or not.
fn cancelling(name: &str, cancel: bool) -> (String, Duration, Ran) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let started = dir.join("the-child-started");
    let finished = dir.join("the-child-finished");
    let _ = std::fs::remove_file(&started);
    let _ = std::fs::remove_file(&finished);
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let first = if cancel { "khora_cancel();" } else { "" };
    let main = format!(
        "module demo::main;

import std::core::{{Fiber, Result, Scope, attempt, scoped}};
import std::process::{{Process, ProcessError}};

fn print(value: String);
extern fn khora_cancel();

pub type Oops = | Bad;

fn held() -> () with {{ process: Process }} raises Oops {{
  print(\"the child is starting\");
  {first}
  let _ = attempt(fn () => process.shell(\"{command}\")!);
  print(\"the fiber reached the end\")
}}

pub fn main() -> Int {{
  let f = Fiber::spawn(fn () =>
    with {{ process: Process::real() }} {{ scoped(fn () => held()!)! }});
  Fiber::wait(f);
  print(\"the fiber was joined\");
  0
}}
",
        command = slow_child(&started, &finished)
    );

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(&db, dir.join("main.kh"), main));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let began = Instant::now();
    let output = Command::new(&exe).output().expect("the program should run");
    let elapsed = began.elapsed();
    let said = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    (said, elapsed, Ran { started: started.exists(), finished: finished.exists() })
}

/// **The control.** The same program with nothing cancelled, so that a wrong
/// shell line fails here rather than being read below as an abandoned child.
#[test]
fn the_child_leaves_its_marks_when_nothing_is_cancelled() {
    let (said, elapsed, ran) = cancelling("process_control", false);
    assert!(said.contains("the fiber reached the end"), "the fiber should finish: {said}");
    assert!(ran.started && ran.finished, "the command itself is wrong: {ran:?}\n{said}");
    assert!(elapsed.as_secs() >= SLEEP, "the child cannot have slept: {elapsed:?}");
}

/// **A fiber already cancelled never starts the child at all**, which is the
/// better of the two things that could happen, and was not what was expected.
///
/// The cancellation is noticed at the `!` the call needs *before* the call is
/// made rather than after it, so there is no child to orphan. Worth pinning on
/// its own: it means a cancelled fiber cannot launch a program, where one that
/// checked only afterwards would launch one every time.
///
/// It also means this program cannot test the item below. Cancelling yourself
/// and then calling something is not being cancelled while inside it.
#[test]
fn a_fiber_cancelled_before_the_call_never_starts_the_child() {
    let (said, elapsed, ran) = cancelling("process_cancel", true);

    assert!(said.contains("the child is starting"), "the fiber never got going: {said}");
    assert!(
        !ran.started && !ran.finished,
        "the child ran, so the cancellation was noticed after the call rather than before it. \
         {ran:?}\n{said}"
    );
    assert!(
        elapsed.as_secs() < SLEEP,
        "nothing should have waited for a child that was never started: {elapsed:?}"
    );
    assert!(
        !said.contains("the fiber reached the end"),
        "the cancellation should have landed at the `!` the run needed, and did not: {said}"
    );
    assert!(said.contains("the fiber was joined"), "the fiber should have been joined: {said}");
}

/// **The one the item is actually about: cancelled *during* the wait.**
///
/// There is no `Fiber::cancel`, so the cancellation has to come from something
/// that already cancels from outside — and a nursery does. Two children: one
/// starts the slow program, the other fails at once. The first failure cancels
/// the siblings, which arrives while the first child is inside
/// `Command::status` with a program running.
///
/// Both marks have to be there afterwards. The child was started, nothing tore
/// the wait down under it, and it was waited for rather than left running with
/// nobody to reap it.
#[test]
fn a_child_cancelled_mid_wait_still_finishes_and_is_reaped() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("process_nursery");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let started = dir.join("the-child-started");
    let finished = dir.join("the-child-finished");
    let _ = std::fs::remove_file(&started);
    let _ = std::fs::remove_file(&finished);
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let main = format!(
        "module demo::main;

import std::clock::{{Clock}};
import std::core::{{ChildFailed, Fiber, Nursery, Result, attempt, nursery}};
import std::process::{{Process, ProcessError}};

fn print(value: String);

pub type Oops = | Bad;

/// A fallible call, so that `!` is a cancellation point. It never fails.
fn mark() -> Int raises Oops {{ 1 }}

/// Runs the slow program. Cancelled from outside while it is inside the wait.
///
/// The `!` afterwards is what proves the cancellation arrived: if the nursery
/// had not cancelled this fiber, the last line would print.
fn slow() -> () raises Oops {{
  with {{ process: Process::real() }} {{
    let _ = attempt(fn () => process.shell(\"{command}\")!);
  }};
  let _ = mark()!;
  print(\"the slow child ran on, which is wrong\")
}}

/// Fails, and is what cancels the sibling.
///
/// **It waits first, and the wait is the whole reliability of this test.** The
/// sibling has to be *inside* the blocking wait when the cancellation lands,
/// and it can only get there after it has been spawned and has started its own
/// child. Half a second against a child that runs for {SLEEP}s is a margin
/// nothing here has to win a race for.
fn doomed() -> () raises Oops {{
  with {{ clock: Clock::real() }} {{ clock.sleep(500) }};
  raise Oops::Bad
}}

/// **Adoption order is load-bearing.** A nursery notices a child's failure in
/// the order it adopted them, so with the slow one first its sibling's failure
/// is not looked at until the slow one has already finished -- and nothing is
/// ever cancelled. Written that way round first, and the `ran on` assertion
/// below is what caught it.
fn both() -> () with {{ nursery: Nursery }} {{
  nursery.adopt(Fiber::spawn(fn () => doomed()!));
  nursery.adopt(Fiber::spawn(fn () => slow()!));
}}

pub fn main() -> Int {{
  let _ = attempt(fn () => nursery(fn () => both())!);
  print(\"the nursery closed\");
  0
}}
",
        command = slow_child(&started, &finished)
    );

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(&db, dir.join("main.kh"), main));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling the nursery program failed:\n  {}", messages.join("\n  "));
    }

    let began = Instant::now();
    let output = Command::new(&exe).output().expect("the program should run");
    let elapsed = began.elapsed();
    let said = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    let ran = Ran { started: started.exists(), finished: finished.exists() };

    assert!(
        ran.started,
        "the sibling failed before the slow child was even started, so this proves nothing about \
         a cancellation arriving mid-wait. {ran:?}\n{said}"
    );
    assert!(
        ran.finished,
        "the child was abandoned part way: the cancellation tore down a wait that had a program \
         running under it. {ran:?}\n{said}"
    );
    assert!(
        elapsed.as_secs() >= SLEEP,
        "the nursery closed after {elapsed:?}, less than the child's own {SLEEP}s: it stopped \
         waiting for a child that was still running"
    );
    assert!(
        !said.contains("ran on, which is wrong"),
        "the slow fiber was never cancelled, so this proves nothing about a cancellation \
         arriving mid-wait -- it only proves a child runs. {said}"
    );
    assert!(said.contains("the nursery closed"), "the nursery should have closed: {said}");
}
