#![cfg(feature = "llvm")]

//! `timeout`, `race` and `Select`, on both fiber backends.
//!
//! **What these guard: a deadline, a race or a select that waits out the
//! work it was meant to stop, or that loses a value on the way.** Each
//! program measures its own waits with the runtime's monotonic clock and
//! prints facts -- an answer, a count, `prompt true` -- so a failure says which
//! claim broke. Nothing here sleeps and then assumes: a program waits for the
//! state it needs (a channel's value, a fiber's finish), and the only sleeps
//! are the ones a deadline is meant to cut short.
//!
//! The socket cases use a listener the test binds on port 0 and never writes
//! to: a read on it waits until something stops it, on every platform.

use std::net::TcpListener;
use std::time::Duration;

use crate::claims::on_both;

/// A watchdog well above anything these should take. A program that reaches
/// it is a deadline that did not land.
const PATIENCE: Duration = Duration::from_secs(60);

/// A listener that accepts every connection and never writes, so a read on
/// one waits until something stops it. Held open by the returned thread for
/// the life of the test process.
fn silent_server() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    port
}

const TIMEOUT_PROGRAM: &str = r#"module main;
import std::core::{Array, Channel, ChildFailed, Fiber, Nursery, Option, Region, Result, Shared, attempt, nursery, print, timeout};
import std::clock::{Clock};
import std::net::socket::{start, connect_to, receive, shut};

extern fn khora_live_count() -> Int;
extern fn khora_monotonic_millis() -> Int;

pub type Oops = | Bad;

fn forever() -> Int with { clock: Clock } {
  loop { clock.sleep(10); };
  0
}

fn counted(count: Shared<Int>) -> () with { clock: Clock } {
  let region = Region::open();
  Region::defer(region, fn () => { let _ = Shared::update(count, fn n => n + 1); });
  loop { clock.sleep(10); }
}

fn crew(count: Shared<Int>) -> () with { clock: Clock } raises ChildFailed {
  // The body returns at once, so the deadline finds this fiber in the
  // nursery's wait for its children, which is not a cancellation point.
  nursery(fn () => {
    nursery.spawn(fn () => with { clock: Clock::real() } { counted(count) });
    nursery.spawn(fn () => with { clock: Clock::real() } { counted(count) });
    nursery.spawn(fn () => with { clock: Clock::real() } { counted(count) });
  })!
}

fn read_silent(port: Int, count: Shared<Int>) -> Int {
  let c = connect_to("127.0.0.1", port);
  let region = Region::open();
  Region::defer(region, fn () => { shut(c); let _ = Shared::update(count, fn n => n + 1); });
  let buffer: Array<U8> = Array::new(16, U8::of(0));
  receive(c, buffer)
}

fn fails() -> Int raises Oops { raise Oops::Bad }

fn say(name: String, got: Option<Int>, began: Int) -> () {
  let took = khora_monotonic_millis() - began;
  let what = match got { Option::Some(v) => "Some(${v})", Option::None => "None" };
  print("${name} ${what} prompt ${took < 2000}");
}

fn run(port: Int) -> () with { clock: Clock } raises ChildFailed {
  let t = khora_monotonic_millis();
  say("in time", timeout(5000, fn () => 42), t);

  let t = khora_monotonic_millis();
  say("sleep", timeout(30, fn () => forever()), t);

  let empty: Channel<Int> = Channel::bounded(1);
  let t = khora_monotonic_millis();
  say("receive", timeout(30, fn () => match Channel::receive(empty) { Option::Some(v) => v, Option::None => -1 }), t);

  let reads = Shared::of(0);
  let t = khora_monotonic_millis();
  say("socket read", timeout(100, fn () => read_silent(port, reads)), t);
  print("socket read finalizers ${Shared::get(reads)}");

  let count = Shared::of(0);
  let t = khora_monotonic_millis();
  let r: Option<Int> = timeout(50, fn () => { crew(count)!; 0 })!;
  say("nursery", r, t);
  print("nursery finalizers ${Shared::get(count)}");

  match timeout(5000, fn () => timeout(30, fn () => forever())) {
    Option::Some(Option::None) => print("nested inner Some(None)"),
    Option::Some(Option::Some(_)) => print("nested inner Some(Some)"),
    Option::None => print("nested inner None"),
  };
  match timeout(30, fn () => timeout(5000, fn () => forever())) {
    Option::None => print("nested outer None"),
    Option::Some(Option::None) => print("nested outer Some(None)"),
    Option::Some(Option::Some(_)) => print("nested outer Some(Some)"),
  };

  let saw = Shared::of(0);
  let ready: Channel<Int> = Channel::bounded(1);
  let child = Fiber::spawn(fn () => with { clock: Clock::real() } {
    let r: Option<Int> = timeout(60000, fn () => { Channel::send(ready, 1); forever() });
    match r { Option::None => Shared::set(saw, 1), Option::Some(_) => Shared::set(saw, 2) };
  });
  let _ = Channel::receive(ready);
  Fiber::cancel(child);
  Fiber::wait(child);
  print("outside cancel canceled ${Fiber::canceled(child)} tail ${Shared::get(saw)}");

  match attempt(fn () => timeout(5000, fn () => fails()!)!) {
    Result::Ok(_) => print("failure answered"),
    Result::Err(_) => print("failure raised"),
  };
  say("zero", timeout(0, fn () => 1), khora_monotonic_millis());

  let before = khora_live_count();
  let mut i = 0;
  while i < 10000 {
    let _ = timeout(60000, fn () => i);
    i = i + 1;
  };
  let mut j = 0;
  while j < 100 {
    let _ = timeout(1, fn () => forever());
    j = j + 1;
  };
  print("leak ${khora_live_count() - before}");
}

pub fn main() -> Int raises ChildFailed {
  if start() {} else { print("no sockets") };
  with { clock: Clock::real() } { run(PORT)! };
  0
}
"#;

/// **`timeout` answers in time, and cuts a body short wherever it waits.**
///
/// In a sleep, a channel receive, a read from a server that never writes, and
/// a nursery whose three children each hold a finalizer: each answers `None`
/// within two seconds of a deadline in the tens of milliseconds, and every
/// finalizer has run by the time it does. Nested, the earlier deadline wins
/// and each `None` belongs to its own call; a cancel from outside is not a
/// `None` and the code after the `timeout` does not run; a failure before
/// the deadline is raised as itself; `timeout(0, ..)` is `None`.
///
/// **And it leaves nothing behind**: 10,000 that answer and 100 that fire
/// leave the live count where it was.
#[test]
fn timeout_answers_in_time_and_cuts_short_every_kind_of_wait() {
    let port = silent_server();
    let source = TIMEOUT_PROGRAM.replace("PORT", &port.to_string());
    for (backend, ran) in on_both("combinators_timeout", &source, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "in time Some(42) prompt true\n\
             sleep None prompt true\n\
             receive None prompt true\n\
             socket read None prompt true\n\
             socket read finalizers 1\n\
             nursery None prompt true\n\
             nursery finalizers 3\n\
             nested inner Some(None)\n\
             nested outer None\n\
             outside cancel canceled true tail 0\n\
             failure raised\n\
             zero None prompt true\n\
             leak 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
    }
}

const RACE_PROGRAM: &str = r#"module main;
import std::core::{Array, Channel, List, Option, Region, Result, Shared, SharedFn, attempt, print, race};
import std::clock::{Clock};
import std::net::socket::{start, connect_to, connect_by, receive, shut};

extern fn khora_live_count() -> Int;
extern fn khora_monotonic_millis() -> Int;

pub type Oops = | Bad;

/// Holds a finalizer that counts, says it is in, and then waits.
fn enter(count: Shared<Int>, ready: Channel<Int>) -> Region {
  let region = Region::open();
  Region::defer(region, fn () => { let _ = Shared::update(count, fn n => n + 1); });
  let _ = Channel::offer(ready, 1);
  region
}

fn sleeper(count: Shared<Int>, ready: Channel<Int>) -> Int {
  let _held = enter(count, ready);
  with { clock: Clock::real() } { clock.sleep(60000) };
  -1
}

fn receiver(count: Shared<Int>, ready: Channel<Int>) -> Int {
  let _held = enter(count, ready);
  let never: Channel<Int> = Channel::bounded(1);
  match Channel::receive(never) { Option::Some(v) => v, Option::None => -2 }
}

fn reader(port: Int, count: Shared<Int>, ready: Channel<Int>) -> Int {
  let c = connect_to("127.0.0.1", port);
  let _held = enter(count, ready);
  let buffer: Array<U8> = Array::new(16, U8::of(0));
  let got = receive(c, buffer);
  shut(c);
  got
}

fn connector(port: Int, count: Shared<Int>, ready: Channel<Int>) -> Int {
  let _held = enter(count, ready);
  connect_by("127.0.0.1", port, khora_monotonic_millis() + 60000)
}

/// Answers once every loser has said it is in, so the measurement is of a
/// loser stopped mid-wait.
fn winner(losers: Int, ready: Channel<Int>, answer: Int) -> Int {
  let mut n = 0;
  while n < losers {
    let _ = Channel::receive(ready);
    n = n + 1;
  };
  answer
}

fn fails() -> Int raises Oops { raise Oops::Bad }

fn report(name: String, got: Int, count: Shared<Int>, began: Int) -> () {
  let took = khora_monotonic_millis() - began;
  print("${name} winner ${got} finalizers ${Shared::get(count)} prompt ${took < 2000}");
  print("MEASURE ${name} ${took}");
}

pub fn main() -> Int {
  if start() {} else { print("no sockets") };

  let count = Shared::of(0);
  let ready: Channel<Int> = Channel::bounded(4);
  let t = khora_monotonic_millis();
  let got = race([
    SharedFn::of(fn _ => sleeper(count, ready)),
    SharedFn::of(fn _ => winner(2, ready, 7)),
    SharedFn::of(fn _ => sleeper(count, ready)),
  ]);
  report("sleep", got, count, t);

  let count = Shared::of(0);
  let ready: Channel<Int> = Channel::bounded(4);
  let t = khora_monotonic_millis();
  let got = race([SharedFn::of(fn _ => receiver(count, ready)), SharedFn::of(fn _ => winner(1, ready, 8))]);
  report("receive", got, count, t);

  let count = Shared::of(0);
  let ready: Channel<Int> = Channel::bounded(4);
  let t = khora_monotonic_millis();
  let got = race([SharedFn::of(fn _ => reader(SILENT, count, ready)), SharedFn::of(fn _ => winner(1, ready, 9))]);
  report("socket read", got, count, t);

  if CONNECT {
    let count = Shared::of(0);
    let ready: Channel<Int> = Channel::bounded(4);
    let t = khora_monotonic_millis();
    let got = race([SharedFn::of(fn _ => connector(QUIET, count, ready)), SharedFn::of(fn _ => winner(1, ready, 10))]);
    report("connect", got, count, t);
  } else {};

  let count = Shared::of(0);
  let ready: Channel<Int> = Channel::bounded(4);
  match attempt(fn () => race([
    SharedFn::of(fn _ => { let _ = Channel::receive(ready); fails()! }),
    SharedFn::of(fn _ => sleeper(count, ready)),
  ])!) {
    Result::Ok(_) => print("failing branch answered"),
    Result::Err(_) => print("failing branch fails the race, finalizers ${Shared::get(count)}"),
  };

  let before = khora_live_count();
  let mut i = 0;
  while i < 10000 {
    let _ = race([SharedFn::of(fn _ => i), SharedFn::of(fn _ => i + 1)]);
    i = i + 1;
  };
  print("leak ${khora_live_count() - before}");
  0
}
"#;

/// **`race` answers with the first branch, stops the others mid-wait, runs
/// their finalizers, and returns within milliseconds.**
///
/// The winner answers only once every loser has said it is inside its wait,
/// so each measurement is of a loser stopped in a sleep, a channel receive or
/// a read from a silent server.
///
/// A failing branch fails the race and the loser is still cleaned up. 10,000
/// races leave the live count where it was.
///
/// The return times are printed as `MEASURE` lines for the report. The
/// connect case is [`race_stops_a_loser_in_a_connect`], which needs a host
/// that never answers and so does not run on Windows.
#[test]
fn race_answers_first_and_stops_the_losers_promptly() {
    let silent = silent_server();
    let source = RACE_PROGRAM
        .replace("SILENT", &silent.to_string())
        .replace("QUIET", "0")
        .replace("CONNECT", "false");
    for (backend, ran) in on_both("combinators_race", &source, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        for line in ran.stdout.lines().filter(|l| l.starts_with("MEASURE")) {
            eprintln!("`{backend}` {line}");
        }
        let facts: String = ran
            .stdout
            .lines()
            .filter(|l| !l.starts_with("MEASURE"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            facts,
            "sleep winner 7 finalizers 2 prompt true\n\
             receive winner 8 finalizers 1 prompt true\n\
             socket read winner 9 finalizers 1 prompt true\n\
             failing branch fails the race, finalizers 1\n\
             leak 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
    }
}

/// **`race` stops a loser waiting on a connect to a host that never
/// answers**, the case of [`race_answers_first_and_stops_the_losers_promptly`]
/// that needs [`crate::connect_cancel::never_answering`] and so a Unix
/// listener whose backlog is full.
#[cfg(not(windows))]
#[test]
fn race_stops_a_loser_in_a_connect() {
    let silent = silent_server();
    let quiet = crate::connect_cancel::never_answering();
    let source = RACE_PROGRAM
        .replace("SILENT", &silent.to_string())
        .replace("QUIET", &quiet.port.to_string())
        .replace("CONNECT", "true");
    for (backend, ran) in on_both("combinators_race_connect", &source, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert!(
            ran.stdout
                .lines()
                .any(|l| l == "connect winner 10 finalizers 1 prompt true"),
            "`{backend}`: {}{}",
            ran.stdout,
            ran.stderr
        );
    }
}

/// **A race over a list built empty at run time stops the program, saying
/// why**, with the status every trap has. A literal `[]` is refused by the
/// checker (`khora-types/tests/stdlib.rs`).
#[test]
fn an_empty_race_traps_with_a_message() {
    const SOURCE: &str = r#"module main;
import std::core::{List, SharedFn, print, race};

fn none(n: Int) -> List<SharedFn<(), Int, {}>> {
  if n > 0 { [SharedFn::of(fn _ => n)] } else { List::Nil }
}

pub fn main() -> () {
  print("started");
  let got = race(none(0));
  print("answered ${got}");
}
"#;
    for (backend, ran) in on_both("combinators_race_empty", SOURCE, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung");
        assert_eq!(ran.stdout, "started\n", "`{backend}`: {}", ran.stderr);
        assert!(
            ran.stderr
                .contains("`race` was given no branches, so there is nothing to wait for"),
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(134), "`{backend}`: {}", ran.stderr);
    }
}

const SELECT_PROGRAM: &str = r#"module main;
import std::core::{Channel, Fiber, Option, Select, Selected, Shared, print};
import std::clock::{Clock};

extern fn khora_live_count() -> Int;
extern fn khora_monotonic_millis() -> Int;

pub type Event = | A(Int) | B(Int);

fn show(s: Selected<Event>) -> String {
  match s {
    Selected::Got(Event::A(n)) => "A ${n}",
    Selected::Got(Event::B(n)) => "B ${n}",
    Selected::Closed => "closed",
    Selected::TimedOut => "timed out",
  }
}

fn both(a: Channel<Int>, b: Channel<Int>) -> Select<Event> {
  Select::new()
    |> Select::on(a, fn n => Event::A(n))
    |> Select::on(b, fn n => Event::B(n))
}

/// Sends `count` values numbered from `from` to `a` or `b` by parity.
fn sender(a: Channel<Int>, b: Channel<Int>, from: Int, count: Int) -> () {
  let mut i = 0;
  while i < count {
    let v = from + i;
    if v % 2 == 0 { Channel::send(a, v) } else { Channel::send(b, v) };
    i = i + 1;
  }
}

/// Selects until both channels are closed and drained, adding up what it got.
fn selector(a: Channel<Int>, b: Channel<Int>, got: Shared<Int>, sum: Shared<Int>) -> () {
  loop {
    match Select::wait(both(a, b)) {
      Selected::Got(Event::A(n)) => { let _ = Shared::update(got, fn x => x + 1); let _ = Shared::update(sum, fn x => x + n); },
      Selected::Got(Event::B(n)) => { let _ = Shared::update(got, fn x => x + 1); let _ = Shared::update(sum, fn x => x + n); },
      Selected::Closed => break,
      Selected::TimedOut => (),
    }
  }
}

/// A plain receiver on one channel, until it is closed and drained.
fn plain(c: Channel<Int>, got: Shared<Int>, sum: Shared<Int>) -> () {
  loop {
    match Channel::receive(c) {
      Option::Some(n) => { let _ = Shared::update(got, fn x => x + 1); let _ = Shared::update(sum, fn x => x + n); },
      Option::None => break,
    }
  }
}

fn run() -> () with { clock: Clock } {
  let a: Channel<Int> = Channel::bounded(4);
  let b: Channel<Int> = Channel::bounded(4);
  Channel::send(b, 5);
  print("ready b: ${show(Select::wait(both(a, b)))}");
  Channel::send(a, 6);
  print("ready a: ${show(Select::wait(both(a, b)))}");

  // Fairness: both always ready.
  let mut from_a = 0;
  let mut i = 0;
  while i < 1000 {
    Channel::send(a, i);
    Channel::send(b, i);
    match Select::wait(both(a, b)) {
      Selected::Got(Event::A(_)) => { from_a = from_a + 1; let _ = Channel::poll(b); },
      _ => { let _ = Channel::poll(a); },
    };
    i = i + 1;
  };
  print("fair ${from_a >= 350 && from_a <= 650}");
  print("MEASURE fair ${from_a} of 1000 from a");
  print("both drained ${Channel::depth(a) + Channel::depth(b)}");

  // A deadline.
  let t = khora_monotonic_millis();
  let r = Select::wait(Select::within(both(a, b), 50));
  let took = khora_monotonic_millis() - t;
  print("deadline: ${show(r)} waited ${took >= 50} prompt ${took < 2000}");

  // A send from another fiber wakes a waiting select.
  let sender_fiber = Fiber::spawn(fn () => with { clock: Clock::real() } {
    clock.sleep(20);
    Channel::send(b, 77);
  });
  print("woken: ${show(Select::wait(both(a, b)))}");
  Fiber::wait(sender_fiber);

  // One closed, then all.
  Channel::close(a);
  let late = Fiber::spawn(fn () => with { clock: Clock::real() } {
    clock.sleep(20);
    Channel::send(b, 88);
  });
  print("one closed: ${show(Select::wait(both(a, b)))}");
  Fiber::wait(late);
  Channel::close(b);
  print("all closed: ${show(Select::wait(both(a, b)))}");

  // Canceled while waiting, with no value lost: the value sent after the
  // cancel goes to the plain receiver waiting behind the selector.
  let c: Channel<Int> = Channel::bounded(1);
  let d: Channel<Int> = Channel::bounded(1);
  let waiting = Shared::of(false);
  let canceled_selector = Fiber::spawn(fn () => {
    Shared::set(waiting, true);
    let _ = Select::wait(both(c, d));
    Shared::set(waiting, false);
  });
  while !Shared::get(waiting) { clock.sleep(1) };
  clock.sleep(20);
  Fiber::cancel(canceled_selector);
  Fiber::wait(canceled_selector);
  Channel::send(c, 41);
  print("after cancel: canceled ${Fiber::canceled(canceled_selector)} tail ran ${!Shared::get(waiting)} value ${match Channel::poll(c) { Option::Some(v) => v, Option::None => -1 }}");

  // 4 senders, 4 selectors and 2 plain receivers on the same two channels:
  // every value arrives exactly once.
  let x: Channel<Int> = Channel::bounded(8);
  let y: Channel<Int> = Channel::bounded(8);
  let got = Shared::of(0);
  let sum = Shared::of(0);
  let s1 = Fiber::spawn(fn () => sender(x, y, 0, 1000));
  let s2 = Fiber::spawn(fn () => sender(x, y, 1000, 1000));
  let s3 = Fiber::spawn(fn () => sender(x, y, 2000, 1000));
  let s4 = Fiber::spawn(fn () => sender(x, y, 3000, 1000));
  let r1 = Fiber::spawn(fn () => selector(x, y, got, sum));
  let r2 = Fiber::spawn(fn () => selector(x, y, got, sum));
  let r3 = Fiber::spawn(fn () => selector(x, y, got, sum));
  let r4 = Fiber::spawn(fn () => selector(x, y, got, sum));
  let p1 = Fiber::spawn(fn () => plain(x, got, sum));
  let p2 = Fiber::spawn(fn () => plain(y, got, sum));
  Fiber::wait(s1);
  Fiber::wait(s2);
  Fiber::wait(s3);
  Fiber::wait(s4);
  Channel::close(x);
  Channel::close(y);
  Fiber::wait(r1);
  Fiber::wait(r2);
  Fiber::wait(r3);
  Fiber::wait(r4);
  Fiber::wait(p1);
  Fiber::wait(p2);
  // 0 + 1 + ... + 3999 = 7998000.
  print("many: got ${Shared::get(got)} sum ${Shared::get(sum)}");

  let e: Channel<Int> = Channel::bounded(1);
  let f: Channel<Int> = Channel::bounded(1);
  let before = khora_live_count();
  let mut k = 0;
  while k < 10000 {
    Channel::send(e, k);
    let _ = Select::wait(both(e, f));
    k = k + 1;
  };
  print("leak ${khora_live_count() - before}");
}

pub fn main() {
  with { clock: Clock::real() } { run() }
}
"#;

/// **A `timeout` inside cleanup bounds its own body, and the rest of the
/// cleanup still runs.** A finalizer, a `claim` abandon handler and a
/// `Shared::update` change function each wait under `timeout(50, ..)` on a
/// channel nobody sends to for two seconds; a fourth finalizer runs in a
/// fiber that has been canceled. Each `timeout` must answer `None` well
/// before the send, the code after it in the cleanup must run, and the
/// canceled fiber must still end canceled.
///
/// The failure this guards against: a deadline is a cancel, and cleanup is
/// shielded from cancels, so a `timeout` there did nothing and the wait ran
/// to the send, or forever without one. Inside a change function the
/// wait gave up and its body answered `Some` of the gave-up value.
#[test]
fn a_timeout_inside_cleanup_bounds_its_own_body() {
    const SOURCE: &str = r#"module main;
import std::core::{Channel, Claim, Fiber, Option, Region, Shared, print, timeout};
import std::clock::{Clock};

extern fn khora_monotonic_millis() -> Int;

fn show(got: Option<Option<Int>>) -> String {
  match got {
    Option::None => "None",
    Option::Some(Option::None) => "Some(None)",
    Option::Some(Option::Some(v)) => "Some(Some(${v}))",
  }
}

fn wait_on(ch: Channel<Int>) -> Option<Option<Int>> {
  timeout(50, fn () => Channel::receive(ch))
}

fn sleepy() -> Option<Int> with { clock: Clock } {
  timeout(50, fn () => { clock.sleep(2000); 7 })
}

fn later(ch: Channel<Int>) -> Fiber<(), {}> {
  Fiber::spawn(fn () => with { clock: Clock::real() } { clock.sleep(2000); Channel::offer(ch, 1); })
}

fn fast(ms: Int) -> String { if ms < 1000 { "fast" } else { "slow ${ms}" } }

fn run() -> () with { clock: Clock } {
  let ch: Channel<Int> = Channel::bounded(1);
  let h = later(ch);
  let seen = Shared::of("unset");
  let ms = Shared::of(0);
  {
    let region = Region::open();
    Region::defer(region, fn () => {
      let t0 = khora_monotonic_millis();
      let got = wait_on(ch);
      Shared::set(ms, khora_monotonic_millis() - t0);
      Shared::set(seen, show(got));
    });
  };
  print("finalizer ${Shared::get(seen)} ${fast(Shared::get(ms))}");

  let ch2: Channel<Int> = Channel::bounded(1);
  let h2 = later(ch2);
  let jobs: Channel<Int> = Channel::bounded(2);
  Channel::send(jobs, 5);
  {
    let c = Channel::claim(jobs, fn job => {
      let t0 = khora_monotonic_millis();
      let got = wait_on(ch2);
      Shared::set(ms, khora_monotonic_millis() - t0);
      Shared::set(seen, "job ${job} ${show(got)}");
    });
    match c { Option::Some(_) => (), Option::None => () };
  };
  print("abandon ${Shared::get(seen)} ${fast(Shared::get(ms))}");

  let cell = Shared::of(0);
  let t0 = khora_monotonic_millis();
  let _ = Shared::update(cell, fn n => {
    let got = match sleepy() { Option::None => "None", Option::Some(v) => "Some(${v})" };
    Shared::set(seen, got);
    n + 1
  });
  print("update ${Shared::get(seen)} ${fast(khora_monotonic_millis() - t0)} cell ${Shared::get(cell)}");

  let ch3: Channel<Int> = Channel::bounded(1);
  let h3 = later(ch3);
  let rest = Shared::of(0);
  let started = Shared::of(0);
  let f = Fiber::spawn(fn () => {
    let region = Region::open();
    Region::defer(region, fn () => {
      let t0 = khora_monotonic_millis();
      let got = wait_on(ch3);
      Shared::set(ms, khora_monotonic_millis() - t0);
      Shared::set(seen, show(got));
      Shared::set(rest, 1);
    });
    Shared::set(started, 1);
    while true { clock.sleep(5) };
  });
  while Shared::get(started) == 0 { clock.sleep(1) };
  Fiber::cancel(f);
  Fiber::wait(f) catch { _ => () };
  print("canceled ${Shared::get(seen)} ${fast(Shared::get(ms))} rest ${Shared::get(rest)} still canceled ${Fiber::canceled(f)}");

  Fiber::wait(h) catch { _ => () };
  Fiber::wait(h2) catch { _ => () };
  Fiber::wait(h3) catch { _ => () };
  clock.sleep(1);
  print("after");
}

pub fn main() {
  with { clock: Clock::real() } { run() };
}
"#;
    let want = "finalizer None fast\nabandon job 5 None fast\nupdate None fast cell 1\n\
                canceled None fast rest 1 still canceled true\nafter\n";
    for (backend, ran) in on_both("combinators_timeout_cleanup", SOURCE, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.stdout, want, "`{backend}`: {}", ran.stderr);
    }
}

/// **A cancel stops a select branch whose only cancellation points are the
/// calls it makes.** The branch is binary recursion with no loop and no `!`,
/// so the entry polls of its calls are its only points. The value is safe
/// once the branch has been entered, so the hold on entry polls has to end
/// there. If it lasted until the select finished, the cancel was suppressed
/// for the whole branch and the code after the select ran.
#[test]
fn a_cancel_stops_a_select_branch_made_only_of_calls() {
    const SOURCE: &str = r#"module main;
import std::core::{Channel, Fiber, Select, Shared, print};
import std::clock::{Clock};

fn count_down(n: Int, seen: Shared<Int>) -> Int {
  if n == 0 { Shared::set(seen, Shared::get(seen) + 1); 1 } else { count_down(n - 1, seen) + count_down(n - 1, seen) }
}

pub fn main() {
  with { clock: Clock::real() } {
    let a: Channel<Int> = Channel::bounded(1);
    let seen = Shared::of(0);
    let tail = Shared::of(0);
    Channel::send(a, 1);
    let f = Fiber::spawn(fn () => {
      let _ = Select::new() |> Select::on(a, fn _v => count_down(22, seen)) |> Select::wait();
      Shared::set(tail, 1);
    });
    while Shared::get(seen) < 1000 { clock.sleep(1) };
    Fiber::cancel(f);
    Fiber::wait(f);
    print("canceled ${Fiber::canceled(f)} cut short ${Shared::get(seen) < 4194304} tail ${Shared::get(tail)}");
  }
}
"#;
    for (backend, ran) in on_both("combinators_select_branch_calls", SOURCE, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(
            ran.stdout, "canceled true cut short true tail 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
    }
}

/// **A selector canceled as its value arrives loses nothing.**
///
/// Each round a fiber waits in `Select::wait` while another fiber sends it a
/// value and then cancels it, so the cancel lands anywhere from before the
/// value is found to after its branch has run. Whatever the selector did not
/// take is still in the channel: what it counted plus what is left equals
/// what was sent, every round. Without the hold on entry polls, a cancel
/// landing at the branch's entry unwound with the value taken and counted by
/// nobody.
#[test]
fn a_selector_canceled_as_its_value_arrives_loses_nothing() {
    const SOURCE: &str = r#"module main;
import std::core::{Channel, Fiber, Option, Select, Selected, Shared, print};

fn take(a: Channel<Int>, b: Channel<Int>, got: Shared<Int>, inside: Shared<Bool>) -> () {
  Shared::set(inside, true);
  let pick = Select::new()
    |> Select::on(a, fn n => n)
    |> Select::on(b, fn n => n);
  match Select::wait(pick) {
    Selected::Got(_) => { let _ = Shared::update(got, fn x => x + 1); },
    Selected::Closed => (),
    Selected::TimedOut => (),
  };
}

fn drain(c: Channel<Int>) -> Int {
  match Channel::poll(c) { Option::Some(_) => 1 + drain(c), Option::None => 0 }
}

pub fn main() {
  let mut lost = 0;
  let mut round = 0;
  while round < 2000 {
    let a: Channel<Int> = Channel::bounded(2);
    let b: Channel<Int> = Channel::bounded(2);
    let got = Shared::of(0);
    let inside = Shared::of(false);
    let selector = Fiber::spawn(fn () => take(a, b, got, inside));
    while !Shared::get(inside) {};
    let _ = Channel::offer(if round % 2 == 0 { a } else { b }, round);
    Fiber::cancel(selector);
    Fiber::wait(selector);
    let left = drain(a) + drain(b);
    if Shared::get(got) + left != 1 { lost = lost + 1 };
    round = round + 1;
  };
  print("rounds 2000 lost ${lost}");
}
"#;
    for (backend, ran) in on_both("combinators_select_cancel", SOURCE, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(
            ran.stdout, "rounds 2000 lost 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
    }
}

/// **`Select::wait` takes one value from whichever channel has one, fairly,
/// and never loses or duplicates one.**
///
/// Picks the ready channel; over 1,000 rounds with both always ready neither
/// is picked under 35%; a deadline answers `TimedOut` no earlier than it was
/// set for; a send from another fiber wakes it; one closed channel is waited
/// past and all closed is `Closed`; a selector canceled while it waits stops
/// without its tail and the next value stays in its channel. Under 4 senders,
/// 4 selectors and 2 plain receivers on the same two channels, all 4,000
/// values arrive exactly once (count and sum). 10,000 waits leave the live
/// count where it was.
#[test]
fn select_takes_one_value_fairly_and_loses_none() {
    for (backend, ran) in on_both("combinators_select", SELECT_PROGRAM, PATIENCE) {
        assert!(!ran.hung, "`{backend}` hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        for line in ran.stdout.lines().filter(|l| l.starts_with("MEASURE")) {
            eprintln!("`{backend}` {line}");
        }
        let facts: String = ran
            .stdout
            .lines()
            .filter(|l| !l.starts_with("MEASURE"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            facts,
            "ready b: B 5\n\
             ready a: A 6\n\
             fair true\n\
             both drained 0\n\
             deadline: timed out waited true prompt true\n\
             woken: B 77\n\
             one closed: B 88\n\
             all closed: closed\n\
             after cancel: canceled true tail ran false value 41\n\
             many: got 4000 sum 7998000\n\
             leak 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
    }
}
