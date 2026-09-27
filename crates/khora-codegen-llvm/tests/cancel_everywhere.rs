#![cfg(feature = "llvm")]

//! Cancellation reaches every function, whatever its `raises` row.
//!
//! The acceptance programs for cancellation on its own channel. Each one is a
//! shape that ran to its end, computed with a zero nobody produced, or took
//! the process down while a cancellation could travel only on an error row.
//! Every program runs on both fiber backends, under a watchdog: a regression
//! here is a hang, and a hang under `Command::output` would take the suite
//! with it.
//!
//! Compiled against `std` itself, because these are programs a user writes.
//!
//! **A program that cancels a fiber at a point waits for the fiber to say it
//! has reached that point**, on a channel, instead of sleeping first. A fiber
//! canceled before its first turn never runs: no finalizer is registered and
//! no change function is entered. On a slow runner a 50 ms sleep ended
//! before the fiber had started, and a test either printed the wrong thing or
//! passed without exercising what it names. A sleep that is left after such
//! a signal is there so the cancel usually lands inside a blocking call the
//! fiber cannot signal from; the signal is what makes the order certain.

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

/// How a run ended.
struct Ran {
    stdout: String,
    stderr: String,
    /// `None` when it was killed by a signal, the watchdog's included.
    code: Option<i32>,
    /// Whether the watchdog killed it.
    hung: bool,
}

/// Compiles `source` into its own directory.
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
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on `backend`, killing it after `patience`. `signal_after`, on
/// Unix, sends SIGTERM that long after it starts.
fn run(exe: &PathBuf, backend: &str, patience: Duration, signal_after: Option<Duration>) -> Ran {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    let mut signaled = false;
    let mut hung = false;
    loop {
        if child.try_wait().expect("waiting").is_some() {
            break;
        }
        if let Some(after) = signal_after {
            if !signaled && started.elapsed() >= after {
                signaled = true;
                #[cfg(unix)]
                {
                    let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
                }
            }
        }
        if started.elapsed() > patience {
            let _ = child.kill();
            hung = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    Ran {
        stdout: stdout.replace("\r\n", "\n"),
        stderr: stderr.replace("\r\n", "\n"),
        code: status.code(),
        hung,
    }
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Builds `source` once and runs it on both backends under a 20 s watchdog.
fn on_both(name: &str, source: &str) -> Vec<(&'static str, Ran)> {
    let exe = build(name, source);
    BACKENDS.iter().map(|b| (*b, run(&exe, b, Duration::from_secs(20), None))).collect()
}

/// **An infallible loop in a fiber stops, and at once.** The loop has no row,
/// its caller has no row, and the thunk has no row. The finalizer runs and the
/// caller's tail does not.
#[test]
fn an_infallible_loop_stops_when_its_fiber_is_canceled() {
    const SOURCE: &str = "module main;
import std::core::{print, Region, Fiber, Channel};
import std::clock::{Clock};

fn spin(n: Int, ready: Channel<Int>) -> Int {
  let region = Region::open();
  Region::defer(region, fn () => print(\"finalizer ran\"));
  Channel::send(ready, 1);
  let mut i = 0;
  let mut total = 0;
  while i < n { total = (total * 31 + i) % 1000003; i = i + 1; };
  print(\"spin reached its end\");
  total
}

fn caller(n: Int, ready: Channel<Int>) -> Int {
  let got = spin(n, ready);
  print(\"caller's tail ran\");
  got
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => caller(300000000000, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    let t0 = clock.monotonic_millis();
    Fiber::wait(f);
    let waited = clock.monotonic_millis() - t0;
    print(\"canceled: ${Fiber::canceled(f)}; prompt: ${waited < 2000}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_loop", SOURCE) {
        assert!(!ran.hung, "`{backend}`: the loop never stopped: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "finalizer ran\ncanceled: true; prompt: true\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **`join` on an infallible child unwinds a parent with no row.** The parent
/// used to go on with a zero where the child's answer should have been.
#[test]
fn join_on_a_stopped_child_unwinds_an_infallible_parent() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Channel};
import std::clock::{Clock};

fn spin() -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn parent(ready: Channel<Int>) -> Int {
  let child = Fiber::spawn(fn () => spin());
  Channel::send(ready, 1);
  let got = Fiber::join(child);
  print(\"parent's tail ran with ${got}\");
  got
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ready: Channel<Int> = Channel::bounded(1);
    let b = Fiber::spawn(fn () => parent(ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(b);
    Fiber::wait(b);
    print(\"parent canceled: ${Fiber::canceled(b)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_join", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "parent canceled: true\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A frame that returns a pointer stops without taking the process down.**
/// A total `catch` in a function returning `String`, on a fiber, used to end
/// the process: there was no zero `String` to hand back.
#[test]
fn a_pointer_returning_frame_stops_without_ending_the_process() {
    const SOURCE: &str = "module main;
import std::core::{print, Region, Fiber, Channel};
import std::clock::{Clock};

fn step() -> Int raises String { 1 }

fn name_it(ready: Channel<Int>) -> String {
  let region = Region::open();
  Region::defer(region, fn () => print(\"finalizer ran\"));
  Channel::send(ready, 1);
  let mut n = 0;
  while n < 300000000000 {
    n = n + (step()! catch { _ => 1 });
  };
  \"unreachable\"
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ready: Channel<Int> = Channel::bounded(1);
    let a = Fiber::spawn(fn () => name_it(ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(a);
    Fiber::wait(a);
    print(\"canceled: ${Fiber::canceled(a)}\");
    print(\"process still here\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_pointer", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "finalizer ran\ncanceled: true\nprocess still here\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **Three infallible frames unwind and release everything they held.**
/// Twenty fibers are canceled in the innermost of three frames, each holding
/// lists and a string; the live count comes back to where it started.
#[test]
fn three_infallible_frames_unwind_and_leak_nothing() {
    const SOURCE: &str = "module main;
import std::core::{print, List, Fiber};

extern fn khora_live_count() -> Int;

fn build(n: Int) -> List<Int> {
  let mut xs: List<Int> = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons(i, xs); i = i + 1; };
  xs
}

fn deepest(held: List<Int>) -> Int {
  let mine = build(50);
  let mut i = 0;
  let mut t = 0;
  while i < 400000000000 { t = (t + i) % 7; i = i + 1; };
  t + List::length(mine) + List::length(held)
}

fn middle() -> Int {
  let a = build(100);
  let s = \"held across the call ${List::length(a)}\";
  let got = deepest(a);
  got + String::byte_length(s)
}

pub fn main() -> Int {
  let before = khora_live_count();
  let mut round = 0;
  while round < 20 {
    let f = Fiber::spawn(fn () => middle());
    Fiber::cancel(f);
    Fiber::wait(f);
    round = round + 1;
  };
  print(\"live delta ${khora_live_count() - before}\");
  0
}
";
    for (backend, ran) in on_both("cancel_everywhere_leak", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "live delta 0\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **`catch { _ => .. }` does not see a cancellation.** The loop's total
/// catch handles every error `step` can raise; the cancellation goes past it,
/// the fiber stops, and the arm never runs for it.
#[test]
fn a_total_catch_does_not_see_a_cancellation() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Channel};
import std::clock::{Clock};

fn step() -> Int raises String { 0 }

fn worker(caught: Shared<Int>, ready: Channel<Int>) -> () {
  Channel::send(ready, 1);
  loop {
    let _ = step()! catch { _ => { Shared::set(caught, Shared::get(caught) + 1); 0 } };
  }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let caught = Shared::of(0);
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => worker(caught, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    Fiber::wait(f);
    print(\"canceled: ${Fiber::canceled(f)}; caught: ${Shared::get(caught)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_catch", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "canceled: true; caught: 0\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A canceled `clock.sleep` does not run the statement after it**, on
/// either backend. The sleep is woken and gives up early, answering as if it
/// had finished; without a check after it the fiber would run one more step of
/// the work it was told to abandon -- here, the `print`.
#[test]
fn a_canceled_sleep_does_not_run_the_statement_after_it() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Channel};
import std::clock::{Clock};

fn napper(ready: Channel<Int>) -> () with { clock: Clock } {
  Channel::send(ready, 1);
  clock.sleep(8000);
  print(\"statement after the sleep ran\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => napper(ready));
    let _ = Channel::receive(ready);
    clock.sleep(100);
    let t0 = clock.monotonic_millis();
    Fiber::cancel(f);
    Fiber::wait(f);
    let waited = clock.monotonic_millis() - t0;
    print(\"canceled: ${Fiber::canceled(f)}; prompt: ${waited < 2000}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_sleep", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "canceled: true; prompt: true\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A `Fiber::join` inside a change function that comes back canceled
/// leaves the cell as it was**, and the fiber stops. The updater is canceled
/// while its change function waits on a child; the cell must keep `41` -- not
/// the zero the unwound change function would have handed back -- and a
/// `String` cell must keep `hello` rather than a null the next read crashes
/// on.
#[test]
fn a_join_canceled_inside_a_change_function_changes_nothing() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Channel};
import std::clock::{Clock};

fn spin() -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn updater(cell: Shared<Int>, inside: Channel<Int>) -> () {
  let other = Fiber::spawn(fn () => spin());
  Shared::update(cell, fn n => { Channel::send(inside, 1); let got = Fiber::join(other); n + got + 1 });
  print(\"TAIL updater ran\");
}

fn updater_s(text: Shared<String>, inside: Channel<Int>) -> () {
  let other = Fiber::spawn(fn () => spin());
  Shared::update(text, fn s => { Channel::send(inside, 1); let got = Fiber::join(other); s + \"!${got}\" });
  print(\"TAIL updater_s ran\");
}

fn modifier(cell: Shared<Int>, inside: Channel<Int>) -> () {
  let other = Fiber::spawn(fn () => spin());
  let answer = Shared::modify(cell, fn n => { Channel::send(inside, 1); let got = Fiber::join(other); { state: n + got, result: got } });
  print(\"TAIL modifier ran ${answer}\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let cell = Shared::of(41);
    let text = Shared::of(\"hello\");
    let inside: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => updater(cell, inside));
    let _ = Channel::receive(inside);
    Fiber::cancel(f);
    Fiber::wait(f);
    print(\"int: canceled ${Fiber::canceled(f)}; cell = ${Shared::get(cell)}\");
    let g = Fiber::spawn(fn () => updater_s(text, inside));
    let _ = Channel::receive(inside);
    Fiber::cancel(g);
    Fiber::wait(g);
    print(\"string: canceled ${Fiber::canceled(g)}; text = ${Shared::get(text)}\");
    let h = Fiber::spawn(fn () => modifier(cell, inside));
    let _ = Channel::receive(inside);
    Fiber::cancel(h);
    Fiber::wait(h);
    print(\"modify: canceled ${Fiber::canceled(h)}; cell = ${Shared::get(cell)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_pinjoin", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "int: canceled true; cell = 41\n\
             string: canceled true; text = hello\n\
             modify: canceled true; cell = 41\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A change function that joins a child somebody else stopped changes
/// nothing, and its caller stops** -- although nobody canceled the caller.
/// Joining a stopped fiber stops the joiner, inside a change function as
/// everywhere else: `main` exits 130 with the cell still `41`, and it does not
/// go on with an update that answered a zero.
#[test]
fn a_change_function_joining_a_stopped_child_changes_nothing() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Region};
import std::clock::{Clock};

fn spin() -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let cell = Shared::of(41);
    let region = Region::open();
    Region::defer(region, fn () => print(\"on the way out, cell = ${Shared::get(cell)}\"));
    let child = Fiber::spawn(fn () => spin());
    Fiber::cancel(child);
    Fiber::wait(child);
    let after = Shared::update(cell, fn n => n + Fiber::join(child) + 1);
    print(\"TAIL main ran; update answered ${after}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_pinjoin2", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "on the way out, cell = 41\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(130), "`{backend}`");
    }
}

/// **A plain cancel does not cut a finalizer short through a change function
/// that joins.** Only `abort` stops cleanup. The finalizer's `update` joins a
/// child that finishes on its own after ~300 ms; the join must wait for it,
/// the cell must hold the joined value, and the statements after the `update`
/// must run. The control is the same finalizer with the join outside the
/// change function, which always waited.
#[test]
fn a_cancel_does_not_cut_short_a_finalizer_whose_change_function_joins() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Region, Channel};
import std::clock::{Clock};

fn spin(n: Int) -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < n { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn worker(cell: Shared<String>, log: Shared<Int>, ready: Channel<Int>) -> () {
  with { clock: Clock::real() } {
    let r = Region::open();
    Region::defer(r, fn () => {
      let other = Fiber::spawn(fn () => spin(30000000));
      Shared::update(cell, fn s => { let got = Fiber::join(other); s + \"+fin${got}\" });
      Shared::set(log, 1);
    });
    Channel::send(ready, 1);
    clock.sleep(5000);
    print(\"TAIL worker body\");
  }
}

fn control(cell: Shared<String>, log: Shared<Int>, ready: Channel<Int>) -> () {
  with { clock: Clock::real() } {
    let r = Region::open();
    Region::defer(r, fn () => {
      let other = Fiber::spawn(fn () => spin(30000000));
      let got = Fiber::join(other);
      Shared::update(cell, fn s => s + \"+fin${got}\");
      Shared::set(log, 1);
    });
    Channel::send(ready, 1);
    clock.sleep(5000);
    print(\"TAIL control body\");
  }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let cell = Shared::of(\"start\");
    let log = Shared::of(0);
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => worker(cell, log, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    Fiber::wait(f);
    print(\"in change fn: canceled ${Fiber::canceled(f)}; finalizer finished ${Shared::get(log)}; cell = ${Shared::get(cell)}\");
    let cell2 = Shared::of(\"start\");
    let log2 = Shared::of(0);
    let g = Fiber::spawn(fn () => control(cell2, log2, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(g);
    Fiber::wait(g);
    print(\"control: canceled ${Fiber::canceled(g)}; finalizer finished ${Shared::get(log2)}; cell = ${Shared::get(cell2)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_finupd", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "in change fn: canceled true; finalizer finished 1; cell = start+fin30993\n\
             control: canceled true; finalizer finished 1; cell = start+fin30993\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **`abort` still ends a finalizer stuck in a change function's join.** The
/// child never finishes, so after the plain cancel the join waits -- the fiber
/// is checked to be still running 200 ms later -- and `abort` must end it with
/// the cell unchanged and the rest of the finalizer skipped.
#[test]
fn abort_ends_a_finalizer_whose_change_function_joins_for_ever() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Region, Channel};
import std::clock::{Clock};

fn spin() -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn worker(cell: Shared<Int>, log: Shared<Int>, ready: Channel<Int>) -> () {
  with { clock: Clock::real() } {
    let r = Region::open();
    Region::defer(r, fn () => {
      let other = Fiber::spawn(fn () => spin());
      Shared::update(cell, fn n => n + Fiber::join(other));
      Shared::set(log, 1);
    });
    Channel::send(ready, 1);
    clock.sleep(5000);
  }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let cell = Shared::of(41);
    let log = Shared::of(0);
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => worker(cell, log, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    clock.sleep(200);
    print(\"after cancel, finished: ${Fiber::finished(f)}\");
    Fiber::abort(f);
    Fiber::wait(f);
    print(\"after abort: cell = ${Shared::get(cell)}; rest of finalizer ran ${Shared::get(log)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_finupd_abort", SOURCE) {
        assert!(!ran.hung, "`{backend}`: abort did not end the finalizer: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "after cancel, finished: false\n\
             after abort: cell = 41; rest of finalizer ran 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A `String` `<` is a cancellation point that leaks nothing.** `pick`'s
/// only way to stop is the comparison, which is a call to `impl Ord for
/// String`, and `held` is moved into the answer after it. Fibers are canceled
/// inside the comparison, once and then twenty times, and the live count must
/// come back to where it started both times. It is read before the `print`,
/// because a count read inside `${..}` also sees the pieces of that string
/// already built.
///
/// **Each fiber compares until it is canceled, so the cancel is the only way
/// out.** It used to compare once and cancel after `clock.sleep(20)`, which
/// assumed the comparison was still running when the cancel arrived. On a
/// macOS arm64 runner it often was not: the program printed `stopped 0` /
/// `stopped 16` with a live delta of 0, so there was nothing to cancel and
/// no leak -- a missing precondition, not a leak. (One comparison of these
/// 32 MiB strings takes about 230 ms on a Linux x86_64 box; whether the
/// runner compares faster or its sleep overruns (the reactor tests show
/// macOS runner sleeps overrunning by 30 ms and more), the fiber finished
/// first either way.) Looping removes the dependence on either: the
/// comparison is nearly all the time the loop spends, so each cancel still
/// lands inside `impl Ord for String`, and the loop's only exit is the
/// cancel.
#[test]
fn a_string_comparison_canceled_leaks_nothing() {
    const SOURCE: &str = "module main;
import std::core::{Fiber, Channel, print};
import std::clock::{Clock};

extern fn khora_live_count() -> Int;

fn pick(held: String, a: String, b: String) -> String {
  if a < b { held } else { held }
}

fn big(seed: String) -> String {
  let mut s = seed;
  let mut i = 0;
  while i < 24 { s = s + s; i = i + 1; };
  s
}

fn until_canceled(round: Int, a: String, b: String, ready: Channel<Int>) -> Int {
  let mut total = 0;
  Channel::send(ready, 1);
  loop {
    let h = \"held-${round}\";
    total = total + String::byte_length(pick(h, a, b));
  }
}

fn rounds(a: String, b: String, n: Int) -> Int with { clock: Clock } {
  let ready: Channel<Int> = Channel::bounded(1);
  let before = khora_live_count();
  let mut round = 0;
  let mut stopped = 0;
  while round < n {
    let f = Fiber::spawn(fn () => until_canceled(round, a, b, ready));
    let _ = Channel::receive(ready);
    clock.sleep(20);
    Fiber::cancel(f);
    Fiber::wait(f);
    if Fiber::canceled(f) { stopped = stopped + 1; };
    round = round + 1;
  };
  let delta = khora_live_count() - before;
  print(\"stopped ${stopped}\");
  delta
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let a = big(\"ab\");
    let b = big(\"ab\");
    let once = rounds(a, b, 1);
    let many = rounds(a, b, 20);
    print(\"live delta: once ${once}, twenty ${many}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_strcmp", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "stopped 1\nstopped 20\nlive delta: once 0, twenty 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A binding moved after a call that stops is released by the unwind.**
///
/// Each shape holds a `String` across a call that is canceled, and hands it
/// on after the call: straight-line, in a loop body, in a `match` arm (the
/// arm's own binding and a `let` inside it), into a constructor, after a call
/// through a closure, and a binding overwritten by the call's result (dead
/// while the call runs, and still holding the old value). The fast ownership
/// plan strikes a moved binding's
/// release at its block, which leaks it on a path that leaves before the
/// take. Each is run once and then twenty times; the live count must grow by
/// the same amount both times.
///
/// Measured red with the per-binding rule off (`holding_at_a_stop` records
/// nothing, so every body gets the fast plan): growth 19 for the straight,
/// loop, constructor and closure-call shapes and 38 for the arm, on both
/// backends. The overwritten shape is red (19) when only its own half of the
/// rule is off, and the arm (19) when `own_arm_bindings` ignores it.
///
/// **Shape 2, a lambda body holding the binding itself, stays at 0
/// either way**: the last-use pass never moves a binding inside a lambda body
/// (it counts the body's reads without walking it), so there is nothing to
/// strike. It is here so that extending the pass into lambdas without
/// extending this rule fails here.
#[test]
fn a_binding_moved_after_a_call_that_stops_leaks_nothing() {
    const SOURCE: &str = "module main;
import std::core::{Fiber, Option, print};
import std::clock::{Clock};

extern fn khora_live_count() -> Int;

fn forever() -> Int {
  let mut i = 0;
  loop { i = i + 1; }
}

fn consume(s: String) -> Int { String::byte_length(s) }

fn straight(tag: String) -> Int {
  let held = \"held-${tag}\";
  let n = forever();
  n + consume(held)
}

fn in_loop(tag: String) -> Int {
  let mut total = 0;
  let mut i = 0;
  while i < 3 {
    let held = \"held-${tag}\";
    let n = forever();
    total = total + n + consume(held);
    i = i + 1;
  };
  total
}

fn in_arm(tag: String) -> Int {
  match Option::Some(\"bound-${tag}\") {
    Option::Some(s) => {
      let held = \"held-${tag}\";
      let n = forever();
      n + consume(s) + consume(held)
    },
    Option::None => 0,
  }
}

fn into_constructor(tag: String) -> Int {
  let held = \"held-${tag}\";
  let n = forever();
  let o = Option::Some(held);
  match o {
    Option::Some(s) => n + consume(s),
    Option::None => n,
  }
}

fn through_a_closure(tag: String) -> Int {
  let held = \"held-${tag}\";
  let spin = fn () => forever();
  let n = spin();
  n + consume(held)
}

fn fresh(n: Int) -> String { \"fresh-${n}\" }

fn overwritten(tag: String) -> Int {
  let mut s = \"old-${tag}\";
  s = fresh(forever());
  consume(s)
}

fn rounds(which: Int, n: Int) -> Int with { clock: Clock } {
  let before = khora_live_count();
  let mut round = 0;
  let mut stopped = 0;
  while round < n {
    let tag = \"t${round}\";
    let f = if which == 0 {
      Fiber::spawn(fn () => straight(tag))
    } else if which == 1 {
      Fiber::spawn(fn () => in_loop(tag))
    } else if which == 2 {
      Fiber::spawn(fn () => { let held = \"held-${tag}\"; let k = forever(); k + consume(held) })
    } else if which == 3 {
      Fiber::spawn(fn () => in_arm(tag))
    } else if which == 4 {
      Fiber::spawn(fn () => into_constructor(tag))
    } else if which == 5 {
      Fiber::spawn(fn () => through_a_closure(tag))
    } else {
      Fiber::spawn(fn () => overwritten(tag))
    };
    clock.sleep(3);
    Fiber::cancel(f);
    Fiber::wait(f);
    if Fiber::canceled(f) { stopped = stopped + 1; };
    round = round + 1;
  };
  let grew = khora_live_count() - before;
  print(\"stopped ${stopped}\");
  grew
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let mut which = 0;
    while which < 7 {
      let once = rounds(which, 1);
      let many = rounds(which, 20);
      print(\"shape ${which}: per-cancel growth ${many - once}\");
      which = which + 1;
    };
    0
  }
}
";
    let mut expected = String::new();
    for shape in 0..7 {
        expected.push_str(&format!("stopped 1\nstopped 20\nshape {shape}: per-cancel growth 0\n"));
    }
    for (backend, ran) in on_both("cancel_everywhere_moved", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, expected, "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **Recursion through a function value stops promptly**, both through a
/// record field (`k.f(k, n - 1)`, which never names `walk`) and through a
/// lambda's own binding. Neither has a loop or a named call cycle; each runs
/// for seconds uncanceled. Both have to stop within 100 ms of the cancel.
///
/// The third shape's only indirect call is made by an intrinsic: `attempt`
/// calls the thunk it is handed, which reaches `walk` again through an
/// `Array` of records. No body in the cycle calls through a value itself, so
/// only counting "hands a closure to an intrinsic" as an indirect call puts a
/// poll on it; without that it ran for 100 s after the cancel.
#[test]
fn recursion_through_a_function_value_stops_promptly() {
    const SOURCE: &str = "module main;
import std::core::{Fiber, print, Array, Result, attempt};
import std::clock::{Clock};

type Knot = { f: (Knot, Int) -> Int };

fn walk(k: Knot, n: Int) -> Int {
  if n < 2 { n } else { k.f(k, n - 1) + k.f(k, n - 2) }
}

fn knot() -> Int { let k: Knot = { f: walk }; walk(k, 42) }

fn by_rec_lambda() -> Int {
  let f: (Int) -> Int = fn n => if n < 2 { n } else { f(n - 1) + f(n - 2) };
  f(45)
}

type Thunk = { t: () -> Int raises String };

fn attempted(t: () -> Int raises String) -> Int {
  match attempt(t) {
    Result::Ok(v) => v,
    Result::Err(_) => 0,
  }
}

fn nothing() -> Int raises String { 0 }

fn through_attempt(limit: Int) -> Int {
  let depth: Array<Int> = Array::new(1, 0);
  let knots: Array<Thunk> = Array::new(1, { t: nothing });
  let t: () -> Int raises String = fn () => {
    let d = Array::get(depth, 0);
    if d > limit { 1 } else {
      Array::set(depth, 0, d + 1);
      let a = attempted(Array::get(knots, 0).t);
      let b = attempted(Array::get(knots, 0).t);
      Array::set(depth, 0, d);
      a + b
    }
  };
  Array::set(knots, 0, { t: t });
  attempted(t)
}

fn stop_after(f: Fiber<Int, {}>, name: String) -> () with { clock: Clock } {
  clock.sleep(30);
  let t = clock.monotonic_millis();
  Fiber::cancel(f);
  Fiber::wait(f);
  print(\"${name}: canceled ${Fiber::canceled(f)}; within 100 ms ${clock.monotonic_millis() - t < 100}\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    stop_after(Fiber::spawn(fn () => knot()), \"knot\");
    stop_after(Fiber::spawn(fn () => by_rec_lambda()), \"lambda\");
    stop_after(Fiber::spawn(fn () => through_attempt(28)), \"attempt\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_knot", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "knot: canceled true; within 100 ms true\n\
             lambda: canceled true; within 100 ms true\n\
             attempt: canceled true; within 100 ms true\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A fiber canceled while it waits in a nursery does not run the nursery's
/// caller's tail.** The wait cancels the children, waits for them, and hands
/// back a count; the caller must stop there rather than take the count for a
/// round that ended and go on to `Shared::set`, which is not a cancellation
/// point of its own.
#[test]
fn a_nursery_wait_that_was_canceled_runs_no_tail() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Channel, nursery, Nursery, ChildFailed};
import std::clock::{Clock};

fn spin(n: Int) -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < n { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn group(log: Shared<Int>, ready: Channel<Int>) -> Int raises ChildFailed {
  let v = nursery(fn () => {
    nursery.adopt(Fiber::spawn(fn () => { spin(300000000000); () }));
    nursery.adopt(Fiber::spawn(fn () => { spin(300000000000); () }));
    Channel::send(ready, 1);
    5
  })!;
  Shared::set(log, 1);
  v
}

fn guarded(log: Shared<Int>, caught: Shared<Int>, ready: Channel<Int>) -> Int {
  group(log, ready)! catch {
    ChildFailed { children } => { Shared::set(caught, children); -1 },
  }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let log = Shared::of(0);
    let caught = Shared::of(0);
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => guarded(log, caught, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    Fiber::wait(f);
    print(\"canceled ${Fiber::canceled(f)}; tail ran ${Shared::get(log)}; catch saw ${Shared::get(caught)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_nursery_tail", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "canceled true; tail ran 0; catch saw 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A cancel a change function's blocking call gave up on is not lost.** The
/// `receive` inside the `update` hands back `None` on the cancel, the change
/// function stores its "gave up" value and returns, and the fiber must stop
/// right after the `update`: reported canceled, and the `Shared::set` after
/// it not run.
///
/// **Each fiber says it is inside its change function before it is
/// canceled.** A fixed pause was not enough: on a slow runner a 50 ms sleep
/// ended before `modifier` reached `modify`, the cancel stopped it before the
/// change function ran, and the cell read 101 where 201 was expected. The
/// `inside` channel fixes the order.
#[test]
fn a_cancel_absorbed_inside_a_change_function_stops_after_the_update() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Channel, Option};
import std::clock::{Clock};

fn updater(cell: Shared<Int>, log: Shared<Int>, ch: Channel<Int>, inside: Channel<Int>) -> () {
  Shared::update(cell, fn n => {
    Channel::send(inside, 1);
    match Channel::receive(ch) { Option::Some(v) => n + v, Option::None => n + 100 }
  });
  Shared::set(log, 1);
}

fn modifier(cell: Shared<Int>, log: Shared<Int>, ch: Channel<Int>, inside: Channel<Int>) -> () {
  let got = Shared::modify(cell, fn n => {
    Channel::send(inside, 1);
    match Channel::receive(ch) { Option::Some(v) => { state: n + v, result: v }, Option::None => { state: n + 100, result: 0 } }
  });
  Shared::set(log, got + 1);
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let cell = Shared::of(1);
    let log = Shared::of(0);
    let ch: Channel<Int> = Channel::bounded(1);
    let inside: Channel<Int> = Channel::bounded(2);
    let f = Fiber::spawn(fn () => updater(cell, log, ch, inside));
    let _in = Channel::receive(inside);
    Fiber::cancel(f);
    Fiber::wait(f);
    let c = Shared::get(cell);
    let l = Shared::get(log);
    print(\"update: canceled ${Fiber::canceled(f)}; cell ${c}; tail ran ${l}\");
    let g = Fiber::spawn(fn () => modifier(cell, log, ch, inside));
    let _in2 = Channel::receive(inside);
    Fiber::cancel(g);
    Fiber::wait(g);
    let c2 = Shared::get(cell);
    let l2 = Shared::get(log);
    print(\"modify: canceled ${Fiber::canceled(g)}; cell ${c2}; tail ran ${l2}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_update_tail", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "update: canceled true; cell 101; tail ran 0\n\
             modify: canceled true; cell 201; tail ran 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A bounded nursery's `adopt`, waiting for room, stops on a cancel.** With
/// room for one child, the second `adopt` waits for the first, a long
/// spinner. The cancel has to reach that child and end the wait, and the
/// statement after the `adopt` must not run. It used to wait the spinner out
/// (16 s) and then carry on, reporting the fiber not canceled.
#[test]
fn a_bounded_adopt_waiting_for_room_stops_on_a_cancel() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared, Channel, bounded_nursery, Nursery, ChildFailed};
import std::clock::{Clock};

fn spin(n: Int) -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < n { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn group(log: Shared<Int>, ready: Channel<Int>) -> Int raises ChildFailed {
  bounded_nursery(1, fn () => {
    nursery.adopt(Fiber::spawn(fn () => { spin(300000000000); () }));
    Shared::set(log, 1);
    Channel::send(ready, 1);
    nursery.adopt(Fiber::spawn(fn () => { spin(10); () }));
    Shared::set(log, 2);
    5
  })!
}

fn guarded(log: Shared<Int>, ready: Channel<Int>) -> Int {
  group(log, ready)! catch { ChildFailed { children } => -1 }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let log = Shared::of(0);
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => guarded(log, ready));
    let _ = Channel::receive(ready);
    let before = Shared::get(log);
    let t1 = clock.monotonic_millis();
    Fiber::cancel(f);
    Fiber::wait(f);
    let took = clock.monotonic_millis() - t1;
    print(\"waiting for room: log ${before}; canceled ${Fiber::canceled(f)}; within 1 s ${took < 1000}; log ${Shared::get(log)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_bounded_adopt", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "waiting for room: log 1; canceled true; within 1 s true; log 1\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A tagged body whose value is a function chosen by a `match` returns
/// that function.** `fetch` is tagged (`Map::get` loops), and its `match`
/// mixes a field read with a named function. The join of function type had
/// no slot, so the body's value was a placeholder: first a null closure the
/// caller called ("the stack ran out"), then a build error saying the body
/// "does not produce" its type. Both arms are taken here, so each closure is
/// called.
#[test]
fn a_tagged_body_returning_a_function_chosen_by_match_runs() {
    const SOURCE: &str = "module main;
import std::core::{print, Map, Option};

type Knot = { t: () -> Int };

fn seven() -> Int { 7 }
fn nine() -> Int { 9 }

fn fetch(knots: Map<Int, Knot>) -> () -> Int {
  match Map::get(knots, 0) {
    Option::Some(k) => k.t,
    Option::None => seven,
  }
}

pub fn main() -> Int {
  let knots: Map<Int, Knot> = Map::new();
  print(\"${fetch(knots)()}\");
  let full: Map<Int, Knot> = Map::new();
  Map::insert(full, 0, { t: nine });
  print(\"${fetch(full)()}\");
  0
}
";
    for (backend, ran) in on_both("cancel_everywhere_no_value", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "7\n9\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A blocking socket call that gave up on a cancel is not taken for a
/// failure.** A canceled `receive` and a canceled `accept_on` come back with
/// their failure value; the fiber must stop there, not count an I/O error that
/// did not happen and run its tail. A channel receive, which already did this,
/// is the control.
///
/// **`start()` first, as every other program that listens does.** It is a
/// no-op on Linux and macOS and `WSAStartup` on Windows, where without it
/// `socket()` fails with `WSANOTINITIALISED`: this program printed
/// `setup false false false` there, and every later call got an invalid
/// handle. Nothing initialized Winsock before `listen_on`: Rust's `std::net`
/// does it lazily, and the runtime's first use of it (`connect_to`, the
/// reactor's waker) comes after. Every test that listens and passes on
/// Windows calls `start()`; this was the only one that did not.
#[test]
fn a_socket_call_that_gave_up_is_not_taken_for_a_failure() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Array, Shared, Channel};
import std::clock::{Clock};
import std::net::socket::{start, listen_on, accept_on, connect_to, receive, invalid_handle};

fn reader(conn: Int, errors: Shared<Int>, ready: Channel<Int>) -> () {
  let buf: Array<U8> = Array::new(64, 0);
  Channel::send(ready, 1);
  let n = receive(conn, buf);
  if n < 0 { Shared::set(errors, Shared::get(errors) + 1); print(\"TAIL recv\"); }
  else { print(\"TAIL recv got bytes\"); }
}

fn acceptor(server: Int, errors: Shared<Int>, ready: Channel<Int>) -> () {
  Channel::send(ready, 1);
  let c = accept_on(server);
  if c == invalid_handle() { Shared::set(errors, Shared::get(errors) + 1); print(\"TAIL accept\"); }
  else { print(\"TAIL accept got one\"); }
}

fn chan(ch: Channel<Int>, ready: Channel<Int>) -> () {
  Channel::send(ready, 1);
  let _ = Channel::receive(ch);
  print(\"TAIL channel\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let errors = Shared::of(0);
    if start() {} else { print(\"no sockets\") };
    let server = listen_on(PORT);
    let client = connect_to(\"127.0.0.1\", PORT);
    let conn = accept_on(server);
    print(\"setup ${server >= 0} ${client >= 0} ${conn >= 0}\");
    let ready: Channel<Int> = Channel::bounded(1);
    let r = Fiber::spawn(fn () => reader(conn, errors, ready));
    let _ = Channel::receive(ready);
    clock.sleep(50); Fiber::cancel(r); Fiber::wait(r);
    print(\"reader canceled ${Fiber::canceled(r)}\");
    let a = Fiber::spawn(fn () => acceptor(server, errors, ready));
    let _ = Channel::receive(ready);
    clock.sleep(50); Fiber::cancel(a); Fiber::wait(a);
    print(\"acceptor canceled ${Fiber::canceled(a)}\");
    let ch: Channel<Int> = Channel::bounded(1);
    let c = Fiber::spawn(fn () => chan(ch, ready));
    let _ = Channel::receive(ready);
    clock.sleep(50); Fiber::cancel(c); Fiber::wait(c);
    print(\"channel canceled ${Fiber::canceled(c)}\");
    print(\"I/O errors counted: ${Shared::get(errors)}\");
    0
  }
}
";
    for backend in BACKENDS {
        // A port the operating system says is free, released for the program
        // to take. Per backend, so the two runs never share one.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let exe = build(
            &format!("cancel_everywhere_gaveup_{backend}"),
            &SOURCE.replace("PORT", &port.to_string()),
        );
        let ran = run(&exe, backend, Duration::from_secs(20), None);
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "setup true true true\n\
             reader canceled true\n\
             acceptor canceled true\n\
             channel canceled true\n\
             I/O errors counted: 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **Three thousand `cancel_within` calls hold fewer than twenty threads.**
/// Once for fibers already finished, once for fibers still running when it
/// is called, whose deadlines are all pending while the threads are counted.
/// A thread per call is what this guards against: three thousand sleeping
/// threads, and a panic out of the runtime when the OS refuses the next.
#[cfg(target_os = "linux")]
#[test]
fn cancel_within_does_not_hold_a_thread_per_call() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Channel};
import std::clock::{Clock};

fn quick(n: Int) -> Int { n + 1 }
fn parked(ch: Channel<Int>) -> Int { let _ = Channel::receive(ch); 0 }

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ch: Channel<Int> = Channel::bounded(1);
    let mut i = 0;
    while i < 3000 {
      let f = Fiber::spawn(fn () => quick(i));
      Fiber::wait(f);
      Fiber::cancel_within(f, 20000);
      let g = Fiber::spawn(fn () => parked(ch));
      Fiber::cancel_within(g, 20000);
      Fiber::wait(g);
      i = i + 1;
    };
    print(\"ready\");
    clock.sleep(3000);
    print(\"done\");
    0
  }
}
";
    let exe = build("cancel_everywhere_within", SOURCE);
    for backend in BACKENDS {
        let mut child = Command::new(&exe)
            .env("KHORA_FIBERS", backend)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the program should run");
        // Wait for "ready" on stdout, then count while the deadlines are pending.
        let mut stdout = child.stdout.take().expect("stdout");
        let mut seen = Vec::new();
        let started = Instant::now();
        let mut byte = [0u8; 1];
        while !seen.ends_with(b"ready\n") && started.elapsed() < Duration::from_secs(60) {
            match stdout.read(&mut byte) {
                Ok(1) => seen.push(byte[0]),
                _ => break,
            }
        }
        assert!(seen.ends_with(b"ready\n"), "`{backend}`: never got ready");
        std::thread::sleep(Duration::from_millis(500));
        let status = std::fs::read_to_string(format!("/proc/{}/status", child.id()))
            .expect("the program's status");
        let threads: usize = status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|n| n.trim().parse().ok())
            .expect("a thread count");
        let _ = child.kill();
        let _ = child.wait();
        assert!(threads < 20, "`{backend}`: {threads} threads with 6000 deadlines set");
    }
}

/// **A frame stopped while it holds a reuse token frees the token.** `walk`
/// takes each `Cons` cell as a token at its arm's head, to build the answer
/// in, and then makes the recursive call; the bottom never returns. So a
/// cancel lands with a thousand frames each holding a cell that no counter
/// and no owner can see -- `khora_live_count` already counted it out, so
/// only the process's memory shows the leak.
///
/// Measured by resident memory after 100 cancels and again after 900 more:
/// with the token left unfreed it grew from 9 MB to 38 MB (threads backend;
/// about 32 KB, one list's cells, per cancel). Freed on the way out, it does
/// not grow. The bound is 4 MB.
#[cfg(target_os = "linux")]
#[test]
fn a_stopped_frame_frees_the_cell_it_was_going_to_reuse() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, List};
import std::clock::{Clock};

fn build(n: Int) -> List<Int> {
  let mut xs: List<Int> = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons(i, xs); i = i + 1; };
  xs
}

fn forever() -> Int {
  let mut i = 0;
  loop { i = i + 1; }
}

fn walk(xs: List<Int>) -> List<Int> {
  match xs {
    List::Nil => { let _ = forever(); List::Nil },
    List::Cons(head, tail) => List::Cons(head, walk(tail)),
  }
}

fn rounds(n: Int) -> () with { clock: Clock } {
  let mut round = 0;
  while round < n {
    let f = Fiber::spawn(fn () => List::length(walk(build(1000))));
    clock.sleep(2);
    Fiber::cancel(f);
    Fiber::wait(f);
    round = round + 1;
  }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    rounds(100);
    print(\"first\");
    clock.sleep(400);
    rounds(900);
    print(\"second\");
    clock.sleep(400);
    0
  }
}
";
    /// Reads stdout up to `marker`, then the program's resident kilobytes.
    fn resident_after(child: &mut std::process::Child, out: &mut impl Read, marker: &[u8]) -> u64 {
        let mut seen = Vec::new();
        let started = Instant::now();
        let mut byte = [0u8; 1];
        while !seen.ends_with(marker) && started.elapsed() < Duration::from_secs(120) {
            match out.read(&mut byte) {
                Ok(1) => seen.push(byte[0]),
                _ => break,
            }
        }
        assert!(seen.ends_with(marker), "never printed {:?}", String::from_utf8_lossy(marker));
        std::thread::sleep(Duration::from_millis(100));
        let status = std::fs::read_to_string(format!("/proc/{}/status", child.id()))
            .expect("the program's status");
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|n| n.trim().trim_end_matches("kB").trim().parse().ok())
            .expect("a resident size")
    }
    let exe = build("cancel_everywhere_token", SOURCE);
    for backend in BACKENDS {
        let mut child = Command::new(&exe)
            .env("KHORA_FIBERS", backend)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the program should run");
        let mut out = child.stdout.take().expect("stdout");
        let first = resident_after(&mut child, &mut out, b"first\n");
        let second = resident_after(&mut child, &mut out, b"second\n");
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            second < first + 4096,
            "`{backend}`: resident memory grew from {first} kB to {second} kB over 900 cancels"
        );
    }
}

/// A fiber whose finalizer blocks for ever on a `receive`.
///
/// **The fiber says its finalizer is registered before it is canceled.** A
/// 50 ms pause was not enough: on a slow Windows runner the cancel arrived
/// before the fiber's first turn, a fiber stopped then never runs, and the
/// program printed no `finalizer started`. Waiting on `ready` fixes the
/// order.
const STUBBORN: &str = "module main;
import std::core::{print, Fiber, Channel, Region};
import std::clock::{Clock};

fn stubborn(ch: Channel<Int>, ready: Channel<Int>) -> () {
  let region = Region::open();
  Region::defer(region, fn () => {
    print(\"finalizer started\");
    let _ = Channel::receive(ch);
    print(\"finalizer gave up\");
  });
  Channel::send(ready, 1);
  let mut n = 0;
  loop { n = n + 1; }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ch: Channel<Int> = Channel::bounded(1);
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => stubborn(ch, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    Fiber::cancel(f);
    clock.sleep(200);
    print(\"after two cancels, finished: ${Fiber::finished(f)}\");
    ESCALATE;
    Fiber::wait(f);
    print(\"stopped\");
    0
  }
}
";

/// **A shielded finalizer survives a double cancel and is ended by
/// `Fiber::abort`.** Canceling twice is canceling once: the finalizer is
/// still waiting after both. `abort` cuts it short.
#[test]
fn a_finalizer_survives_a_second_cancel_and_is_ended_by_abort() {
    let source = STUBBORN.replace("ESCALATE", "Fiber::abort(f)");
    for (backend, ran) in on_both("cancel_everywhere_abort", &source) {
        assert!(!ran.hung, "`{backend}`: abort did not end the finalizer: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "finalizer started\nafter two cancels, finished: false\nstopped\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **Without the escalation the same program hangs.** The control for the
/// test above and the one below: it is what makes either of them evidence
/// that `abort` or the deadline ended the finalizer, rather than something
/// else ending it.
#[test]
fn a_finalizer_blocked_for_ever_hangs_its_fiber_without_an_escalation() {
    let source = STUBBORN.replace("ESCALATE", "()");
    let exe = build("cancel_everywhere_no_escalation", &source);
    for backend in BACKENDS {
        let ran = run(&exe, backend, Duration::from_secs(3), None);
        assert!(ran.hung, "`{backend}`: something other than abort ended it: {}", ran.stdout);
        assert_eq!(ran.stdout, "finalizer started\nafter two cancels, finished: false\n");
    }
}

/// **A nursery child whose finalizer blocks for ever is ended by the
/// deadline.** The parent is canceled with `Fiber::cancel_within`; its child,
/// in a nursery, is in shielded cleanup waiting on a `receive` nobody answers.
/// When the deadline passes the parent is aborted, and the abort reaches the
/// child through the nursery. Without the deadline, the same program is
/// killed by the watchdog.
#[test]
fn a_deadline_ends_a_nursery_child_stuck_in_cleanup() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Fibers, Channel, Region, Nursery};
import std::clock::{Clock};

fn stubborn(ch: Channel<Int>, ready: Channel<Int>) -> () {
  let region = Region::open();
  Region::defer(region, fn () => {
    print(\"child's finalizer started\");
    let _ = Channel::receive(ch);
  });
  Channel::send(ready, 1);
  let mut n = 0;
  loop { n = n + 1; }
}

fn fan(ch: Channel<Int>, ready: Channel<Int>) -> () with { nursery: Nursery } {
  nursery.adopt(Fiber::spawn(fn () => stubborn(ch, ready)))
}

fn body(ch: Channel<Int>, ready: Channel<Int>) -> () {
  let crew = Fibers::open();
  let _ = with { nursery: handler for Nursery { adopt: fn f => Fibers::adopt(crew, f) } } {
    fan(ch, ready)
  };
  let _ = Fibers::wait(crew);
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ch: Channel<Int> = Channel::bounded(1);
    let ready: Channel<Int> = Channel::bounded(1);
    let parent = Fiber::spawn(fn () => body(ch, ready));
    let _ = Channel::receive(ready);
    STOP;
    Fiber::wait(parent);
    print(\"parent stopped\");
    0
  }
}
";
    let with_deadline = build(
        "cancel_everywhere_deadline",
        &SOURCE.replace("STOP", "Fiber::cancel_within(parent, 300)"),
    );
    let without = build("cancel_everywhere_no_deadline", &SOURCE.replace("STOP", "Fiber::cancel(parent)"));
    for backend in BACKENDS {
        let ran = run(&with_deadline, backend, Duration::from_secs(20), None);
        assert!(!ran.hung, "`{backend}`: the deadline did not end the child: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: stdout {:?}, stderr {:?}", ran.stdout, ran.stderr);
        assert_eq!(ran.stdout, "child's finalizer started\nparent stopped\n", "`{backend}`");

        let ran = run(&without, backend, Duration::from_secs(3), None);
        assert!(ran.hung, "`{backend}`: without a deadline it should hang: {}", ran.stdout);
    }
}

/// **SIGTERM to a `main` with no row: finalizers run, exit 130.** `main`
/// is in an infallible loop when the signal arrives.
#[cfg(unix)]
#[test]
fn sigterm_stops_a_main_with_no_row_and_runs_its_finalizers() {
    const SOURCE: &str = "module main;
import std::core::{print, Region};

pub fn main() -> Int {
  let region = Region::open();
  Region::defer(region, fn () => print(\"main's finalizer ran\"));
  print(\"ready\");
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  print(\"ran to the end ${t}\");
  0
}
";
    let exe = build("cancel_everywhere_sigterm", SOURCE);
    for backend in BACKENDS {
        let ran = run(&exe, backend, Duration::from_secs(20), Some(Duration::from_millis(500)));
        assert!(!ran.hung, "`{backend}`: SIGTERM did not stop it: {}", ran.stdout);
        assert_eq!(ran.stdout, "ready\nmain's finalizer ran\n", "`{backend}`");
        assert_eq!(ran.code, Some(130), "`{backend}`: {}", ran.stderr);
    }
}

/// **A finalizer that blocks does not swallow another fiber's finalizer.** A
/// region's finalizers run while the heap is releasing it; a finalizer that
/// parks (here, on a `receive` nobody answers) used to leave that release
/// open on its worker thread, and every object the next fiber on the worker
/// freed -- its own region included -- was queued behind the parked one and
/// never released. The target was canceled, reported finished and
/// canceled, and its finalizer never ran. Several blocked cleanups are
/// spread across the workers, so the target is certain to land on one.
#[test]
fn a_blocked_finalizer_does_not_swallow_the_next_fibers_finalizer() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Channel, Region, Shared};
import std::clock::{Clock};

fn sticky(ch: Channel<Int>) -> () {
  let region = Region::open();
  Region::defer(region, fn () => { let _ = Channel::receive(ch); () });
  let _ = Channel::receive(ch);
  ()
}

fn target(ch: Channel<Int>, flag: Shared<Int>, ready: Channel<Int>) -> () {
  let region = Region::open();
  Region::defer(region, fn () => { Shared::set(flag, 1) });
  Channel::send(ready, 1);
  let _ = Channel::receive(ch);
  ()
}

fn trial(n: Int) -> Int {
  with { clock: Clock::real() } {
    let ch: Channel<Int> = Channel::bounded(1);
    let other: Channel<Int> = Channel::bounded(1);
    let flag = Shared::of(0);
    let mut j = 0;
    while j < n {
      let g = Fiber::spawn(fn () => sticky(other));
      Fiber::detach(g);
      j = j + 1;
    };
    clock.sleep(20);
    let ready: Channel<Int> = Channel::bounded(1);
    let h = Fiber::spawn(fn () => target(ch, flag, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(h);
    Fiber::wait(h);
    let ran = Shared::get(flag);
    Fiber::detach(h);
    ran
  }
}

pub fn main() -> Int {
  let mut missed = 0;
  let mut round = 0;
  while round < 3 {
    missed = missed + (1 - trial(4)) + (1 - trial(8)) + (1 - trial(12));
    round = round + 1;
  };
  print(\"finalizers missed: ${missed}\");
  0
}
";
    for (backend, ran) in on_both("cancel_everywhere_blocked_finalizer", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "finalizers missed: 0\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A loop that goes round by `continue` stops too.** The check sat at the
/// end of the body, and `continue` jumps to the head without reaching it, so a
/// loop that always went round that way never checked and a canceled fiber in
/// one ran to its end. `while`, `loop` and `for`, each with its only way round
/// a `continue`.
#[test]
fn a_loop_that_goes_round_by_continue_stops() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Channel, Step, Iterator, Range};
import std::clock::{Clock};

fn by_while(n: Int) -> Int {
  let mut i = 0;
  let mut total = 0;
  while i < n {
    i = i + 1;
    if i > 0 { total = (total * 31 + i) % 1000003; continue; };
  };
  total
}

fn by_loop(n: Int) -> Int {
  let mut i = 0;
  let mut total = 0;
  loop {
    i = i + 1;
    if i >= n { break total; };
    total = (total * 31 + i) % 1000003;
    continue;
  }
}

fn by_for(n: Int) -> Int {
  let mut total = 0;
  for i in Range::Of(0, n) {
    if i >= 0 { total = (total * 31 + i) % 1000003; continue; };
  };
  total
}

fn timed(which: Int) -> Bool {
  with { clock: Clock::real() } {
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => {
      Channel::send(ready, 1);
      match which {
        0 => by_while(300000000000),
        1 => by_loop(300000000000),
        _ => by_for(300000000000),
      }
    });
    let _ = Channel::receive(ready);
    clock.sleep(50);
    let t0 = clock.monotonic_millis();
    Fiber::cancel(f);
    Fiber::wait(f);
    let waited = clock.monotonic_millis() - t0;
    Fiber::canceled(f) && waited < 2000
  }
}

pub fn main() -> Int {
  print(\"while ${timed(0)}; loop ${timed(1)}; for ${timed(2)}\");
  0
}
";
    for (backend, ran) in on_both("cancel_everywhere_continue", SOURCE) {
        assert!(!ran.hung, "`{backend}`: a loop never stopped: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "while true; loop true; for true\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// A function that calls through a function value and defers a cleanup,
/// run under `Fiber::abort` `TRIALS` times. `DEFERRED` is the `Region::defer`
/// statement, so the two tests below differ only in how the finalizer is
/// written.
///
/// Each child says it has started only after the finalizer is registered, and
/// then spins in `body()` until it is stopped, so the abort is the only way
/// out and always lands after the registration.
const ABORTED_NEXT_TO_A_CALL: &str = "module main;
import std::core::{print, Fiber, Channel, Region, Shared};

fn guarded(ran: Shared<Int>, started: Channel<Int>, body: () -> ()) -> () {
  let region = Region::open();
  DEFERRED
  Channel::send(started, 1);
  body()
}

fn spin() -> () {
  let mut n = 0;
  loop { n = n + 1; }
}

pub fn main() -> Int {
  let ran = Shared::of(0);
  let mut trial = 0;
  while trial < 20 {
    let started: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => guarded(ran, started, spin));
    let _ = Channel::receive(started);
    Fiber::abort(f);
    Fiber::wait(f);
    trial = trial + 1;
  }
  print(\"finalizer ran ${Shared::get(ran)} of 20\");
  0
}
";

/// **`abort` does not skip a finalizer that has not started.** The finalizer
/// is a lambda written in `Region::defer`'s argument, inside a function that
/// calls `body()`. Such a function's lambdas poll when entered, and a forced
/// stop fired at that poll, before the finalizer's first line, so it ran 0
/// times in 20 on both backends: the shape of `transaction`'s rollback and a
/// pool's give-back.
#[test]
fn abort_runs_a_finalizer_written_next_to_a_call_through_a_function_value() {
    let source = ABORTED_NEXT_TO_A_CALL
        .replace("DEFERRED", "Region::defer(region, fn () => Shared::set(ran, Shared::get(ran) + 1));");
    for (backend, ran) in on_both("cancel_everywhere_abort_literal_finalizer", &source) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "finalizer ran 20 of 20\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A finalizer bound to a `let` first keeps its entry poll, and `abort`
/// still skips it.** Pinned, because it is a decision rather than an
/// accident: a lambda with a binding can be called through it, by itself
/// among others, so it may be the far end of a cycle that polls nowhere else.
/// Only a lambda written in the argument is exempt. If this starts running
/// the finalizer, the exemption has widened, and the argument on
/// `Backend::lambdas_poll_in` has to be made again for the new shape.
#[test]
fn abort_still_skips_a_let_bound_finalizer_next_to_a_call_through_a_function_value() {
    let source = ABORTED_NEXT_TO_A_CALL.replace(
        "DEFERRED",
        "let finalizer = fn () => Shared::set(ran, Shared::get(ran) + 1);\n  Region::defer(region, finalizer);",
    );
    for (backend, ran) in on_both("cancel_everywhere_abort_bound_finalizer", &source) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "finalizer ran 0 of 20\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// `std::db::transaction` under `Fiber::abort`, with a handler that counts.
///
/// Each child says it has started from inside the innermost body, after
/// every level's rollback is registered and its `BEGIN` or `SAVEPOINT` has
/// been answered, and then spins until it is stopped, so the abort is the
/// only way out. The handler records every verb in `log`, which `main`
/// reads once all 20 trials are over.
const ABORTED_TRANSACTION: &str = "module main;
import std::core::{print, Fiber, Channel, List, Result, Shared};
import std::db::{Db, DbError, transaction};

fn note(log: Shared<List<String>>, what: String) -> () {
  Shared::set(log, List::Cons(what, Shared::get(log)));
}

fn counting(log: Shared<List<String>>) -> Db {
  let depth = Shared::of(0);
  handler for Db {
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) => Result::Ok(1),
    depth: fn () => Shared::get(depth),
    begin: fn () => { Shared::set(depth, 1); Result::Ok(()) },
    commit: fn () => { Shared::set(depth, 0); note(log, \"commit\"); Result::Ok(()) },
    rollback: fn () => { Shared::set(depth, 0); note(log, \"rollback\"); Result::Ok(()) },
    savepoint: fn level => { Shared::set(depth, level + 1); Result::Ok(()) },
    release: fn level => { Shared::set(depth, level); note(log, \"release\"); Result::Ok(()) },
    rollback_to: fn level => {
      if Shared::get(depth) > level {
        Shared::set(depth, level);
        note(log, \"rollback to\")
      };
      Result::Ok(())
    },
    broken: fn () => note(log, \"broken\"),
  }
}

fn spin(started: Channel<Int>) -> Result<Int, DbError> {
  Channel::send(started, 1);
  let mut n = 0;
  loop { n = n + 1; }
}

fn worker(log: Shared<List<String>>, started: Channel<Int>) -> () {
  with { db: counting(log) } {
    let _ = BODY;
    ()
  }
}

/// The log, oldest first.
fn shown(log: List<String>) -> String {
  let mut out = \"\";
  let mut rest = log;
  let mut going = true;
  while going {
    match rest {
      List::Nil => going = false,
      List::Cons(what, more) => { out = what + \"; \" + out; rest = more },
    }
  };
  out
}

pub fn main() -> Int {
  let log: Shared<List<String>> = Shared::of(List::Nil);
  let mut trial = 0;
  while trial < 20 {
    let started: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => worker(log, started));
    let _ = Channel::receive(started);
    Fiber::abort(f);
    Fiber::wait(f);
    note(log, \"|\");
    trial = trial + 1;
  }
  print(shown(Shared::get(log)));
  0
}
";

/// The expected log: `per_trial` for each of 20 trials.
fn twenty(per_trial: &str) -> String {
    format!("{}\n", per_trial.repeat(20))
}

/// **`Fiber::abort` in a transaction's body runs its rollback, 20 times in
/// 20.** What this prevents: a connection handed back to a pool inside an
/// open transaction because the rollback was skipped whole. `transaction`
/// calls `body()`, so a finalizer that first called a helper stopped at the
/// helper's entry poll, before reading whether the transaction had settled,
/// and rolled back 0 times in 20 on both backends.
#[test]
fn abort_rolls_back_a_transaction() {
    let source = ABORTED_TRANSACTION.replace("BODY", "transaction(fn () => spin(started))");
    for (backend, ran) in on_both("cancel_everywhere_abort_transaction", &source) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, twenty("rollback; |; "), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **`Fiber::abort` inside a nested transaction undoes the inner savepoint,
/// then the enclosing transaction, in that order, 20 times in 20.** The order
/// is the region order: the inner `transaction`'s region ends first. An
/// enclosing rollback that ran first would leave the inner `ROLLBACK TO`
/// naming a savepoint the server had already dropped.
#[test]
fn abort_in_a_nested_transaction_undoes_inner_then_outer() {
    let source = ABORTED_TRANSACTION
        .replace("BODY", "transaction(fn () => transaction(fn () => spin(started)))");
    for (backend, ran) in on_both("cancel_everywhere_abort_nested_transaction", &source) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, twenty("rollback to; rollback; |; "), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}
