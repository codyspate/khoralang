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
import std::core::{{Eq, Fiber, List, Option, Result, Show, Validated, acquire, attempt, print, scoped}};
import std::db::{{Cell, Db, DbError, Row, transaction}};
import std::decimal::{{Decimal}};
import std::schema::{{Decode, Raw, Rejection, list}};

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
    broken: fn () => print("broken"),
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
        // A row is read through a schema, so `std::schema` and what it
        // imports come too.
        SourceFile::new(&db, dir.join("json.kh"), std_source("json.kh")),
        SourceFile::new(&db, dir.join("time.kh"), std_source("time.kh")),
        SourceFile::new(&db, dir.join("schema.kh"), std_source("schema.kh")),
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

/// A handler whose `rollback` refuses, for the two tests about what that costs.
///
/// Separate from `recording` rather than a second flag on it, because a flag
/// that is `false` in eight call sites and `true` in two reads as a handler
/// with a mode and is really two handlers.
const BRITTLE: &str = r#"
/// Records, and refuses to roll back.
fn brittle() -> Db {
  handler for Db {
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) => Result::Ok(1),
    begin: fn () => {
      print("begin");
      Result::Ok(())
    },
    commit: fn () => {
      print("commit");
      Result::Ok(())
    },
    rollback: fn () => {
      print("rollback refused");
      Result::Err(DbError::Rejected("the rollback failed too"))
    },
    broken: fn () => print("broken"),
  }
}
"#;

/// **A rollback that fails does not hide the reason it was needed.**
///
/// The policy is one line of `std::db` — `let _ = db.rollback()` — and the
/// argument for it is in the comment above that line: a caller who sees
/// `RolledBack` knows the transaction did not commit, which is the fact they
/// have to act on, and the engine's complaint about the rollback is a second
/// problem and a worse thing to report.
///
/// It is a deliberate discard, so it is worth a test: the failure that
/// surfaces must be the body's, and swapping the two would be a one-character
/// change that no other test here would notice.
#[test]
fn a_failing_rollback_does_not_hide_the_reason_for_it() {
    let out = run_with(
        "db_rollback_fails",
        &format!(
            r#"{CANCELLABLE}{BRITTLE}
fn worker() -> () raises Oops {{
  with {{ db: brittle() }} {{
    let answer: Result<Int, DbError> = transaction(fn () => {{
      mark()!;
      Result::Err(DbError::Rejected("the body failed"))
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
  Fiber::wait(f);"#,
    );
    assert!(
        out.contains("the body failed"),
        "the body's reason must survive a rollback that also failed, got: {out:?}"
    );
    assert!(
        !out.contains("the rollback failed too"),
        "the rollback's own complaint must not be what the caller is told, got: {out:?}"
    );
}

/// **On the cancellation path a failed rollback reaches the handler.**
///
/// The error path has somebody to tell: it returns `RolledBack` and the caller
/// reads it. A cancelled fiber has no caller waiting for an answer, so the
/// failure used to go nowhere — and the connection went back to a pool having
/// neither committed nor, as far as anything knew, rolled back.
///
/// `broken` is where it goes now. The handler *is* the connection and is the
/// only thing left that can act on it; `packages/postgres` closes the request
/// channel, which ends the serving fiber and shuts the socket, so the next
/// borrower is answered `Disconnected` rather than handed somebody else's
/// uncommitted rows.
///
/// The transcript is the assertion: rollback attempted, rollback refused,
/// handler told, and the parent carrying on.
#[test]
fn a_failed_rollback_during_cancellation_tells_the_handler() {
    let out = run_with(
        "db_rollback_fails_cancelled",
        &format!(
            r#"{CANCELLABLE}{BRITTLE}
fn worker() -> () raises Oops {{
  with {{ db: brittle() }} {{
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
  Fiber::wait(f);
  print("the parent carried on");"#,
    );
    assert_eq!(
        out, "begin\nrollback refused\nbroken\nthe parent carried on\n",
        "a rollback that failed has to reach the handler as `broken`"
    );
}

/// **A cancelled lease comes back to a connection that is not mid-transaction.**
///
/// This is the composition `packages/postgres` is built on and neither half
/// tested: `with_db` registers the lease's return with a region it opens, and
/// `transaction` registers its rollback with a region of its own, nested
/// inside. The pool's correctness is entirely the claim that the inner
/// finalizer runs first.
///
/// If it did not, a cancelled fiber would put a connection back in the idle
/// channel with an open transaction on it, and the next borrower would inherit
/// somebody else's uncommitted rows and locks. No engine reports that; it
/// looks like the second query being wrong.
///
/// So the assertion is the order, not the presence: `rollback` before the
/// lease goes back, on the cancellation path.
#[test]
fn a_cancelled_lease_is_returned_only_after_the_rollback() {
    let out = run_with(
        "db_lease_ordering",
        &format!(
            r#"{CANCELLABLE}
fn worker() -> () raises Oops {{
  with {{ db: recording(false) }} {{
    // The two regions `with_db` and `transaction` open, in the order the pool
    // opens them.
    scoped(fn () => {{
      acquire("connection", fn _back => print("lease returned"));
      transaction(fn () => {{
        db.execute("insert", List::Nil);
        khora_cancel();
        mark()!;
        Result::Ok(1)
      }})!;
      print("the transaction returned, which is wrong");
    }})!
  }}
}}
"#
        ),
        r#"  let f = Fiber::spawn(fn () => worker()!);
  Fiber::wait(f);
  print("the parent carried on");"#,
    );
    assert_eq!(
        out,
        "begin\nexecute\nrollback\nlease returned\nthe parent carried on\n",
        "a pooled connection must not go back holding an open transaction"
    );
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
    broken: fn () => print("broken"),
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

/// **A row is read through a schema, by column name.** `Row::sequence` puts
/// every row's problems on one report with the row's index in the path, so a
/// query whose second row has drifted from the type says so rather than
/// dropping it; a `Money` cell survives as the exact decimal it was.
#[test]
fn a_row_reads_through_its_column_names() {
    let out = run_with(
        "db_row_schema",
        r#"derive(Show, Decode)
pub type Entry = { id: Int, memo: String, amount: Decimal, paid: Bool };

fn money(text: String) -> Cell {
  match Decimal::of_string(text) {
    Option::Some(d) => Cell::Money(d),
    Option::None => Cell::Null,
  }
}

fn row(cells: List<Cell>) -> Row {
  { columns: ["id", "memo", "amount", "paid"], cells: cells }
}

fn shown(answer: Validated<List<Entry>, Rejection>) -> String {
  match answer {
    Validated::Valid(entries) =>
      List::fold(entries, "", fn (acc, e) => acc + "${e.id} ${e.memo} ${e.amount} ${e.paid}; "),
    Validated::Invalid(problems) => Rejection::report(problems),
  }
}"#,
        r#"let good = row([Cell::Number(7), Cell::Text("x"), money("1.50"), Cell::Flag(true)]);
  let bad = row([Cell::Text("no"), Cell::Null, money("2.00"), Cell::Flag(false)]);
  print(shown(list(Entry::schema()).decode(Row::sequence(List::Cons(good, List::Nil)))));
  print(shown(list(Entry::schema()).decode(Row::sequence(List::Cons(good, List::Cons(bad, List::Nil))))));
  match Row::named(good, "memo") {
    Option::Some(Cell::Text(text)) => print(text),
    _ => print("no memo"),
  };
  let nameless: Row = { columns: List::Nil, cells: [Cell::Number(1)] };
  print(Show::show(Row::to_raw(nameless)));"#,
    );

    assert_eq!(
        out,
        "7 x 1.50 true; \n\
         [1].id should be a whole number, and is \"no\"\n[1].memo should be text, and is null\n\
         x\n\
         Raw::Sequence([Raw::Number(1)])\n"
    );
}
