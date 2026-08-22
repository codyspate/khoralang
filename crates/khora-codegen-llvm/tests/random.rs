#![cfg(feature = "llvm")]

//! `std::random` and the millisecond half of `std::env::Clock`, against the
//! real `std`.
//!
//! The two landed together because they are the same shape: a thing only the
//! operating system knows, bound once in `khora-rt`, and a capability over it
//! so that a caller's signature says it reached for one.
//!
//! **Nothing here asserts that random numbers are unpredictable.** That is not
//! a property a test can check, and a test that tries is a test that fails on
//! the run where a fair coin comes up heads twenty times. What these check is
//! the shape: a pinned seed replays exactly, a different seed diverges, a range
//! contains its draws, a buffer comes back filled, and a monotonic clock does
//! not go backwards. The one property worth the whole capability — that a test
//! can pin the sequence — is the first one below.
//!
//! Compiled against `std` itself rather than a copy, because the point of most
//! of these is that the library composes.

mod harness;

use std::path::PathBuf;
use std::process::Command;

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

fn build(name: &str, main: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }
    exe
}

fn run(name: &str, main: &str) -> String {
    let exe = build(name, main);
    let output = Command::new(&exe).output().expect("the program should run");
    assert!(output.status.success(), "`{name}` exited with {:?}", output.status.code());
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

// --- the seed ---------------------------------------------------------------

/// **The whole reason randomness is a capability.** One seed, two runs of the
/// same code, the same three numbers — so a test that draws is a test that
/// either always passes or always fails, and never one in a hundred.
///
/// The third block changes only the seed. If it printed the same numbers the
/// seed would not be reaching the generator at all, which is the failure a
/// replay test cannot otherwise see.
#[test]
fn a_seeded_source_replays_and_a_different_seed_does_not() {
    let out = run(
        "random_seeded",
        "module main;
import std::core::{print};
import std::random::{Random};

fn three() -> () with { rng: Random } {
  print(Int::to_string(rng.int()));
  print(Int::to_string(rng.int()));
  print(Int::to_string(rng.int()))
}

export fn main() -> () {
  with { rng: Random::seeded(1234) } { three(); };
  with { rng: Random::seeded(1234) } { three(); };
  with { rng: Random::seeded(1235) } { three(); }
}
",
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 9, "three draws from each of three sources:\n{out}");
    assert_eq!(lines[0..3], lines[3..6], "the same seed replayed the same sequence");
    assert_ne!(lines[0..3], lines[6..9], "a different seed drew something else");
    // Three draws in a row that are equal would mean the state is not advancing
    // — a generator stuck on its seed passes the replay check perfectly.
    assert!(lines[0] != lines[1] || lines[1] != lines[2], "the state advanced:\n{out}");
}

/// The real source, and how a program splits one it can write down.
///
/// There is no free `os_seed` on purpose — entropy that arrived outside a
/// requirement row is entropy the manifest could not have denied — so a seed
/// comes out of a `Random` like everything else does. Draw one, log it, hand it
/// to `seeded`, and the run can happen again. That is the whole recipe for
/// reproducing a failure that only shows up in production.
#[test]
fn a_real_source_can_split_off_a_seed_that_replays() {
    let out = run(
        "random_real_split",
        "module main;
import std::core::{print};
import std::random::{Random};

fn two() -> () with { rng: Random } {
  print(Int::to_string(rng.int()));
  print(Int::to_string(rng.int()))
}

/// The seed a real source handed out, and two replays of it.
fn record() -> () with { rng: Random } {
  let seed = rng.int();
  with { rng: Random::seeded(seed) } { two(); };
  with { rng: Random::seeded(seed) } { two(); }
}

export fn main() -> () {
  with { rng: Random::real() } { record(); }
}
",
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0..2], lines[2..4], "the recorded seed replayed the run");
}

// --- the range --------------------------------------------------------------

/// A thousand draws, all of them inside `[10, 20)`, and both ends reached.
///
/// Seeded, so this is a deterministic assertion rather than a probabilistic
/// one: the counts below are what this generator does, not what it usually
/// does. Reaching both ends is worth checking because the two ways to get a
/// range wrong — an off-by-one at the top and a bias that never reaches the
/// bottom — each leave every draw inside the range.
#[test]
fn a_range_contains_its_draws() {
    let out = run(
        "random_range",
        "module main;
import std::core::{print};
import std::random::{Random};

fn survey(low: Int, high: Int, rounds: Int) -> () with { rng: Random } {
  let mut inside = 0;
  let mut least = high;
  let mut most = low;
  let mut round = 0;
  while round < rounds {
    let n = rng.in_range(low, high);
    inside = if n >= low { if n < high { inside + 1 } else { inside } } else { inside };
    least = if n < least { n } else { least };
    most = if n > most { n } else { most };
    round = round + 1;
  };
  print(Int::to_string(inside));
  print(Int::to_string(least));
  print(Int::to_string(most))
}

export fn main() -> () {
  with { rng: Random::seeded(7) } { survey(10, 20, 1000); };
  // Negative bounds, because `low + scale(..)` is where an unsigned reduction
  // would quietly go wrong.
  with { rng: Random::seeded(7) } { survey(-5, 5, 1000); };
  // A range of one has exactly one answer, and a thousand of them.
  with { rng: Random::seeded(7) } { survey(3, 4, 1000); }
}
",
    );
    assert_eq!(
        out, "1000\n10\n19\n1000\n-5\n4\n1000\n3\n3\n",
        "every draw inside, and both ends of each range reached"
    );
}

/// An empty range stops the program and says so, the way an index outside an
/// array does.
///
/// The alternative was to hand back `low`, and that is the version worth
/// refusing: `in_range(n, n)` is almost always an off-by-one somewhere above,
/// and a plausible-looking number is how it reaches production. This one runs
/// to a non-zero exit rather than through `run`, which asserts success.
#[test]
fn an_empty_range_stops_the_program() {
    let exe = build(
        "random_empty_range",
        "module main;
import std::core::{print};
import std::random::{Random};

export fn main() -> () {
  with { rng: Random::seeded(1) } { print(Int::to_string(rng.in_range(4, 4))); }
}
",
    );
    let output = Command::new(&exe).output().expect("the program should run");
    assert!(!output.status.success(), "an empty range should not produce a number");
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(complaint.contains("empty"), "it should say what was wrong, said: {complaint}");
}

// --- the bytes --------------------------------------------------------------

/// A buffer comes back filled, the same way twice from one seed and a
/// different way from another.
///
/// The zero count is the check that matters. `bytes` is one loop away from
/// filling nothing, and an all-zero buffer would satisfy "every element is a
/// byte" perfectly — so a run of thirty-two bytes with only a handful of zeros
/// in it is the evidence that the loop ran and the draws reached the array.
#[test]
fn bytes_fill_the_buffer_they_are_given() {
    let out = run(
        "random_bytes",
        "module main;
import std::core::{Array, print};
import std::random::{Random};

fn digest(buffer: Array<U8>) -> Int {
  let mut total = 0;
  let mut at = 0;
  while at < Array::length(buffer) {
    total = Int::wrapping_add(Int::wrapping_mul(total, 31), U8::to_int(Array::get(buffer, at)));
    at = at + 1;
  };
  total
}

fn zeros(buffer: Array<U8>) -> Int {
  let mut count = 0;
  let mut at = 0;
  while at < Array::length(buffer) {
    count = if U8::to_int(Array::get(buffer, at)) == 0 { count + 1 } else { count };
    at = at + 1;
  };
  count
}

fn take(size: Int) -> () with { rng: Random } {
  let buffer: Array<U8> = Array::new(size, 0);
  rng.bytes(buffer);
  print(Int::to_string(Array::length(buffer)));
  print(Int::to_string(digest(buffer)));
  print(Int::to_string(zeros(buffer)))
}

export fn main() -> () {
  with { rng: Random::seeded(99) } { take(32); };
  with { rng: Random::seeded(99) } { take(32); };
  with { rng: Random::seeded(100) } { take(32); };
  // An empty buffer is not a special case, and asking for one must not draw or
  // trap.
  with { rng: Random::seeded(99) } { take(0); }
}
",
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 12, "three lines per buffer:\n{out}");
    assert_eq!(lines[0..3], lines[3..6], "the same seed filled the buffer the same way");
    assert_ne!(lines[1], lines[7], "a different seed filled it differently");
    assert_eq!(lines[0], "32", "the whole buffer, and no more of it");
    let zeros: i32 = lines[2].parse().expect("a count");
    assert!(zeros < 8, "thirty-two drawn bytes should not be mostly zero, got {zeros}");
    assert_eq!(lines[9..12], ["0", "0", "0"], "an empty buffer fills without complaint");
}

// --- the clock --------------------------------------------------------------

/// Monotonic means monotonic: the second reading is never below the first, and
/// the elapsed time it reports is a real one rather than a constant.
///
/// The work in between is a loop rather than a sleep because there is nothing
/// to sleep with yet, and because a busy loop of a million wrapping multiplies
/// takes long enough to show up in milliseconds on any machine that can run
/// this test at all. If it ever does not, the assertion is still the honest one
/// — `>=`, not `>`.
#[test]
fn the_monotonic_clock_does_not_go_backwards() {
    let out = run(
        "clock_monotonic",
        "module main;
import std::core::{print};
import std::env::{Clock};

fn burn(rounds: Int) -> Int {
  let mut mixed = 1;
  let mut at = 0;
  while at < rounds {
    mixed = Int::wrapping_add(Int::wrapping_mul(mixed, 6364136223846793005), 1);
    at = at + 1;
  };
  mixed
}

fn measure() -> () with { clock: Clock } {
  let before = clock.monotonic_millis();
  burn(2000000);
  let after = clock.monotonic_millis();
  print(if after >= before { \"ordered\" } else { \"went backwards\" });
  print(if after - before < 60000 { \"plausible\" } else { \"absurd\" });
  // The wall clock and its seconds are two views of one reading, so they must
  // agree about which second it is.
  let seconds = clock.unix_seconds();
  let millis = clock.unix_millis();
  print(if millis / 1000 - seconds <= 1 { \"agreed\" } else { \"straddled\" });
  print(Int::to_string(seconds))
}

export fn main() -> () {
  with { clock: Clock::real() } { measure(); }
}
",
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert_eq!(lines[0], "ordered");
    assert_eq!(lines[1], "plausible");
    assert_eq!(lines[2], "agreed");

    // And the wall clock is the host's, not an arbitrary origin.
    let seconds: i64 = lines[3].parse().expect("a number");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs() as i64;
    assert!((seconds - now).abs() < 120, "the clock said {seconds}, the host says {now}");
}

/// A test's own clock, which is the other half of why this is a capability:
/// code that measures itself can be handed a timeline that never moves, or one
/// that moves exactly as far as the test says.
#[test]
fn a_clock_can_be_replaced_wholesale() {
    let out = run(
        "clock_pinned",
        "module main;
import std::core::{Shared, print};
import std::env::{Clock};

/// Ordinary code. It has no idea whether real time exists.
fn how_long() -> Int with { clock: Clock } {
  let before = clock.monotonic_millis();
  let after = clock.monotonic_millis();
  after - before
}

export fn main() -> () {
  // Every reading is five milliseconds after the last, forever.
  let ticks = Shared::of(0);
  with { clock: handler for Clock {
    unix_seconds: fn () => 1000,
    unix_millis: fn () => 1000000,
    monotonic_millis: fn () => Shared::update(ticks, fn t => t + 5),
  } } {
    print(Int::to_string(how_long()));
    print(Int::to_string(how_long()));
  }
}
",
    );
    assert_eq!(out, "5\n5\n", "the pinned clock advanced exactly as told");
}

// --- what is left behind -----------------------------------------------------

/// Nothing. A source holds a `Shared<Int>` and a handler holding closures that
/// captured it; a filled buffer is an array. All of it goes when the block that
/// made it does.
///
/// The count is read into a local *before* anything is printed, because
/// `Int::to_string` and the concatenation around it allocate — a live count
/// taken after building the string it goes into reads as a leak that is not
/// there.
#[test]
fn a_random_source_leaves_nothing_behind() {
    let out = run(
        "random_leaks",
        "module main;
import std::core::{Array, print};
import std::random::{Random};

extern fn khora_live_count() -> Int;

fn spin() -> Int with { rng: Random } {
  let buffer: Array<U8> = Array::new(64, 0);
  rng.bytes(buffer);
  let mut total = 0;
  let mut at = 0;
  while at < 100 {
    total = total + rng.in_range(0, 10);
    at = at + 1;
  };
  rng.int();
  total + U8::to_int(Array::get(buffer, 63)) - U8::to_int(Array::get(buffer, 63))
}

fn work() -> Int {
  let mut sum = 0;
  with { rng: Random::seeded(5) } { sum = sum + spin(); };
  with { rng: Random::real() } { spin(); };
  sum
}

export fn main() -> () {
  let total = work();
  let live = khora_live_count();
  print(if total >= 0 { if total < 900 { \"in range\" } else { \"too big\" } } else { \"negative\" });
  print(Int::to_string(live))
}
",
    );
    assert_eq!(out, "in range\n0\n", "the trailing 0 is the live-object count");
}
