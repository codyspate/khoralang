#![cfg(feature = "llvm")]

//! `Redacted`, `Validated`, and the two `List` instances they needed.
//!
//! All three are ports of ideas from Effect-TS, and all three came out
//! different in Khora because the type system can hold something TypeScript's
//! cannot. `Redacted` there is a `toString` override, which a structured
//! logger walks straight past; here it is a missing `Encode` impl, which is a
//! build failure. That difference is the thing worth testing, so these tests
//! check the *absence* of an instance as carefully as the presence of one.

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

const HEAD: &str = "module demo::main;
import std::core::{Eq, List, Redacted, Result, Show, Validated, print};
";

fn program(name: &str, body: &str) -> (PathBuf, KhoraDatabase, SourceRoot) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    // `std::schema` and what it imports, so that a derived schema is what a
    // program here derives rather than a name it cannot find.
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("json.kh"), std_source("json.kh")),
        SourceFile::new(&db, dir.join("decimal.kh"), std_source("decimal.kh")),
        SourceFile::new(&db, dir.join("time.kh"), std_source("time.kh")),
        SourceFile::new(&db, dir.join("schema.kh"), std_source("schema.kh")),
        SourceFile::new(&db, dir.join("main.kh"), format!("{HEAD}\n{body}\n")),
    ];
    let root = SourceRoot::new(&db, files);
    (exe, db, root)
}

/// Compiles `body` as a whole module and gives back what it printed.
fn run(name: &str, body: &str) -> String {
    let (exe, db, root) = program(name, body);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// [`run`], for a body that is supposed to be *refused*.
fn refused(name: &str, body: &str) -> Vec<String> {
    let (exe, db, root) = program(name, body);
    khora_codegen_llvm::compile(&db, root, &exe)
        .err()
        .unwrap_or_else(|| panic!("`{name}` compiled, and it should not have"))
        .into_iter()
        .map(|e| e.message)
        .collect()
}

/// **A secret shows as `<redacted>`, inside anything.**
///
/// The second half is the part that matters. A `Redacted` on its own is easy
/// to be careful with; the leak people actually ship is a config record in a
/// structured log, and the whole reason this type implements `Show` at all is
/// so that record still derives one.
#[test]
fn a_secret_prints_as_nothing_even_inside_a_record() {
    let out = run(
        "redacted_show",
        r#"derive(Show)
type Config = {
  host: String,
  password: Redacted<String>,
};

fn main() -> () {
  let key = Redacted::of("hunter2");
  print(key.show());
  let config: Config = { host: "db.internal", password: key };
  print(config.show());
}"#,
    );

    assert_eq!(
        out,
        "<redacted>\nConfig { host: db.internal, password: <redacted> }\n"
    );
    assert!(!out.contains("hunter2"), "the secret reached stdout: {out}");
}

/// The value is still reachable, by a word a reviewer can grep for.
#[test]
fn exposing_a_secret_gives_it_back() {
    let out = run(
        "redacted_expose",
        r#"fn main() -> () {
  print(Redacted::expose(Redacted::of("hunter2")));
}"#,
    );
    assert_eq!(out, "hunter2\n");
}

/// **The build stops rather than serialising a secret.**
///
/// This is the test that says why `Redacted` is a type and not a convention.
/// `derive(Encode)` walks the fields, finds no instance, and refuses — so a
/// record that would have put a password in a response body cannot be written
/// by accident, and the person who meant it writes `expose` instead. The same
/// record derives `Decode`, because `Redacted` decodes through `secret`: it
/// reads and does not write, which is the reason encoding is a trait of its
/// own and not a second half of the schema.
#[test]
fn a_record_holding_a_secret_derives_decode_and_refuses_encode() {
    let messages = refused(
        "redacted_encode",
        r#"import std::schema::{Decode, Encode};

derive(Decode, Encode)
type Leak = {
  password: Redacted<String>,
};

fn main() -> () { print("unreachable"); }"#,
    );

    assert!(
        messages.iter().any(|m| m.contains("derive(Encode)") && m.contains("Redacted")),
        "expected the derive to name the field it could not write, got {messages:?}"
    );
    assert!(
        !messages.iter().any(|m| m.contains("derive(Decode)")),
        "and the schema to derive without complaint: {messages:?}"
    );
}

/// **Every reason, not the first one** — the whole of what `Validated` is for.
#[test]
fn validation_collects_both_sides_failures() {
    let out = run(
        "validated_both",
        r#"fn field(name: String, ok: Bool) -> Validated<String, String> {
  if ok { Validated::of(name) } else { Validated::error("missing ${name}") }
}

fn main() -> () {
  print(Validated::map2(
    field("HOST", false),
    field("PORT", false),
    fn (a, b) => a + b,
  ).show());

  print(Validated::map2(
    field("HOST", true),
    field("PORT", false),
    fn (a, b) => a + b,
  ).show());

  print(Validated::map2(
    field("HOST", true),
    field("PORT", true),
    fn (a, b) => a + "/" + b,
  ).show());
}"#,
    );

    assert_eq!(
        out,
        "Validated::Invalid([missing HOST, missing PORT])\n\
         Validated::Invalid([missing PORT])\n\
         Validated::Valid(HOST/PORT)\n"
    );
}

/// The combining function does not run when anything failed, and the failures
/// arrive at a `raises` boundary as a list rather than as the first one.
#[test]
fn a_failed_validation_neither_runs_nor_forgets() {
    let out = run(
        "validated_to_result",
        r#"fn nothing(why: String) -> Validated<Int, String> { Validated::error(why) }

fn main() -> () {
  let failed = Validated::map2(
    nothing("no host"),
    nothing("no port"),
    fn (_a, _b) => 99,
  );
  print(Validated::unwrap_or(failed, 0).show());
  print(match Validated::to_result(failed) {
    Result::Ok(_v) => "ok",
    Result::Err(errors) => errors.show(),
  });
}"#,
    );

    assert_eq!(out, "0\n[no host, no port]\n");
}

/// **`List` gained `Show` and `Eq` because `Validated` needed them**, and the
/// reason it needed them generalises: a record holding a `List` could not
/// derive `Show`, so the container people reach for by default was the one
/// that made a struct unprintable.
#[test]
fn a_list_shows_as_the_literal_that_builds_it() {
    let out = run(
        "list_show",
        r#"derive(Show)
type Batch = {
  ids: List<Int>,
};

fn empty() -> List<Int> { List::Nil }

fn main() -> () {
  print([1, 2, 3].show());
  print(empty().show());
  let batch: Batch = { ids: [7] };
  print(batch.show());
  print([1, 2].eq([1, 2]).show());
  print([1, 2].eq([1, 2, 3]).show());
  print([1, 2].eq([1, 3]).show());
}"#,
    );

    assert_eq!(out, "[1, 2, 3]\n[]\nBatch { ids: [7] }\ntrue\nfalse\nfalse\n");
}
