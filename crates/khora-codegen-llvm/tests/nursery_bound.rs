#![cfg(feature = "llvm")]

//! `bounded_nursery(n)` runs exactly `n` children at once, and no limit below
//! 1 exists.
//!
//! **What these guard: a bound that admitted `n + 1`.** A child handed to the
//! nursery already running (`nursery.adopt(Fiber::spawn(..))`) was counted only
//! after it started, so while the spawner waited for room, `n` children and
//! the one it held were all running. `nursery.spawn(body)` admits first.
//!
//! **The peak is forced, not hoped for.** Each child parks on a gate that the
//! child bringing the count to `n` closes, so the first `n` are all inside at
//! once before any leaves: the peak reaches `n` on every run, and a child
//! started past the limit while they wait would make it `n + 1`. Children
//! after the first wave find the gate closed and only spin.

use std::time::Duration;

use crate::claims::{build, on_both, run, BACKENDS};

/// One child entering past the gate, and the producer that spawns `count`.
const PRELUDE: &str = r#"
fn spin(n: Int) -> Int {
  let mut i = 0;
  let mut acc = 0;
  while i < n { acc = acc + i; i = i + 1; };
  acc
}

fn child(limit: Int, live: Shared<Int>, peak: Shared<Int>, gate: Channel<Int>) -> () {
  let now = Shared::modify(live, fn n => { state: n + 1, result: n + 1 });
  let _ = Shared::update(peak, fn p => if now > p { now } else { p });
  if now >= limit { Channel::close(gate); };
  let _ = Channel::receive(gate);
  let _ = spin(200000);
  let _ = Shared::update(live, fn n => n - 1);
}
"#;

/// **Peak concurrency is exactly `n`, for n = 1, 2, 4 and 8.**
#[test]
fn a_bounded_nursery_runs_exactly_its_limit() {
    let source = format!(
        r#"module main;
import std::core::{{Channel, ChildFailed, Nursery, Shared, bounded_nursery, print}};
{PRELUDE}
fn launch(limit: Int, live: Shared<Int>, peak: Shared<Int>, gate: Channel<Int>) -> () with {{ nursery: Nursery }} {{
  let mut i = 0;
  while i < 4 * limit + 8 {{
    nursery.spawn(fn () => child(limit, live, peak, gate));
    i = i + 1;
  }}
}}

fn peak_at(limit: Int) -> Int raises ChildFailed {{
  let live = Shared::of(0);
  let peak = Shared::of(0);
  let gate: Channel<Int> = Channel::bounded(1);
  bounded_nursery(limit, fn () => launch(limit, live, peak, gate))!;
  Shared::get(peak)
}}

pub fn main() -> () raises ChildFailed {{
  print("n1 ${{peak_at(1)!}} n2 ${{peak_at(2)!}} n4 ${{peak_at(4)!}} n8 ${{peak_at(8)!}}");
}}
"#
    );
    for (backend, ran) in on_both("nursery_exact", &source, Duration::from_secs(120)) {
        assert!(!ran.hung, "`{backend}` hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.stdout, "n1 1 n2 2 n4 4 n8 8\n", "`{backend}`: {}", ran.stderr);
    }
}

/// **Two fibers spawning into one bounded crew at once** each wait for room;
/// neither starts a child past the limit while the other also waits.
#[test]
fn two_spawners_share_one_bound() {
    let source = format!(
        r#"module main;
import std::core::{{Channel, Fiber, Fibers, Shared, print}};
{PRELUDE}
fn feed(crew: Fibers, live: Shared<Int>, peak: Shared<Int>, gate: Channel<Int>) -> () {{
  let mut i = 0;
  while i < 12 {{
    Fibers::spawn(crew, fn () => child(3, live, peak, gate));
    i = i + 1;
  }}
}}

pub fn main() -> () {{
  let live = Shared::of(0);
  let peak = Shared::of(0);
  let gate: Channel<Int> = Channel::bounded(1);
  let crew = Fibers::bounded(3);
  let other = Fiber::spawn(fn () => feed(crew, live, peak, gate));
  feed(crew, live, peak, gate);
  Fiber::wait(other)! catch {{ _ => () }};
  let _ = Fibers::wait(crew);
  print("peak ${{Shared::get(peak)}} live ${{Shared::get(live)}}");
}}
"#
    );
    for (backend, ran) in on_both("nursery_two_spawners", &source, Duration::from_secs(120)) {
        assert!(!ran.hung, "`{backend}` hung: {}", ran.stdout);
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.stdout, "peak 3 live 0\n", "`{backend}`: {}", ran.stderr);
    }
}

/// **A limit computed as 0 at run time traps, naming the value.** A literal
/// is refused by the checker (`khora-types/tests/stdlib.rs`); a computed one
/// reaches the runtime, which stops before the body runs.
#[test]
fn a_computed_limit_below_one_traps_with_the_message() {
    const SOURCE: &str = r#"module main;
import std::core::{ChildFailed, List, bounded_nursery, print};

fn limit(of: List<Int>) -> Int { List::length(of) }

pub fn main() -> () raises ChildFailed {
  let none: List<Int> = List::Nil;
  bounded_nursery(limit(none), fn () => print("the body ran"))!;
  print("returned");
}
"#;
    let exe = build("nursery_limit_zero", SOURCE);
    for backend in BACKENDS {
        let ran = run(&exe, backend, Duration::from_secs(20), None);
        assert!(!ran.hung, "`{backend}` hung");
        assert_eq!(ran.code, Some(134), "`{backend}`: {}", ran.stderr);
        assert_eq!(ran.stdout, "", "`{backend}`: the body or the rest ran");
        assert!(
            ran.stderr.contains(
                "`bounded_nursery` needs a limit of at least 1, and this one is 0. \
                 `nursery` is the one without a limit"
            ),
            "`{backend}`: {}",
            ran.stderr
        );
    }
}
