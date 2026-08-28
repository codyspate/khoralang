#![cfg(feature = "llvm")]

//! `Clock.sleep`, compiled and run.
//!
//! Two claims, and the second is the one the design is for.
//!
//! **A real sleep really waits**, which is checked against the monotonic clock
//! rather than against a stopwatch in Rust, so the test measures what the
//! program measures.
//!
//! **A fake clock does not**, and needs nothing but a handler to say so. That
//! is the whole reason `sleep` is an operation on `Clock` rather than an
//! intrinsic: a language whose sleep is ambient has to grow a parallel test
//! clock the runtime knows about, plus documentation warning you to fork the
//! sleeping code or deadlock. Here the capability is the seam and a fake is
//! four lines — `docs/design/effect-survey.md` §3.1.
//!
//! The concurrency claim underneath — that a sleeping *fiber* gives its worker
//! back — cannot be asserted on wall time without making a flaky test out of a
//! busy machine. What is checked instead is that more fibers than there are
//! workers all sleep and all finish, which is false if a sleep holds a worker
//! and the pool is smaller than the crew.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

const HEAD: &str = r#"module demo::main;
import std::core::{Eq, Fiber, Fibers, Nursery, Ord, Shared, Show, print};
import std::clock::{Clock};

/// How long the clock says a sleep of `millis` took.
fn measured(millis: Int) -> Int with { clock: Clock } {
  let start = clock.monotonic_millis();
  clock.sleep(millis);
  clock.monotonic_millis() - start
}
"#;

fn run(name: &str, body: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        // The clock and nothing else. Before `Clock` had a module of its own
        // this needed `env_native.kh`, and `permissions.kh` and `grants.kh`
        // behind it, to compile a program that only wanted to sleep.
        SourceFile::new(&db, dir.join("clock_native.kh"), std_source("clock_native.kh")),
        SourceFile::new(&db, dir.join("main.kh"), format!("{HEAD}\n{body}\n")),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// **The real clock really waits**, and the program's own clock says so.
///
/// A lower bound only. An upper bound would be a claim about scheduling
/// latency on whatever machine happens to be running the suite, which is how a
/// timing test becomes a flaky one.
#[test]
fn a_real_sleep_takes_at_least_as_long_as_it_was_asked_for() {
    let out = run(
        "sleep_real",
        r#"fn main() -> () {
  with { clock: Clock::real() } {
    print(if measured(60) >= 55 { "waited" } else { "returned early" });
    // Zero returns at once and is not an error.
    print(if measured(0) < 50 { "no wait" } else { "waited anyway" });
    print(if measured(0 - 5) < 50 { "no wait" } else { "waited anyway" });
  }
}"#,
    );

    assert_eq!(out, "waited\nno wait\nno wait\n");
}

/// **A fake clock is a handler and nothing else.**
///
/// The test that would need a whole test runtime in a language where sleeping
/// is ambient. Here it is four lines, and the sleeping code is not written
/// differently to accommodate it — `measured` is the same function.
#[test]
fn a_fake_clock_does_not_wait_and_needs_no_runtime_support() {
    let out = run(
        "sleep_fake",
        r#"fn main() -> () {
  // Time advances only when somebody sleeps, and by exactly as much.
  let now = Shared::of(0);
  with { clock: handler for Clock {
    unix_seconds: fn () => 0,
    unix_millis: fn () => 0,
    monotonic_millis: fn () => Shared::get(now),
    sleep: fn millis => { Shared::update(now, fn t => t + millis); () },
  } } {
    print(Int::to_string(measured(60)));
    print(Int::to_string(measured(900000)));
    print(Int::to_string(Shared::get(now)));
  }
}"#,
    );

    // Fifteen minutes of waiting, instantly, and the clock agrees it happened.
    assert_eq!(out, "60\n900000\n900060\n");
}

/// **More sleeping fibers than workers, and all of them finish.**
///
/// False if a sleeping fiber holds its worker: the pool would have nothing
/// left to run the ones that had not started, and the nursery would wait for
/// children that could never be scheduled.
#[test]
fn many_fibers_sleep_at_once() {
    let out = run(
        "sleep_fibers",
        r#"fn napper(done: Shared<Int>) -> () with { clock: Clock } {
  clock.sleep(40);
  Shared::update(done, fn n => n + 1);
}

fn main() -> () {
  with { clock: Clock::real() } {
    let done = Shared::of(0);
    let crew = Fibers::open();
    with { nursery: handler for Nursery { adopt: fn f => Fibers::adopt(crew, f) } } {
      let mut spawned = 0;
      while spawned < 64 {
        nursery.adopt(Fiber::spawn(fn () => napper(done)));
        spawned = spawned + 1
      }
    };
    Fibers::wait(crew);
    print(Int::to_string(Shared::get(done)));
  }
}"#,
    );

    assert_eq!(out, "64\n");
}
