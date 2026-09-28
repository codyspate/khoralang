#![cfg(feature = "llvm")]

//! Every runtime entry that publishes a value marks it shared, one test per
//! entry.
//!
//! **What these guard: a value reaching a second fiber unmarked.** Each
//! program builds a fresh list on one fiber, hands it through one runtime
//! entry, and releases it on another. A debug build records the fiber that
//! made each object and checks it at every count of an unshared one, so if
//! the entry did not mark the list, the receiving fiber's first count traps
//! with a message naming both fibers. Counting is atomic either way, so the
//! trap is the only symptom: without it a missed mark changes nothing today,
//! and becomes a race only once local objects are counted without a lock.
//!
//! Each test was watched red with its entry's mark removed, and
//! [`the_owner_check_traps_on_a_foreign_count`] keeps the check itself from
//! going quiet: it counts an object with another fiber's id and requires the
//! trap.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// What every program starts with: the imports and a list builder.
const HEAD: &str = "module main;

import std::core::{print, Fiber, Fibers, Channel, Shared, Region, List, Option};

fn make(n: Int) -> List<String> {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons(\"x${i}\", xs); i = i + 1; };
  xs
}
";

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

/// Builds `body` after [`HEAD`] in the debug profile, which is the one with
/// the owner check.
fn build(name: &str, body: &str) -> PathBuf {
    build_whole(name, &format!("{HEAD}\n{body}"))
}

/// Builds a whole program, header and all, in the debug profile.
fn build_whole(name: &str, source: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sharing_{name}"));
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    let outcome =
        khora_codegen_llvm::compile_with(&db, root, &exe, khora_codegen_llvm::Profile::Debug);
    if let Err(errors) = outcome {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Builds and runs `body` on both fiber backends, and requires `expected` on
/// stdout, a clean exit and no owner trap.
fn crosses_marked(name: &str, body: &str, expected: &str) {
    runs_clean(name, &build(name, body), expected);
}

/// Runs `exe` on both fiber backends, and requires `expected` on stdout, a
/// clean exit and no owner trap.
fn runs_clean(name: &str, exe: &PathBuf, expected: &str) {
    for backend in ["threads", "scheduler"] {
        let out = Command::new(exe)
            .env("KHORA_FIBERS", backend)
            .output()
            .expect("the program should run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stdout.trim() == expected,
            "`{name}` on `{backend}`: {:?}, stdout {stdout:?}, stderr {stderr:?}",
            out.status.code()
        );
    }
}

/// **Captured by `spawn`.** The child reads and releases a list the parent built.
#[test]
fn a_spawn_marks_what_it_captures() {
    crosses_marked(
        "spawn",
        "pub fn main() -> Int {
  let xs = make(10);
  let f = Fiber::spawn(fn () => List::length(xs));
  let n = Fiber::join(f);
  print(\"got ${n}\");
  0
}",
        "got 10",
    );
}

/// **Returned by `join`.** The child builds a list and the parent releases it.
#[test]
fn a_fiber_marks_its_answer_before_storing_it() {
    crosses_marked(
        "outcome",
        "pub fn main() -> Int {
  let f = Fiber::spawn(fn () => make(10));
  let xs = Fiber::join(f);
  print(\"got ${List::length(xs)}\");
  0
}",
        "got 10",
    );
}

/// **Sent over a channel.** The receiver releases what the sender built.
#[test]
fn a_send_marks_the_value_it_enqueues() {
    crosses_marked(
        "send",
        "pub fn main() -> Int {
  let ch: Channel<List<String>> = Channel::bounded(4);
  let f = Fiber::spawn(fn () => match Channel::receive(ch) {
    Option::Some(got) => List::length(got),
    Option::None => 0,
  });
  Channel::send(ch, make(10));
  let n = Fiber::join(f);
  print(\"got ${n}\");
  0
}",
        "got 10",
    );
}

/// **Put in a `Shared` by `of`.** Another fiber reads the list out and releases it.
#[test]
fn a_cell_marks_what_it_is_opened_with() {
    crosses_marked(
        "open",
        "pub fn main() -> Int {
  let cell = Shared::of(make(10));
  let f = Fiber::spawn(fn () => List::length(Shared::get(cell)));
  let n = Fiber::join(f);
  print(\"got ${n}\");
  0
}",
        "got 10",
    );
}

/// **Put in a `Shared` by `set`.**
#[test]
fn a_cell_marks_what_is_set_into_it() {
    crosses_marked(
        "set",
        "pub fn main() -> Int {
  let cell = Shared::of(List::Nil);
  Shared::set(cell, make(10));
  let f = Fiber::spawn(fn () => List::length(Shared::get(cell)));
  let n = Fiber::join(f);
  print(\"got ${n}\");
  0
}",
        "got 10",
    );
}

/// **Put in a `Shared` by `update`**: what the change function returned.
#[test]
fn a_cell_marks_what_update_returned() {
    crosses_marked(
        "update",
        "pub fn main() -> Int {
  let cell: Shared<List<String>> = Shared::of(List::Nil);
  let _ = Shared::update(cell, fn (old) => make(10));
  let f = Fiber::spawn(fn () => List::length(Shared::get(cell)));
  let n = Fiber::join(f);
  print(\"got ${n}\");
  0
}",
        "got 10",
    );
}

/// **Put in a `Shared` by `modify`**: the new state the change function returned.
#[test]
fn a_cell_marks_what_modify_returned() {
    crosses_marked(
        "modify",
        "pub fn main() -> Int {
  let cell: Shared<List<String>> = Shared::of(List::Nil);
  let k = Shared::modify(cell, fn (old) => { state: make(10), result: 1 });
  let f = Fiber::spawn(fn () => List::length(Shared::get(cell)));
  let n = Fiber::join(f);
  print(\"got ${n} ${k}\");
  0
}",
        "got 10 1",
    );
}

/// **Deferred into a `Region`.** A child defers a finalizer capturing its own list; the parent's region runs and releases it.
#[test]
fn a_region_marks_a_deferred_finalizer() {
    crosses_marked(
        "defer",
        "fn count(xs: List<String>) -> Int { List::length(xs) }

fn run() -> Int {
  let r = Region::open();
  let f = Fiber::spawn(fn () => {
    let xs = make(10);
    Region::defer(r, fn () => { let _ = count(xs); () });
    0
  });
  Fiber::join(f)
}

pub fn main() -> Int {
  let n = run();
  print(\"got 10 ${n}\");
  0
}",
        "got 10 0",
    );
}

/// **Adopted into a nursery.** A child adopts a handle it made into the parent's nursery, and the parent releases it. The handle is born shared, so this row's mark and the birth mark each cover it: the test goes red only with both removed.
#[test]
fn a_nursery_marks_an_adopted_handle() {
    crosses_marked(
        "adopt",
        "pub fn main() -> Int {
  let crew = Fibers::open();
  let f = Fiber::spawn(fn () => {
    Fibers::adopt(crew, Fiber::spawn(fn () => { let _ = List::length(make(10)); () }));
    0
  });
  let _ = Fiber::join(f);
  let _failed = Fibers::wait(crew);
  print(\"got 10\");
  0
}",
        "got 10",
    );
}

/// **A finalizer deferred by one fiber may capture a `mut` record, written
/// after the defer, and run on another.** `Region::defer` does not require
/// `Share` captures, so this is the one route a non-`Share` value has across
/// fibers. Marking the finalizer when it was deferred left the fresh list the
/// child stored afterwards local to the child, and the region's release on
/// the main fiber counted it: a trap on a correct program. It is marked when
/// the region runs it instead.
#[test]
fn a_finalizer_sees_what_was_stored_after_it_was_deferred() {
    let exe = build_whole(
        "defer_mut",
        r#"module main;

import std::core::{print, Fiber, Region, List};

type Holder = { mut xs: List<String> };

fn make(n: Int) -> List<String> {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons("x${i}", xs); i = i + 1; };
  xs
}

pub fn main() -> Int {
  let r = Region::open();
  let f = Fiber::spawn(fn () => {
    let h: Holder = { xs: List::Nil };
    Region::defer(r, fn () => print("finalizer sees ${List::length(h.xs)}"));
    h.xs = make(5);
    0
  });
  let n = Fiber::join(f);
  print("joined ${n}");
  0
}
"#,
    );
    runs_clean("defer_mut", &exe, "joined 0\nfinalizer sees 5");
}

/// **The same through a `Map`**, which is `mut` inside: entries inserted after
/// the defer are local objects until the finalizer runs.
#[test]
fn a_finalizer_sees_a_map_filled_after_it_was_deferred() {
    let exe = build_whole(
        "defer_map",
        r#"module main;

import std::core::{print, Fiber, Region, Map};

pub fn main() -> Int {
  let r = Region::open();
  let f = Fiber::spawn(fn () => {
    let seen: Map<String, Int> = Map::new();
    Region::defer(r, fn () => print("finalizer sees ${Map::len(seen)}"));
    Map::insert(seen, "a", 1);
    Map::insert(seen, "b", 2);
    0
  });
  let n = Fiber::join(f);
  print("joined ${n}");
  0
}
"#,
    );
    runs_clean("defer_map", &exe, "joined 0\nfinalizer sees 2");
}

/// **`std`'s own `acquire`, from a child, into its parent's `Scope`.** A
/// `Scope` is `Share`, so a child may be handed one. The child acquires a
/// connection, whose `mut` fields it then writes, and the parent's `scoped`
/// runs the finalizer when it ends.
#[test]
fn a_child_acquires_into_its_parents_scope() {
    let exe = build_whole(
        "scope_child",
        r#"module main;

import std::core::{print, Fiber, Scope, List, scoped, acquire, ChildFailed};

fn make(n: Int) -> List<String> {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons("x${i}", xs); i = i + 1; };
  xs
}

type Conn = { name: String, mut uses: Int, mut log: List<String> };

fn open_conn(n: Int) -> Conn { { name: "c${n}", uses: 0, log: List::Nil } }
fn close_conn(c: Conn) -> () { print("closed ${c.name} after ${c.uses} uses, log ${List::length(c.log)}") }

fn work() -> Int with { scope: Scope } {
  let c = acquire(open_conn(1), fn c => close_conn(c));
  c.uses = c.uses + 1;
  c.log = make(3);
  List::length(c.log)
}

fn part_a() -> Int with { scope: Scope } {
  let s = scope;
  let f = Fiber::spawn(fn () => with { scope: s } { work() });
  Fiber::join(f)
}

pub fn main() -> Int raises ChildFailed {
  let a = scoped(fn () => part_a())!;
  print("A ${a}");
  0
}
"#,
    );
    runs_clean("scope_child", &exe, "closed c1 after 1 uses, log 3\nA 3");
}

/// **A `test` block's raised error.** `khora test` runs each block on a
/// thread and fiber of its own and releases an error that escapes it on the
/// runner's, so the error was made on one fiber and counted on another. The
/// error here is a record, a counted object: a field-less case is a static,
/// and the check exempts statics, which is how `testing.rs`'s own escaping
/// error test missed this in the debug build.
#[test]
fn a_test_marks_the_error_it_raises() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sharing_test_raise");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "tests.exe" } else { "tests" });
    let _ = std::fs::remove_file(&exe);
    let source = "module main;

import std::core::{assert};

type Odd = { n: Int, why: String };

fn halve(n: Int) -> Int raises Odd {
  if n % 2 == 1 { raise { n: n, why: \"odd ${n}\" } };
  n / 2
}

test \"an odd number has no half\" { assert(halve(7)! == 3); }
";
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile_tests(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }
    let out = Command::new(&exe).output().expect("the suite should run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("test an odd number has no half ... raised")
            && stdout.contains("0 passed, 1 failed")
            && out.status.code() == Some(1),
        "{:?}, stdout {stdout:?}, stderr {stderr:?}",
        out.status.code()
    );
}

/// **The check has teeth.** A count of an object whose recorded owner is
/// another fiber traps, with the message the tests above rely on. Without
/// this, a check that had gone quiet would pass every test in this file.
///
/// The word is built by hand: fiber 99 in the owner bits, a count of one,
/// and neither flag. Fiber 99 is not running in a program that spawns
/// nothing, so the running fiber cannot be it.
#[test]
fn the_owner_check_traps_on_a_foreign_count() {
    let word = (99u64 << khora_rt::KHORA_OWNER_SHIFT) | 1;
    let exe = build(
        "control",
        &format!(
            "extern fn khora_rc_check(word: Int) -> ();

pub fn main() -> Int {{
  khora_rc_check({word});
  print(\"not trapped\");
  0
}}"
        ),
    );
    let out = Command::new(&exe).output().expect("the program should run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(134), "stderr {stderr:?}");
    assert!(
        stderr.contains(
            "khora: object made on fiber 99 was counted on fiber "
        ),
        "{stderr:?}"
    );
    assert!(
        stderr.contains(
            "without being shared -- a runtime entry published it without marking it"
        ),
        "{stderr:?}"
    );
}
