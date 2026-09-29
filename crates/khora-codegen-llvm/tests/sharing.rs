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
//! A deferred finalizer is not among the entries: a `Region` stays on the
//! fiber that opened it, so a finalizer is run on the fiber that deferred
//! it and its captures never cross. Programs that hand one across are
//! refused, and those tests sit next to the rewrite a program takes.
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

/// Builds `body` after [`HEAD`], in the profile `KHORA_PROFILE` names: the
/// debug profile, which is the one with the owner check, unless a run of
/// this file asks for release to see what the counts do without it.
fn build(name: &str, body: &str) -> PathBuf {
    build_whole(name, &format!("{HEAD}\n{body}"))
}

/// Builds a whole program, header and all, as [`build`] does.
fn build_whole(name: &str, source: &str) -> PathBuf {
    build_whole_as(name, source, khora_codegen_llvm::Profile::from_env())
}

/// Builds a whole program in `profile`.
fn build_whole_as(name: &str, source: &str, profile: khora_codegen_llvm::Profile) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sharing_{name}"));
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    let outcome = khora_codegen_llvm::compile_with(&db, root, &exe, profile);
    if let Err(errors) = outcome {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Builds and runs `body` on both fiber backends, and requires `expected` on
/// stdout, a clean exit and no owner trap: once with every count atomic, and
/// once with local objects counted plain (`KHORA_RC_LOCAL=1`).
///
/// **Both, because the second is where a missed mark does harm.** With every
/// count atomic, a missed mark is a wrong owner and nothing more; with local
/// counts plain it is two threads plain-counting one object. The owner check
/// traps in either build, so each row goes red in each with its mark
/// removed.
fn crosses_marked(name: &str, body: &str, expected: &str) {
    runs_clean(name, &build(name, body), expected);
    let local = format!("{name}_local");
    khora_codegen_llvm::force_local_counts_on_this_thread(true);
    let exe = build(&local, body);
    khora_codegen_llvm::force_local_counts_on_this_thread(false);
    runs_clean(&local, &exe, expected);
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

/// Compiles a whole program and requires the checker to refuse it, with a
/// message that names `scoped`: the rewrite a refused crossing takes.
fn refused(name: &str, source: &str) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sharing_{name}"));
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    match khora_codegen_llvm::compile_with(&db, root, &exe, khora_codegen_llvm::Profile::Debug) {
        Ok(()) => panic!("`{name}` compiled; a region or scope reached another fiber"),
        Err(errors) => {
            let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
            assert!(
                messages.iter().any(|m| m.contains("stays on the fiber that opened it") && m.contains("scoped")),
                "`{name}` was refused for another reason: {messages:#?}"
            );
        }
    }
}

/// **A child cannot defer into a region its parent opened.** A finalizer's
/// captures need not be `Share`, so this one holds a `mut` record the child
/// writes after the defer; run by the parent's release, it read a field the
/// child could replace and free under it. Before a region stayed on its
/// fiber this compiled and ran, and the rows below it pinned the marks that
/// made the counts safe -- which left the fields racing.
#[test]
fn a_child_cannot_defer_into_its_parents_region() {
    refused(
        "defer_mut",
        r#"module main;

import std::core::{print, Fiber, Region, List};

type Holder = { mut xs: List<String> };

pub fn main() -> Int {
  let r = Region::open();
  let f = Fiber::spawn(fn () => {
    let h: Holder = { xs: List::Nil };
    Region::defer(r, fn () => print("finalizer sees ${List::length(h.xs)}"));
    h.xs = List::Cons("x", List::Nil);
    0
  });
  let n = Fiber::join(f);
  print("joined ${n}");
  0
}
"#,
    );
}

/// **The same through a `Map`**, which is `mut` inside.
#[test]
fn a_child_cannot_defer_a_map_into_its_parents_region() {
    refused(
        "defer_map",
        r#"module main;

import std::core::{print, Fiber, Region, Map};

pub fn main() -> Int {
  let r = Region::open();
  let f = Fiber::spawn(fn () => {
    let seen: Map<String, Int> = Map::new();
    Region::defer(r, fn () => print("finalizer sees ${Map::len(seen)}"));
    Map::insert(seen, "a", 1);
    0
  });
  let n = Fiber::join(f);
  print("joined ${n}");
  0
}
"#,
    );
}

/// **`std`'s own `acquire`, from a child, into its parent's `Scope`.** The
/// child acquires a connection whose `mut` fields it goes on writing, so the
/// parent's `scoped` would release it on the parent's fiber.
#[test]
fn a_child_cannot_acquire_into_its_parents_scope() {
    refused(
        "scope_child",
        r#"module main;

import std::core::{print, Fiber, Scope, List, scoped, acquire, ChildFailed};

type Conn = { name: String, mut uses: Int };

fn work() -> Int with { scope: Scope } {
  let c = acquire({ name: "c1", uses: 0 }, fn c => print("closed ${c.name} after ${c.uses} uses"));
  c.uses = c.uses + 1;
  c.uses
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
}

/// **The rewrite: each child opens its own `scoped`.** Two children each
/// acquire a connection with `mut` fields, write it, and close it when their
/// own `scoped` ends. Every defer and every release is on the child that
/// opened the region, so the runtime's owner check on the defer passes, and
/// in this debug build the finalizer's captures -- never marked shared --
/// are counted only by the fiber that made them, or the owner check traps.
#[test]
fn each_child_releases_what_it_acquires_in_its_own_scope() {
    let exe = build_whole(
        "twofibers",
        r#"module main;

import std::core::{print, Fiber, Scope, List, scoped, acquire};

type Conn = { name: String, mut uses: Int, mut log: List<String> };

fn make(n: Int) -> List<String> {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons("x${i}", xs); i = i + 1; };
  xs
}

fn work(n: Int) -> Int with { scope: Scope } {
  let c = acquire({ name: "c${n}", uses: 0, log: List::Nil },
    fn c => print("closed ${c.name} after ${c.uses} uses, log ${List::length(c.log)}"));
  c.uses = c.uses + 1;
  c.log = make(n);
  List::length(c.log)
}

pub fn main() -> Int {
  let a = Fiber::spawn(fn () => scoped(fn () => work(3)));
  let na = Fiber::join(a);
  let b = Fiber::spawn(fn () => scoped(fn () => work(4)));
  let nb = Fiber::join(b);
  print("joined ${na + nb}");
  0
}
"#,
    );
    runs_clean(
        "twofibers",
        &exe,
        "closed c3 after 1 uses, log 3\nclosed c4 after 1 uses, log 4\njoined 7",
    );
}

/// **The rewrite inside a function that has `scope` of its own**, the shape
/// of a server's handler loop. Each child is handed `scoped` with a named
/// function, joined or adopted into a nursery, and what it acquires is
/// released on the child when its own `scoped` ends: before the join returns,
/// and, for the adopted child, before that child's `scoped` call returns. The
/// adopted child waits for `run` to signal on a channel, so the order of its
/// lines is fixed. The model is the review's `rewrite_in_scope.kh`, whose
/// lambda spelling released into the parent's scope instead.
#[test]
fn the_rewrite_works_inside_a_function_with_its_own_scope() {
    let exe = build_whole(
        "rewrite_in_scope",
        r#"module main;

import std::core::{print, Fiber, Scope, Nursery, List, Channel, scoped, nursery, acquire, ChildFailed};

type Conn = { name: String, mut uses: Int, mut log: List<String> };

fn work(name: String) -> Int with { scope: Scope } {
  let c = acquire({ name: name, uses: 0, log: List::Nil },
    fn c => print("released ${c.name} after ${c.uses}"));
  c.uses = c.uses + 1;
  c.log = List::Cons("x", c.log);
  c.uses
}

fn joined_work() -> Int with { scope: Scope } { work("joined") }
fn adopted_work() -> () with { scope: Scope } { let _ = work("adopted"); () }
fn opens_its_own() -> Int { scoped(fn () => work("helper")) }
fn adopted_after(go: Channel<Int>) -> () {
  let _ = Channel::receive(go);
  scoped(adopted_work);
  print("adopted scoped ended");
}

fn run() -> Int with { scope: Scope, nursery: Nursery } {
  let _ = acquire(0, fn n => print("released run's own"));
  let f = Fiber::spawn(fn () => scoped(joined_work));
  let n = Fiber::join(f);
  print("joined ${n}");
  let g = Fiber::spawn(fn () => opens_its_own());
  let m = Fiber::join(g);
  print("joined helper ${m}");
  let go: Channel<Int> = Channel::bounded(1);
  nursery.adopt(Fiber::spawn(fn () => adopted_after(go)));
  print("run ends");
  Channel::send(go, 1);
  n + m
}

pub fn main() -> Int raises ChildFailed {
  let n = scoped(fn () => nursery(fn () => run())!)!;
  print("after scoped ${n}");
  0
}
"#,
    );
    runs_clean(
        "rewrite_in_scope",
        &exe,
        "released joined after 1\njoined 1\nreleased helper after 1\njoined helper 1\nrun ends\n\
         released adopted after 1\nadopted scoped ended\nreleased run's own\nafter scoped 2",
    );
}

/// **The lambda spelling of the rewrite, inside a function that has `scope`,
/// releases on the child.** `scoped(fn () => work())` hands its lambda a
/// scope, and that one shadows `run`'s, so "released c" comes before the
/// join returns. Resolved to `run`'s, the child captured its parent's scope
/// and was refused; before scopes stayed on their fiber, it compiled and
/// released after `run` ended. The review's `rewrite_in_scope.kh`.
#[test]
fn the_lambda_rewrite_releases_on_the_child_inside_a_scope() {
    let exe = build_whole(
        "lambda_in_scope",
        r#"module main;

import std::core::{print, Fiber, Scope, scoped, acquire};

type Conn = { name: String, mut uses: Int };

fn work() -> Int with { scope: Scope } {
  let c = acquire({ name: "c", uses: 0 }, fn c => print("released ${c.name} after ${c.uses}"));
  c.uses = c.uses + 1;
  c.uses
}

fn run() -> Int with { scope: Scope } {
  let f = Fiber::spawn(fn () => scoped(fn () => work()));
  let n = Fiber::join(f);
  print("joined ${n}");
  print("run ends");
  n
}

pub fn main() -> Int {
  let n = scoped(fn () => run());
  print("after scoped ${n}");
  0
}
"#,
    );
    runs_clean(
        "lambda_in_scope",
        &exe,
        "released c after 1\njoined 1\nrun ends\nafter scoped 1",
    );
}

/// **On one fiber, a lambda handed to `scoped` inside a function that has
/// `scope` releases at that `scoped`'s end**, as a named function does.
/// Resolved to the enclosing `scope`, what the lambda's `work` acquired was
/// released when the *outer* `scoped` ended: after "inner scoped ended",
/// with nothing refused and nothing trapped. The review's
/// `scoped_shadow.kh`.
#[test]
fn a_lambda_handed_to_scoped_uses_the_scope_it_is_handed() {
    let exe = build_whole(
        "scoped_shadow",
        r#"module main;

import std::core::{print, Scope, scoped, acquire};

fn work(tag: String) -> Int with { scope: Scope } {
  let _ = acquire(1, fn n => print("released ${tag}"));
  1
}

fn w_named() -> Int with { scope: Scope } { work("named") }

fn inner_scoped() -> Int with { scope: Scope } {
  let _ = scoped(fn () => work("lambda"));
  print("inner scoped ended (lambda)");
  let _ = scoped(w_named);
  print("inner scoped ended (named)");
  1
}

pub fn main() -> Int {
  let _ = scoped(fn () => inner_scoped());
  print("outer scoped ended");
  0
}
"#,
    );
    runs_clean(
        "scoped_shadow",
        &exe,
        "released lambda\ninner scoped ended (lambda)\nreleased named\ninner scoped ended (named)\n\
         outer scoped ended",
    );
}

/// **Nested handlers pick the nearest scope.** Inside a function that has
/// `scope`:
/// - a lambda inside the lambda handed to `scoped`, handed the scope again
///   by a function that forwards it, releases at that `scoped`'s end;
/// - a `with { scope: .. }` block inside the lambda is nearer than what
///   `scoped` hands it, and releases when its region does;
/// - a `with { scope: .. }` block in the function is nearer than the
///   function's own `scope`;
/// - `nursery`, whose parameter hands a capability the same way, inside a
///   lambda handed to `scoped`;
/// - in `main`, which has no `scope`, a lambda handed to `scoped` inside a
///   `with { scope: .. }` block: the handed scope is nearer than the block's.
///
/// Resolved to the lexical `scope`, the first and last two released late:
/// after "run ends", and after "scoped in with ended".
#[test]
fn nested_handlers_pick_the_nearest_scope() {
    let exe = build_whole(
        "nested_scopes",
        r#"module main;

import std::core::{print, Scope, Region, scoped, acquire, nursery};

fn work(tag: String) -> Int with { scope: Scope } {
  let _ = acquire(1, fn n => print("released ${tag}"));
  1
}

fn twice(f: () -> Int with { scope: Scope }) -> Int with { scope: Scope } { f() + f() }

fn nested_lambda() -> Int with { scope: Scope } {
  let _ = scoped(fn () => twice(fn () => work("nested")));
  print("nested scoped ended");
  1
}

fn with_inside() -> Int with { scope: Scope } {
  {
    let r = Region::open();
    let _ = scoped(fn () => {
      let n = with { scope: handler for Scope { defer: fn f => Region::defer(r, f) } } { work("with-in-lambda") };
      print("lambda body ends");
      n
    });
    print("with-inside scoped ended");
  };
  print("region ended");
  1
}

fn with_block() -> Int with { scope: Scope } {
  let n = {
    let r = Region::open();
    with { scope: handler for Scope { defer: fn f => Region::defer(r, f) } } { work("with-block") }
  };
  print("with-block ended");
  n
}

fn in_nursery() -> Int with { scope: Scope } {
  let _ = scoped(fn () => nursery(fn () => { let _ = work("nursery"); 1 })!)! catch { _ => 0 };
  print("nursery scoped ended");
  1
}

fn in_main_with() -> Int {
  let r = Region::open();
  with { scope: handler for Scope { defer: fn f => Region::defer(r, f) } } {
    let _ = scoped(fn () => work("lambda in with"));
    print("scoped in with ended");
    1
  }
}

fn run() -> Int with { scope: Scope } {
  let _ = acquire(0, fn n => print("released run's own"));
  let _ = nested_lambda();
  let _ = with_inside();
  let _ = with_block();
  let _ = in_nursery();
  print("run ends");
  1
}

pub fn main() -> Int {
  let _ = scoped(run);
  let _ = in_main_with();
  print("end");
  0
}
"#,
    );
    runs_clean(
        "nested_scopes",
        &exe,
        "released nested\nreleased nested\nnested scoped ended\n\
         lambda body ends\nwith-inside scoped ended\nreleased with-in-lambda\nregion ended\n\
         released with-block\nwith-block ended\n\
         released nursery\nnursery scoped ended\n\
         run ends\nreleased run's own\n\
         released lambda in with\nscoped in with ended\nend",
    );
}

/// **A raised record with a `mut` field, joined from two fibers, is refused
/// before it can run.** The RC Stage 2 review's `raise_mut_two.kh`: the
/// parent catches the error and stores a fresh list into it while a second
/// child, joining the same handle, reads that field. The runtime marks the
/// error at the handover, which makes the counts atomic and leaves the field
/// a race, so a spawned fiber's error is held to `Share`.
#[test]
fn an_error_with_a_mut_field_cannot_leave_a_fiber() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sharing_raise_mut_two");
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let source = r#"module main;

import std::core::{print, Fiber, List, Channel};

type Oops = { mut xs: List<String> };

fn make(n: Int) -> List<String> {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons("x${i}", xs); i = i + 1; };
  xs
}

fn fails() -> Int raises Oops { raise { xs: make(3) } }

fn spin(xs: List<String>, times: Int) -> Int {
  let mut acc = 0;
  let mut i = 0;
  while i < times { let ys = xs; acc = acc + List::length(ys); i = i + 1; };
  acc
}

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => fails()!);
  let go_on: Channel<Int> = Channel::bounded(1);
  let g = Fiber::spawn(fn () => {
    let _ = Channel::receive(go_on);
    Fiber::join(f)! catch { Oops { xs } => spin(xs, 200000) }
  });
  let mut mine_xs = List::Nil;
  let _ = Fiber::join(f)! catch { e => { e.xs = make(10); mine_xs = e.xs; 0 } };
  Channel::send(go_on, 1);
  let mine = spin(mine_xs, 200000);
  let theirs = Fiber::join(g);
  print("mine ${mine} theirs ${theirs}");
  0
}
"#;
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    let Err(errors) =
        khora_codegen_llvm::compile_with(&db, root, &exe, khora_codegen_llvm::Profile::Debug)
    else {
        panic!("a fiber raising a `mut` record compiled");
    };
    let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`Oops` does not implement `Share`, which a spawned fiber's error requires")),
        "{messages:#?}"
    );
}

/// **A capture whose type is never settled is refused at build**, after
/// `khora check` accepted it: nothing pins `x`, so the backend cannot lay
/// out `go`, and says so. Pinned so that a checker change which starts
/// accepting such a program at build, or starts refusing it as a sharing
/// error, is seen: an unsolved capture has no value, and so nothing crosses.
#[test]
fn a_capture_never_solved_is_refused_at_build() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sharing_never_solved");
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let source = "module main;

import std::core::{Fiber};

pub fn main() -> Int {
  let go = fn x => Fiber::spawn(fn () => { let _k = x; 3 });
  0
}
";
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    let Err(errors) =
        khora_codegen_llvm::compile_with(&db, root, &exe, khora_codegen_llvm::Profile::Debug)
    else {
        panic!("a closure whose capture was never solved compiled");
    };
    let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("never pinned down"))
            && !messages.iter().any(|m| m.contains("cannot be handed to another fiber")),
        "{messages:#?}"
    );
}

/// **A fiber raising a region is refused before it can run.** Unrefused,
/// the parent's catch dropped the child's region last and ran the child's
/// finalizer on the parent (a debug build trapped in the owner check; a
/// release build raced). The review's `raise_region.kh`.
#[test]
fn a_child_cannot_raise_its_region_to_the_parent() {
    refused(
        "raise_region",
        r#"module main;

import std::core::{print, Fiber, Region, List};

type H = { mut n: Int, mut xs: List<String> };
type Oops = { why: String, r: Region };

fn child() -> Int raises Oops {
  let h: H = { n: 0, xs: List::Nil };
  let r = Region::open();
  Region::defer(r, fn () => print("finalizer sees n=${h.n}"));
  h.n = 1;
  raise { why: "carrying the region", r: r }
}

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => child()!);
  let got = Fiber::join(f)! catch {
    Oops { why, r } => { print("parent caught: ${why}"); 7 },
  };
  print("parent after catch ${got}");
  0
}
"#,
    );
}

/// **A region reaching a child through a capture whose type is solved after
/// the spawn is refused before it can run.** At the spawn, `x` is still a
/// variable; `go(r)` solves it to `Region` afterwards, and the capture is
/// asked again then. Unrefused, the child was handed the region and dropped
/// it last, and only the runtime's release check -- a fatal error, exit 134
/// -- kept its finalizer off the child. That check stays as the backstop,
/// tested without the compiler by `khora-rt`'s
/// `region::tests::a_release_on_another_fiber_is_fatal`: this was the one
/// program that reached it, and no route known reaches it now. The review's
/// `var_capture.kh`, with the child waiting on a channel so that its
/// reference would be the last on either backend.
#[test]
fn a_region_captured_through_a_type_solved_later_is_refused() {
    refused(
        "var_capture",
        r#"module main;

import std::core::{print, Fiber, Region, List, Channel};

type H = { mut n: Int, mut xs: List<String> };

fn make(n: Int) -> List<String> {
  let mut xs = List::Nil;
  let mut i = 0;
  while i < n { xs = List::Cons("x${i}", xs); i = i + 1; };
  xs
}

pub fn main() -> Int {
  let h: H = { n: 0, xs: List::Nil };
  let let_go: Channel<Int> = Channel::bounded(1);
  let go = fn x => Fiber::spawn(fn () => { let _ = Channel::receive(let_go); let _keep = x; 3 });
  let r = Region::open();
  Region::defer(r, fn () => print("finalizer sees n=${h.n} len=${List::length(h.xs)}"));
  let f = go(r);
  h.n = 1;
  h.xs = make(5);
  Channel::send(let_go, 1);
  let k = Fiber::join(f);
  print("parent joined ${k}, n=${h.n}");
  0
}
"#,
    );
}

/// **A `bench` block has a root scope of its own**, as a test does. Each
/// bench runs on a spawned fiber, which the root refuses. The review's
/// `benchroot.kh`.
#[test]
fn a_bench_block_has_its_own_root_scope() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sharing_bench_root");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "benches.exe" } else { "benches" });
    let _ = std::fs::remove_file(&exe);
    let source = r#"module main;

import std::core::{print, Scope, acquire};

type Conn = { name: String, mut uses: Int };

fn uses(name: String) -> Int with { scope: Scope } {
  let c = acquire({ name: name, uses: 0 }, fn c => print("released ${c.name}"));
  c.uses = c.uses + 1;
  c.uses
}

bench "acquiring into the root scope" {
  let _ = uses("b") with { scope: Scope::root() };
}

pub fn main() -> Int { 0 }
"#;
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile_benches(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }
    for backend in ["threads", "scheduler"] {
        let out = Command::new(&exe).env("KHORA_FIBERS", backend).output().expect("the benches should run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let released = stdout.find("released b");
        let reported = stdout.find("bench acquiring into the root scope ... P50");
        assert!(
            out.status.success() && released.is_some() && reported.is_some() && released < reported,
            "`{backend}`: the bench's root scope was not released when the bench ended: {:?}, \
             stderr {stderr:?}, stdout starts {:?}",
            out.status.code(),
            &stdout[..stdout.len().min(300)]
        );
    }
}

/// **A child reaching for the root region traps.** It is reachable by name,
/// so no type rule keeps a child off it, and a finalizer the child deferred
/// there would run at exit on the main fiber while the child might still be
/// writing what it captured.
#[test]
fn a_child_reaching_the_root_region_traps() {
    let exe = build_whole(
        "root_child",
        r#"module main;

import std::core::{print, Fiber, Region};

type Holder = { mut n: Int };

fn child() -> Int {
  let h: Holder = { n: 0 };
  Region::defer(Region::root(), fn () => print("root finalizer sees n=${h.n}"));
  h.n = 5;
  h.n
}

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => child());
  let n = Fiber::join(f);
  print("joined ${n}");
  0
}
"#,
    );
    for backend in ["threads", "scheduler"] {
        let out = Command::new(&exe).env("KHORA_FIBERS", backend).output().expect("the program should run");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(134), "`{backend}`: stderr {stderr:?}");
        assert!(
            stderr.contains("`Region::root()` or `Scope::root()` reached from a spawned fiber")
                && stderr.contains("scoped"),
            "`{backend}`: {stderr:?}"
        );
    }
}

/// **A `test` block has a root scope of its own**, released when the test
/// ends. Each test runs on a spawned fiber, which the root region refuses, so
/// without one a test reaching `Scope::root()` would trap; with the program's
/// root, its finalizer would run at exit, after the summary line, on another
/// fiber. It runs before the test's verdict is reported instead.
#[test]
fn a_test_block_has_its_own_root_scope() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sharing_test_root");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "tests.exe" } else { "tests" });
    let _ = std::fs::remove_file(&exe);
    let source = r#"module main;

import std::core::{print, Scope, acquire, assert};

type Conn = { name: String, mut uses: Int };

fn uses(name: String) -> Int with { scope: Scope } {
  let c = acquire({ name: name, uses: 0 }, fn c => print("released ${c.name} after ${c.uses}"));
  c.uses = c.uses + 1;
  c.uses
}

test "the first test reaching the root scope" {
  let n = uses("one") with { scope: Scope::root() };
  assert(n == 1);
}

test "the second test reaching the root scope" {
  let n = uses("two") with { scope: Scope::root() };
  assert(n == 1);
}
"#;
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile_tests(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }
    for backend in ["threads", "scheduler"] {
        // One at a time, so the order of the lines is the order of events.
        let out = Command::new(&exe)
            .env("KHORA_FIBERS", backend)
            .arg("--filter=first")
            .output()
            .expect("the suite should run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "`{backend}`: stdout {stdout:?}, stderr {stderr:?}");
        let released = stdout.find("released one after 1");
        let reported = stdout.find("test the first test reaching the root scope ... ok");
        assert!(
            released.is_some() && reported.is_some() && released < reported,
            "`{backend}`: the test's root scope was not released when the test ended: {stdout:?}"
        );

        let both = Command::new(&exe).env("KHORA_FIBERS", backend).output().expect("the suite should run");
        let stdout = String::from_utf8_lossy(&both.stdout);
        assert!(
            both.status.success()
                && stdout.contains("released one after 1")
                && stdout.contains("released two after 1")
                && stdout.contains("2 passed, 0 failed"),
            "`{backend}`: each test has a root of its own: {stdout:?}"
        );
    }
}

/// **A `test` block's raised error.** `khora test` runs each block on a
/// thread and fiber of its own and releases an error that escapes it on the
/// runner's, so the error was made on one fiber and counted on another. The
/// error here is a record, a counted object: a field-less case is a static,
/// and the check exempts statics, which is how `testing.rs`'s own escaping
/// error test missed this in the debug build.
#[test]
fn a_test_marks_the_error_it_raises() {
    for local in [false, true] {
        test_raise_is_marked(local);
    }
}

/// [`a_test_marks_the_error_it_raises`], with local counts plain if `local`.
fn test_raise_is_marked(local: bool) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sharing_test_raise_{local}"));
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
    khora_codegen_llvm::force_local_counts_on_this_thread(local);
    let built = khora_codegen_llvm::compile_tests(&db, root, &exe);
    khora_codegen_llvm::force_local_counts_on_this_thread(false);
    if let Err(errors) = built {
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
        "local counts {local}: {:?}, stdout {stdout:?}, stderr {stderr:?}",
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
    let exe = build_whole_as(
        "control",
        &format!(
            "{HEAD}\nextern fn khora_rc_check(word: Int) -> ();

pub fn main() -> Int {{
  khora_rc_check({word});
  print(\"not trapped\");
  0
}}"
        ),
        khora_codegen_llvm::Profile::Debug,
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
