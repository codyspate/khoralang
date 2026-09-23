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
    let mut signalled = false;
    let mut hung = false;
    loop {
        if child.try_wait().expect("waiting").is_some() {
            break;
        }
        if let Some(after) = signal_after {
            if !signalled && started.elapsed() >= after {
                signalled = true;
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
fn an_infallible_loop_stops_when_its_fiber_is_cancelled() {
    const SOURCE: &str = "module main;
import std::core::{print, Region, Fiber};
import std::clock::{Clock};

fn spin(n: Int) -> Int {
  let region = Region::open();
  Region::defer(region, fn () => print(\"finalizer ran\"));
  let mut i = 0;
  let mut total = 0;
  while i < n { total = (total * 31 + i) % 1000003; i = i + 1; };
  print(\"spin reached its end\");
  total
}

fn caller(n: Int) -> Int {
  let got = spin(n);
  print(\"caller's tail ran\");
  got
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let f = Fiber::spawn(fn () => caller(300000000000));
    clock.sleep(50);
    Fiber::cancel(f);
    let t0 = clock.monotonic_millis();
    Fiber::wait(f);
    let waited = clock.monotonic_millis() - t0;
    print(\"cancelled: ${Fiber::cancelled(f)}; prompt: ${waited < 2000}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_loop", SOURCE) {
        assert!(!ran.hung, "`{backend}`: the loop never stopped: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "finalizer ran\ncancelled: true; prompt: true\n",
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
import std::core::{print, Fiber};
import std::clock::{Clock};

fn spin() -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn parent() -> Int {
  let child = Fiber::spawn(fn () => spin());
  let got = Fiber::join(child);
  print(\"parent's tail ran with ${got}\");
  got
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let b = Fiber::spawn(fn () => parent());
    clock.sleep(50);
    Fiber::cancel(b);
    Fiber::wait(b);
    print(\"parent cancelled: ${Fiber::cancelled(b)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_join", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "parent cancelled: true\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A frame that returns a pointer stops without taking the process down.**
/// A total `catch` in a function returning `String`, on a fiber, used to end
/// the process: there was no zero `String` to hand back.
#[test]
fn a_pointer_returning_frame_stops_without_ending_the_process() {
    const SOURCE: &str = "module main;
import std::core::{print, Region, Fiber};
import std::clock::{Clock};

fn step() -> Int raises String { 1 }

fn name_it() -> String {
  let region = Region::open();
  Region::defer(region, fn () => print(\"finalizer ran\"));
  let mut n = 0;
  while n < 300000000000 {
    n = n + (step()! catch { _ => 1 });
  };
  \"unreachable\"
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let a = Fiber::spawn(fn () => name_it());
    clock.sleep(50);
    Fiber::cancel(a);
    Fiber::wait(a);
    print(\"cancelled: ${Fiber::cancelled(a)}\");
    print(\"process still here\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_pointer", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "finalizer ran\ncancelled: true\nprocess still here\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **Three infallible frames unwind and release everything they held.**
/// Twenty fibers are cancelled in the innermost of three frames, each holding
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
import std::core::{print, Fiber, Shared};
import std::clock::{Clock};

fn step() -> Int raises String { 0 }

fn worker(caught: Shared<Int>) -> () {
  loop {
    let _ = step()! catch { _ => { Shared::set(caught, Shared::get(caught) + 1); 0 } };
  }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let caught = Shared::of(0);
    let f = Fiber::spawn(fn () => worker(caught));
    clock.sleep(50);
    Fiber::cancel(f);
    Fiber::wait(f);
    print(\"cancelled: ${Fiber::cancelled(f)}; caught: ${Shared::get(caught)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_catch", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "cancelled: true; caught: 0\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A cancelled `clock.sleep` does not run the statement after it**, on
/// either backend. The sleep is woken and gives up early, answering as if it
/// had finished; without a check after it the fiber would run one more step of
/// the work it was told to abandon -- here, the `print`.
#[test]
fn a_cancelled_sleep_does_not_run_the_statement_after_it() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber};
import std::clock::{Clock};

fn napper() -> () with { clock: Clock } {
  clock.sleep(8000);
  print(\"statement after the sleep ran\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let f = Fiber::spawn(fn () => napper());
    clock.sleep(100);
    let t0 = clock.monotonic_millis();
    Fiber::cancel(f);
    Fiber::wait(f);
    let waited = clock.monotonic_millis() - t0;
    print(\"cancelled: ${Fiber::cancelled(f)}; prompt: ${waited < 2000}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_sleep", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(ran.stdout, "cancelled: true; prompt: true\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A `Fiber::join` inside a change function that comes back cancelled
/// leaves the cell as it was**, and the fiber stops. The updater is cancelled
/// while its change function waits on a child; the cell must keep `41` -- not
/// the zero the unwound change function would have handed back -- and a
/// `String` cell must keep `hello` rather than a null the next read crashes
/// on.
#[test]
fn a_join_cancelled_inside_a_change_function_changes_nothing() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Shared};
import std::clock::{Clock};

fn spin() -> Int {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = (t * 31 + i) % 1000003; i = i + 1; };
  t
}

fn updater(cell: Shared<Int>) -> () {
  let other = Fiber::spawn(fn () => spin());
  Shared::update(cell, fn n => { let got = Fiber::join(other); n + got + 1 });
  print(\"TAIL updater ran\");
}

fn updater_s(text: Shared<String>) -> () {
  let other = Fiber::spawn(fn () => spin());
  Shared::update(text, fn s => { let got = Fiber::join(other); s + \"!${got}\" });
  print(\"TAIL updater_s ran\");
}

fn modifier(cell: Shared<Int>) -> () {
  let other = Fiber::spawn(fn () => spin());
  let answer = Shared::modify(cell, fn n => { let got = Fiber::join(other); { state: n + got, result: got } });
  print(\"TAIL modifier ran ${answer}\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let cell = Shared::of(41);
    let text = Shared::of(\"hello\");
    let f = Fiber::spawn(fn () => updater(cell));
    clock.sleep(50);
    Fiber::cancel(f);
    Fiber::wait(f);
    print(\"int: cancelled ${Fiber::cancelled(f)}; cell = ${Shared::get(cell)}\");
    let g = Fiber::spawn(fn () => updater_s(text));
    clock.sleep(50);
    Fiber::cancel(g);
    Fiber::wait(g);
    print(\"string: cancelled ${Fiber::cancelled(g)}; text = ${Shared::get(text)}\");
    let h = Fiber::spawn(fn () => modifier(cell));
    clock.sleep(50);
    Fiber::cancel(h);
    Fiber::wait(h);
    print(\"modify: cancelled ${Fiber::cancelled(h)}; cell = ${Shared::get(cell)}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_pinjoin", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "int: cancelled true; cell = 41\n\
             string: cancelled true; text = hello\n\
             modify: cancelled true; cell = 41\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A change function that joins a child somebody else stopped changes
/// nothing, and its caller stops** -- although nobody cancelled the caller.
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

/// **A `String` `<` is a cancellation point that leaks nothing.** `pick`'s
/// only way to stop is the comparison, which is a call to `impl Ord for
/// String`, and `held` is moved into the answer after it. Fibers are cancelled
/// inside the comparison, once and then twenty times; the live count moves by
/// the same amount both times. (That amount is 1, for one round as for twenty
/// -- measured, not explained here. A leak on the unwind is a delta that grows
/// with the rounds, which is what this pins.)
#[test]
fn a_string_comparison_cancelled_leaks_nothing() {
    const SOURCE: &str = "module main;
import std::core::{Fiber, print};
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

fn rounds(a: String, b: String, n: Int) -> Int with { clock: Clock } {
  let before = khora_live_count();
  let mut round = 0;
  let mut stopped = 0;
  while round < n {
    let f = Fiber::spawn(fn () => { let h = \"held-${round}\"; String::byte_length(pick(h, a, b)) });
    clock.sleep(20);
    Fiber::cancel(f);
    Fiber::wait(f);
    if Fiber::cancelled(f) { stopped = stopped + 1; };
    round = round + 1;
  };
  print(\"stopped ${stopped}\");
  khora_live_count() - before
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let a = big(\"ab\");
    let b = big(\"ab\");
    let once = rounds(a, b, 1);
    let many = rounds(a, b, 20);
    print(\"per-cancel growth ${many - once}\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_strcmp", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout, "stopped 1\nstopped 20\nper-cancel growth 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **Recursion through a function value stops promptly**, both through a
/// record field (`k.f(k, n - 1)`, which never names `walk`) and through a
/// lambda's own binding. Neither has a loop or a named call cycle; each runs
/// for seconds uncancelled. Both have to stop within 100 ms of the cancel.
#[test]
fn recursion_through_a_function_value_stops_promptly() {
    const SOURCE: &str = "module main;
import std::core::{Fiber, print};
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

fn stop_after(f: Fiber<Int, {}>, name: String) -> () with { clock: Clock } {
  clock.sleep(30);
  let t = clock.monotonic_millis();
  Fiber::cancel(f);
  Fiber::wait(f);
  print(\"${name}: cancelled ${Fiber::cancelled(f)}; within 100 ms ${clock.monotonic_millis() - t < 100}\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    stop_after(Fiber::spawn(fn () => knot()), \"knot\");
    stop_after(Fiber::spawn(fn () => by_rec_lambda()), \"lambda\");
    0
  }
}
";
    for (backend, ran) in on_both("cancel_everywhere_knot", SOURCE) {
        assert!(!ran.hung, "`{backend}`: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "knot: cancelled true; within 100 ms true\n\
             lambda: cancelled true; within 100 ms true\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`");
    }
}

/// **A blocking socket call that gave up on a cancel is not taken for a
/// failure.** A cancelled `receive` and a cancelled `accept_on` come back with
/// their failure value; the fiber must stop there, not count an I/O error that
/// did not happen and run its tail. A channel receive, which already did this,
/// is the control.
#[test]
fn a_socket_call_that_gave_up_is_not_taken_for_a_failure() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Array, Shared, Channel};
import std::clock::{Clock};
import std::net::socket::{listen_on, accept_on, connect_to, receive, invalid_handle};

fn reader(conn: Int, errors: Shared<Int>) -> () {
  let buf: Array<U8> = Array::new(64, 0);
  let n = receive(conn, buf);
  if n < 0 { Shared::set(errors, Shared::get(errors) + 1); print(\"TAIL recv\"); }
  else { print(\"TAIL recv got bytes\"); }
}

fn acceptor(server: Int, errors: Shared<Int>) -> () {
  let c = accept_on(server);
  if c == invalid_handle() { Shared::set(errors, Shared::get(errors) + 1); print(\"TAIL accept\"); }
  else { print(\"TAIL accept got one\"); }
}

fn chan(ch: Channel<Int>) -> () {
  let _ = Channel::receive(ch);
  print(\"TAIL channel\");
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let errors = Shared::of(0);
    let server = listen_on(PORT);
    let client = connect_to(\"127.0.0.1\", PORT);
    let conn = accept_on(server);
    print(\"setup ${server >= 0} ${client >= 0} ${conn >= 0}\");
    let r = Fiber::spawn(fn () => reader(conn, errors));
    clock.sleep(50); Fiber::cancel(r); Fiber::wait(r);
    print(\"reader cancelled ${Fiber::cancelled(r)}\");
    let a = Fiber::spawn(fn () => acceptor(server, errors));
    clock.sleep(50); Fiber::cancel(a); Fiber::wait(a);
    print(\"acceptor cancelled ${Fiber::cancelled(a)}\");
    let ch: Channel<Int> = Channel::bounded(1);
    let c = Fiber::spawn(fn () => chan(ch));
    clock.sleep(50); Fiber::cancel(c); Fiber::wait(c);
    print(\"channel cancelled ${Fiber::cancelled(c)}\");
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
             reader cancelled true\n\
             acceptor cancelled true\n\
             channel cancelled true\n\
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

/// A fiber whose finalizer blocks for ever on a `receive`.
const STUBBORN: &str = "module main;
import std::core::{print, Fiber, Channel, Region};
import std::clock::{Clock};

fn stubborn(ch: Channel<Int>) -> () {
  let region = Region::open();
  Region::defer(region, fn () => {
    print(\"finalizer started\");
    let _ = Channel::receive(ch);
    print(\"finalizer gave up\");
  });
  let mut n = 0;
  loop { n = n + 1; }
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ch: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => stubborn(ch));
    clock.sleep(50);
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
/// `Fiber::abort`.** Cancelling twice is cancelling once: the finalizer is
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
/// deadline.** The parent is cancelled with `Fiber::cancel_within`; its child,
/// in a nursery, is in shielded cleanup waiting on a `receive` nobody answers.
/// When the deadline passes the parent is aborted, and the abort reaches the
/// child through the nursery. Without the deadline, the same program is
/// killed by the watchdog.
#[test]
fn a_deadline_ends_a_nursery_child_stuck_in_cleanup() {
    const SOURCE: &str = "module main;
import std::core::{print, Fiber, Fibers, Channel, Region, Nursery};
import std::clock::{Clock};

fn stubborn(ch: Channel<Int>) -> () {
  let region = Region::open();
  Region::defer(region, fn () => {
    print(\"child's finalizer started\");
    let _ = Channel::receive(ch);
  });
  let mut n = 0;
  loop { n = n + 1; }
}

fn fan(ch: Channel<Int>) -> () with { nursery: Nursery } {
  nursery.adopt(Fiber::spawn(fn () => stubborn(ch)))
}

fn body(ch: Channel<Int>) -> () {
  let crew = Fibers::open();
  let _ = with { nursery: handler for Nursery { adopt: fn f => Fibers::adopt(crew, f) } } {
    fan(ch)
  };
  let _ = Fibers::wait(crew);
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let ch: Channel<Int> = Channel::bounded(1);
    let parent = Fiber::spawn(fn () => body(ch));
    clock.sleep(100);
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
        assert_eq!(ran.stdout, "child's finalizer started\nparent stopped\n", "`{backend}`");
        assert_eq!(ran.code, Some(0), "`{backend}`");

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
