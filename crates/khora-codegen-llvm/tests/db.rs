#![cfg(feature = "llvm")]

//! The `Db` capability's contract, compiled and run.
//!
//! There is no engine here and that is the point. `ecosystem.md` decided that
//! the engine is a package and what `std` owns is the shape and **what a
//! transaction does when its body does not return normally** — the part that
//! fails in production, never in testing, and that every package would
//! otherwise answer differently.
//!
//! So these run against a handler that records what it was asked to do. That
//! is a stronger test of the contract than any database would be: it can say
//! *rollback happened and commit did not*, which is the whole claim.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn run(name: &str, body: &str) -> String {
    run_with(name, "", body)
}

/// [`run`], with extra items above `main`.
///
/// A test that needs a fiber needs something to spawn, and a thunk is not an
/// item — so the ones that reach for cancellation write functions of their own
/// here rather than everything being squeezed into `main`.
fn run_with(name: &str, items: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, Fiber, List, Option, Result, Show, attempt, print}};
import std::db::{{Cell, Db, DbError, Row, transaction}};

/// A handler that says what it was told to do, as it is told.
///
/// **The printed order is the record.** No cell to hold a log in, no state to
/// get wrong, and the thing being asserted — that `rollback` happened and
/// `commit` did not — is visible in the transcript rather than decoded from a
/// number.
fn recording(fails: Bool) -> Db {{
  handler for Db {{
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) => {{
      print("execute");
      Result::Ok(1)
    }},
    begin: fn () => {{
      print("begin");
      Result::Ok(())
    }},
    commit: fn () => {{
      print("commit");
      if fails {{ Result::Err(DbError::Rejected("no")) }} else {{ Result::Ok(()) }}
    }},
    rollback: fn () => {{
      print("rollback");
      Result::Ok(())
    }},
  }}
}}

{items}

fn main() -> () {{
{body}
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("decimal.kh"), std_source("decimal.kh")),
        SourceFile::new(&db, dir.join("db.kh"), std_source("db.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// A body that returns commits, and does not roll back.
#[test]
fn a_body_that_succeeds_commits() {
    let out = run(
        "db_commit",
        r#"  with { db: recording(false) } {
    let answer = transaction(fn () => {
      db.execute("insert", List::Nil);
      Result::Ok(7)
    });
    match answer {
      Result::Ok(value) => print(Int::to_string(value)),
      Result::Err(problem) => print(problem.show()),
    }
  }"#,
    );
    assert_eq!(out, "begin\nexecute\ncommit\n7\n");
}

/// **The case the whole module exists for.** A body that fails rolls back, and
/// never commits.
#[test]
fn a_body_that_fails_rolls_back_and_does_not_commit() {
    let out = run(
        "db_rollback",
        r#"  with { db: recording(false) } {
    // Annotated because the body only ever fails, so nothing says what `A` is.
    let answer: Result<Int, DbError> = transaction(fn () => {
      db.execute("insert", List::Nil);
      Result::Err(DbError::Rejected("the invariant did not hold"))
    });
    match answer {
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }
  }"#,
    );
    assert_eq!(
        out,
        "begin\nexecute\nrollback\nrolled back: rejected: the invariant did not hold\n",
        "a failed body must roll back, and the reason must survive"
    );
}

/// A commit that is refused is reported as itself, not disguised.
#[test]
fn a_refused_commit_is_reported() {
    let out = run(
        "db_commit_fails",
        r#"  with { db: recording(true) } {
    let answer = transaction(fn () => Result::Ok(1));
    match answer {
      Result::Ok(_) => print("succeeded, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }
  }"#,
    );
    assert_eq!(out, "begin\ncommit\nrejected: no\n");
}

/// Cells do not coerce: a number read as text is `None`, because a schema
/// misunderstanding should be visible rather than rendered.
#[test]
fn cells_do_not_coerce() {
    let out = run(
        "db_cells",
        r#"  let number = Cell::Number(42);
  print(match Cell::text(number) { Option::Some(t) => t, Option::None => "not text" });
  print(match Cell::number(number) { Option::Some(n) => Int::to_string(n), Option::None => "?" });
  print(Cell::is_null(Cell::Null).show());
  print(Cell::is_null(number).show());
  print(number.show());"#,
    );
    assert_eq!(out, "not text\n42\ntrue\nfalse\n42\n");
}

// --- the third way out ------------------------------------------------------

/// Items for the cancellation tests: something to fail with, something to
/// fail at, and the runtime's own `khora_cancel`.
///
/// **The fiber cancels itself**, which `tests/fibers.rs` explains at greater
/// length: a parent that cancels immediately after spawning wins the race and
/// the child stops at its first mark, which is correct and proves less. Here
/// the interesting moment is precisely "after `begin`, before `commit`", and
/// that is a moment only the child can name.
const CANCELLABLE: &str = r#"extern fn khora_cancel();

pub type Oops = | Bad;

/// A fallible call, so that `!` marks a cancellation point. It never fails;
/// the `!` is the whole of its job.
fn mark() -> Int raises Oops { 1 }
"#;

/// **The half 13.3 named.** A fiber cancelled inside a transaction rolls back,
/// and does not commit.
///
/// The cancellation never touches a line of `transaction`: it travels out of
/// the body on a tagged return, and what runs the rollback is the region
/// ending — the same mechanism that would have run it if the body had raised,
/// reached by a path the source of `transaction` does not mention.
#[test]
fn a_cancelled_fiber_rolls_back_and_does_not_commit() {
    let out = run_with(
        "db_cancelled",
        &format!(
            r#"{CANCELLABLE}
fn worker() -> () raises Oops {{
  with {{ db: recording(false) }} {{
    transaction(fn () => {{
      db.execute("insert", List::Nil);
      khora_cancel();
      mark()!;
      print("the body carried on, which is wrong");
      Result::Ok(1)
    }})!;
    print("the transaction returned, which is wrong");
  }}
}}
"#
        ),
        r#"  let f = Fiber::spawn(fn () => worker()!);
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
  print("the parent carried on");"#,
    );
    assert_eq!(
        out,
        "begin\nexecute\nrollback\nthe parent carried on\n",
        "a cancelled transaction must roll back, and must not commit"
    );
}

/// The finalizer does not fire twice, and the ordinary path is unchanged: a
/// body that commits has nothing left for the region to undo.
///
/// Worth its own test because "roll back unless settled" is a flag, and a flag
/// read on the wrong side of the commit would send a `ROLLBACK` after every
/// successful transaction — which no engine would refuse and every reader
/// would eventually notice.
#[test]
fn a_committed_transaction_does_not_roll_back_on_the_way_out() {
    let out = run_with(
        "db_commit_settles",
        &format!(
            r#"{CANCELLABLE}
fn worker() -> () raises Oops {{
  with {{ db: recording(false) }} {{
    match transaction(fn () => {{
      mark()!;
      Result::Ok(7)
    }})! {{
      Result::Ok(value) => print(Int::to_string(value)),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  let f = Fiber::spawn(fn () => worker()!);
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);"#,
    );
    assert_eq!(out, "begin\ncommit\n7\n", "no rollback after a commit");
}

/// A body that fails rolls back exactly once, not once for the `match` and
/// again for the region.
#[test]
fn a_failed_body_rolls_back_exactly_once() {
    let out = run_with(
        "db_rollback_once",
        &format!(
            r#"{CANCELLABLE}
fn worker() -> () raises Oops {{
  with {{ db: recording(false) }} {{
    let answer: Result<Int, DbError> = transaction(fn () => {{
      mark()!;
      Result::Err(DbError::Rejected("no"))
    }})!;
    match answer {{
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  let f = Fiber::spawn(fn () => worker()!);
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);"#,
    );
    assert_eq!(out, "begin\nrollback\nrolled back: rejected: no\n");
}

/// **The rollback's own work is not cancellable.** A real `rollback` sends a
/// statement and reads a reply, and every `!` on that path is a cancellation
/// point that would find the flag still set — so a rollback caused by a
/// cancellation would be interrupted by the same cancellation, before it
/// reached the server.
///
/// The handler here stands in for that: it does fallible work before saying it
/// rolled back. Without `cancel::Shielded` in the runtime, the `!` inside
/// `attempt` fires, `rollback` never prints, and the connection goes back to
/// the pool inside an open transaction.
#[test]
fn a_rollback_may_do_fallible_work_while_the_cancellation_waits() {
    let out = run_with(
        "db_rollback_shielded",
        &format!(
            r#"{CANCELLABLE}
/// Like `recording`, but its rollback has a cancellation point in it.
fn talkative() -> Db {{
  handler for Db {{
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) => Result::Ok(1),
    begin: fn () => {{ print("begin"); Result::Ok(()) }},
    commit: fn () => {{ print("commit"); Result::Ok(()) }},
    rollback: fn () => {{
      // Two statements and a mark between them: if the cancellation were
      // observed here, the second would not run.
      print("rolling back");
      match attempt(fn () => mark()!) {{
        Result::Ok(_) => print("rolled back"),
        Result::Err(_) => print("the rollback failed"),
      }};
      Result::Ok(())
    }},
  }}
}}

fn worker() -> () raises Oops {{
  with {{ db: talkative() }} {{
    transaction(fn () => {{
      khora_cancel();
      mark()!;
      Result::Ok(1)
    }})!;
    print("the transaction returned, which is wrong");
  }}
}}
"#
        ),
        r#"  let f = Fiber::spawn(fn () => worker()!);
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
  print("the parent carried on");"#,
    );
    assert_eq!(
        out,
        "begin\nrolling back\nrolled back\nthe parent carried on\n",
        "a finalizer must run to its end even though the fiber is stopping"
    );
}
