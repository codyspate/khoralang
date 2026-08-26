#![cfg(feature = "llvm")]

//! Fibers, end to end.
//!
//! `docs/design/fibers.md` decides what one is: a stackful coroutine
//! multiplexed onto worker threads, implemented for now as an operating-system
//! thread. What these pin is the part a program can see, which is the part that
//! does not change when the implementation does — a handle you can join and
//! cancel, and a release that waits.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

const FIBERS: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

pub type Fiber;
impl Fiber {
  fn spawn<'e>(body: () -> () raises 'e) -> Fiber;
  fn join(self) -> ();
  fn cancel(self) -> ();
}

pub type Region;
impl Region {
  fn open() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}
";

/// A fiber runs the closure it was handed, and `join` waits for it.
#[test]
fn a_fiber_runs_and_is_joined() {
    let ran = run(
        "fiber_join",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let f = Fiber::spawn(fn () => print(1));
  Fiber::join(f);
  print(2);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n");
    assert_eq!(ran.code, Some(0));
}

/// The structured part, and it needed nothing of its own: releasing the last
/// reference to a handle joins, so a fiber cannot outlive the binding that
/// holds it. Nobody wrote `join` here.
#[test]
fn a_fiber_cannot_outlive_the_binding_that_holds_it() {
    let ran = run(
        "fiber_scoped",
        &format!(
            "{FIBERS}
fn work() -> Int {{
  let f = Fiber::spawn(fn () => print(1));
  2
}}

fn main() -> Int {{ print(work()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n", "the fiber finished before `work` returned");
    assert_eq!(ran.code, Some(0));
}

/// And on the way out of a raise, because the release is the same release.
#[test]
fn a_fiber_is_waited_for_when_a_raise_passes_through() {
    let ran = run(
        "fiber_raise",
        &format!(
            "{FIBERS}
pub type Oops = | Bad;

fn work() -> Int raises Oops {{
  let f = Fiber::spawn(fn () => print(1));
  raise Oops::Bad
}}

fn main() -> Int raises Oops {{ work()!; 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n");
    assert_eq!(ran.code, Some(1));
}

/// A closure captures, so a fiber can be given something to work on.
#[test]
fn a_fiber_closes_over_its_environment() {
    let ran = run(
        "fiber_capture",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let n = 21;
  let f = Fiber::spawn(fn () => print(n * 2));
  Fiber::join(f);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// Several at once, all waited for.
#[test]
fn several_fibers_are_all_waited_for() {
    let ran = run(
        "fiber_many",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let a = Fiber::spawn(fn () => print(1));
  Fiber::join(a);
  let b = Fiber::spawn(fn () => print(2));
  Fiber::join(b);
  let c = Fiber::spawn(fn () => print(3));
  Fiber::join(c);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// Cancellation is per fiber. The parent cancels the child and carries on —
/// which is the whole reason the flag stopped being one per process.
#[test]
fn cancelling_a_fiber_does_not_cancel_the_parent() {
    let ran = run(
        "fiber_cancel_child",
        &format!(
            "{FIBERS}
pub type Oops = | Bad;
fn ok(n: Int) -> Int raises Oops {{ n }}

fn main() -> Int raises Oops {{
  let f = Fiber::spawn(fn () => print(1));
  Fiber::cancel(f);
  Fiber::join(f);
  print(ok(2)!);
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n2\n",
        "the parent passed its own cancellation point untouched"
    );
    assert_eq!(ran.code, Some(0), "and exited normally rather than 130");
}

/// A fiber's own region ends with the fiber, so a child's finalizers run in
/// the child. Nothing about regions knew fibers were coming.
#[test]
fn a_fibers_region_ends_with_the_fiber() {
    let ran = run(
        "fiber_region",
        &format!(
            "{FIBERS}
fn child() -> () {{
  let region = Region::open();
  Region::defer(region, fn () => print(2));
  print(1);
}}

fn main() -> Int {{
  let f = Fiber::spawn(fn () => child());
  Fiber::join(f);
  print(3);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// Nothing leaks: not the handle, not the closure, not what it captured.
#[test]
fn a_fiber_leaves_nothing_behind() {
    let ran = run(
        "fiber_leaks",
        &format!(
            "{FIBERS}
fn work() -> () {{
  let text = \"held\";
  let f = Fiber::spawn(fn () => print(1));
  Fiber::join(f);
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

// --- a fiber root that absorbs a cancellation ------------------------------

const CANCELLABLE: &str = "module t;
fn print(value: Int);
extern fn khora_cancel();
extern fn khora_live_count() -> Int;

pub type Fiber;
impl Fiber {
  fn spawn<'e>(body: () -> () raises 'e) -> Fiber;
  fn join(self) -> ();
  fn cancel(self) -> ();
}

pub type Region;
impl Region {
  fn open() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}

pub type Oops = | Bad;
fn ok(n: Int) -> Int raises Oops { n }
";

/// Phase 5's exit criterion, for one fiber: cancelled, it runs every finalizer
/// in scope, and it stops *itself* rather than the program.
///
/// The fiber cancels itself so the test is deterministic — a parent that
/// cancels immediately after spawning wins the race and the child stops at its
/// first mark, which is correct but proves less.
#[test]
fn a_cancelled_fiber_runs_every_finalizer_and_stops_only_itself() {
    let ran = run(
        "fiber_cancel_finalizers",
        &format!(
            "{CANCELLABLE}
fn worker() -> () raises Oops {{
  let region = Region::open();
  Region::defer(region, fn () => print(99));
  print(1);
  khora_cancel();
  ok(1)!;
  print(2);
}}

fn run_it() -> () {{
  let f = Fiber::spawn(fn () => worker()!);
  Fiber::join(f);
}}

fn main() -> Int {{
  run_it();
  print(3);
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n99\n3\n0\n",
        "the child ran, was stopped, released its region, and the parent carried on"
    );
    assert_eq!(ran.code, Some(0), "and the program was not taken down with it");
}

/// Cancelling a child immediately after spawning it, which **is** a race, and
/// the test says so rather than pretending otherwise.
///
/// This asserted `"3\n"` or `"1\n3\n"` and was flaky on macOS, which produced
/// `"1\n2\n3\n"` — the child reaching the end before the parent's `cancel`
/// landed. That is a legal interleaving and the test was wrong, not the
/// runtime: a fiber is an operating-system thread today, so nothing orders
/// `Fiber::cancel` against the child arriving at its first cancellation point.
/// The old doc comment claimed it was "not a race to lose".
///
/// What is actually guaranteed, and what this now checks, is that every
/// outcome is one the program could have produced *without* a cancellation
/// somewhere in it — the child stops at a cancellation point or not at all,
/// never halfway through one. A torn state would be `"2\n3\n"`: `print(2)`
/// without `print(1)` before it.
///
/// **Phase 11 makes the strong version true.** With a real scheduler a child
/// spawned and cancelled before any suspension point has not been scheduled at
/// all, so `"3\n"` becomes the only answer. Worth tightening then, and worth
/// not asserting until it is.
#[test]
fn a_fiber_cancelled_before_it_starts_does_nothing() {
    let ran = run(
        "fiber_cancel_early",
        &format!(
            "{CANCELLABLE}
fn worker() -> () raises Oops {{
  print(1);
  ok(1)!;
  print(2);
}}

fn main() -> Int {{
  let f = Fiber::spawn(fn () => worker()!);
  Fiber::cancel(f);
  Fiber::join(f);
  print(3);
  0
}}
"
        ),
    );
    assert!(
        matches!(ran.stdout.as_str(), "3\n" | "1\n3\n" | "1\n2\n3\n"),
        "the child stopped at a cancellation point or finished, and nothing in \
         between: {:?}",
        ran.stdout
    );
    assert_eq!(ran.code, Some(0), "and the parent was not taken down with it");
}

/// An infallible fiber has no channel to be stopped on, and runs to its end.
/// That is the same rule as everywhere else — a cancellation point is a `!` in
/// something that can raise — seen from the fiber's side.
#[test]
fn an_infallible_fiber_runs_to_its_end() {
    let ran = run(
        "fiber_infallible",
        &format!(
            "{CANCELLABLE}
fn main() -> Int {{
  let f = Fiber::spawn(fn () => {{ khora_cancel(); print(1); }});
  Fiber::join(f);
  print(2);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n");
    assert_eq!(ran.code, Some(0));
}

// --- nurseries -------------------------------------------------------------

/// `std::core` spells these the same way; they are here so the file stays one
/// module. Note the idiom: `nursery.adopt(Fiber::spawn(fn () => ..))`, one call
/// inside another. A thunk cannot be *forwarded* to `Fiber::spawn`, because a
/// fiber's body has to be written where it starts for what it closes over to be
/// checked against the rule that a mutable value may not cross.
const NURSERY: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

pub type Fiber;
impl Fiber {
  fn spawn<'e>(body: () -> () raises 'e) -> Fiber;
  fn join(self) -> ();
  fn cancel(self) -> ();
}

pub type Fibers;
pub trait Share {}
/// A nursery is adopted into from more than one fiber; that is what it is for,
/// and `khora_fibers_adopt` locks. Without this the handler below is refused,
/// which is the point of the rule — a bodyless type has to say.
impl Share for Fibers {}
impl Fibers {
  fn open() -> Fibers;
  fn adopt(self, fiber: Fiber) -> ();
  fn wait(self) -> ();
}

pub effect Nursery { adopt: (Fiber) -> (), }

pub type Oops = | Bad;
fn ok(n: Int) -> Int raises Oops { n }

";

/// The ordinary path: the block waits for every child before it finishes.
#[test]
fn a_nursery_waits_for_its_children() {
    let ran = run(
        "nursery_wait",
        &format!(
            "{NURSERY}
fn first() -> () raises Oops {{ print(1); }}
fn second() -> () raises Oops {{ print(2); }}

fn work() -> () {{
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => first()!));
    nursery.adopt(Fiber::spawn(fn () => second()!));
  }}
  Fibers::wait(crew);
  print(3);
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    // The children run at the same time, so which prints first is not
    // something to assert — that they *both* finished before the block
    // did is the whole claim.
    assert!(
        ran.stdout == "1\n2\n3\n0\n" || ran.stdout == "2\n1\n3\n0\n",
        "both children ran before the block finished: {:?}",
        ran.stdout
    );
    assert_eq!(ran.code, Some(0));
}

/// The failure path, and the whole reason a nursery is a value with a release
/// rather than a pair of calls. A raise leaving the block cancels the children
/// and waits — the answers they were computing are no longer wanted — and the
/// child here would otherwise run forever.
///
/// Nobody wrote the cancel. It is what releasing the nursery does, on the one
/// path that skipped `wait`.
#[test]
fn a_raise_out_of_a_nursery_cancels_its_children() {
    let ran = run(
        "nursery_raise",
        &format!(
            "{NURSERY}
fn forever() -> () raises Oops {{
  print(1);
  loop {{ ok(1)!; }}
}}

fn boom() -> Int raises Oops {{ raise Oops::Bad }}

fn work() -> Int raises Oops {{
  let crew = Fibers::open();
  let value = with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => forever()!));
    boom()!
  }};
  Fibers::wait(crew);
  value
}}

fn main() -> Int {{
  let v = work()! catch {{ Oops::Bad => 7 }};
  print(v);
  print(khora_live_count());
  0
}}
"
        ),
    );
    // Whether the child reaches its first `print` before the parent raises is a
    // race, and not one worth removing: what is being pinned is that the child
    // was *stopped* — the program terminates at all, since `forever` loops until
    // cancelled — and that nothing leaked on the way.
    assert!(
        ran.stdout == "7\n0\n" || ran.stdout == "1\n7\n0\n",
        "stopped, with the fallback and nothing left over: {:?}",
        ran.stdout
    );
    assert_eq!(ran.code, Some(0));
}

/// Every child, not just the first one that finishes.
#[test]
fn a_nursery_waits_for_all_of_them() {
    let ran = run(
        "nursery_all",
        &format!(
            "{NURSERY}
fn one() -> () raises Oops {{ print(1); }}
fn two() -> () raises Oops {{ print(2); }}
fn three() -> () raises Oops {{ print(3); }}

fn work() -> () {{
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => one()!));
    nursery.adopt(Fiber::spawn(fn () => two()!));
    nursery.adopt(Fiber::spawn(fn () => three()!));
  }}
  Fibers::wait(crew);
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    // All three ran and all three were waited for. Which order they finished
    // in is theirs to decide — they are on three fibers, and the test that
    // asserted an order was asserting something the nursery never promised.
    for child in ["1", "2", "3"] {
        assert!(ran.stdout.contains(child), "child {child} did not run: {:?}", ran.stdout);
    }
    assert!(ran.stdout.ends_with("0\n"), "nothing left over: {:?}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// A wildcard `catch` must not swallow a cancellation.
///
/// A cancellation travels the same tagged channel an error does and is in
/// nobody's row, so `catch { _ => .. }` taking the switch's default would stop
/// one dead — and a nursery whose children cannot be cancelled is not
/// structured concurrency at all. It keeps the propagate path by an explicit
/// case on its `which`.
///
/// The fiber cancels itself, so the test is deterministic: the mark inside the
/// `catch` is the first one it reaches afterwards.
#[test]
fn a_wildcard_catch_does_not_swallow_a_cancellation() {
    let ran = run(
        "fiber_cancel_wildcard",
        &format!(
            "{CANCELLABLE}
fn worker() -> () raises Oops {{
  let region = Region::open();
  Region::defer(region, fn () => print(99));
  print(1);
  khora_cancel();
  // If the arm caught the cancellation, `ok(1)!` would come back as -1 and
  // `2` would print. It stops here instead, and the finalizer still runs.
  print(ok(1)! catch {{ _ => 0 - 1, }});
  print(2);
}}

fn run_it() -> () {{
  let f = Fiber::spawn(fn () => worker()!);
  Fiber::join(f);
}}

fn main() -> Int {{
  run_it();
  print(3);
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n99\n3\n0\n",
        "the cancellation went past the arm, the region was released, and the \
         parent carried on"
    );
    assert_eq!(ran.code, Some(0));
}

/// A program that spawns must be compiled with **atomic** reference counting,
/// and this is what says so.
///
/// A program that never mentions `Fiber::spawn` counts references with plain
/// arithmetic — `docs/design/reuse.md` §4 — which is sound only because the
/// compiler proved there is one thread. The failure mode of getting that wrong
/// is a data race in a refcount: memory corruption a long way from its cause,
/// and nothing a test would reliably catch. So the runtime is told what was
/// decided, and `khora_fiber_spawn` refuses to start a thread in a program that
/// claimed it would not.
///
/// This spawns, so it must not have claimed that. Two fibers and the parent
/// each build and release counted objects, and the live count returns to zero —
/// which it would not if the counts were racing, and which never prints at all
/// if the abort fires.
#[test]
fn a_program_that_spawns_counts_references_atomically() {
    let ran = run(
        "spawn_forces_atomics",
        &format!(
            "{FIBERS}
pub type Cell = | Nil | One(next: Cell);

/// Enough building and releasing to lose a count if two threads shared one
/// without atomics.
fn churn(depth: Int) -> Int {{
  if depth == 0 {{ 0 }} else {{ let c = Cell::One(Cell::Nil); depth + churn(depth - 1) }}
}}

/// The handles are released with this block, which is also how they join.
fn work() -> () {{
  let one = Fiber::spawn(fn () => {{ let _ = churn(2000); }});
  let two = Fiber::spawn(fn () => {{ let _ = churn(2000); }});
  let _ = churn(2000);
  Fiber::join(one);
  Fiber::join(two)
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "0
", "every counted object must be released");
    assert_eq!(ran.code, Some(0));
}

// --- what `attempt` may not turn into a value -------------------------------

/// `attempt`, and something for it to run.
const ATTEMPTING: &str = "
pub type Result<A, E> = | Ok(A) | Err(E);

/// The intrinsic, declared: catching *whatever* a body raises is not something
/// `catch` can express, so the compiler supplies the body.
fn attempt<A, E, 'e>(body: () -> A with 'e raises E) -> Result<A, E> with 'e;
";

/// **A cancellation is not a failure, and `attempt` may not say it is.**
///
/// `effect-runtime.md` §6 promises that nothing a program writes can swallow a
/// cancellation: a `catch` names error constructors and a cancellation is not
/// one. `lower_catch` keeps that promise by routing the reserved tags back to
/// the propagate path even under a `_` arm. `attempt` is the other total
/// handler and it did not — it branched on "the tag is not zero" and packed
/// whatever it found into `Err`.
///
/// Two things were wrong with that, and the second is the serious one. A
/// cancelled computation came back as an ordinary failure, so a retry policy
/// would retry a fiber that had been asked to stop. And a cancellation carries
/// no payload, so the `Err` held a null typed as the body's error — a
/// `problem.show()` away from reading through it.
///
/// Found on the way to 13.3, by a rollback that ran fallible work in a
/// finalizer and was told its own cancellation was a database error.
#[test]
fn attempt_does_not_turn_a_cancellation_into_an_error() {
    let ran = run(
        "fiber_attempt_cancelled",
        &format!(
            "{CANCELLABLE}{ATTEMPTING}
fn worker() -> () raises Oops {{
  let region = Region::open();
  Region::defer(region, fn () => print(99));
  print(1);
  khora_cancel();
  match attempt(fn () => ok(7)!) {{
    Result::Ok(n) => print(n),
    Result::Err(_) => print(-1),
  }};
  print(2);
}}

fn run_it() -> () {{
  let f = Fiber::spawn(fn () => worker()!);
  Fiber::join(f);
}}

fn main() -> Int {{
  run_it();
  print(3);
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n99\n3\n",
        "the cancellation must pass through `attempt` rather than becoming its `Err`"
    );
    assert_eq!(ran.code, Some(0), "and stop the fiber, not the program");
}

/// The ordinary reading is untouched: a body that actually fails is still a
/// value, which is the whole point of `attempt`.
#[test]
fn attempt_still_makes_a_real_failure_a_value() {
    let ran = run(
        "fiber_attempt_error",
        &format!(
            "{CANCELLABLE}{ATTEMPTING}
fn bad() -> Int raises Oops {{ raise Oops::Bad }}

fn main() -> Int {{
  match attempt(fn () => bad()!) {{
    Result::Ok(n) => print(n),
    Result::Err(_) => print(-1),
  }};
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1\n");
    assert_eq!(ran.code, Some(0));
}
