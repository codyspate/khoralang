#![cfg(all(feature = "llvm", unix))]

//! What a compiled program does when the operating system asks it to stop.
//!
//! **Without these, deleting `khora-rt/src/signals.rs` leaves the suite
//! green.** The signal path is the one piece of the runtime that no unit test
//! can reach: it needs a real process, a real `kill`, and a wait on the real
//! exit status, because what is under test is precisely the difference between
//! a process that ends and one that does not.
//!
//! # Why the *sign* of the status is load-bearing
//!
//! A process that ran its finalizers and exited deliberately reports
//! `exit(130)`. One that was killed by `SIGTERM` reports `WIFSIGNALED` with
//! `WTERMSIG == 15`. A shell renders both as a number in `$?` and cannot tell
//! them apart; [`ExitStatus::signal`] can, and every assertion below reads it
//! rather than the code, because "it stopped" is not the claim — "it stopped
//! *the way it promised to*" is.
//!
//! # Why `khora-cli/tests` and not `khora-codegen-llvm/tests/fibers.rs`
//!
//! These programs need `std::core`'s `print`, `scoped` and `nursery`. The
//! codegen tests compile a bare `module t;` with no std prelude, which is why
//! an earlier attempt at a runtime-behaviour test there could not be written.
//!
//! # What these cost
//!
//! Each one builds a package with the real compiler — seconds, not
//! milliseconds — and then waits on a process. They are the slowest tests in
//! this crate and there is no cheaper way to ask the question.

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

mod pinned;

/// A built program, and the temporary directory it lives in.
struct Built {
    _tmp: tempfile::TempDir,
    exe: PathBuf,
}

/// Compiles `body` as a one-module package and answers where the binary is.
///
/// Panics with the compiler's own output on a failed build: a test that went
/// on to run a binary that was never produced would report "no such file",
/// which names the symptom two steps after the cause.
fn build(body: &str) -> Built {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let project = tmp.path().join("project");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");
    // The toolchain pin, because a project without one is refused, and these
    // live in the system temporary directory with no manifest above them to
    // inherit one from.
    std::fs::write(
        project.join("khora.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{}\"\n",
            khora_toolchain::RUNNING,
        ),
    )
    .expect("a manifest");
    std::fs::write(project.join("src").join("main.kh"), body).expect("a source file");

    let exe = project.join("app");
    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if let Some(archive) = pinned::runtime() {
        command.env("KHORA_RT_LIB", archive);
    }
    let out = command
        .args(["build", ".", "--out"])
        .arg(&exe)
        .current_dir(&project)
        .env("KHORA_HOME", &home)
        .output()
        .expect("could not run `khora`");
    assert!(
        out.status.success(),
        "the program under test did not build:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    Built { _tmp: tmp, exe }
}

/// A running program, held so a test can signal it and read what it said.
struct Running {
    child: Child,
    out: BufReader<ChildStdout>,
}

/// Starts the program and waits until it says it is ready.
///
/// **The readiness line rather than a sleep.** A signal that arrives before
/// the watcher thread is parked in `sigwait` is a different experiment from
/// the one intended, and a fixed sleep is a bet on a machine's load rather
/// than a fact about the program. Every program here prints `ready` as its
/// last act before the loop it is going to be stopped in.
///
/// `backend` is what `KHORA_FIBERS` is set to; an empty string unsets it,
/// which is the thread backend and the default.
fn start(built: &Built, backend: &str) -> Running {
    let mut command = Command::new(&built.exe);
    if backend.is_empty() {
        command.env_remove("KHORA_FIBERS");
    } else {
        command.env("KHORA_FIBERS", backend);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not start the program under test");
    let mut out = BufReader::new(child.stdout.take().expect("a piped stdout"));
    let mut line = String::new();
    out.read_line(&mut line).expect("reading the readiness line");
    assert_eq!(line.trim(), "ready", "the program did not reach its loop");
    Running { child, out }
}

impl Running {
    /// Sends a signal by name, through `kill(1)`.
    ///
    /// The crate has no `libc` dependency and this does not justify adding
    /// one: what a test needs from a signal is that the kernel delivered it,
    /// and `kill` is the same syscall behind a name this file can read.
    fn signal(&self, name: &str) {
        let sent = Command::new("kill")
            .args([&format!("-{name}"), &self.child.id().to_string()])
            .status()
            .expect("could not run kill(1)");
        assert!(sent.success(), "kill(1) refused to send {name}");
    }

    /// Waits for the program to end, or gives up and reports that it did not.
    ///
    /// **A hang is the failure most of these tests are looking for**, so the
    /// timeout has to end the process rather than the test run: a `wait` with
    /// no deadline turns a regression into a suite that never finishes, which
    /// is the shape CI cannot report on.
    ///
    /// Answers the signal that killed it (negated, as a `Child` reports it) or
    /// the code it exited with, plus everything it printed.
    fn finish(mut self, within: Duration) -> Ended {
        let deadline = Instant::now() + within;
        let status = loop {
            match self.child.try_wait().expect("waiting on the program") {
                Some(status) => break Some(status),
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let mut said = String::new();
        // Whatever is left in the pipe, including anything a finalizer wrote
        // on the way out. The write end is closed by now either way.
        let _ = std::io::Read::read_to_string(&mut self.out, &mut said);
        match status {
            Some(status) => Ended::Ended { signal: status.signal(), code: status.code(), said },
            None => Ended::StillRunning { said },
        }
    }
}

/// How a program ended, told apart the way `wait(2)` tells them apart.
enum Ended {
    /// It ended on its own. Exactly one of `signal` and `code` is set.
    Ended { signal: Option<i32>, code: Option<i32>, said: String },
    /// It outlived the deadline and was killed with `SIGKILL`.
    StillRunning { said: String },
}

impl Ended {
    /// Asserts a graceful stop: exit 130, not a signal death.
    ///
    /// 130 is what a cancellation reaching the entry point already produces
    /// (`docs/reference/traps.md`), so this is the status the table promises
    /// rather than a new one invented for signals.
    fn stopped_gracefully(&self, what: &str) -> &str {
        match self {
            Ended::Ended { signal: Some(signo), said, .. } => {
                panic!("{what}: killed by signal {signo} rather than stopping. It said: {said:?}")
            }
            Ended::Ended { code, said, .. } => {
                assert_eq!(*code, Some(130), "{what}: wrong status. It said: {said:?}");
                said
            }
            Ended::StillRunning { said } => {
                panic!("{what}: still running at the deadline. It said: {said:?}")
            }
        }
    }

    /// Asserts a real signal death: `WIFSIGNALED`, with `signo` in `WTERMSIG`.
    ///
    /// **Not `exit(128 + signo)`**, which prints the same number through a
    /// shell and means something else to a supervisor reading `WTERMSIG`.
    fn killed_by(&self, signo: i32, what: &str) {
        match self {
            Ended::Ended { signal: Some(got), said, .. } => {
                assert_eq!(*got, signo, "{what}: wrong signal. It said: {said:?}")
            }
            Ended::Ended { code: Some(code), said, .. } => panic!(
                "{what}: exited {code} rather than dying of a signal -- an exit that \
                 imitates one is what this is here to catch. It said: {said:?}"
            ),
            Ended::Ended { code: None, said, .. } => {
                panic!("{what}: neither a signal nor a code. It said: {said:?}")
            }
            Ended::StillRunning { said } => {
                panic!("{what}: still running at the deadline. It said: {said:?}")
            }
        }
    }
}

/// The backends, and the fact that they fail differently.
///
/// `on_the_scheduler()` reads `KHORA_FIBERS` and defaults to the thread
/// backend, so a test that ran only the default would be testing one of two
/// implementations of everything below.
const BACKENDS: [&str; 2] = ["", "scheduler"];

/// A program that parks in a fallible loop with a finalizer registered.
const SCOPED_FINALIZER: &str = r#"module app::main;

import std::core::{Scope, acquire, print, scoped};

pub type Stop = | Halted;

fn tick() -> () raises Stop { () }

fn spin() -> () with { scope: Scope } raises Stop {
  acquire(0, fn _ => print("FINALIZER"));
  print("ready");
  loop { tick()!; }
}

pub fn main() -> Int raises Stop {
  scoped(fn () => spin()!)!;
  0
}
"#;

/// The same, with a finalizer slow enough that a second signal can land while
/// it is running.
const SLOW_FINALIZER: &str = r#"module app::main;

import std::core::{Scope, acquire, print, scoped};
import std::clock::{Clock};

pub type Stop = | Halted;

fn tick() -> () raises Stop { () }

fn spin() -> () with { scope: Scope, clock: Clock } raises Stop {
  acquire(0, fn _ => clock.sleep(5000));
  print("ready");
  loop { tick()!; }
}

pub fn main() -> Int raises Stop {
  with { clock: Clock::real() } {
    scoped(fn () => spin()!)!;
  };
  0
}
"#;

/// Two children under one nursery, each with its own finalizer.
const TWO_CHILDREN: &str = r#"module app::main;

import std::core::{Channel, ChildFailed, Fiber, Nursery, Scope, acquire, nursery, print, scoped};

pub type Stop = | Halted;

fn tick() -> () raises Stop { () }

fn child(name: String, up: Channel<Int>) -> () with { scope: Scope } raises Stop {
  acquire(name, fn n => print("FINALIZER " + n));
  let _ = up.send(1);
  loop { tick()!; }
}

// `ready` waits for both children to hold their finalizers. A child the
// signal reaches before it starts never runs at all, so it has no finalizer
// to run, and printing `ready` straight after adopting them raced the
// second child's first turn.
fn both() -> () with { nursery: Nursery } raises Stop {
  let up: Channel<Int> = Channel::bounded(2);
  nursery.adopt(Fiber::spawn(fn () => scoped(fn () => child("one", up)!)!));
  nursery.adopt(Fiber::spawn(fn () => scoped(fn () => child("two", up)!)!));
  let _ = up.receive();
  let _ = up.receive();
  print("ready");
}

pub fn main() -> Int raises Stop + ChildFailed {
  nursery(fn () => both()!)!;
  0
}
"#;

/// The shape §1.1 is about: a `main` that reaches no cancellation point. A
/// loop would be one, and so would any runtime call, so it waits in a foreign
/// `pause()` -- third-party C, which runs to its end.
const INFALLIBLE: &str = r#"module app::main;

import std::core::{print};

extern fn pause() -> Int;

pub fn main() -> Int {
  print("ready");
  pause()
}
"#;

/// A `SIGTERM` runs the finalizers a cancellation would have run.
///
/// This is the whole promise: `std::db` says a transaction rolls back when its
/// fiber is cancelled, and before the watcher that promise was not kept on any
/// deploy, because a deploy is a `SIGTERM`.
#[test]
fn a_scoped_finalizer_runs_on_sigterm() {
    let built = build(SCOPED_FINALIZER);
    for backend in BACKENDS {
        let program = start(&built, backend);
        program.signal("TERM");
        let ended = program.finish(Duration::from_secs(10));
        let said = ended.stopped_gracefully(&format!("SIGTERM on {backend:?}"));
        assert!(said.contains("FINALIZER"), "the finalizer did not run on {backend:?}: {said:?}");
    }
}

/// `SIGINT` is `SIGTERM`. Ctrl-C is a request to stop.
#[test]
fn sigint_behaves_as_sigterm() {
    let built = build(SCOPED_FINALIZER);
    for backend in BACKENDS {
        let program = start(&built, backend);
        program.signal("INT");
        let ended = program.finish(Duration::from_secs(10));
        let said = ended.stopped_gracefully(&format!("SIGINT on {backend:?}"));
        assert!(said.contains("FINALIZER"), "the finalizer did not run on {backend:?}: {said:?}");
    }
}

/// A nursery's children both stop, and both run their finalizers.
///
/// **Transitively is the word that matters.** The signal reaches the root, and
/// what has to happen next is what cancelling a nursery does — otherwise a
/// server's connections keep their transactions open while `main` unwinds
/// around them.
///
/// **What this does not assert is the exit status**, because this shape does
/// not produce 130: the nursery absorbs its children's cancellations, returns
/// normally, and `main` runs on to its `0`. Measured on both backends. That is
/// a defect — a supervisor reading the status of a signalled shutdown is told
/// it succeeded — but it is a defect about *status* rather than about
/// unwinding, and pinning the wrong number here would freeze it. The
/// limitations page records it.
#[test]
fn a_two_child_nursery_stops_both_children_and_runs_both_finalizers() {
    let built = build(TWO_CHILDREN);
    for backend in BACKENDS {
        let program = start(&built, backend);
        program.signal("TERM");
        let ended = program.finish(Duration::from_secs(10));
        let said = ended.stopped_gracefully(&format!("two children on {backend:?}"));
        assert!(said.contains("FINALIZER one"), "the first child did not unwind: {said:?}");
        assert!(said.contains("FINALIZER two"), "the second child did not unwind: {said:?}");
    }
}

/// The second signal kills a process that is shutting down gracefully.
///
/// **The operator holds the deadline**, which is the reason there is no grace
/// period in the language: having asked once and waited, sending another is
/// how they say so. The finalizer here sleeps five seconds precisely so the
/// graceful path cannot finish first and make this test pass for the wrong
/// reason.
#[test]
fn the_second_signal_kills_a_process_shutting_down_gracefully() {
    let built = build(SLOW_FINALIZER);
    for backend in BACKENDS {
        let program = start(&built, backend);
        program.signal("TERM");
        std::thread::sleep(Duration::from_millis(500));
        program.signal("TERM");
        let ended = program.finish(Duration::from_secs(10));
        ended.killed_by(15, &format!("the second SIGTERM on {backend:?}"));
    }
}

/// A `main` that reaches no cancellation point dies rather than hanging.
///
/// **This is the regression the watcher introduced and the fallback closes.**
/// Such a program has nowhere for a cancellation to travel; before the watcher
/// existed `SIGTERM`
/// killed it outright, and a watcher that swallowed the signal instead would
/// make the simplest program anybody writes stop answering `kill` — which is
/// worse than the gap the watcher closes.
///
/// So the watcher notices nobody can hear it and restores the default
/// disposition. No finalizer is asserted here because there is none to run:
/// what is promised is that the process ends, the way every other program on
/// the machine does.
#[test]
fn an_infallible_main_dies_rather_than_hanging() {
    let built = build(INFALLIBLE);
    for backend in BACKENDS {
        let program = start(&built, backend);
        program.signal("TERM");
        let ended = program.finish(Duration::from_secs(10));
        ended.killed_by(15, &format!("an infallible main on {backend:?}"));
    }
}
