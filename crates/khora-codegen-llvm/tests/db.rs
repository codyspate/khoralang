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

use crate::harness;

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
import std::core::{{Eq, Fiber, List, Option, Result, Share, Shared, Show, Validated, acquire, attempt, print, scoped}};
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
  let depth = Shared::of(0);
  handler for Db {{
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) => {{
      print("execute");
      Result::Ok(1)
    }},
    depth: fn () => Shared::get(depth),
    begin: fn () => {{
      print("begin");
      Shared::set(depth, 1);
      Result::Ok(())
    }},
    commit: fn () => {{
      print("commit");
      Shared::set(depth, 0);
      if fails {{ Result::Err(DbError::Rejected("no")) }} else {{ Result::Ok(()) }}
    }},
    rollback: fn () => {{
      print("rollback");
      Shared::set(depth, 0);
      Result::Ok(())
    }},
    savepoint: fn level => {{
      print("savepoint " + Int::to_string(level));
      Shared::set(depth, level + 1);
      Result::Ok(())
    }},
    release: fn level => {{
      print("release " + Int::to_string(level));
      Shared::set(depth, level);
      Result::Ok(())
    }},
    rollback_to: fn level => {{
      if Shared::get(depth) <= level {{
        print("rollback to " + Int::to_string(level) + ", never opened")
      }} else {{
        print("rollback to " + Int::to_string(level));
        Shared::set(depth, level)
      }};
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
const CANCELABLE: &str = r#"extern fn khora_cancel();

pub type Oops = | Bad;

/// A fallible call, so that `!` marks a cancellation point. It never fails;
/// the `!` is the whole of its job.
fn mark() -> Int raises Oops { 1 }
"#;

/// **The half 13.3 named.** A fiber canceled inside a transaction rolls back,
/// and does not commit.
///
/// The cancellation never touches a line of `transaction`: it travels out of
/// the body on a tagged return, and what runs the rollback is the region
/// ending — the same mechanism that would have run it if the body had raised,
/// reached by a path the source of `transaction` does not mention.
#[test]
fn a_canceled_fiber_rolls_back_and_does_not_commit() {
    let out = run_with(
        "db_canceled",
        &format!(
            r#"{CANCELABLE}
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
  // canceled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f)! catch { Oops::Bad => () };
  print("the parent carried on");"#,
    );
    assert_eq!(
        out,
        "begin\nexecute\nrollback\nthe parent carried on\n",
        "a canceled transaction must roll back, and must not commit"
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
            r#"{CANCELABLE}
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
  // canceled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f)! catch { Oops::Bad => () };"#,
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
    depth: fn () => 0,
    savepoint: fn _level => Result::Err(DbError::Rejected("one level only")),
    release: fn _level => Result::Err(DbError::Rejected("one level only")),
    rollback_to: fn _level => Result::Ok(()),
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
            r#"{CANCELABLE}{BRITTLE}
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
  Fiber::wait(f)! catch { Oops::Bad => () };"#,
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
/// reads it. A canceled fiber has no caller waiting for an answer, so the
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
        "db_rollback_fails_canceled",
        &format!(
            r#"{CANCELABLE}{BRITTLE}
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
  Fiber::wait(f)! catch { Oops::Bad => () };
  print("the parent carried on");"#,
    );
    assert_eq!(
        out, "begin\nrollback refused\nbroken\nthe parent carried on\n",
        "a rollback that failed has to reach the handler as `broken`"
    );
}

/// **A canceled lease comes back to a connection that is not mid-transaction.**
///
/// This is the composition `packages/postgres` is built on and neither half
/// tested: `with_db` registers the lease's return with a region it opens, and
/// `transaction` registers its rollback with a region of its own, nested
/// inside. The pool's correctness is entirely the claim that the inner
/// finalizer runs first.
///
/// If it did not, a canceled fiber would put a connection back in the idle
/// channel with an open transaction on it, and the next borrower would inherit
/// somebody else's uncommitted rows and locks. No engine reports that; it
/// looks like the second query being wrong.
///
/// So the assertion is the order, not the presence: `rollback` before the
/// lease goes back, on the cancellation path.
#[test]
fn a_canceled_lease_is_returned_only_after_the_rollback() {
    let out = run_with(
        "db_lease_ordering",
        &format!(
            r#"{CANCELABLE}
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
  Fiber::wait(f)! catch { Oops::Bad => () };
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
            r#"{CANCELABLE}
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
  // canceled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f)! catch { Oops::Bad => () };"#,
    );
    assert_eq!(out, "begin\nrollback\nrolled back: rejected: no\n");
}

/// **The rollback's own work is not cancelable.** A real `rollback` sends a
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
            r#"{CANCELABLE}
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
    depth: fn () => 0,
    savepoint: fn _level => Result::Err(DbError::Rejected("one level only")),
    release: fn _level => Result::Err(DbError::Rejected("one level only")),
    rollback_to: fn _level => Result::Ok(()),
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
  // canceled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f)! catch { Oops::Bad => () };
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
pub type Entry = { pub id: Int, pub memo: String, pub amount: Decimal, pub paid: Bool };

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

// ---------------------------------------------------------------------------
// The connection goes away
// ---------------------------------------------------------------------------
//
// **Network loss is not a statement being refused, and the difference is the
// pool.** An engine that rejects a statement has answered: the transaction can
// be rolled back, the connection is fine, and handing it back is right. A
// connection that *died* has answered nothing -- what was sent may or may not
// have arrived -- so the connection is unusable and the next borrower must not
// be given it.
//
// `broken` is how the handler is told, and these pin which failures reach it.
// Testing this found that a commit failing with `Disconnected` did not: it was
// treated exactly like a commit the engine refused, so a connection lost
// mid-commit went back to the pool as healthy.

/// A handler whose connection dies at a chosen point.
///
/// `where_it_dies` picks the operation that reports `Disconnected`; everything
/// else works. `rollback` fails too whenever the connection is already gone,
/// which is what a real one does -- there is no socket left to send it on.
const DYING: &str = r#"
fn dying(where_it_dies: String) -> Db {
  let gone = Shared::of(false);
  handler for Db {
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) =>
      if where_it_dies.eq("execute") {
        Shared::set(gone, true);
        print("execute lost the connection");
        Result::Err(DbError::Disconnected("the server went away"))
      } else {
        Result::Ok(1)
      },
    begin: fn () =>
      if where_it_dies.eq("begin") {
        Shared::set(gone, true);
        print("begin lost the connection");
        Result::Err(DbError::Disconnected("the server went away"))
      } else {
        print("begin");
        Result::Ok(())
      },
    commit: fn () =>
      if where_it_dies.eq("commit") {
        Shared::set(gone, true);
        print("commit lost the connection");
        Result::Err(DbError::Disconnected("the server went away"))
      } else if where_it_dies.eq("refuse-commit") {
        print("commit refused");
        Result::Err(DbError::Rejected("a deferred constraint"))
      } else {
        print("commit");
        Result::Ok(())
      },
    rollback: fn () =>
      if Shared::get(gone) {
        print("rollback had no connection");
        Result::Err(DbError::Disconnected("the server went away"))
      } else {
        print("rollback");
        Result::Ok(())
      },
    broken: fn () => print("the handler was told the connection is broken"),
    depth: fn () => 0,
    savepoint: fn _level => Result::Err(DbError::Rejected("one level only")),
    release: fn _level => Result::Err(DbError::Rejected("one level only")),
    rollback_to: fn _level => Result::Ok(()),
  }
}
"#;

/// **A connection lost mid-body: rolled back as far as anything can be, and
/// the handler told.**
///
/// The rollback cannot succeed either -- there is nothing to send it on -- so
/// this is the case where the transaction's fate is genuinely unknown, and the
/// only correct thing left is to stop anybody reusing the connection.
#[test]
fn a_connection_lost_during_the_body_tells_the_handler() {
    let out = run_with(
        "db_lost_body",
        &format!(
            r#"{DYING}
fn worker() -> () {{
  with {{ db: dying("execute") }} {{
    let answer: Result<Int, DbError> = transaction(fn () =>
      match db.execute("insert", List::Nil) {{
        Result::Err(problem) => Result::Err(problem),
        Result::Ok(_) => Result::Ok(1),
      }});
    match answer {{
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  worker();"#,
    );
    assert!(out.contains("execute lost the connection"), "the body should have failed: {out:?}");
    assert!(out.contains("rollback had no connection"), "a rollback should be tried: {out:?}");
    assert!(
        out.contains("the handler was told the connection is broken"),
        "a rollback that could not be sent leaves the state unknown, and the handler has to be \
         told so the connection is not reused: {out:?}"
    );
    assert!(!out.contains("committed, which is wrong"), "nothing should commit: {out:?}");
    assert!(
        out.contains("the server went away"),
        "the caller should be told the reason, not just that something failed: {out:?}"
    );
}

/// **A connection lost during the commit tells the handler.**
///
/// The one this found. It used to be indistinguishable from a commit the
/// engine refused: `Result::Err(problem) => Result::Err(problem)` and nothing
/// else, so the connection went back to the pool with a `COMMIT` that may or
/// may not have been executed on the other end of a dead socket.
#[test]
fn a_connection_lost_during_the_commit_tells_the_handler() {
    let out = run_with(
        "db_lost_commit",
        &format!(
            r#"{DYING}
fn worker() -> () {{
  with {{ db: dying("commit") }} {{
    let answer: Result<Int, DbError> = transaction(fn () => Result::Ok(1));
    match answer {{
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  worker();"#,
    );
    assert!(out.contains("commit lost the connection"), "the commit should have failed: {out:?}");
    assert!(
        out.contains("the handler was told the connection is broken"),
        "a commit that never reached the server leaves the transaction's outcome unknown: {out:?}"
    );
    assert!(!out.contains("committed, which is wrong"), "nothing committed: {out:?}");
}

/// **A commit the engine *refused* does not tell the handler**, and this is
/// the guard against the fix above being "always call `broken`".
///
/// A deferred constraint violation is an answer. The transaction is over, the
/// connection is healthy, and throwing it away on every failed commit would
/// empty a pool under exactly the load that needs one.
#[test]
fn a_refused_commit_leaves_the_connection_alone() {
    let out = run_with(
        "db_refused_commit",
        &format!(
            r#"{DYING}
fn worker() -> () {{
  with {{ db: dying("refuse-commit") }} {{
    let answer: Result<Int, DbError> = transaction(fn () => Result::Ok(1));
    match answer {{
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  worker();"#,
    );
    assert!(out.contains("commit refused"), "the commit should have been refused: {out:?}");
    assert!(
        !out.contains("the handler was told the connection is broken"),
        "the engine answered, so the connection is fine and must stay in the pool: {out:?}"
    );
    assert!(out.contains("a deferred constraint"), "the reason should reach the caller: {out:?}");
}

/// **A connection already gone when the transaction begins tells the handler.**
///
/// The same argument at the other end: a `BEGIN` that could not be sent leaves
/// a connection nothing else should borrow.
#[test]
fn a_connection_lost_at_begin_tells_the_handler() {
    let out = run_with(
        "db_lost_begin",
        &format!(
            r#"{DYING}
fn worker() -> () {{
  with {{ db: dying("begin") }} {{
    let answer: Result<Int, DbError> = transaction(fn () => Result::Ok(1));
    match answer {{
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  worker();"#,
    );
    assert!(out.contains("begin lost the connection"), "begin should have failed: {out:?}");
    assert!(
        out.contains("the handler was told the connection is broken"),
        "a connection that could not begin a transaction is not one to hand on: {out:?}"
    );
    assert!(!out.contains("rollback"), "there is no transaction to roll back: {out:?}");
}

// ---------------------------------------------------------------------------
// A cancellation between `BEGIN` and the rollback's registration
// ---------------------------------------------------------------------------
//
// **What these prevent: a connection handed back to a pool inside an open
// transaction, where the next borrower's autocommit writes are answered `Ok`
// and never committed.** Every point between `BEGIN` reaching the server and
// the rollback's registration is a place a cancel unwinds with the server
// inside a transaction and nothing left to end it. Against a real server a
// rollback registered after `begin` returned left about half of 300 cancels
// that way, and lost writes answered `Ok`.
//
// The handler here is canceled inside the operation it is asked to perform,
// after saying it was asked: the `BEGIN` has reached the server and the fiber
// stops while it waits for the reply. That is the widest form of the gap, and
// no later registration point can close it -- only one before `begin` can.

/// A handler canceled inside the operation named `at`.
///
/// `turn` is a loop that goes round twice, so its back edge is a cancellation
/// point in any function; a cancel set just before it is taken there.
const CANCELED_AT: &str = r#"extern fn khora_cancel();

fn turn() -> () {
  let mut i = 0;
  while i < 2 { i = i + 1 };
}

fn canceled_in(at: String) -> Db {
  handler for Db {
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (_sql, _binds) => Result::Ok(1),
    begin: fn () => {
      print("begin");
      if at.eq("begin") { khora_cancel(); turn() } else {};
      Result::Ok(())
    },
    commit: fn () => {
      print("commit");
      if at.eq("commit") { khora_cancel(); turn() } else {};
      Result::Ok(())
    },
    rollback: fn () => {
      print("rollback sent");
      if at.eq("rollback") { khora_cancel(); turn() } else {};
      print("rollback answered");
      Result::Ok(())
    },
    broken: fn () => print("broken"),
    depth: fn () => 0,
    savepoint: fn _level => Result::Err(DbError::Rejected("one level only")),
    release: fn _level => Result::Err(DbError::Rejected("one level only")),
    rollback_to: fn _level => Result::Ok(()),
  }
}

fn worker(at: String, fails: Bool) -> () {
  with { db: canceled_in(at) } {
    let _answer: Result<Int, DbError> = transaction(fn () =>
      if fails { Result::Err(DbError::Rejected("no")) } else { Result::Ok(1) });
    ()
  }
}
"#;

fn canceled_at(name: &str, at: &str, fails: bool) -> String {
    run_with(
        name,
        CANCELED_AT,
        &format!(
            "  let f = Fiber::spawn(fn () => worker(\"{at}\", {fails}));\n  \
             Fiber::wait(f);\n  print(\"the parent carried on\");"
        ),
    )
}

/// **A cancel that arrives while `BEGIN` waits for its reply rolls back.**
///
/// The server has the `BEGIN`; the fiber has not been told. Without a
/// rollback the connection goes back to its pool inside the transaction.
#[test]
fn a_cancel_while_begin_waits_for_its_reply_rolls_back() {
    let out = canceled_at("db_cancel_in_begin", "begin", false);
    assert_eq!(
        out, "begin\nrollback sent\nrollback answered\nthe parent carried on\n",
        "the `BEGIN` reached the server, so a cancel before its reply must still roll back"
    );
}

/// **A cancel that arrives while `COMMIT` waits for its reply rolls back as
/// well**, so the connection is never handed on with the transaction's state
/// in doubt: either the `COMMIT` ran and the `ROLLBACK` is a harmless no-op,
/// or it did not and the `ROLLBACK` ends the transaction.
#[test]
fn a_cancel_while_commit_waits_for_its_reply_rolls_back() {
    let out = canceled_at("db_cancel_in_commit", "commit", false);
    assert_eq!(
        out, "begin\ncommit\nrollback sent\nrollback answered\nthe parent carried on\n",
        "a commit cut short by a cancel must be followed by a rollback"
    );
}

/// **The rollback after a failed body is cleanup, and a cancel does not cut
/// it short.** What this prevents: a cancel arriving while that `ROLLBACK`
/// waits for its reply, stopping the fiber with the transaction's end
/// unheard and the connection on its way back to a pool.
#[test]
fn a_cancel_during_the_rollback_of_a_failed_body_does_not_stop_it() {
    let out = canceled_at("db_cancel_in_rollback", "rollback", true);
    assert_eq!(
        out, "begin\nrollback sent\nrollback answered\nthe parent carried on\n",
        "the rollback of a failed body must run to its end"
    );
}

/// **A commit whose connection was lost says the outcome is unknown.** The
/// `COMMIT` may have run on the server before the connection went, so a
/// caller told only "disconnected" could reasonably retry a transaction that
/// is already committed.
#[test]
fn a_commit_lost_with_its_connection_says_the_outcome_is_unknown() {
    let out = run_with(
        "db_lost_commit_unknown",
        &format!(
            r#"{DYING}
fn worker() -> () {{
  with {{ db: dying("commit") }} {{
    let answer: Result<Int, DbError> = transaction(fn () => Result::Ok(1));
    match answer {{
      Result::Ok(_) => print("committed, which is wrong"),
      Result::Err(problem) => print(problem.show()),
    }}
  }}
}}
"#
        ),
        r#"  worker();"#,
    );
    assert!(
        out.contains("disconnected: the connection was lost during the commit, so it is not known whether the transaction committed: the server went away"),
        "the caller must be told the commit's outcome is unknown, got: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// A transaction inside a transaction
// ---------------------------------------------------------------------------
//
// **What these prevent: an inner transaction committing or rolling back the
// enclosing one.** An inner `BEGIN` is only a warning to PostgreSQL, so the
// inner `COMMIT` committed the outer body's writes and the inner `ROLLBACK`
// undid them: the caller was told `Err` with every row committed, or `Ok`
// with only some. The transcript is the assertion: which level each verb
// was sent for, in order.

/// A body run at one level inside another, for the nesting tests.
const NESTED: &str = r#"extern fn khora_cancel();

pub type Oops = | Bad;

fn mark() -> Int raises Oops { 1 }

fn told(r: Result<Int, DbError>) -> String {
  match r { Result::Ok(n) => "Ok " + Int::to_string(n), Result::Err(p) => "Err " + p.show() }
}

fn inner(fails: Bool) -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    db.execute("inner", List::Nil);
    if fails { Result::Err(DbError::Rejected("inner")) } else { Result::Ok(2) }
  })
}

fn outer(inner_fails: Bool, outer_fails: Bool) -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    db.execute("outer", List::Nil);
    print("inner said " + told(inner(inner_fails)));
    if outer_fails { Result::Err(DbError::Rejected("outer")) } else { Result::Ok(1) }
  })
}
"#;

fn nested(name: &str, inner_fails: bool, outer_fails: bool) -> String {
    run_with(
        name,
        NESTED,
        &format!(
            "  with {{ db: recording(false) }} {{\n    print(\"outer said \" + told(outer({inner_fails}, {outer_fails})));\n  }}"
        ),
    )
}

/// **An inner `Ok` keeps its writes for the enclosing transaction to commit**,
/// and commits nothing itself.
#[test]
fn an_inner_transaction_that_succeeds_is_released_not_committed() {
    assert_eq!(
        nested("db_nested_ok_ok", false, false),
        "begin\nexecute\nsavepoint 1\nexecute\nrelease 1\ninner said Ok 2\ncommit\nouter said Ok 1\n"
    );
}

/// **An inner `Err` undoes only the inner writes**, and the enclosing body
/// carries on and commits its own.
#[test]
fn an_inner_transaction_that_fails_undoes_only_itself() {
    assert_eq!(
        nested("db_nested_err_ok", true, false),
        "begin\nexecute\nsavepoint 1\nexecute\nrollback to 1\ninner said Err rolled back: rejected: inner\ncommit\nouter said Ok 1\n"
    );
}

/// **An enclosing failure takes the released inner writes with it**: the
/// only commit is the outermost one, and it is never sent.
#[test]
fn an_outer_failure_rolls_back_an_inner_success() {
    assert_eq!(
        nested("db_nested_ok_err", false, true),
        "begin\nexecute\nsavepoint 1\nexecute\nrelease 1\ninner said Ok 2\nrollback\nouter said Err rolled back: rejected: outer\n"
    );
}

/// **Each level is named by its depth**, so the middle of three undoes its own
/// savepoint, which holds the innermost one's released writes, and nothing
/// of the outermost.
#[test]
fn the_middle_of_three_levels_undoes_itself_and_what_it_holds() {
    let out = run_with(
        "db_nested_three",
        NESTED,
        r#"  with { db: recording(false) } {
    let answer = transaction(fn () => {
      let middle: Result<Int, DbError> = transaction(fn () => {
        print("innermost said " + told(inner(false)));
        Result::Err(DbError::Rejected("middle"))
      });
      print("middle said " + told(middle));
      Result::Ok(1)
    });
    print("outermost said " + told(answer));
  }"#,
    );
    assert_eq!(
        out,
        "begin\nsavepoint 1\nsavepoint 2\nexecute\nrelease 2\ninnermost said Ok 2\n\
         rollback to 1\nmiddle said Err rolled back: rejected: middle\ncommit\noutermost said Ok 1\n"
    );
}

/// **A cancel in an inner body undoes the inner savepoint, then the enclosing
/// transaction, in that order.** Inner regions end first, so no acknowledged
/// write survives a rollback it belonged to.
#[test]
fn a_cancel_in_an_inner_body_undoes_inner_then_outer() {
    let out = run_with(
        "db_nested_cancel",
        NESTED,
        r#"  let f = Fiber::spawn(fn () => {
    with { db: recording(false) } {
      transaction(fn () => {
        db.execute("outer", List::Nil);
        transaction(fn () => {
          db.execute("inner", List::Nil);
          khora_cancel();
          mark()!;
          print("the inner body carried on, which is wrong");
          Result::Ok(2)
        })!;
        print("the outer body carried on, which is wrong");
        Result::Ok(1)
      })!;
      ()
    }
  });
  Fiber::wait(f)! catch { Oops::Bad => () };
  print("the parent carried on");"#,
    );
    assert_eq!(
        out,
        "begin\nexecute\nsavepoint 1\nexecute\nrollback to 1\nrollback\nthe parent carried on\n"
    );
}

/// **A cancel while `SAVEPOINT` waits for its reply rolls back the enclosing
/// transaction and sends nothing for the savepoint that was never opened.**
/// The inner undo is registered before `SAVEPOINT` goes out, so it runs for a
/// level the connection never reached, and PostgreSQL answers a
/// `ROLLBACK TO` for an unknown savepoint with an error that aborts the
/// enclosing transaction.
#[test]
fn a_cancel_before_the_savepoint_opened_is_harmless() {
    let out = run_with(
        "db_nested_cancel_in_savepoint",
        r#"extern fn khora_cancel();

fn turn() -> () {
  let mut i = 0;
  while i < 2 { i = i + 1 };
}

fn stalls() -> Db {
  let depth = Shared::of(0);
  let base = recording(false);
  handler for Db {
    query: fn (sql, binds) => base.query(sql, binds),
    execute: fn (sql, binds) => base.execute(sql, binds),
    depth: fn () => Shared::get(depth),
    begin: fn () => { Shared::set(depth, 1); base.begin() },
    commit: fn () => { Shared::set(depth, 0); base.commit() },
    rollback: fn () => { Shared::set(depth, 0); base.rollback() },
    savepoint: fn level => {
      print("savepoint " + Int::to_string(level) + " sent");
      khora_cancel();
      turn();
      Shared::set(depth, level + 1);
      Result::Ok(())
    },
    release: fn level => { Shared::set(depth, level); base.release(level) },
    rollback_to: fn level =>
      if Shared::get(depth) <= level {
        print("rollback to " + Int::to_string(level) + ", never opened");
        Result::Ok(())
      } else {
        Shared::set(depth, level);
        print("rollback to " + Int::to_string(level));
        Result::Ok(())
      },
    broken: fn () => base.broken(),
  }
}

fn worker() -> () {
  with { db: stalls() } {
    let _answer: Result<Int, DbError> = transaction(fn () => {
      db.execute("outer", List::Nil);
      let _inner: Result<Int, DbError> = transaction(fn () => Result::Ok(2));
      Result::Ok(1)
    });
    ()
  }
}
"#,
        r#"  let f = Fiber::spawn(fn () => worker());
  Fiber::wait(f);
  print("the parent carried on");"#,
    );
    assert_eq!(
        out,
        "begin\nexecute\nsavepoint 1 sent\nrollback to 1, never opened\nrollback\nthe parent carried on\n"
    );
}

/// **A handler that does not support nesting refuses it; it never opens a
/// second transaction.** What this prevents: the migration a handler author
/// reaches for first, answering `depth` with 0 always, which makes a nested
/// `transaction` send a second `BEGIN`. On PostgreSQL that is only a warning,
/// so the inner `ROLLBACK` ends the outer transaction, the outer body's
/// earlier writes are lost and its later ones autocommit, and the caller is
/// told `Ok`. Answering `depth` truthfully and refusing `savepoint` turns the
/// nested `transaction` into that refusal, and the outer one carries on.
#[test]
fn a_handler_without_nesting_refuses_it_instead_of_beginning_twice() {
    let out = run_with(
        "db_flat_handler",
        r#"/// Supports one level only, and says so.
fn flat() -> Db {
  let depth = Shared::of(0);
  handler for Db {
    query: fn (_sql, _binds) => Result::Ok(List::Nil),
    execute: fn (sql, _binds) => { print("execute " + sql); Result::Ok(1) },
    depth: fn () => Shared::get(depth),
    begin: fn () => { print("begin"); Shared::set(depth, 1); Result::Ok(()) },
    commit: fn () => { print("commit"); Shared::set(depth, 0); Result::Ok(()) },
    rollback: fn () => { print("rollback"); Shared::set(depth, 0); Result::Ok(()) },
    savepoint: fn _level => Result::Err(DbError::Rejected("this handler does not nest")),
    release: fn _level => Result::Err(DbError::Rejected("this handler does not nest")),
    rollback_to: fn _level => Result::Ok(()),
    broken: fn () => print("broken"),
  }
}

fn told(r: Result<Int, DbError>) -> String {
  match r { Result::Ok(n) => "Ok " + Int::to_string(n), Result::Err(p) => "Err " + p.show() }
}"#,
        r#"  with { db: flat() } {
    let outer = transaction(fn () => {
      db.execute("1", List::Nil);
      let inner: Result<Int, DbError> = transaction(fn () => {
        db.execute("2", List::Nil);
        Result::Ok(2)
      });
      print("inner said " + told(inner));
      db.execute("3", List::Nil);
      Result::Ok(1)
    });
    print("outer said " + told(outer));
  }"#,
    );
    assert_eq!(
        out,
        "begin\nexecute 1\ninner said Err rejected: this handler does not nest\nexecute 3\ncommit\nouter said Ok 1\n",
        "the nested transaction must be refused, with one `begin` and no second one"
    );
}
