#![cfg(feature = "llvm")]

//! `if`, `for` and `..` inside a list literal, compiled and run.
//!
//! **What these guard: a literal whose elements are right and whose order,
//! effects or cost are not.** The element forms desugar in HIR into pushes
//! onto an accumulator and one `List::reverse_onto`, so every value can be
//! right while an effect runs twice, a spread is evaluated out of position, a
//! raise mid-literal leaks the cells already built, or the element loop skips
//! the back-edge that makes a `for` a cancellation point. Each test below pins
//! one of those.
//!
//! Compiled against `std` itself, because `List::reverse_onto`, `Range` and
//! `Iterator` are what the desugaring actually calls.

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
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on `backend` under a watchdog: a regression in the
/// cancellation test is a hang, and a hang would take the suite with it.
fn run(exe: &PathBuf, backend: &str) -> String {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    while child.try_wait().expect("waiting").is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("`{backend}`: {} did not finish in 30 s", exe.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    assert_eq!(status.code(), Some(0), "`{backend}` exited badly: {stderr}");
    stdout.replace("\r\n", "\n")
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Every form, and each against the spelling it replaces.
#[test]
fn each_form_makes_the_elements_it_says() {
    let exe = build(
        "list_elements_values",
        r#"module main;
import std::core::{Iterator, Step, List, Range, print};

fn render(n: Int) -> String { "r${n}" }

pub fn main() -> Int {
  let debug = true;
  let quiet = false;
  let rows = [1, 2, 3];
  let footer = [98, 99];
  let pairs = [(1, "a"), (2, "b")];
  let none: List<Int> = [];
  print("a ${[0, if debug => 7, for r in rows => r * 10, ..footer]}");
  print("b ${[if quiet => 1 else 2, if quiet => 3]}");
  print("c ${[for r in rows => if r != 2 => r]}");
  print("d ${[if debug => for r in rows => r + 100]}");
  print("e ${[for r in rows => for s in Range::Of(0, r) => r * 10 + s]}");
  print("f ${[..rows, 5, ..footer]}");
  print("g ${[..rows.reverse(), ..[]]}");
  print("h ${[for r in rows => render(r)]}");
  print("l ${[for (n, s) in pairs => "${s}${n}"]}");
  print("m ${[if quiet => 1 else if debug => 2 else 3]}");
  print("n ${[if debug => if quiet => 1 else 2]}");
  print("o ${[for r in rows => r, for r in none => r]}");
  0
}
"#,
    );
    assert_eq!(
        run(&exe, "threads"),
        "a [0, 7, 10, 20, 30, 98, 99]\n\
         b [2]\n\
         c [1, 3]\n\
         d [101, 102, 103]\n\
         e [10, 20, 21, 30, 31, 32]\n\
         f [1, 2, 3, 5, 98, 99]\n\
         g [3, 2, 1]\n\
         h [r1, r2, r3]\n\
         l [a1, b2]\n\
         m [2]\n\
         n [2]\n\
         o [1, 2, 3]\n"
    );
}

/// **The additive guarantee, run.** A block-bodied `if` or `for` in a literal
/// is one element holding the value of an expression; the element forms did
/// not take it over.
#[test]
fn a_block_bodied_if_or_for_is_still_one_element() {
    let exe = build(
        "list_elements_additive",
        r#"module main;
import std::core::{Iterator, Step, List, print};

fn log() -> () { print("logged") }

pub fn main() -> Int {
  let quiet = false;
  let rows = [1, 2, 3];
  let a = [if quiet { log() }];
  let b = [for r in rows { () }];
  let c = [if quiet { 1 } else { 2 }, 3];
  print("${List::length(a)} ${List::length(b)} ${c}");
  0
}
"#,
    );
    assert_eq!(run(&exe, "threads"), "1 1 [2, 3]\n");
}

/// **Left to right, each once**, including the effects inside a `for`
/// element's body: condition, value, iterable, each item's value in turn,
/// then a spread at its own position -- a trailing one too, which the
/// lowering holds back to be the tail and must not evaluate early.
#[test]
fn elements_are_evaluated_left_to_right_once() {
    let exe = build(
        "list_elements_order",
        r#"module main;
import std::core::{Iterator, Step, List, print};

fn noisy(tag: String, v: Int) -> Int { print(tag); v }

pub fn main() -> Int {
  let k = [
    noisy("one", 1),
    if noisy("cond", 1) == 1 => noisy("two", 2),
    for r in [noisy("xs", 3), noisy("xs'", 4)] => noisy("item ${r}", r),
    ..[noisy("spread", 5)],
    noisy("last", 6),
    ..[noisy("tail", 7)],
  ];
  print("${k}");
  0
}
"#,
    );
    assert_eq!(
        run(&exe, "threads"),
        "one\ncond\ntwo\nxs\nxs'\nitem 3\nitem 4\nspread\nlast\ntail\n[1, 2, 3, 4, 5, 6, 7]\n"
    );
}

/// **`!` inside a `for` element leaves the function mid-literal**, after the
/// effects to its left and before the ones to its right, and the cells the
/// literal had built are freed: the live count comes back to where it was,
/// on both backends.
///
/// The same for a *trailing* spread whose operand raises or returns. That
/// operand is the one the lowering keeps apart, as the tail the built list is
/// turned onto, and passed as the turn's second argument it leaked the
/// accumulator -- 7 objects a call here -- because an argument that leaves
/// early does not free the arguments evaluated before it.
#[test]
fn a_raise_mid_literal_stops_in_order_and_frees_what_was_built() {
    let exe = build(
        "list_elements_raise",
        r#"module main;
import std::core::{Iterator, Step, List, Range, print};

extern fn khora_live_count() -> Int;

type Bad = | Bad(n: Int);

fn check(n: Int) -> Int raises Bad {
  print("check ${n}");
  if n == 3 { raise Bad::Bad(n) } else { n }
}

fn all(xs: List<Int>, tail: List<Int>) -> List<Int> raises Bad {
  [0, ..xs, for x in xs => check(x)!, if true => 9, ..tail]
}

fn quietly(n: Int) -> Int raises Bad { if n == 500 { raise Bad::Bad(n) } else { n } }

fn many(xs: List<Int>) -> List<Int> raises Bad {
  [0, ..xs, for x in xs => quietly(x)!, if true => 1]
}

fn faill(n: Int, at: Int) -> List<Int> raises Bad { if n == at { raise Bad::Bad(n) } else { [n, n] } }

// A trailing spread's operand is evaluated after everything to its left, so
// when it leaves early the literal has already built cells to free.
fn tail_raise(xs: List<Int>, at: Int) -> List<Int> raises Bad { [0, ..xs, 4, ..faill(4, at)!] }

fn tail_return(xs: List<Int>, stop: Bool) -> List<Int> {
  [0, ..xs, 4, ..(if stop { return [] } else { xs })]
}

pub fn main() -> Int {
  let ok = all([1, 2], [7])! catch { Bad::Bad(n) => [0 - n] };
  print("ok ${ok}");
  let bad = all([1, 2, 3, 4], [7])! catch { Bad::Bad(n) => [0 - n] };
  print("bad ${bad}");

  let xs = [for i in Range::Of(0, 1000) => i];
  let before = khora_live_count();
  let mut k = 0;
  while k < 20 {
    let r = many(xs)! catch { Bad::Bad(_) => [] };
    k = k + List::length(r) + 1;
  };
  let after = khora_live_count();
  print("delta ${after - before}");

  let ys = [1, 2, 3, 4, 5];
  let b1 = khora_live_count();
  let mut j = 0;
  while j < 10 {
    let r = tail_raise(ys, 4)! catch { Bad::Bad(_) => [] };
    j = j + List::length(r) + 1;
  };
  let a1 = khora_live_count();
  print("tail_raise delta ${a1 - b1}");
  let b2 = khora_live_count();
  j = 0;
  while j < 10 {
    let r = tail_return(ys, true);
    j = j + List::length(r) + 1;
  };
  let a2 = khora_live_count();
  print("tail_return delta ${a2 - b2}");
  print("${tail_raise(ys, 9)! catch { Bad::Bad(_) => [] }} ${tail_return(ys, false)}");
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, backend),
            "check 1\ncheck 2\nok [0, 1, 2, 1, 2, 9, 7]\n\
             check 1\ncheck 2\ncheck 3\nbad [-3]\n\
             delta 0\n\
             tail_raise delta 0\n\
             tail_return delta 0\n\
             [0, 1, 2, 3, 4, 5, 4, 4, 4] [0, 1, 2, 3, 4, 5, 4, 1, 2, 3, 4, 5]\n",
            "`{backend}`"
        );
    }
}

/// **A `for` element over a range that never ends stops when its fiber is
/// canceled**, on both backends. The element is the statement loop's
/// expansion, back-edge and all, so this is the claim that it kept the
/// cancellation point.
#[test]
fn a_for_element_is_a_cancellation_point() {
    let exe = build(
        "list_elements_cancel",
        r#"module main;
import std::core::{Iterator, Step, List, Range, Fiber, Channel, print};

fn huge(ready: Channel<Int>) -> Int {
  Channel::send(ready, 1);
  // Makes no elements, so a regression here is a hang the watchdog ends
  // rather than a list that grows until the machine runs out of memory.
  let xs = [for i in Range::Of(0, 1000000000000) => if i < 0 => i];
  print("the literal finished");
  List::length(xs)
}

pub fn main() -> Int {
  let ready: Channel<Int> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => huge(ready));
  let _ = Channel::receive(ready);
  Fiber::cancel(f);
  Fiber::wait(f);
  print("canceled: ${Fiber::canceled(f)}");
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "canceled: true\n", "`{backend}`");
    }
}

/// **A trailing spread is shared, not copied.** `[1, 2, ..xs]` costs the same
/// however long `xs` is; the same spread anywhere else walks `xs` and makes a
/// cell per element.
///
/// The two elements cost four allocations, not two: each is pushed onto the
/// accumulator and allocated again when `reverse_onto` turns it, which frees
/// the old cell rather than reusing it.
#[test]
fn a_trailing_spread_is_shared_and_a_middle_one_is_walked() {
    let exe = build(
        "list_elements_spread_cost",
        r#"module main;
import std::core::{Iterator, Step, List, Range, print};

extern fn khora_alloc_count() -> Int;

pub fn main() -> Int {
  let xs = [for i in Range::Of(0, 1000) => i];
  let a0 = khora_alloc_count();
  let front = [1, 2, ..xs];
  let a1 = khora_alloc_count();
  let middle = [1, ..xs, 2];
  let a2 = khora_alloc_count();
  print("${a1 - a0} ${List::length(front)} ${List::length(middle)}");
  print("${a2 - a1 >= 1000}");
  0
}
"#,
    );
    assert_eq!(run(&exe, "threads"), "4 1002 1002\ntrue\n");
}

/// **The literal costs what the code it replaces costs.** `[for i in r => i]`
/// against the documented idiom, a `for` that pushes and one `reverse`,
/// counted in allocations rather than timed.
///
/// Neither reuses cells in the turn: `reverse_onto` allocates one per
/// element, so both make two per element. This pins that the literal is no
/// worse, not that number; a turn that learns to reuse would lower both.
#[test]
fn a_for_element_allocates_no_more_than_push_and_reverse() {
    let exe = build(
        "list_elements_allocations",
        r#"module main;
import std::core::{Iterator, Step, List, Range, print};

extern fn khora_alloc_count() -> Int;

fn comp(n: Int) -> List<Int> { [for i in Range::Of(0, n) => i] }

fn hand(n: Int) -> List<Int> {
  let mut acc = List::Nil;
  for i in Range::Of(0, n) { acc = List::Cons(i, acc); };
  acc.reverse()
}

pub fn main() -> Int {
  let a0 = khora_alloc_count();
  let c = comp(10000);
  let a1 = khora_alloc_count();
  let h = hand(10000);
  let a2 = khora_alloc_count();
  print("${a1 - a0} ${a2 - a1} ${List::length(c)} ${List::length(h)}");
  0
}
"#,
    );
    let out = run(&exe, "threads");
    let fields: Vec<i64> = out.split_whitespace().map(|f| f.parse().expect("a number")).collect();
    assert_eq!(fields[2..], [10000, 10000], "{out}");
    assert!(fields[0] <= fields[1], "the literal allocated more than push-and-reverse: {out}");
}
