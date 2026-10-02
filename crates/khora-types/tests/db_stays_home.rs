//! A `Db` stays on the fiber it was installed on.
//!
//! **What these guard: two fibers on one lease's connection.** A pool lends
//! the borrower's fiber the connection itself, and the driver writes the
//! connection's `mut` fields on every statement, so a second fiber using the
//! lease's `db` counts and writes objects the first fiber made: a race the
//! debug owner check traps on, and a use-after-free in a release build. Two
//! fibers on one connection also share its transaction depth, so each could
//! release or roll back the other's savepoint. So a spawned fiber cannot
//! take its parent's `db`, and the refusal names the rewrite: take a lease
//! in the spawned fiber.
//!
//! The other half is what the rule buys, and is guarded here too: a handler
//! for `Db` never crosses, so it may capture a record with `mut` fields,
//! which is what a lent connection is. Every program below that compiles
//! uses such a handler, and was refused before `Db` stayed on its fiber.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_hir::HirError;

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the source directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic from one program compiled together with `std`, which is
/// where `Db` is declared.
fn errors_with_std(program: &str) -> Vec<HirError> {
    let std_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut paths = Vec::new();
    sources(&std_dir, &mut paths);
    paths.sort();

    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("the sources should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    let mine = SourceFile::new(&db, PathBuf::from("program.kh"), program.to_string());
    files.push(mine);
    SourceRoot::new(&db, files);

    khora_types::diagnostics(&db, mine).to_vec()
}

/// A lent connection, as a pool's lease would install it: a handler for
/// `Db` over a record with a `mut` field, and `with_lease`, which takes one
/// on the calling fiber the way `postgres::pool::with_db` does.
const LEASE: &str = "import std::core::{Fiber, List, Result, Channel};
import std::db::{Db, DbError, Row, Cell};

type Conn = { mut statements: Int };

fn lent(c: Conn) -> Db {
  handler for Db {
    query: fn (_sql, _binds) => { c.statements = c.statements + 1; Result::Ok(List::Nil) },
    execute: fn (_sql, _binds) => { c.statements = c.statements + 1; Result::Ok(c.statements) },
    depth: fn () => 0,
    begin: fn () => Result::Ok(()),
    commit: fn () => Result::Ok(()),
    rollback: fn () => Result::Ok(()),
    savepoint: fn _level => Result::Ok(()),
    release: fn _level => Result::Ok(()),
    rollback_to: fn _level => Result::Ok(()),
    broken: fn () => (),
    query_each: fn (_sql, sets) => List::map(sets, fn _values => { c.statements = c.statements + 1; Result::Ok(List::Nil) }),
  }
}

fn with_lease<A, 'ef, 'er>(body: () -> A with { 'ef | db: Db } raises 'er) -> A with 'ef raises 'er {
  let c: Conn = { statements: 0 };
  with { db: lent(c) } { body()! }
}

fn count() -> Int with { db: Db } {
  match db.execute(\"select 1\", List::Nil) { Result::Ok(n) => n, Result::Err(_) => 0 }
}
";

fn program(body: &str) -> String {
    format!("module main;\n\n{LEASE}\n{body}")
}

/// The byte range of the `nth` occurrence (from 0) of `what` in `text`.
fn span_of(text: &str, what: &str, nth: usize) -> (usize, usize) {
    let start = text.match_indices(what).nth(nth).map(|(i, _)| i).expect("the text should be there");
    (start, start + what.len())
}

/// The one-based line of byte offset `at`.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

fn span(error: &HirError) -> (usize, usize) {
    (usize::from(error.range.start()), usize::from(error.range.end()))
}

/// The refusals whose message mentions `about`.
fn refusals<'e>(errors: &'e [HirError], about: &str) -> Vec<&'e HirError> {
    errors.iter().filter(|e| e.message.contains(about)).collect()
}

/// **The refusal, at the use, naming the spawn and the rewrite.** A body
/// that spawns a fiber which calls something needing `db`: the capture is
/// implicit, so the caret goes on the call that needs it, and the message
/// says which spawn took it there.
#[test]
fn a_fiber_spawned_in_a_lease_cannot_use_its_db() {
    let text = program(
        "fn fanout() -> Int with { db: Db } {
  let a = Fiber::spawn(fn () => count());
  Fiber::join(a)
}

pub fn main() -> Int { with_lease(fanout) }
",
    );
    let errors = errors_with_std(&text);
    let found = refusals(&errors, "`db` cannot be handed to another fiber");
    assert_eq!(found.len(), 1, "expected one refusal of `db`, got {errors:#?}");
    let refusal = found[0];
    assert!(
        refusal.message.contains("take a lease in the spawned fiber"),
        "the refusal should name the rewrite: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("Fiber::spawn(fn () => with_db(pool, work))"),
        "the refusal should spell the rewrite: {}",
        refusal.message
    );
    let spawn = span_of(&text, "Fiber::spawn(fn () => count())", 0);
    let use_ = span_of(&text, "count()", 1);
    assert_eq!(span(refusal), use_, "the caret should be on the use: {}", refusal.message);
    let line = line_of(&text, spawn.0);
    assert!(
        refusal.message.contains(&format!("spawned at line {line}, column 11")),
        "the refusal should say where the fiber was spawned (line {line}): {}",
        refusal.message
    );
    assert_eq!(errors.len(), 1, "nothing else should be refused: {errors:#?}");
}

/// **A use written by name** is the same refusal, with the caret on `db`.
#[test]
fn a_fiber_that_names_db_is_refused_at_the_name() {
    let text = program(
        "fn fanout() -> Int with { db: Db } {
  let a = Fiber::spawn(fn () => {
    let _ = db.execute(\"select 1\", List::Nil);
    1
  });
  Fiber::join(a)
}

pub fn main() -> Int { with_lease(fanout) }
",
    );
    let errors = errors_with_std(&text);
    let found = refusals(&errors, "`db` cannot be handed to another fiber");
    assert_eq!(found.len(), 1, "expected one refusal of `db`, got {errors:#?}");
    let use_ = span_of(&text, "db.execute(\"select 1\", List::Nil);\n    1", 0);
    assert_eq!(span(found[0]).0, use_.0, "the caret should be on `db`: {}", found[0].message);
    assert!(found[0].message.contains("take a lease in the spawned fiber"), "{}", found[0].message);
}

/// **The rewrite compiles**: each spawned fiber takes a lease of its own,
/// also from inside a body that holds one.
#[test]
fn a_spawn_that_takes_its_own_lease_compiles() {
    let text = program(
        "fn fanout() -> Int with { db: Db } {
  let a = Fiber::spawn(fn () => with_lease(count));
  let b = Fiber::spawn(fn () => with_lease(fn () => count() + 1));
  count() + Fiber::join(a) + Fiber::join(b)
}

pub fn main() -> Int { with_lease(fanout) }
",
    );
    let errors = errors_with_std(&text);
    assert!(errors.is_empty(), "the rewrite should compile, got {errors:#?}");
}

/// **A `Db` kept past its body, on the same fiber, compiles.** The rule is
/// about fibers, not lifetimes: a handler returned out of the body and
/// installed again later never left the fiber.
#[test]
fn a_db_used_after_the_body_on_the_same_fiber_compiles() {
    let text = program(
        "fn keep() -> Db with { db: Db } { db }

pub fn main() -> Int {
  let d = with_lease(keep);
  let n = with { db: d } { count() };
  let other = Fiber::spawn(fn () => 2);
  n + Fiber::join(other)
}
",
    );
    let errors = errors_with_std(&text);
    assert!(errors.is_empty(), "a `Db` kept on its own fiber should compile, got {errors:#?}");
}

/// **Every other route is refused with the same rewrite**: a channel of
/// `Db`s, a `Shared` cell holding one, a record holding one handed to a
/// fiber, and a handler for another effect that uses `db`, which would let
/// the handler carry it across.
#[test]
fn every_route_to_another_fiber_names_the_lease() {
    let mut missed = Vec::new();
    for (route, body, about) in [
        (
            "a channel",
            "pub fn main() -> Int {
  let ch: Channel<Db> = Channel::bounded(1);
  0
}
",
            "`Db` does not implement `Share`",
        ),
        (
            "a shared cell",
            "import std::core::{Shared};

fn keep() -> Int with { db: Db } {
  let cell = Shared::of(db);
  0
}

pub fn main() -> Int { 0 }
",
            "`Db` does not implement `Share`",
        ),
        (
            "a record",
            "type Holder = { d: Db };

fn go() -> Int with { db: Db } {
  let h: Holder = { d: db };
  let a = Fiber::spawn(fn () => with { db: h.d } { count() });
  Fiber::join(a)
}

pub fn main() -> Int { with_lease(go) }
",
            "`h` cannot be handed to another fiber",
        ),
        (
            "another effect's handler",
            "effect Tally { total: () -> Int }

fn tally() -> Tally with { db: Db } {
  handler for Tally { total: fn () => count() }
}

pub fn main() -> Int { 0 }
",
            "`Tally`'s `total` captures `db`",
        ),
    ] {
        let errors = errors_with_std(&program(body));
        let found = refusals(&errors, about);
        if !found.iter().any(|e| e.message.contains("take a lease in the spawned fiber")) {
            missed.push(format!("{route}: expected a refusal about {about} naming the lease, got {errors:#?}"));
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
}

/// **A certified closure is not a spawn**, and the refusal says which it
/// is: `SharedFn::of`'s closure may be called on any fiber, so it cannot
/// hold its maker's `db` either, but there is no spawn to point at.
#[test]
fn a_certified_closure_cannot_take_db_and_is_not_called_a_spawn() {
    let text = program(
        "import std::core::{SharedFn};

fn certify() -> SharedFn<Int, Int, Never> with { db: Db } {
  SharedFn::of(fn n => n + count())
}

pub fn main() -> Int { 0 }
",
    );
    let errors = errors_with_std(&text);
    let found = refusals(&errors, "`db` cannot be handed to another fiber");
    assert_eq!(found.len(), 1, "expected one refusal of `db`, got {errors:#?}");
    let message = &found[0].message;
    assert!(
        !message.contains("the fiber spawned at"),
        "a certified closure is not a spawn: {message}"
    );
    let certified = span_of(&text, "SharedFn::of(fn n", 0).0;
    let line = line_of(&text, certified);
    assert!(
        message.contains(&format!("the closure certified at line {line}, column 3")),
        "{message}"
    );
    assert_eq!(span(found[0]), span_of(&text, "count()", 1), "the caret should be on the use");
}

/// **A module's own type called `Db` is held to the rule**, because it is
/// matched by name, and the refusal says so rather than leave its author
/// wondering why their effect cannot cross.
#[test]
fn a_type_of_ones_own_called_db_is_told_why() {
    let text = "module main;

import std::core::{Fiber};

effect Db { get: () -> Int }

fn go() -> Int with { db: Db } {
  let a = Fiber::spawn(fn () => db.get());
  Fiber::join(a)
}

pub fn main() -> Int {
  with { db: handler for Db { get: fn () => 1 } } { go() }
}
";
    let errors = errors_with_std(text);
    let found = refusals(&errors, "`db` cannot be handed to another fiber");
    assert_eq!(found.len(), 1, "expected one refusal of `db`, got {errors:#?}");
    assert!(
        found[0].message.contains("std's `Db` -- which the compiler knows by name"),
        "the refusal should say the rule is by name: {}",
        found[0].message
    );
}
