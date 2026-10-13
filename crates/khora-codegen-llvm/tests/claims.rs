#![cfg(feature = "llvm")]

//! `Channel::claim`: a job taken off a queue that cannot be lost in-process.
//!
//! **What these guard: a value that left the queue and reached nobody.** A
//! plain `receive` followed by a call that can stop drops the job when a
//! cancel lands on that call: it is out of the channel and in no one's hands,
//! and the program exits 0. A claim registers its abandon handler before
//! anything after the take can stop, so an unsettled claim runs the handler on
//! every way out of the code holding it.
//!
//! - [`two_hundred_cut_rounds_lose_no_claimed_job`]: the measurement the
//!   cookbook makes, with the plain loop as a control that must lose jobs, and
//!   the hand-written region pattern the cookbook keeps beside `claim`.
//! - [`a_cancel_racing_the_take_loses_nothing_and_leaks_nothing`]: a cancel
//!   delivered right after the take, 1,000 times.
//! - [`every_way_out_of_an_unsettled_claim_runs_its_handler`]: return, raise,
//!   break, a discarded claim, one stored and dropped later, `done` and
//!   `give_back`.
//! - [`abort_runs_a_handler_that_only_records_or_requeues`], under
//!   `Fiber::abort`.
//! - [`give_back_to_a_full_queue_abandons_and_never_waits`], under a watchdog,
//!   because the blocking version of this hangs for good.
//! - [`sigterm_runs_the_handler_of_an_unsettled_claim`], root cancellation.
//!
//! Every program runs on both fiber backends, under a watchdog.

use crate::harness;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

pub(crate) const BACKENDS: [&str; 2] = ["threads", "scheduler"];

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

/// Compiles `source` into its own directory.
pub(crate) fn build(name: &str, source: &str) -> PathBuf {
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

/// How a run ended.
pub(crate) struct Ran {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    /// `None` when it was killed by a signal, the watchdog's included.
    pub(crate) code: Option<i32>,
    /// Whether the watchdog killed it.
    pub(crate) hung: bool,
}

/// Runs `exe` on `backend`, killing it after `patience`. `signal_when`, on
/// Unix, sends SIGTERM once the program has printed that line.
pub(crate) fn run(exe: &PathBuf, backend: &str, patience: Duration, signal_when: Option<&str>) -> Ran {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    let mut hung = false;
    let mut stdout = String::new();
    let mut out = child.stdout.take().expect("stdout");
    // Read stdout on a thread, so a program that fills the pipe cannot block
    // and so the signal can wait for a line.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match out.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(String::from_utf8_lossy(&buf[..n]).into_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut signaled = false;
    loop {
        while let Ok(piece) = rx.try_recv() {
            stdout.push_str(&piece);
        }
        if let Some(line) = signal_when {
            if !signaled && stdout.contains(line) {
                signaled = true;
                #[cfg(unix)]
                {
                    let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
                }
            }
        }
        if child.try_wait().expect("waiting").is_some() {
            break;
        }
        if started.elapsed() > patience {
            let _ = child.kill();
            hung = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = child.wait().expect("reaping");
    let _ = reader.join();
    while let Ok(piece) = rx.try_recv() {
        stdout.push_str(&piece);
    }
    let mut stderr = String::new();
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    Ran {
        stdout: stdout.replace("\r\n", "\n"),
        stderr: stderr.replace("\r\n", "\n"),
        code: status.code(),
        hung,
    }
}

/// Builds `source` once and runs it on both backends.
pub(crate) fn on_both(name: &str, source: &str, patience: Duration) -> Vec<(&'static str, Ran)> {
    let exe = build(name, source);
    BACKENDS.iter().map(|b| (*b, run(&exe, b, patience, None))).collect()
}

/// One `name taken T served S abandoned A lost L unbalanced U` line.
struct Tally {
    taken: i64,
    served: i64,
    abandoned: i64,
    lost: i64,
    unbalanced: i64,
}

fn tally(stdout: &str, name: &str) -> Tally {
    let line = stdout
        .lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no `{name}` line in:\n{stdout}"));
    let field = |key: &str| -> i64 {
        let words: Vec<&str> = line.split_whitespace().collect();
        let at = words.iter().position(|w| *w == key).unwrap_or_else(|| panic!("no `{key}` in `{line}`"));
        words[at + 1].parse().unwrap_or_else(|_| panic!("`{key}` is not a number in `{line}`"))
    };
    Tally {
        taken: field("taken"),
        served: field("served"),
        abandoned: field("abandoned"),
        lost: field("lost"),
        unbalanced: field("unbalanced"),
    }
}

/// The harness: three worker loops on one shape, 200 rounds each.
///
/// Each round: a fresh channel and worker; the worker says it is ready; four
/// jobs go in; a cancel arrives after a jittered spin, inside the work.
/// `taken` is counted on the producer side, as four minus what is left in the
/// channel, so it does not depend on the worker getting as far as counting.
///
/// **The counters are written with `Shared::get` and `Shared::set`, which
/// cannot stop.** A `Shared::update` hands a function to the runtime and is a
/// cancellation point, so a cancel landing on one right after `done` would
/// leave a settled job uncounted: the harness losing the job, not the claim.
const HARNESS: &str = r#"module main;

import std::core::{Channel, Claim, Fiber, Option, Region, Shared, print};

pub type Halt = | Now;

fn spin(n: Int) -> Int {
  let mut i = 0;
  let mut acc = 0;
  while i < n { acc = acc + i; i = i + 1; };
  acc
}

fn serve(job: Int) -> () raises Halt {
  let _ = spin(20000 + job);
}

fn bump(cell: Shared<Int>) -> () {
  Shared::set(cell, Shared::get(cell) + 1);
}

// The control: the loop the cookbook warns about.
fn plain(jobs: Channel<Int>, ready: Channel<Int>, served: Shared<Int>, abandoned: Shared<Int>) -> () raises Halt {
  let _ = Channel::send(ready, 1);
  loop {
    match Channel::receive(jobs) {
      Option::None => break,
      Option::Some(job) => {
        serve(job)!;
        bump(served);
      },
    }
  }
}

fn claimed(jobs: Channel<Int>, ready: Channel<Int>, served: Shared<Int>, abandoned: Shared<Int>) -> () raises Halt {
  let _ = Channel::send(ready, 1);
  loop {
    match Channel::claim(jobs, fn _ => bump(abandoned)) {
      Option::None => break,
      Option::Some(c) => {
        serve(c.value)!;
        c.done();
        bump(served);
      },
    }
  }
}

// The cookbook's hand-written pattern, for code that cannot use `claim`:
// nothing at all between the receive and the defer.
fn manual(jobs: Channel<Int>, ready: Channel<Int>, served: Shared<Int>, abandoned: Shared<Int>) -> () raises Halt {
  let _ = Channel::send(ready, 1);
  loop {
    match Channel::receive(jobs) {
      Option::None => break,
      Option::Some(job) => {
        let settled = Shared::of(false);
        let region = Region::open();
        Region::defer(region, fn () => {
          if !Shared::get(settled) { bump(abandoned); };
        });
        serve(job)!;
        Shared::set(settled, true);
        bump(served);
      },
    }
  }
}

fn drain(jobs: Channel<Int>) -> Int {
  let mut left = 0;
  loop {
    match Channel::poll(jobs) {
      Option::None => break,
      Option::Some(_) => { left = left + 1; },
    }
  }
  left
}

fn series(name: String, which: Int) -> () {
  let rounds = 200;
  let mut r = 0;
  let mut seed = 12345;
  let mut taken = 0;
  let mut served_all = 0;
  let mut abandoned_all = 0;
  let mut unbalanced = 0;
  while r < rounds {
    let jobs: Channel<Int> = Channel::bounded(4);
    let ready: Channel<Int> = Channel::bounded(1);
    let served = Shared::of(0);
    let abandoned = Shared::of(0);
    let hand = if which == 0 {
      Fiber::spawn(fn () => plain(jobs, ready, served, abandoned)!)
    } else if which == 1 {
      Fiber::spawn(fn () => claimed(jobs, ready, served, abandoned)!)
    } else {
      Fiber::spawn(fn () => manual(jobs, ready, served, abandoned)!)
    };
    let _ = Channel::receive(ready);
    let mut k = 0;
    while k < 4 { let _ = Channel::send(jobs, k); k = k + 1; }
    seed = (seed * 1103515245 + 12345) % 2147483648;
    let _ = spin(seed % 60000);
    Fiber::cancel(hand);
    Fiber::wait(hand)! catch { _ => () };
    let t = 4 - drain(jobs);
    let s = Shared::get(served);
    let a = Shared::get(abandoned);
    if t != s + a { unbalanced = unbalanced + 1; };
    taken = taken + t;
    served_all = served_all + s;
    abandoned_all = abandoned_all + a;
    r = r + 1;
  }
  let lost = taken - served_all - abandoned_all;
  print("${name} taken ${taken} served ${served_all} abandoned ${abandoned_all} lost ${lost} unbalanced ${unbalanced}");
}

pub fn main() -> () {
  series("plain", 0);
  series("claim", 1);
  series("manual", 2);
}
"#;

/// **Two hundred cancels landing at jittered moments lose no claimed job.**
///
/// The plain loop is the control and must lose jobs here, on both backends:
/// if it did not, the cancels were landing somewhere that proves nothing.
/// `claim` and the hand-written region pattern must account for every job
/// taken, in every round: `taken == served + abandoned`.
#[test]
fn two_hundred_cut_rounds_lose_no_claimed_job() {
    for (backend, ran) in on_both("claims_harness", HARNESS, Duration::from_secs(240)) {
        assert!(!ran.hung, "`{backend}` hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        let plain = tally(&ran.stdout, "plain");
        assert!(
            plain.lost > 0 && plain.abandoned == 0,
            "`{backend}`: the control lost nothing, so the cancels never landed in the \
             window this measures:\n{}",
            ran.stdout
        );
        for name in ["claim", "manual"] {
            let t = tally(&ran.stdout, name);
            assert_eq!(t.lost, 0, "`{backend}`: `{name}` lost jobs:\n{}", ran.stdout);
            assert_eq!(t.unbalanced, 0, "`{backend}`: `{name}` unbalanced rounds:\n{}", ran.stdout);
            assert_eq!(t.taken, t.served + t.abandoned, "`{backend}`: `{name}`:\n{}", ran.stdout);
            assert!(t.abandoned > 0, "`{backend}`: `{name}` was never cut mid-job:\n{}", ran.stdout);
        }
    }
}

/// **A cancel delivered right after the take, 1,000 times.** The worker parks
/// in the take on an empty channel; `main` sends one job and cancels at once.
/// The send wins, so the take comes back holding the job, and the worker's
/// next statement is a `!` call where the cancel lands.
///
/// The plain receive is the control and loses jobs. A claim loses none, and
/// `khora_live_count` is unchanged across the 1,000 rounds: a claim that was
/// abandoned leaves no region, closure or flag behind.
#[test]
fn a_cancel_racing_the_take_loses_nothing_and_leaks_nothing() {
    const SOURCE: &str = r#"module main;

import std::core::{Channel, Claim, Fiber, Option, Shared, print};

extern fn khora_live_count() -> Int;

pub type Halt = | Now;

fn nothing() -> () raises Halt { () }

fn worker_plain(jobs: Channel<Int>, parked: Channel<Int>, served: Shared<Int>, abandoned: Shared<Int>) -> () raises Halt {
  let _ = Channel::send(parked, 1);
  match Channel::receive(jobs) {
    Option::None => (),
    Option::Some(_v) => {
      nothing()!;
      Shared::set(served, 1);
    },
  }
}

fn worker_claim(jobs: Channel<Int>, parked: Channel<Int>, served: Shared<Int>, abandoned: Shared<Int>) -> () raises Halt {
  let _ = Channel::send(parked, 1);
  match Channel::claim(jobs, fn _ => Shared::set(abandoned, 1)) {
    Option::None => (),
    Option::Some(c) => {
      nothing()!;
      c.done();
      Shared::set(served, 1);
    },
  }
}

fn spin(n: Int) -> Int {
  let mut i = 0;
  let mut acc = 0;
  while i < n { acc = acc + i; i = i + 1; };
  acc
}

// 1000 lost, 1 abandoned, 100 served, 10000 never taken.
fn round(seed: Int, plain: Bool) -> Int {
  let jobs: Channel<Int> = Channel::bounded(1);
  let parked: Channel<Int> = Channel::bounded(1);
  let served = Shared::of(0);
  let abandoned = Shared::of(0);
  let f = if plain {
    Fiber::spawn(fn () => worker_plain(jobs, parked, served, abandoned)!)
  } else {
    Fiber::spawn(fn () => worker_claim(jobs, parked, served, abandoned)!)
  };
  let _ = Channel::receive(parked);
  let _ = spin(seed % 3000);
  let _ = Channel::send(jobs, 1);
  Fiber::cancel(f);
  Fiber::wait(f)! catch { _ => () };
  let left = match Channel::poll(jobs) { Option::None => 0, Option::Some(_) => 1 };
  let taken = 1 - left;
  let s = Shared::get(served);
  let a = Shared::get(abandoned);
  if taken != s + a { 1000 } else if a == 1 { 1 } else if s == 1 { 100 } else { 10000 }
}

fn series(name: String, plain: Bool) -> () {
  let before = khora_live_count();
  let mut r = 0;
  let mut lost = 0;
  let mut abandoned = 0;
  let mut served = 0;
  let mut seed = 99;
  while r < 1000 {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    let x = round(seed, plain);
    if x == 1000 { lost = lost + 1; } else if x == 1 { abandoned = abandoned + 1; } else if x == 100 { served = served + 1; };
    r = r + 1;
  }
  let after = khora_live_count();
  let delta = after - before;
  print("${name} lost ${lost} abandoned ${abandoned} served ${served} live ${delta}");
}

pub fn main() -> () {
  series("plain", true);
  series("claim", false);
}
"#;
    for (backend, ran) in on_both("claims_race", SOURCE, Duration::from_secs(240)) {
        assert!(!ran.hung, "`{backend}` hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        let field = |name: &str, key: &str| -> i64 {
            let line = ran.stdout.lines().find(|l| l.starts_with(name)).expect("a line");
            let words: Vec<&str> = line.split_whitespace().collect();
            let at = words.iter().position(|w| *w == key).expect("a key");
            words[at + 1].parse().expect("a number")
        };
        assert!(
            field("plain", "lost") > 0,
            "`{backend}`: the control lost nothing, so the race never happened:\n{}",
            ran.stdout
        );
        assert_eq!(field("claim", "lost"), 0, "`{backend}`:\n{}", ran.stdout);
        assert!(field("claim", "abandoned") > 0, "`{backend}`: never cut:\n{}", ran.stdout);
        assert_eq!(field("claim", "live"), 0, "`{backend}`: leaked:\n{}", ran.stdout);
    }
}

/// **Every way out of an unsettled claim runs its handler, once.** One job
/// each, deterministic: exactly one of served, abandoned and returned is 1.
///
/// The stored case also says *when*: a claim kept in a list is abandoned when
/// the list goes, not before, which is the delay the cookbook warns of.
#[test]
fn every_way_out_of_an_unsettled_claim_runs_its_handler() {
    const SOURCE: &str = r#"module main;

import std::core::{Channel, Claim, List, Option, Shared, print};

pub type Halt = | Now;

type Tally = { served: Shared<Int>, abandoned: Shared<Int> };

fn bump(s: Shared<Int>) -> () { Shared::set(s, Shared::get(s) + 1); }

fn fresh() -> Tally { { served: Shared::of(0), abandoned: Shared::of(0) } }

fn one_job() -> Channel<Int> {
  let jobs: Channel<Int> = Channel::bounded(2);
  let _ = Channel::send(jobs, 7);
  jobs
}

fn report(name: String, t: Tally, back: Channel<Int>) -> () {
  let s = Shared::get(t.served);
  let a = Shared::get(t.abandoned);
  let r = Channel::depth(back);
  print("${name}: served ${s} abandoned ${a} returned ${r}");
}

fn fails() -> () raises Halt { raise Halt::Now }

fn case_done(jobs: Channel<Int>, t: Tally) -> () {
  match Channel::claim(jobs, fn _ => bump(t.abandoned)) {
    Option::None => (),
    Option::Some(c) => { c.done(); bump(t.served); },
  }
}

fn case_raise(jobs: Channel<Int>, t: Tally) -> () raises Halt {
  match Channel::claim(jobs, fn _ => bump(t.abandoned)) {
    Option::None => (),
    Option::Some(c) => { fails()!; c.done(); bump(t.served); },
  }
}

fn case_return(jobs: Channel<Int>, t: Tally) -> Int {
  match Channel::claim(jobs, fn _ => bump(t.abandoned)) {
    Option::None => 0,
    Option::Some(c) => { if c.value == 7 { return 1; }; c.done(); 2 },
  }
}

fn case_break(jobs: Channel<Int>, t: Tally) -> () {
  loop {
    match Channel::claim(jobs, fn _ => bump(t.abandoned)) {
      Option::None => break,
      Option::Some(_c) => break,
    }
  }
}

fn case_discard(jobs: Channel<Int>, t: Tally) -> () {
  let _ = Channel::claim(jobs, fn _ => bump(t.abandoned));
}

fn case_stored(jobs: Channel<Int>, t: Tally) -> () {
  let mut held: List<Claim<Int>> = List::Nil;
  match Channel::claim(jobs, fn _ => bump(t.abandoned)) {
    Option::None => (),
    Option::Some(c) => { held = List::Cons(c, held); },
  }
  print("stored: abandoned while still held ${Shared::get(t.abandoned)}");
}

fn case_give_back(jobs: Channel<Int>, back: Channel<Int>, t: Tally) -> Bool {
  match Channel::claim(jobs, fn _ => bump(t.abandoned)) {
    Option::None => false,
    Option::Some(c) => c.give_back(back),
  }
}

pub fn main() -> () {
  let none: Channel<Int> = Channel::bounded(1);
  { let t = fresh(); case_done(one_job(), t); report("done", t, none); };
  { let t = fresh(); case_raise(one_job(), t)! catch { _ => () }; report("raise", t, none); };
  { let t = fresh(); let _ = case_return(one_job(), t); report("return", t, none); };
  { let t = fresh(); case_break(one_job(), t); report("break", t, none); };
  { let t = fresh(); case_discard(one_job(), t); report("discard", t, none); };
  { let t = fresh(); case_stored(one_job(), t); report("stored", t, none); };
  {
    let t = fresh();
    let back: Channel<Int> = Channel::bounded(2);
    let landed = case_give_back(one_job(), back, t);
    print("give_back answered ${landed}");
    report("give_back", t, back);
  };
}
"#;
    for (backend, ran) in on_both("claims_paths", SOURCE, Duration::from_secs(60)) {
        assert!(!ran.hung, "`{backend}` hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "done: served 1 abandoned 0 returned 0\n\
             raise: served 0 abandoned 1 returned 0\n\
             return: served 0 abandoned 1 returned 0\n\
             break: served 0 abandoned 1 returned 0\n\
             discard: served 0 abandoned 1 returned 0\n\
             stored: abandoned while still held 0\n\
             stored: served 0 abandoned 1 returned 0\n\
             give_back answered true\n\
             give_back: served 0 abandoned 0 returned 1\n",
            "`{backend}`"
        );
    }
}

/// **`Fiber::abort` still runs a handler that only records or requeues.**
/// `abort` stops cleanup at its first cancellation point, so a handler that
/// calls a function that can stop may be cut short; one made of `offer` and
/// `Shared::set`, neither of which can stop, runs to its end.
///
/// The worker is parked mid-job when it is aborted. One handler records,
/// the other requeues: the job must be back in the queue afterwards.
#[test]
fn abort_runs_a_handler_that_only_records_or_requeues() {
    const SOURCE: &str = r#"module main;

import std::core::{Channel, Claim, Fiber, Option, Shared, print};

pub type Halt = | Now;

fn park(stop: Channel<Int>) -> () raises Halt {
  let _ = Channel::receive(stop);
}

fn records(jobs: Channel<Int>, parked: Channel<Int>, stop: Channel<Int>, seen: Shared<Int>) -> () raises Halt {
  match Channel::claim(jobs, fn job => Shared::set(seen, job)) {
    Option::None => (),
    Option::Some(c) => {
      let _ = Channel::send(parked, 1);
      park(stop)!;
      c.done();
    },
  }
}

fn requeues(jobs: Channel<Int>, parked: Channel<Int>, stop: Channel<Int>) -> () raises Halt {
  match Channel::claim(jobs, fn job => { let _ = Channel::offer(jobs, job); }) {
    Option::None => (),
    Option::Some(c) => {
      let _ = Channel::send(parked, 1);
      park(stop)!;
      c.done();
    },
  }
}

pub fn main() -> () {
  {
    let jobs: Channel<Int> = Channel::bounded(1);
    let parked: Channel<Int> = Channel::bounded(1);
    let stop: Channel<Int> = Channel::bounded(1);
    let seen = Shared::of(0);
    let _ = Channel::send(jobs, 41);
    let f = Fiber::spawn(fn () => records(jobs, parked, stop, seen)!);
    let _ = Channel::receive(parked);
    Fiber::abort(f);
    Fiber::wait(f)! catch { _ => () };
    print("recorded ${Shared::get(seen)}");
  };
  {
    let jobs: Channel<Int> = Channel::bounded(1);
    let parked: Channel<Int> = Channel::bounded(1);
    let stop: Channel<Int> = Channel::bounded(1);
    let _ = Channel::send(jobs, 42);
    let f = Fiber::spawn(fn () => requeues(jobs, parked, stop)!);
    let _ = Channel::receive(parked);
    Fiber::abort(f);
    Fiber::wait(f)! catch { _ => () };
    let back = match Channel::poll(jobs) { Option::None => 0, Option::Some(v) => v };
    print("requeued ${back}");
  };
}
"#;
    for (backend, ran) in on_both("claims_abort", SOURCE, Duration::from_secs(60)) {
        assert!(!ran.hung, "`{backend}` hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.stdout, "recorded 41\nrequeued 42\n", "`{backend}`: {}", ran.stderr);
    }
}

/// **`give_back` to a full queue falls through to the handler and never
/// waits.** A blocking requeue there hangs for good: the handler runs
/// shielded, and a shield blinds the blocking primitives. So both halves are
/// under the watchdog: `give_back` into a full queue answers false and the
/// handler runs, and a handler that `offer`s into a full queue returns.
#[test]
fn give_back_to_a_full_queue_abandons_and_never_waits() {
    const SOURCE: &str = r#"module main;

import std::core::{Channel, Claim, Option, Shared, print};

pub fn main() -> () {
  let jobs: Channel<Int> = Channel::bounded(1);
  let full: Channel<Int> = Channel::bounded(1);
  let _ = Channel::send(full, 0);
  let dead = Shared::of(0);
  let _ = Channel::send(jobs, 5);
  let answered = match Channel::claim(jobs, fn job => {
    if !Channel::offer(full, job) { Shared::set(dead, job); };
  }) {
    Option::None => true,
    Option::Some(c) => c.give_back(full),
  };
  print("give_back ${answered} dead-lettered ${Shared::get(dead)} full ${Channel::depth(full)}");
}
"#;
    for (backend, ran) in on_both("claims_give_back_full", SOURCE, Duration::from_secs(20)) {
        assert!(!ran.hung, "`{backend}`: a requeue to a full queue waited: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.stdout, "give_back false dead-lettered 5 full 1\n", "`{backend}`");
    }
}

/// **SIGTERM is a root cancellation, and an unsettled claim's handler runs.**
/// `main` holds a claim and is inside a `!` call when the signal arrives; the
/// program ends at 130, having run the handler on the way out.
#[cfg(unix)]
#[test]
fn sigterm_runs_the_handler_of_an_unsettled_claim() {
    const SOURCE: &str = r#"module main;

import std::core::{Channel, Claim, Option, print};

pub type Halt = | Now;

fn step(i: Int) -> Int raises Halt { (i * 31 + 7) % 1000003 }

fn work() -> Int raises Halt {
  let mut i = 0;
  let mut t = 0;
  while i < 300000000000 { t = step(t)!; i = i + 1; };
  t
}

pub fn main() -> Int raises Halt {
  let jobs: Channel<Int> = Channel::bounded(1);
  let _ = Channel::send(jobs, 9);
  match Channel::claim(jobs, fn job => print("abandoned ${job}")) {
    Option::None => 0,
    Option::Some(c) => {
      print("ready");
      let t = work()!;
      c.done();
      t
    },
  }
}
"#;
    let exe = build("claims_sigterm", SOURCE);
    for backend in BACKENDS {
        let ran = run(&exe, backend, Duration::from_secs(20), Some("ready\n"));
        assert!(!ran.hung, "`{backend}`: SIGTERM did not stop it: {}", ran.stdout);
        assert_eq!(ran.stdout, "ready\nabandoned 9\n", "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.code, Some(130), "`{backend}`: {}", ran.stderr);
    }
}
