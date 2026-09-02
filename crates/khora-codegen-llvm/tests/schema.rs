#![cfg(feature = "llvm")]

//! `std::schema`, against the real standard library.
//!
//! The claims worth a test each are the ones that are silent when they are
//! wrong: every problem is reported rather than the first, a path says which
//! field, a decimal survives exactly, and a secret never reaches the message.
//! Roadmap #141, `docs/design/schema.md`.

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

fn run(name: &str, main: &str) -> String {
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
    let out = Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "{name} exited badly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

const HEAD: &str = "module main;

import std::core::{List, Option, Pair, Redacted, Result, Show, Validated, print};
import std::decimal::{Decimal};
import std::schema::{Rejection, Schema, Shape, Raw, bool, decimal, int, many, optional, refine,
                     secret, string, struct2, struct3};

pub type Listen = { host: String, port: Int };

fn listen(host: String, port: Int) -> Listen { { host: host, port: port } }

fn rec(entries: List<Pair<String, Raw>>) -> Raw { Raw::Record(entries) }

fn field(key: String, value: Raw) -> Pair<String, Raw> { { key: key, value: value } }

fn problems<A>(v: Validated<A, Rejection>) -> String {
  match Validated::to_result(v) {
    Result::Ok(_a) => \"no problems\",
    Result::Err(errors) =>
      List::fold(errors, \"\", fn (acc, e) => acc + Rejection::describe(e) + \"; \"),
  }
}

fn port() -> Schema<Int> {
  refine(int(), \"between 1 and 65535\", fn p => p > 0 && p < 65536)
}

fn listen_schema() -> Schema<Listen> {
  struct2(\"host\", string(), \"port\", port(), listen)
}

// Khora has no anonymous record type, so each shape a test combines into gets
// a name and a constructor -- which is also what the combinator wants as its
// assembler, so it reads better than a lambda would.
pub type Named_ = { listen: Listen, name: String };
fn named_(listen: Listen, name: String) -> Named_ { { listen: listen, name: name } }

pub type Money = { rate: Decimal, big: Int };
fn money(rate: Decimal, big: Int) -> Money { { rate: rate, big: big } }

pub type Tokens = { public: Int, token: Redacted<Int> };
fn tokens(public: Int, token: Redacted<Int>) -> Tokens { { public: public, token: token } }

pub type Words = { count: Int, phrase: Redacted<String> };
fn words(count: Int, phrase: Redacted<String>) -> Words { { count: count, phrase: phrase } }

pub type Settings = { listen: Listen, password: Redacted<String>, debug: Option<Bool> };
fn settings(listen: Listen, password: Redacted<String>, debug: Option<Bool>) -> Settings {
  { listen: listen, password: password, debug: debug }
}
";

/// A record decodes, and a nested one keeps its shape.
#[test]
fn a_record_decodes() {
    let out = run(
        "schema_decode",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let input = rec([field(\"host\", Raw::Text(\"localhost\")),
                   field(\"port\", Raw::Number(\"8080\"))]);
  match Validated::to_result(Schema::decode(listen_schema(), input)) {{
    Result::Ok(l) => print(\"${{l.host}}:${{l.port}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(out, "localhost:8080\n");
}

/// **Every problem, not the first.** A person fixing a deployment wants the
/// list; reporting one bad key at a time turns one edit into three rounds.
/// This is the property `std::config` already had and the reason `decode`
/// answers a `Validated` rather than raising.
#[test]
fn every_problem_is_reported_with_its_path() {
    let out = run(
        "schema_all_problems",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let input = rec([field(\"port\", Raw::Number(\"99999\"))]);
  print(problems(Schema::decode(listen_schema(), input)));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "host is not set; port must be between 1 and 65535; \n",
        "both, and the refinement's own sentence"
    );
}

/// A nested path reads the way somebody would write it.
#[test]
fn a_nested_path_is_written_the_way_it_is_read() {
    let out = run(
        "schema_nested_path",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let outer = struct2(\"listen\", listen_schema(), \"name\", string(), named_);
  let input = rec([field(\"listen\", rec([field(\"host\", Raw::Text(\"h\"))])),
                   field(\"name\", Raw::Text(\"svc\"))]);
  print(problems(Schema::decode(outer, input)));

  let items = many(port());
  let list = Raw::Sequence([Raw::Number(\"80\"), Raw::Number(\"0\")]);
  print(problems(Schema::decode(items, list)));
  0
}}
"
        ),
    );
    assert_eq!(out, "listen.port is not set; \n[1] must be between 1 and 65535; \n");
}

/// **A decimal survives exactly**, which is the reason `Raw::Number` keeps the
/// token's text rather than holding a `Float`. A price read through a double
/// is the wrong price, and `std::json` is where that goes unnoticed -- #142.
#[test]
fn a_decimal_is_exact() {
    let out = run(
        "schema_exact",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let input = rec([field(\"rate\", Raw::Number(\"0.0725\")),
                   field(\"big\", Raw::Number(\"9007199254740993\"))]);
  let s = struct2(\"rate\", decimal(), \"big\", int(), money);
  match Validated::to_result(Schema::decode(s, input)) {{
    Result::Ok(v) => print(\"${{v.rate}} ${{v.big}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "0.0725 9007199254740993\n",
        "the decimal keeps its scale and the integer keeps its last digit"
    );
}

/// **A secret never reaches the message.**
///
/// A decode error quotes what it found, which is most of what makes one worth
/// reading -- and quoting a password is the easiest imaginable way to put one
/// in a log. The wrapper is unconditional on `Problem::Wrong` so no future
/// variant can forget it, and only `describe` decides whether to expose it.
#[test]
fn a_secret_is_never_quoted_in_an_error() {
    let out = run(
        "schema_secret",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let s = struct2(\"public\", int(), \"token\", secret(int()), tokens);
  let input = rec([field(\"public\", Raw::Text(\"not a number\")),
                   field(\"token\", Raw::Text(\"s3cr3t-value\"))]);
  print(problems(Schema::decode(s, input)));
  0
}}
"
        ),
    );
    assert!(!out.contains("s3cr3t-value"), "the secret must not be in the message: {out:?}");
    assert!(out.contains("not a number"), "and an ordinary field still says what it saw: {out:?}");
    assert_eq!(
        out,
        "public should be a whole number, and is `not a number`; \
         token should be a whole number; \n"
    );
}

/// And a secret that decodes shows as nothing, the way `Redacted` does.
#[test]
fn a_secret_that_decodes_still_hides() {
    let out = run(
        "schema_secret_ok",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let s = struct2(\"public\", int(), \"token\", secret(string()), words);
  let input = rec([field(\"public\", Raw::Number(\"1\")),
                   field(\"token\", Raw::Text(\"s3cr3t-value\"))]);
  match Validated::to_result(Schema::decode(s, input)) {{
    Result::Ok(v) => print(\"${{v.count}} ${{v.phrase}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(out, "1 <redacted>\n");
    assert!(!out.contains("s3cr3t"), "{out:?}");
}

/// **The keys a configuration needs, without starting the program.**
///
/// The question a deployment asks, and the reason a schema carries an untyped
/// `Shape` beside its closure rather than being a closure alone.
#[test]
fn the_shape_answers_which_keys_are_needed() {
    let out = run(
        "schema_keys",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let s = struct3(\"listen\", listen_schema(), \"password\", secret(string()), \"debug\",
                  optional(bool()), settings);
  print(\"${{Shape::keys(s.shape)}}\");
  print(\"${{Shape::keys(listen_schema().shape)}}\");
  0
}}
"
        ),
    );
    assert_eq!(out, "[listen, password, debug]\n[host, port]\n");
}

/// Absent is `None`; present and wrong is still an error.
///
/// The distinction that matters: a misspelled setting must not read as one
/// nobody set.
#[test]
fn optional_tells_absent_from_wrong() {
    let out = run(
        "schema_optional",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let s = optional(int());
  match Validated::to_result(Schema::decode(s, Raw::Absent)) {{
    Result::Ok(v) => print(\"absent gives ${{v}}\"),
    Result::Err(_e) => print(\"absent refused\"),
  }};
  print(problems(Schema::decode(s, Raw::Text(\"eighty\"))));
  0
}}
"
        ),
    );
    assert_eq!(out, "absent gives None\nthe value should be a whole number, and is `eighty`; \n");
}
