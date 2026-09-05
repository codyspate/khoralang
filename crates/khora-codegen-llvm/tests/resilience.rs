#![cfg(feature = "llvm")]

//! `std::resilience` — the schedule arithmetic, and the drivers on a fake clock.
//!
//! **Every one of these runs in microseconds and none of them waits**, which
//! is the whole argument for `Clock.sleep` being an operation on a capability.
//! A retry test in a language with an ambient sleep either takes as long as the
//! policy says or needs a test runtime that knows how to lie about time; here
//! the fake clock records what it was asked to wait for and returns, so the
//! assertions are on the *sequence of delays* rather than on a stopwatch.
//!
//! That is also a stronger test than a timing one would be. "It took about
//! 700ms" cannot tell 100+200+400 from 300+400 — the recorded sequence can.

use crate::harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_sources(db: &KhoraDatabase) -> Vec<SourceFile> {
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
    out
}

/// A clock that records every wait and never performs one, and a `Random` that
/// always draws the bottom of its range so a jittered schedule is decidable.
const HEAD: &str = r#"module demo::main;
import std::core::{Eq, List, Option, Result, Shared, Show, print};
import std::clock::{Clock};
import std::random::{Random};
import std::resilience::{Schedule, Tried, repeat, retry, retry_counting, retry_while};

pub type Oops = | Bad(which: Int);

/// Where a fake clock writes down what it was asked to do.
///
/// `now` advances by exactly what was slept, so `monotonic_millis` stays
/// consistent with the waits — a schedule reading the elapsed time (`UpTo`)
/// has to see the time its own sleeps produced.
fn fake(now: Shared<Int>, waits: Shared<List<Int>>) -> Clock {
  handler for Clock {
    unix_seconds: fn () => 0,
    unix_millis: fn () => 0,
    monotonic_millis: fn () => Shared::get(now),
    sleep: fn millis => {
      Shared::update(waits, fn seen => List::Cons(millis, seen));
      Shared::update(now, fn t => t + millis);
      ()
    },
  }
}

/// Always the low end of the range, so jitter is a fixed percentage.
fn floor_draws() -> Random {
  handler for Random {
    int: fn () => 0,
    in_range: fn (low, _high) => low,
    bytes: fn _buffer => (),
  }
}

/// The waits in the order they happened.
fn shown(waits: List<Int>) -> String { reversed(waits, List::Nil).show() }

fn reversed(from: List<Int>, onto: List<Int>) -> List<Int> {
  match from {
    List::Nil => onto,
    List::Cons(head, tail) => reversed(tail, List::Cons(head, onto)),
  }
}
"#;

fn run(name: &str, body: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(&db, dir.join("main.kh"), format!("{HEAD}\n{body}\n")));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// **Doubling, capped, and the cap holds.**
#[test]
fn an_exponential_schedule_doubles_until_the_cap() {
    let out = run(
        "resilience_exponential",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let tries = Shared::of(0);
    let outcome = retry_while(
      Schedule::Intersect(Schedule::Exponential(100, 200, 500), Schedule::Times(8)),
      fn _e => true,
      fn () => raise Oops::Bad(Shared::update(tries, fn n => n + 1)),
    )! catch { Oops::Bad(n) => n };
    print(Int::to_string(outcome));
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    // Eight attempts, seven waits: 100, 200, 400, then the cap.
    assert_eq!(out, "8\n[100, 200, 400, 500, 500, 500, 500]\n");
}

/// **`Spaced` does not drift**, which only an absolute-instant schedule gets
/// right. Every attempt but the first is preceded by a wait that lands it on a
/// multiple of the interval, whatever the body cost — here the body costs 30ms
/// of clock, so the waits are 70 rather than 100.
#[test]
fn a_spaced_schedule_stays_on_its_grid_when_the_body_runs_long() {
    let out = run(
        "resilience_spaced",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let _ = retry_while(
      Schedule::Intersect(Schedule::Spaced(100), Schedule::Times(4)),
      fn _e => true,
      fn () => {
        // The body takes 30ms of clock, every time.
        Shared::update(now, fn t => t + 30);
        raise Oops::Bad(0)
      },
    )! catch { Oops::Bad(n) => n };
    print(shown(Shared::get(waits)));
    print(Int::to_string(Shared::get(now)));
  }
}"#,
    );

    // Attempts begin at 0, 100, 200, 300 — so the waits shrink as the body
    // eats into each interval, and the last attempt still starts on the grid.
    assert_eq!(out, "[70, 70, 70]\n330\n");
}

/// **`UpTo` is a wall-clock budget**, and it refuses an attempt that would
/// *begin* after the deadline rather than one that ends after it.
#[test]
fn a_budget_stops_before_an_attempt_that_would_start_too_late() {
    let out = run(
        "resilience_budget",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let tries = Shared::of(0);
    let outcome = retry_while(
      Schedule::UpTo(Schedule::Spaced(100), 250),
      fn _e => true,
      fn () => raise Oops::Bad(Shared::update(tries, fn n => n + 1)),
    )! catch { Oops::Bad(n) => n };
    // Attempts at 0, 100, 200. The fourth would begin at 300, past the budget.
    print(Int::to_string(outcome));
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    assert_eq!(out, "3\n[100, 100]\n");
}

/// **Jitter is a percentage of the delay, not of the instant.**
///
/// Scaling the instant would scale everything that had already happened, which
/// on the fourth attempt of a long-running retry is a wait computed from a
/// number that has nothing to do with the policy.
#[test]
fn jitter_scales_the_delay_and_leaves_the_past_alone() {
    let out = run(
        "resilience_jitter",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  // Draws the bottom of `[50, 100)`, so every delay is halved.
  with { clock: fake(now, waits), random: floor_draws() } {
    let _ = retry_while(
      Schedule::Intersect(Schedule::backoff(200, 100000), Schedule::Times(4)),
      fn _e => true,
      fn () => raise Oops::Bad(0),
    )! catch { Oops::Bad(n) => n };
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    // 200, 400, 800 halved.
    assert_eq!(out, "[100, 200, 400]\n");
}

/// **A retry that succeeds stops**, and the value comes back.
#[test]
fn retrying_stops_at_the_first_success() {
    let out = run(
        "resilience_success",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let tries = Shared::of(0);
    let answer = retry(Schedule::Exponential(100, 200, 5000), fn () => {
      let made = Shared::update(tries, fn n => n + 1);
      if made < 3 { raise Oops::Bad(made) } else { 42 }
    })! catch { Oops::Bad(_n) => 0 };
    print(Int::to_string(answer));
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    assert_eq!(out, "42\n[100, 200]\n");
}

/// **A permanent failure is not retried**, which is what `retry_while` is for.
///
/// Retrying a 404 is the failure this exists to prevent, and it costs the
/// caller one `fn`.
#[test]
fn a_failure_the_caller_calls_permanent_is_not_retried() {
    let out = run(
        "resilience_permanent",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let tries = Shared::of(0);
    let outcome = retry_while(
      Schedule::Times(10),
      // Only `Bad(1)` is worth trying again, and the body never answers it.
      fn why => match why { Oops::Bad(n) => n == 1 },
      fn () => {
        Shared::update(tries, fn n => n + 1);
        raise Oops::Bad(99)
      },
    )! catch { Oops::Bad(n) => n };
    print(Int::to_string(outcome));
    print(Int::to_string(Shared::get(tries)));
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    assert_eq!(out, "99\n1\n[]\n");
}

/// **`repeat` is the other half**: run on the schedule until something breaks,
/// and say how many runs there were.
#[test]
fn repeating_runs_on_the_schedule_until_it_fails() {
    let out = run(
        "resilience_repeat",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let runs = Shared::of(0);
    let done = repeat(Schedule::Spaced(30000), fn () => {
      let made = Shared::update(runs, fn n => n + 1);
      if made > 3 { raise Oops::Bad(made) } else { () }
    });
    print(Int::to_string(done));
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    // Three good runs, three waits, and the fourth run failed.
    assert_eq!(out, "3\n[30000, 30000, 30000]\n");
}

/// **A `Schedule` is a value**: comparable, printable, and decidable with no
/// clock anywhere near it. That is what the ADT buys over a closure, and it is
/// what makes a policy something a log line can carry.
#[test]
fn a_schedule_is_a_value_that_can_be_read_and_compared() {
    let out = run(
        "resilience_value",
        r#"fn main() -> () {
  let policy = Schedule::Intersect(Schedule::Exponential(100, 200, 5000), Schedule::Times(3));
  print(policy.show());
  print(policy.eq(Schedule::Intersect(
    Schedule::Exponential(100, 200, 5000),
    Schedule::Times(3),
  )).show());
  print(policy.eq(Schedule::Times(3)).show());
}"#,
    );

    assert_eq!(
        out,
        "Schedule::Intersect(Schedule::Exponential(100, 200, 5000), Schedule::Times(3))\n\
         true\nfalse\n"
    );
}


/// **The count `retry` cannot tell you.**
///
/// A caller who wanted to log "succeeded on attempt 3" kept a `Shared<Int>`
/// and took a lock on every attempt to find out — a mutex and a
/// fiber-crossing allocation to count to three. `repeat` has always answered a
/// count because it cannot fail and has nothing else to report; `retry`
/// answers the value and drops it.
///
/// `retry_counting` does not raise, which is the only way the count survives a
/// run that ended badly: "it failed after four goes" is exactly the line
/// somebody wants when it did. `retry` and `retry_while` are wrappers that do
/// the `match` and raise.
#[test]
fn a_retry_can_report_how_many_goes_it_took() {
    let out = run(
        "resilience_counting",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    // Succeeds on the third go, and says so.
    let tries = Shared::of(0);
    let third = retry_counting(
      Schedule::Intersect(Schedule::Spaced(10), Schedule::Times(8)),
      fn _e => true,
      fn () => {
        let n = Shared::update(tries, fn t => t + 1);
        if n < 3 { raise Oops::Bad(n) } else { n }
      },
    );
    print(Int::to_string(third.attempts));
    match third.outcome {
      Result::Ok(value) => print("ok " + Int::to_string(value)),
      Result::Err(Oops::Bad(n)) => print("bad " + Int::to_string(n)),
    };

    // First go, which is the count that must never be zero.
    // The predicate is annotated because an infallible body leaves the error
    // type with nothing to fix it.
    let once = retry_counting(Schedule::Times(8), fn (_e: Oops) => true, fn () => 7);
    print(Int::to_string(once.attempts));

    // And a run that never succeeds: the count survives, and the *last*
    // failure is the one reported.
    let failed = Shared::of(0);
    let never = retry_counting(
      Schedule::Intersect(Schedule::Spaced(10), Schedule::Times(4)),
      fn _e => true,
      fn () => raise Oops::Bad(Shared::update(failed, fn t => t + 1)),
    );
    print(Int::to_string(never.attempts));
    match never.outcome {
      Result::Ok(value) => print("ok " + Int::to_string(value)),
      Result::Err(Oops::Bad(n)) => print("bad " + Int::to_string(n)),
    };
  }
}"#,
    );

    assert_eq!(out, "3\nok 3\n1\n4\nbad 4\n");
}

/// **`retry` is a wrapper over `retry_counting` now**, so this pins that the
/// raising half still raises and still gives back the last failure.
#[test]
fn retry_still_raises_the_last_failure() {
    let out = run(
        "resilience_still_raises",
        r#"fn main() -> () {
  let now = Shared::of(0);
  let waits = Shared::of(List::Nil);
  with { clock: fake(now, waits), random: floor_draws() } {
    let tries = Shared::of(0);
    let outcome = retry(
      Schedule::Intersect(Schedule::Spaced(10), Schedule::Times(3)),
      fn () => raise Oops::Bad(Shared::update(tries, fn n => n + 1)),
    )! catch { Oops::Bad(n) => n };
    print(Int::to_string(outcome));
    print(shown(Shared::get(waits)));
  }
}"#,
    );

    assert_eq!(out, "3\n[10, 10]\n");
}
