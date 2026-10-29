#![cfg(feature = "llvm")]

//! `acquire` registers the release of what it is handed before anything can
//! stop the fiber.
//!
//! **What these guard: a resource opened and its release registered
//! nowhere.** `acquire(open(..), release)` is the documented way to tie a
//! file or a socket to a scope, and a fiber canceled between the open and
//! the registration unwound with the handle made and nothing holding it: a
//! file descriptor per canceled `read_text`, 291 in 1,000 on the thread
//! backend. A server that cancels its handlers (timeouts, `race` losers, a
//! nursery shutting down) runs into that as `EMFILE`.
//!
//! The stops were `acquire`'s own entry poll and the `Scope` handler's,
//! reached through `scope.defer`; and in `std`, a listener made before the
//! two `with_data` lambdas that set it up, and an HTTP server that bound its
//! port outside the region that closes it.
//!
//! - [`a_canceled_read_text_leaves_no_file_open`]: 1,000 fibers looping
//!   `std::fs::read_text`, canceled at varied points, against the runtime's
//!   exact count of open file handles (and `/dev/fd` where it exists).
//! - [`a_canceled_acquire_of_a_connect_leaves_no_socket_open`], the
//!   documented `acquire(connect_to(..), fn c => shut(c))` against a local
//!   listener.
//! - [`a_canceled_listen_leaves_no_listener_open`], `Router::listen_quietly`.
//! - [`acquire_returns_the_value_and_releases_it_once`]: the ordinary path,
//!   and a cancel after the registration.
//!
//! Every program runs on both fiber backends, under a watchdog. The cancel
//! offsets are `(n * 7919) % spread` spin iterations, so 1,000 rounds land
//! all over the loop rather than at one point of it.

use std::time::Duration;

use crate::claims::{on_both, Ran};

/// Far over what any of these needs on a loaded runner, and short of the
/// suite's own timeout.
const PATIENCE: Duration = Duration::from_secs(240);

/// The shared harness: `work` is the body each fiber loops, `warm` how many
/// uncanceled rounds of it run before the baseline is taken (the runtime's
/// own descriptors, the reactor's and the blocking pool's, are made on first
/// use), and `count` an expression answering how many of the resource are
/// open.
fn storm(imports: &str, decls: &str, work: &str, count: &str) -> String {
    format!(
        "module main;

import std::core::{{ChildFailed, Fiber, List, Result, attempt, print}};
import std::fs::{{FsRead}};
{imports}

{decls}

fn work() -> () {{
{work}
}}

fn loops() -> () {{
  let mut i = 0;
  while i < 1000000 {{ work(); i = i + 1 }}
}}

fn spin(k: Int) -> Int {{
  let mut i = 0;
  let mut s = 0;
  while i < k {{ s = s + i; i = i + 1 }};
  s
}}

/// Descriptors this process holds, on a system that lists them; -1 where
/// it does not.
fn fds() -> Int {{
  with {{ reads: FsRead::real() }} {{
    match attempt(fn () => reads.read_dir(\"/dev/fd\")!) {{
      Result::Ok(names) => List::length(names),
      Result::Err(_) => -1,
    }}
  }}
}}

pub fn main() -> Int raises ChildFailed {{
  let mut k = 0;
  while k < 20 {{
    let g = Fiber::spawn(fn () => loops());
    let _ = spin(200000);
    Fiber::cancel(g);
    Fiber::wait(g)! catch {{ _ => () }};
    k = k + 1
  }};
  let base = fds();
  let open = {count};
  let mut canceled = 0;
  let mut n = 0;
  while n < 1000 {{
    let f = Fiber::spawn(fn () => loops());
    let _ = spin((n * 7919) % 300000);
    Fiber::cancel(f);
    Fiber::wait(f)! catch {{ _ => () }};
    if Fiber::canceled(f) {{ canceled = canceled + 1 }};
    n = n + 1
  }};
  let fd_delta = if base < 0 {{ 0 }} else {{ fds() - base }};
  print(\"canceled ${{Int::to_string(canceled)}} open ${{Int::to_string({count} - open)}} fds ${{Int::to_string(fd_delta)}}\");
  0
}}
"
    )
}

/// The `canceled N open O fds F` line, as three numbers.
fn tally(backend: &str, ran: &Ran) -> (i64, i64, i64) {
    assert!(!ran.hung, "{backend}: the program hung: {}{}", ran.stdout, ran.stderr);
    assert_eq!(ran.code, Some(0), "{backend}: {}{}", ran.stdout, ran.stderr);
    let line = ran
        .stdout
        .lines()
        .find(|l| l.starts_with("canceled "))
        .unwrap_or_else(|| panic!("{backend}: no tally in {}", ran.stdout));
    let words: Vec<&str> = line.split_whitespace().collect();
    let at = |key: &str| -> i64 {
        let i = words.iter().position(|w| *w == key).unwrap_or_else(|| panic!("no `{key}` in `{line}`"));
        words[i + 1].parse().unwrap_or_else(|_| panic!("`{key}` is not a number in `{line}`"))
    };
    (at("canceled"), at("open"), at("fds"))
}

/// **1,000 fibers canceled while they loop `read_text` leave no file open.**
///
/// Each canceled fiber was somewhere in a `read_text` -- the open, the
/// `acquire`, the read, the close -- or between two. The runtime's count of
/// open file handles is exact, so the bound is 0, and on a system that lists
/// its descriptors the process's own count must agree.
#[test]
fn a_canceled_read_text_leaves_no_file_open() {
    let program = storm(
        "import std::fs::{read_text};",
        "extern fn khora_fs_handles_open() -> Int;",
        "  with { reads: FsRead::real() } {
    let _ = attempt(fn () => read_text(\"HOSTFILE\")!);
  }",
        "khora_fs_handles_open()",
    )
    .replace("HOSTFILE", &readable_file());
    for (backend, ran) in on_both("acquire_read_text", &program, PATIENCE) {
        let (canceled, open, fds) = tally(backend, &ran);
        assert!(canceled >= 900, "{backend}: too few fibers were canceled to prove anything: {}", ran.stdout);
        assert_eq!(open, 0, "{backend}: canceled reads left files open: {}", ran.stdout);
        assert_eq!(fds, 0, "{backend}: canceled reads left descriptors open: {}", ran.stdout);
    }
}

/// A file every runner can read, written for the test so its size is known.
fn readable_file() -> String {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("acquire_read_text.txt");
    std::fs::write(&path, "a line of text\n").expect("a file to read");
    path.to_string_lossy().replace('\\', "/")
}

/// A listener on an ephemeral loopback port that accepts and drops every
/// connection until the guard is dropped.
#[cfg(unix)]
struct Accepting {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Accepting {
    fn start() -> Accepting {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        listener.set_nonblocking(true).expect("non-blocking");
        let port = listener.local_addr().expect("an address").port();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = stop.clone();
        let thread = std::thread::spawn(move || {
            while !seen.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(_) => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        });
        Accepting { port, stop, thread: Some(thread) }
    }
}

#[cfg(unix)]
impl Drop for Accepting {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// **The documented `acquire(connect_to(..), fn c => shut(c))` leaves no
/// socket open when its fiber is canceled.** `connect_to` ends with the
/// socket made and checks for a cancel only when the connect gave up, so a
/// cancel landing after it returns a socket is acted on once the release is
/// registered.
///
/// Counted with `/dev/fd`, so not on Windows, which has no such listing.
#[cfg(unix)]
#[test]
fn a_canceled_acquire_of_a_connect_leaves_no_socket_open() {
    let listener = Accepting::start();
    let program = storm(
        "import std::core::{Scope, acquire, scoped};\nimport std::net::socket::{connect_to, shut};",
        "fn ask(port: Int) -> Int with { scope: Scope } {
  acquire(connect_to(\"127.0.0.1\", port), fn c => shut(c))
}",
        &format!("  let _ = scoped(fn () => ask({}));", listener.port),
        "0",
    );
    for (backend, ran) in on_both("acquire_connect", &program, PATIENCE) {
        let (canceled, _, fds) = tally(backend, &ran);
        assert!(canceled >= 900, "{backend}: too few fibers were canceled to prove anything: {}", ran.stdout);
        assert_eq!(fds, 0, "{backend}: canceled connects left sockets open: {}", ran.stdout);
    }
}

/// **A canceled `Router::listen_quietly` leaves no listening socket open.**
/// The listener is registered for release by the call that makes it, with
/// nothing between that can stop. Port 0, so every round binds a fresh
/// ephemeral port and two rounds never contend.
#[cfg(unix)]
#[test]
fn a_canceled_listen_leaves_no_listener_open() {
    let program = storm(
        "import std::net::http::{Router};",
        "",
        "  let _ = attempt(fn () => Router::listen_quietly(Router::new(), 0)!);",
        "0",
    );
    for (backend, ran) in on_both("acquire_listen", &program, PATIENCE) {
        let (canceled, _, fds) = tally(backend, &ran);
        assert!(canceled >= 900, "{backend}: too few fibers were canceled to prove anything: {}", ran.stdout);
        assert_eq!(fds, 0, "{backend}: canceled servers left listeners open: {}", ran.stdout);
    }
}

/// **`acquire`'s hold ends with `acquire`, however it is left.** A user
/// `Scope` handler whose `defer` waits on a child under a `timeout` is
/// unwound when the deadline fires, with no `khora_canceled` on the way.
///
/// The fiber then cancels itself and calls `probe`, whose only point before
/// it sets the flag is its entry poll. With the hold ended only on
/// `acquire`'s normal return, that entry poll was let past the cancel. The
/// control is the same handler reached through `scope.defer` directly.
#[test]
fn an_unwound_acquire_does_not_hold_the_next_entry_poll() {
    let program = r#"module main;
import std::core::{ChildFailed, Fiber, Option, Region, Scope, Shared, acquire, print, timeout};

extern fn khora_cancel() -> ();

fn spin(k: Int) -> Int {
  let mut i = 0;
  let mut s = 0;
  while i < k { s = s + i; i = i + 1 };
  s
}

fn probe(flag: Shared<Int>, g: (Int) -> Int) -> () {
  Shared::set(flag, 1);
  let _ = g(1);
  ()
}

fn holder(r: Region) -> Scope {
  handler for Scope {
    defer: fn f => {
      let c = Fiber::spawn(fn () => { let _ = spin(300000000); () });
      Fiber::wait(c)! catch { _ => () };
      Region::defer(r, f)
    }
  }
}

fn case(use_acquire: Bool, flag: Shared<Int>, fired: Shared<Int>) -> () {
  let r = Region::open();
  let h = holder(r);
  let answered = timeout(30, fn () => {
    with { scope: h } {
      if use_acquire { let _ = acquire(5, fn _x => ()); () } else { scope.defer(fn () => ()) }
    }
  });
  match answered { Option::Some(_) => (), Option::None => Shared::set(fired, 1) };
  khora_cancel();
  probe(flag, fn x => x);
}

pub fn main() -> Int raises ChildFailed {
  let fa = Shared::of(0);
  let ta = Shared::of(0);
  let fb = Shared::of(0);
  let tb = Shared::of(0);
  let a = Fiber::spawn(fn () => case(true, fa, ta));
  Fiber::wait(a)! catch { _ => () };
  let b = Fiber::spawn(fn () => case(false, fb, tb));
  Fiber::wait(b)! catch { _ => () };
  print("acquire fired ${Shared::get(ta)} ran ${Shared::get(fa)}");
  print("control fired ${Shared::get(tb)} ran ${Shared::get(fb)}");
  0
}
"#;
    for (backend, ran) in on_both("acquire_unwound", program, Duration::from_secs(120)) {
        assert!(!ran.hung, "{backend}: the program hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: {}{}", ran.stdout, ran.stderr);
        assert_eq!(
            ran.stdout,
            "acquire fired 1 ran 0\ncontrol fired 1 ran 0\n",
            "{backend}: `ran 1` is an entry poll let past a pending cancel after `acquire` was \
             left: {}",
            ran.stderr
        );
    }
}

/// **A `Scope` handler that calls `acquire` again through itself is stopped
/// by `cancel` and by `abort`.** The handler is held in a `mut` field, so the
/// cycle has no named function but `acquire` and no loop, `!` or wait: its
/// only polls are the handler lambda's entry, which `acquire` holds. Holding
/// it at every level, the cycle ran to the end of its recursion under both,
/// or out of stack.
///
/// **The request is made while the cycle is known to be running, and the
/// cycle cannot finish first.** A round of the cycle goes 2,000 levels deep
/// and unwinds, and the fiber starts round after round until it is stopped,
/// so it is still cycling whenever the request lands. The first version
/// recursed once, 20,000 deep, and canceled when the depth passed 50; on a
/// loaded runner the child could finish all of it before the parent's
/// request, which read as an `abort` that failed. The parent sets `sent`
/// after `cancel` or `abort` returns, and the handler records the level at
/// which it first reads it; reading it through a `Shared` cell orders the
/// request before everything that follows. From there the next entry poll
/// that is not held is at most two levels on, so at most two more may run;
/// a fiber that stopped before it read `sent` stopped sooner still, and
/// nothing but the request stops this one before its 40 million levels.
///
/// Under the depth mutant (every level holds) the round runs to its end,
/// hundreds of levels. The force rule alone is not visible here, because the
/// nested level's unheld poll stops a force as well, one level later; its
/// own guards are the runtime's unit tests. Each kind runs 20 times per
/// backend, so a miss one round in ten shows on every run.
#[test]
fn a_handler_that_reenters_acquire_is_stopped_by_cancel_and_abort() {
    let program = r#"module main;
import std::core::{ChildFailed, Fiber, Option, Region, Scope, Shared, acquire, print};

type Cell = { mut s: Option<Scope> };

fn spin(k: Int) -> Int {
  let mut i = 0;
  let mut s = 0;
  while i < k { s = s + i; i = i + 1 };
  s
}

fn round(levels: Shared<Int>, sent: Shared<Int>, seen: Shared<Int>) -> () {
  let r = Region::open();
  let cell: Cell = { s: Option::None };
  let depth = Shared::of(0);
  let h = handler for Scope {
    defer: fn f => {
      Region::defer(r, f);
      Shared::set(depth, Shared::get(depth) + 1);
      Shared::set(levels, Shared::get(levels) + 1);
      if Shared::get(seen) == 0 && Shared::get(sent) == 1 { Shared::set(seen, Shared::get(levels)) } else {};
      if Shared::get(depth) < 2000 {
        match cell.s {
          Option::Some(sc) => with { scope: sc } { let _ = acquire(0, fn _x => ()); () },
          Option::None => (),
        }
      } else {}
    }
  };
  cell.s = Option::Some(h);
  with { scope: h } {
    let _ = acquire(0, fn _x => ());
  }
}

fn start(levels: Shared<Int>, sent: Shared<Int>, seen: Shared<Int>) -> () {
  let mut rounds = 0;
  while rounds < 20000 { round(levels, sent, seen); rounds = rounds + 1 }
}

fn run(name: String, force: Bool) -> Bool raises ChildFailed {
  let levels = Shared::of(0);
  let sent = Shared::of(0);
  let seen = Shared::of(0);
  let f = Fiber::spawn(fn () => start(levels, sent, seen));
  while Shared::get(levels) < 50 { let _ = spin(10); };
  if force { Fiber::abort(f) } else { Fiber::cancel(f) };
  Shared::set(sent, 1);
  Fiber::wait(f)! catch { _ => () };
  let past = Shared::get(levels) - Shared::get(seen);
  // Seen at 0: it stopped before it could read `sent`, which is sooner.
  let stopped = Fiber::canceled(f) && (Shared::get(seen) == 0 || past <= 2);
  if !stopped {
    print("${name} ran ${past} levels past the request (seen at ${Shared::get(seen)}), canceled ${Fiber::canceled(f)}")
  } else {};
  stopped
}

pub fn main() -> Int raises ChildFailed {
  let mut cancels = 0;
  let mut aborts = 0;
  let mut i = 0;
  while i < 20 {
    if run("cancel", false)! { cancels = cancels + 1 } else {};
    if run("abort", true)! { aborts = aborts + 1 } else {};
    i = i + 1
  };
  print("cancel stopped ${cancels} of 20");
  print("abort stopped ${aborts} of 20");
  0
}
"#;
    for (backend, ran) in on_both("acquire_reentered", program, Duration::from_secs(120)) {
        assert!(!ran.hung, "{backend}: the program hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: {}{}", ran.stdout, ran.stderr);
        assert_eq!(
            ran.stdout,
            "cancel stopped 20 of 20\nabort stopped 20 of 20\n",
            "{backend}: {}",
            ran.stderr
        );
    }
}

/// **`acquire` hands its value back and releases it once, on the ordinary
/// path and on a cancel that lands after the registration.**
///
/// The cancel waits for real state: the fiber says on a channel that it is
/// past its `acquire`, then parks on a receive nobody answers, which is
/// where the cancel finds it.
#[test]
fn acquire_returns_the_value_and_releases_it_once() {
    let program = "module main;
import std::core::{Channel, Fiber, Scope, Shared, acquire, print, scoped};

fn take(released: Shared<Int>) -> Int with { scope: Scope } {
  acquire(42, fn v => Shared::set(released, Shared::get(released) + v))
}

fn held(released: Shared<Int>, past: Channel<Int>, never: Channel<Int>) -> () with { scope: Scope } {
  let v = acquire(1, fn x => Shared::set(released, Shared::get(released) + x));
  let _ = past.send(v);
  let _ = never.receive();
  print(\"TAIL the parked fiber ran on\");
}

pub fn main() -> Int {
  let released = Shared::of(0);
  let got = scoped(fn () => take(released));
  print(\"value ${got} released ${Shared::get(released)}\");

  let after = Shared::of(0);
  let mut round = 0;
  while round < 20 {
    let past: Channel<Int> = Channel::bounded(1);
    let never: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => scoped(fn () => held(after, past, never)));
    let _ = past.receive();
    Fiber::cancel(f);
    Fiber::wait(f);
    round = round + 1
  };
  print(\"canceled after registering: released ${Shared::get(after)}/20\");
  0
}
";
    for (backend, ran) in on_both("acquire_ordinary", program, Duration::from_secs(60)) {
        assert!(!ran.hung, "{backend}: the program hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: {}{}", ran.stdout, ran.stderr);
        assert_eq!(
            ran.stdout,
            "value 42 released 42\ncanceled after registering: released 20/20\n",
            "{backend}: {}",
            ran.stderr
        );
    }
}
