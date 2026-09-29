#![cfg(feature = "llvm")]

//! Every way a value reaches another fiber, in one program, counted to zero.
//!
//! **What this guards: a reference count that two threads adjust at once.**
//! Values cross by several routes: a spawn capture, a join (twice), a
//! channel send while the sender keeps reading, a `Shared`, a nursery child
//! and `outcome`. On each route both sides hold the same header and count
//! it. If any route leaves the count to plain arithmetic, the two threads
//! lose updates. The program then ends with objects still live, frees one
//! twice, or crashes. The fixture has each side read the value thousands
//! of times, so a lost update is close to certain, not a rare race.
//!
//! **It is shown to fail.** [`the_fixture_fails_when_counts_are_not_atomic`]
//! builds the same program with every count forced plain and requires it to
//! go wrong. Without that control, this file could pass while counting
//! nothing. A later change that makes some counts plain has to keep this
//! green, and the control shows it would go red.
//!
//! A separate binary, because it sets `KHORA_UNBOXED` and the environment
//! belongs to the process. See `tests/suite.rs`.

mod harness;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The fixture. Each section builds a record of a string and a list of
/// strings on one fiber and reads it on another while the first keeps
/// reading its own reference. The total is fixed by the sizes, so a wrong
/// read shows as a wrong number as well as a crash or a leak.
const CROSSINGS: &str = "module main;

import std::core::{print, Fiber, Fibers, Channel, Shared, List, Option, Outcome, ChildFailed};

extern fn khora_live_count() -> Int;

type Rec = { name: String, tags: List<String> };

fn make(n: Int) -> Rec {
  let mut tags = List::Nil;
  let mut i = 0;
  while i < n { tags = List::Cons(\"t${i}\", tags); i = i + 1; };
  { name: \"rec-${n}\", tags: tags }
}

fn read(r: Rec, times: Int) -> Int {
  let mut i = 0;
  let mut acc = 0;
  while i < times {
    acc = acc + String::byte_length(r.name) + List::length(r.tags);
    i = i + 1;
  };
  acc
}

fn crossings() -> Int raises ChildFailed {
  // Captured by a spawn, and read here at the same time.
  let a = make(50);
  let f1 = Fiber::spawn(fn () => read(a, 20000));
  let here1 = read(a, 20000);
  let r1 = Fiber::join(f1);

  // Returned by a join, twice: the handle keeps its copy after the first.
  let f2 = Fiber::spawn(fn () => make(40));
  let b = Fiber::join(f2);
  let b2 = Fiber::join(f2);
  let r2 = read(b, 1000) + read(b2, 1000);

  // Sent over a channel, and the sender keeps reading what it sent.
  let ch: Channel<Rec> = Channel::bounded(4);
  let c = make(30);
  let f3 = Fiber::spawn(fn () => match Channel::receive(ch) {
    Option::Some(got) => read(got, 20000),
    Option::None => 0,
  });
  Channel::send(ch, c);
  let here3 = read(c, 20000);
  let r3 = Fiber::join(f3);

  // Held in a `Shared`, and read out of it by two fibers at once.
  let cell = Shared::of(make(20));
  let f4 = Fiber::spawn(fn () => read(Shared::get(cell), 20000));
  let here4 = read(Shared::get(cell), 20000);
  let r4 = Fiber::join(f4);

  // Captured by a nursery's child.
  let d = make(10);
  let crew = Fibers::open();
  Fibers::adopt(crew, Fiber::spawn(fn () => { let _ = read(d, 20000); () }));
  let here5 = read(d, 20000);
  let _failed = Fibers::wait(crew);

  // Handed back by `outcome` rather than `join`.
  let f6 = Fiber::spawn(fn () => make(5));
  let r6 = match Fiber::outcome(f6)! {
    Outcome::Answered(v) => read(v, 10),
    Outcome::Stopped => 0,
  };

  here1 + r1 + r2 + here3 + r3 + here4 + r4 + here5 + r6
}

pub fn main() -> Int raises ChildFailed {
  let total = crossings()!;
  let live = khora_live_count();
  print(\"total ${total} live ${live}\");
  0
}
";

/// What a correct run prints. The total is fixed by the sizes in the fixture.
const CORRECT: &str = "total 5132100 live 0";

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

/// The tests here set process-wide state while they compile, so they take
/// turns.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Compiles `source` into its own directory, with `KHORA_UNBOXED` set to
/// `unboxed` and counts forced plain if `plain`.
fn build(name: &str, source: &str, unboxed: &str, plain: bool) -> PathBuf {
    build_as(name, source, unboxed, plain, false, khora_codegen_llvm::Profile::from_env())
}

/// [`build`], with `KHORA_RC_LOCAL=1` set while it compiles if `local`, in
/// `profile`.
fn build_as(
    name: &str,
    source: &str,
    unboxed: &str,
    plain: bool,
    local: bool,
    profile: khora_codegen_llvm::Profile,
) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY-of-a-sort: process-wide, and every test in this binary holds the
    // lock while it is set.
    unsafe { std::env::set_var("KHORA_UNBOXED", unboxed) };
    if local {
        unsafe { std::env::set_var("KHORA_RC_LOCAL", "1") };
    } else {
        unsafe { std::env::remove_var("KHORA_RC_LOCAL") };
    }
    khora_codegen_llvm::force_plain_counts_on_this_thread(plain);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    let outcome = khora_codegen_llvm::compile_with(&db, root, &exe, profile);
    khora_codegen_llvm::force_plain_counts_on_this_thread(false);
    unsafe { std::env::remove_var("KHORA_RC_LOCAL") };
    unsafe { std::env::remove_var("KHORA_UNBOXED") };
    if let Err(errors) = outcome {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on `backend` under a 60 s watchdog. The line it printed, or
/// how it ended if it did not end well.
fn run(exe: &PathBuf, backend: &str) -> Result<String, String> {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    loop {
        if child.try_wait().expect("waiting").is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("hung for 60 s".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().expect("reaping");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("{:?}: {stdout} {}", out.status, stderr.trim()))
    }
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Builds with `unboxed` and runs on both backends, three times each.
fn every_crossing_counts_to_zero(unboxed: &str) {
    let exe = build(&format!("crossings_unboxed_{unboxed}"), CROSSINGS, unboxed, false);
    for backend in BACKENDS {
        for attempt in 1..=3 {
            let ran = run(&exe, backend);
            assert_eq!(
                ran.as_deref(),
                Ok(CORRECT),
                "`KHORA_UNBOXED={unboxed}`, `{backend}`, run {attempt}"
            );
        }
    }
}

/// Values held flat where they can be: a `Rec` is a pair of words, not a
/// heap object, so what crosses is its fields.
#[test]
fn every_crossing_counts_to_zero_unboxed() {
    every_crossing_counts_to_zero("1");
}

/// Every record behind a header, so what crosses is the record itself.
#[test]
fn every_crossing_counts_to_zero_boxed() {
    every_crossing_counts_to_zero("0");
}

/// **The control: with plain counts the fixture has to go wrong.**
///
/// Plain counts lose updates when two threads count one object at once, so
/// every run on either backend should then end with a wrong count: a leak,
/// a double free or a crash. Requiring every run to fail would make the
/// test depend on how threads are scheduled. So this requires at least one
/// failure on each backend.
///
/// **It runs until the first wrong run, not a fixed five, and each side
/// reads five times as long as in the fixture.** On two free CPUs the first
/// run is wrong nearly every time. A busy runner is closer to one CPU, where
/// an update is lost only when the kernel preempts a thread between the load
/// and the store of a count: on the Windows runner, sharing two cores with
/// the rest of the suite, five runs of the fixture as it stands were all
/// correct. Measured here on one CPU, the thread backend with the fixture's
/// reads was correct 17 to 19 runs in 20. Longer reads give more
/// preemptions a chance to land in a count; more runs give more chances. What
/// it costs is time on a machine where plain counts somehow stopped losing
/// updates: [`CONTROL_RUNS`] runs or [`CONTROL_PATIENCE`] per backend,
/// whichever is first, before it fails.
///
/// **Skipped on a machine with one CPU.** There the kernel almost never
/// preempts a thread between the load and the store of a plain count, so
/// the thread backend loses no update and correct runs say nothing
/// about the fixture. Measured: plain counts on one CPU gave 20 correct runs
/// of 20 on threads, and 0 of 20 on two CPUs or more.
#[test]
fn the_fixture_fails_when_counts_are_not_atomic() {
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        eprintln!("skipped: one CPU cannot show a lost plain-count update");
        return;
    }
    assert_eq!(CROSSINGS.matches(FIXTURE_READS).count(), 8, "the fixture's read counts moved");
    let longer = CROSSINGS.replace(FIXTURE_READS, CONTROL_READS);
    let exe = build("crossings_plain", &longer, "1", true);
    let correct = control_correct();
    for backend in BACKENDS {
        let began = Instant::now();
        let mut runs: Vec<Result<String, String>> = Vec::new();
        while runs.len() < CONTROL_RUNS && began.elapsed() < CONTROL_PATIENCE {
            let ran = run(&exe, backend);
            let wrong = ran.as_deref() != Ok(correct.as_str());
            runs.push(ran);
            if wrong {
                break;
            }
        }
        assert!(
            runs.iter().any(|ran| ran.as_deref() != Ok(correct.as_str())),
            "`{backend}`: {} runs with plain counts were all correct, so the fixture \
             cannot tell atomic counts from plain ones: {:?}",
            runs.len(),
            runs.first()
        );
    }
}

/// **Every crossing with plain counts on local objects** (`KHORA_RC_LOCAL=1`),
/// in both profiles and both layouts.
///
/// What this guards: a route the runtime does not mark. The fixture's
/// values cross while both sides keep counting them, so with local counts
/// plain an unmarked one loses updates -- a wrong total, a leak or a crash
/// in release -- and traps on the owner check in debug. Marked, they are
/// counted atomically and the total is exact.
#[test]
fn every_crossing_counts_to_zero_with_local_counts() {
    use khora_codegen_llvm::Profile;
    for profile in [Profile::Debug, Profile::Release] {
        for unboxed in ["1", "0"] {
            let name = format!("crossings_local_{unboxed}_{}", profile.name());
            let exe = build_as(&name, CROSSINGS, unboxed, false, true, profile);
            for backend in BACKENDS {
                for attempt in 1..=3 {
                    let ran = run(&exe, backend);
                    assert_eq!(
                        ran.as_deref(),
                        Ok(CORRECT),
                        "`KHORA_RC_LOCAL=1`, `{}`, `KHORA_UNBOXED={unboxed}`, `{backend}`, \
                         run {attempt}",
                        profile.name()
                    );
                }
            }
        }
    }
}

/// A function that only ever counts objects its own fiber made, in a
/// program that spawns.
const LOCAL_ONLY: &str = "module main;

import std::core::{print, Fiber, List};

fn churn(n: Int) -> Int {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons(\"x${i}\", xs); i = i + 1; };
  let ys = xs;
  List::length(xs) + List::length(ys)
}

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => churn(10));
  print(\"${churn(10) + Fiber::join(f)}\");
  0
}
";

/// **`KHORA_RC_LOCAL=1` is what turns the local path on**, and so does the
/// test-only force the `sharing::` rows build with; nothing else does.
///
/// What this guards: a switch that stopped switching. The crossing tests
/// pass whether counts are plain or locked, so without this they could run
/// "with the switch on" and test nothing new. The switch is read when a
/// program is compiled, so this compiles one three ways and reads the IR:
/// with the variable or the force, `churn`'s counts include a relaxed `store
/// atomic`, which only the local path emits; with neither, none.
///
/// **And `main` tells the runtime, exactly when the path is on.** The
/// runtime's own counts (`khora_drop` from drop glue, `khora_dup`) are plain
/// on a local object only after `khora_rc_local`. Without the call a program
/// built with the switch still counts every freed child with a lock; with it
/// in a switch-off build, the runtime would count plainly behind generated
/// code that does not.
#[test]
fn the_switch_emits_the_local_path() {
    use khora_codegen_llvm::Profile;
    let stores = |local: bool, forced: bool| {
        let name = format!("local_switch_{local}_{forced}");
        // SAFETY, for both: process-wide, set and cleared under the lock the
        // other tests here hold while they touch the environment. Another
        // test's build that sees it only writes its IR beside its program.
        let held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("KHORA_EMIT_LLVM", "1") };
        drop(held);
        khora_codegen_llvm::force_local_counts_on_this_thread(forced);
        let exe = build_as(&name, LOCAL_ONLY, "1", false, local, Profile::Debug);
        khora_codegen_llvm::force_local_counts_on_this_thread(false);
        let held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("KHORA_EMIT_LLVM") };
        drop(held);
        assert_eq!(run(&exe, "threads").as_deref(), Ok("40"), "local {local}, forced {forced}");
        let ir = std::fs::read_to_string(exe.with_extension("ll")).expect("the IR was dumped");
        let churn = ir
            .split("\ndefine ")
            .find(|f| f.contains("main$churn"))
            .expect("`churn` was emitted")
            .to_string();
        let tells = ir.contains("call void @khora_rc_local()");
        (churn.matches("store atomic i64").count(), tells)
    };
    let (off, off_tells) = stores(false, false);
    assert_eq!(off, 0, "a relaxed count store with the switch off");
    assert!(!off_tells, "`main` switched the runtime to local counts with the switch off");
    let (on, on_tells) = stores(true, false);
    assert!(on > 0, "no relaxed count store with `KHORA_RC_LOCAL=1`");
    assert!(on_tells, "`main` did not tell the runtime about `KHORA_RC_LOCAL=1`");
    let (forced, forced_tells) = stores(false, true);
    assert!(forced > 0, "no relaxed count store with the test-only force");
    assert!(forced_tells, "`main` did not tell the runtime about the test-only force");
}

/// How many times each side of a crossing reads in [`CROSSINGS`].
const FIXTURE_READS: &str = "20000";

/// How many times each side reads in the control's build of it.
const CONTROL_READS: &str = "100000";

/// Most runs the control makes per backend before it gives up.
const CONTROL_RUNS: usize = 40;

/// Longest the control keeps running one backend before it gives up.
const CONTROL_PATIENCE: Duration = Duration::from_secs(60);

/// What a correct run of the control's build prints. The eight long reads
/// each add a record's length (name plus tags: 56, 56, 36, 36, 26, 26, 16,
/// 0 for the nursery child, whose answer is dropped) per read, 252 in all,
/// and the rest of the fixture adds a fixed 92100; [`CORRECT`] is the same
/// sum at 20000 reads.
fn control_correct() -> String {
    let reads: u64 = CONTROL_READS.parse().expect("a count");
    format!("total {} live 0", 252 * reads + 92100)
}

/// **[`control_correct`] is the fixture's own sum**, checked here against
/// [`CORRECT`] at the fixture's read count, so a wrong formula cannot make
/// every control run look wrong.
#[test]
fn the_control_expects_what_the_fixture_prints() {
    let reads: u64 = FIXTURE_READS.parse().expect("a count");
    assert_eq!(format!("total {} live 0", 252 * reads + 92100), CORRECT);
}
