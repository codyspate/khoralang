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
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, List, Option, Result, Show, print}};
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
        r#"  let db = recording(false);
  let answer = transaction(db, fn () => {
    db.execute("insert", List::Nil);
    Result::Ok(7)
  });
  match answer {
    Result::Ok(value) => print(Int::to_string(value)),
    Result::Err(problem) => print(problem.show()),
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
        r#"  let db = recording(false);
  // Annotated because the body only ever fails, so nothing says what `A` is.
  let answer: Result<Int, DbError> = transaction(db, fn () => {
    db.execute("insert", List::Nil);
    Result::Err(DbError::Rejected("the invariant did not hold"))
  });
  match answer {
    Result::Ok(_) => print("committed, which is wrong"),
    Result::Err(problem) => print(problem.show()),
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
        r#"  let db = recording(true);
  let answer = transaction(db, fn () => Result::Ok(1));
  match answer {
    Result::Ok(_) => print("succeeded, which is wrong"),
    Result::Err(problem) => print(problem.show()),
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
