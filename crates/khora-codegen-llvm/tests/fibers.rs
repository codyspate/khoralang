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

pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> {
  fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;
  fn join(self) -> A raises 'r;
  fn wait(self) -> ();
  fn cancel(self) -> ();
  fn detach(self) -> ();
}

pub type Region;
impl Region {
  fn open() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}

// A boxed answer, so that a join can be counted rather than only read.
pub type Answer = { n: Int };
fn make(n: Int) -> Answer { { n: n } }
fn first(a: Answer) -> Int { a.n }
fn quiet() -> () { }
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

pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> {
  fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;
  fn join(self) -> A raises 'r;
  fn wait(self) -> ();
  fn cancel(self) -> ();
  fn detach(self) -> ();
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
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
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
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
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

pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> {
  fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;
  fn join(self) -> A raises 'r;
  fn wait(self) -> ();
  fn cancel(self) -> ();
  fn detach(self) -> ();
}

pub type Fibers;
pub trait Share {}
/// A nursery is adopted into from more than one fiber; that is what it is for,
/// and `khora_fibers_adopt` locks. Without this the handler below is refused,
/// which is the point of the rule — a bodyless type has to say.
impl Share for Fibers {}
impl Fibers {
  fn open() -> Fibers;
  fn adopt<'er>(self, fiber: Fiber<(), 'er>) -> ();
  fn wait(self) -> Int;
}

pub effect Nursery { adopt: (Fiber<(), 'er>) -> (), }

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
  let _stopped = Fibers::wait(crew);
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

/// Two `adopt`s where the first is inside a nested block, which was a double
/// free.
///
/// `nursery` here is an *invented* capability binding: the label is not in
/// scope, so lowering declares a local for it and files it as the lambda's
/// evidence. It declared one per mention, and `declare` files a name in the
/// innermost lexical scope -- so the first mention's local left scope with the
/// block, the second mention found nothing and invented another, and the frame
/// released one capability object twice.
///
/// The nested block never had to run. `if false {{ .. }}` crashed just as
/// reliably, because lowering visits both arms. Two mentions in the *same*
/// scope were always fine, which is why the shape that found it was the
/// ordinary one: spawn the consumers, then spawn the producers.
#[test]
fn a_capability_mentioned_in_a_nested_block_is_one_binding() {
    let ran = run(
        "nursery_nested_mention",
        &format!(
            "{NURSERY}
fn first() -> () raises Oops {{ print(1); }}
fn second() -> () raises Oops {{ print(2); }}

fn scope(body: () -> () with {{ nursery: Nursery }}) -> () {{
  let crew = Fibers::open();
  body() with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }};
  let _stopped = Fibers::wait(crew);
}}

fn work() -> () {{
  scope(fn () => {{
    {{ nursery.adopt(Fiber::spawn(fn () => first()!)); }};
    nursery.adopt(Fiber::spawn(fn () => second()!));
  }});
  print(3);
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert!(
        ran.stdout == "1\n2\n3\n0\n" || ran.stdout == "2\n1\n3\n0\n",
        "both children ran and nothing was left alive: {:?}",
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
  let _stopped = Fibers::wait(crew);
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

/// A child that fails is the nursery's failure, and it stops the siblings.
///
/// **The three things that were each wrong on their own.** A child that raised
/// was waited for, its outcome discarded, and a line printed on stderr; the
/// siblings ran to completion on an answer nobody would use; and the nursery
/// returned the body's value, so the program exited 0. A failure that leaves no
/// trace in the program is a failure the program cannot act on.
///
/// `forever` has a `!` in its loop, which is what a cancellation point is. It
/// runs until it is told to stop, so the test terminating at all is the
/// evidence that it was told.
///
/// **The count is one, and that is the second assertion in it.** Two children
/// finish here and both of them stop early — one by raising and two by being
/// cancelled because of it. A cancellation is not a failure: it is what a
/// nursery does to its children, and counting it would make every early exit
/// look like a fault.
#[test]
fn a_child_that_fails_stops_its_siblings_and_is_counted() {
    let ran = run(
        "nursery_child_failed",
        &format!(
            "{NURSERY}
fn forever() -> () raises Oops {{
  loop {{ ok(1)!; }}
}}

fn bad() -> () raises Oops {{ raise Oops::Bad }}

fn work() -> Int {{
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => bad()!));
    nursery.adopt(Fiber::spawn(fn () => forever()!));
    nursery.adopt(Fiber::spawn(fn () => forever()!));
  }}
  Fibers::wait(crew)
}}

fn main() -> Int {{
  print(work());
  print(khora_live_count());
  0
}}
"
        ),
    );
    // One failure, two cancellations, and nothing left over. That the program
    // ends at all is the other half: `forever` loops until it is cancelled, so
    // a nursery that let its siblings run would hang here rather than fail.
    assert_eq!(ran.stdout, "1\n0\n", "one failure and nothing leaked: {:?}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// Nothing went wrong, so the count is nothing.
///
/// The other side of the one above, and worth its own test because a count that
/// is never zero is a `ChildFailed` on every nursery in the program.
#[test]
fn a_nursery_whose_children_all_finished_counts_none() {
    let ran = run(
        "nursery_no_failures",
        &format!(
            "{NURSERY}
fn fine() -> () raises Oops {{ print(1); }}

fn work() -> Int {{
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => fine()!));
    nursery.adopt(Fiber::spawn(fn () => fine()!));
  }}
  Fibers::wait(crew)
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n1\n0\n0\n", "no failures, nothing leaked: {:?}", ran.stdout);
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
  let _stopped = Fibers::wait(crew);
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
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
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
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
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

/// **A fiber answers, and the answer comes back through `join`.**
///
/// The thing that was missing: `join` gave back `()`, so the only way a fiber
/// could say anything was a `Channel` or a `Shared`.
#[test]
fn a_fiber_answers_and_the_answer_crosses() {
    let ran = run(
        "fiber_answer",
        &format!(
            "{FIBERS}
fn main() -> Int {{
  let n = Fiber::spawn(fn () => 21 * 2);
  print(Fiber::join(n));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// **A boxed answer joined twice leaks nothing.**
///
/// Joining twice is joining once, and each join takes a reference of its own —
/// the half no output can show. The handle's release lets go of the one the
/// runtime kept, so the counts are the joiners plus one, and getting that
/// wrong is not a wrong answer but a service that grows all day.
#[test]
fn joining_twice_leaves_nothing_behind() {
    let ran = run(
        "fiber_answer_counts",
        &format!(
            "{FIBERS}
fn take() -> () {{
  let s = Fiber::spawn(fn () => make(1));
  print(first(Fiber::join(s)));
  print(first(Fiber::join(s)));
  print(first(Fiber::join(s)));
}}

fn main() -> Int {{
  take();
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n1\n1\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// **A child's failure becomes the joiner's, with the error's own type.**
///
/// The row is on the handle, so the `catch` arm names the case rather than
/// wildcarding it — which is the whole reason `Fiber` carries `'r`.
#[test]
fn a_joined_failure_keeps_its_type() {
    let ran = run(
        "fiber_answer_raises",
        &format!(
            "{FIBERS}
pub type Oops = | Bad(code: Int);

fn worker(n: Int) -> Int raises Oops {{
  if n > 0 {{ raise Oops::Bad(n) }} else {{ 0 }}
}}

fn twice() -> () {{
  let f = Fiber::spawn(fn () => worker(5)!);
  print(Fiber::join(f)! catch {{ Oops::Bad(code) => 0 - code, }});
  print(Fiber::join(f)! catch {{ Oops::Bad(code) => 0 - code, }});
}}

fn main() -> Int {{
  twice();
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-5\n-5\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// **An infallible fiber's join needs no `!`**, which is what the row on the
/// type buys and what an erased handle could not have given.
///
/// A *compile* claim rather than an output one: `main` has no `raises` clause
/// and no `catch`, so this program only exists if the join is infallible.
#[test]
fn joining_a_fiber_that_cannot_fail_needs_no_mark() {
    let ran = run(
        "fiber_answer_infallible",
        &format!(
            "{FIBERS}
fn tally(n: Int) -> Int {{ n + n }}

fn main() -> Int {{
  print(Fiber::join(Fiber::spawn(fn () => tally(3))));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "6\n");
    assert_eq!(ran.code, Some(0));
}

/// **`detach` lets go without waiting.**
///
/// The valve that keeps a hung finalizer from taking its parent with it, and
/// what a `timeout` over an uninterruptible body would be a lie without.
#[test]
fn a_detached_fiber_is_not_waited_for() {
    let ran = run(
        "fiber_detach",
        &format!(
            "{FIBERS}
fn go() -> () {{
  let f = Fiber::spawn(fn () => quiet());
  Fiber::detach(f);
  print(1);
}}

fn main() -> Int {{
  go();
  print(2);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n");
    assert_eq!(ran.code, Some(0));
}

/// **An adopted child keeps the channel it is cancelled on**, which is why
/// `Nursery::adopt` takes `Fiber<(), 'er>` and not `Fiber<(), {}>`.
///
/// The empty row was tried and it reads better: "settle your failure before
/// you hand this over" turns a line on stderr into a compile error. It is also
/// wrong. A cancellation travels out on the same tagged return an error does,
/// so a child whose row is empty *cannot be stopped* — and a nursery that
/// cannot cancel its children is not a nursery.
///
/// The child here loops forever with a cancellation point in it, the nursery's
/// block ends, and the program finishes. Under the empty-row version it hung.
#[test]
fn an_adopted_child_can_still_be_cancelled() {
    let ran = run(
        "adopted_cancellable",
        &format!(
            "{NURSERY}
fn forever() -> () raises Oops {{
  print(1);
  loop {{ ok(1)!; }}
}}

fn work() -> () {{
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => forever()!));
  }}
  // No `Fibers::wait`: leaving the block releases the nursery, which cancels
  // what is still running and then waits for it. If the child had no channel
  // to be stopped on, this would never return.
  print(2);
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );

    // **That it finished at all is the assertion.** Whether the child reached
    // its `print` before the block ended is a race and not the point; under
    // the empty-row version this hung, and a hang is what a nursery that
    // cannot stop its children looks like.
    //
    // Which is why the check is on the lines present and not on their order.
    // It used to be `ends_with("2\n0\n")` -- an assertion about exactly the
    // ordering the paragraph above calls a race -- and it failed about one run
    // in four with `2\n1\n0\n`: two threads writing to one stream, not a
    // nursery that let a child outlive it. A test that contradicts its own
    // comment is the comment being right.
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert!(lines.contains(&"2"), "the block finished: {}", ran.stdout);
    assert!(lines.contains(&"0"), "and nothing was left over: {}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// **A loop back-edge is a cancellation point**, in something that can raise.
///
/// It was only a safepoint, and the difference hung a nursery. `loop { sleep;
/// work }` is how every periodic job in every language is written, and that
/// fiber could not be stopped: the runtime woke it out of the sleep, and it
/// went round again without ever asking why it had woken. A nursery that had
/// to unwind past one waited for ever.
///
/// No sleep here, because none is needed and a test that waits is a test that
/// is sometimes wrong: an ordinary counting loop with no `!` in it has exactly
/// the same shape and exactly the same problem. It ran for ever.
///
/// The fiber cancels itself so the ordering is fixed rather than raced.
#[test]
fn a_loop_stops_at_its_back_edge_when_cancelled() {
    let ran = run(
        "fiber_cancel_loop",
        &format!(
            "{CANCELLABLE}
fn spinner() -> () raises Oops {{
  let region = Region::open();
  Region::defer(region, fn () => print(99));
  let mut n = 0;
  loop {{
    n = n + 1;
    print(n);
    if n == 2 {{ khora_cancel(); }}
  }}
}}

fn run_it() -> () {{
  let f = Fiber::spawn(fn () => spinner()!);
  Fiber::wait(f);
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
        ran.stdout, "1\n2\n99\n3\n0\n",
        "the loop must stop at the first back-edge after the cancellation, \
         run its finalizer, and leave the parent alone"
    );
    assert_eq!(ran.code, Some(0), "the parent is not cancelled with it");
}

/// The same for `while`, which is a different lowering and had the same gap.
#[test]
fn a_while_loop_stops_at_its_back_edge_when_cancelled() {
    let ran = run(
        "fiber_cancel_while",
        &format!(
            "{CANCELLABLE}
fn counter() -> () raises Oops {{
  let mut n = 0;
  let mut going = true;
  while going {{
    n = n + 1;
    print(n);
    if n == 2 {{ khora_cancel(); }}
    going = true;
  }}
}}

fn run_it() -> () {{
  let f = Fiber::spawn(fn () => counter()!);
  Fiber::wait(f);
}}

fn main() -> Int {{ run_it(); print(3); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// **And a function with no error row still runs to its end**, which is the
/// half that keeps the language rule the one it was.
///
/// Widening a back-edge into a cancellation point does not widen *which*
/// functions have one: an error row is still the only channel a cancellation
/// travels on. "A fiber with no error row has no channel to be interrupted on"
/// is `docs/design/fibers.md`'s sentence and it is still true — so this loop
/// counts all the way to five with a cancellation pending throughout.
#[test]
fn a_loop_in_an_infallible_function_is_not_a_cancellation_point() {
    let ran = run(
        "fiber_cancel_infallible",
        &format!(
            "{CANCELLABLE}
fn counting() -> () {{
  let mut n = 0;
  while n < 5 {{
    n = n + 1;
    print(n);
    if n == 2 {{ khora_cancel(); }}
  }}
}}

fn worker() -> () raises Oops {{
  counting();
  print(50);
}}

fn run_it() -> () {{
  let f = Fiber::spawn(fn () => worker()!);
  Fiber::wait(f);
}}

fn main() -> Int {{ run_it(); print(3); 0 }}
"
        ),
    );
    // Every iteration runs, and so does `print(50)`: calling an infallible
    // function is not a `!` and `worker`'s body has no loop of its own, so
    // there is no cancellation point anywhere between the flag being set and
    // `worker` returning. That is the rule working, not a gap in it -- the
    // fiber stops at its root, having done what it was written to do.
    assert_eq!(
        ran.stdout, "1\n2\n3\n4\n5\n50\n3\n",
        "an infallible loop has no channel to be interrupted on"
    );
    assert_eq!(ran.code, Some(0));
}
