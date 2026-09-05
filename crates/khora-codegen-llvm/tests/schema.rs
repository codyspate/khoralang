#![cfg(feature = "llvm")]

//! `std::schema`, against the real standard library.
//!
//! The claims worth a test each are the ones that are silent when they are
//! wrong: every problem is reported rather than the first, a path says which
//! field, a decimal survives exactly, and a secret never reaches the message.
//! Roadmap #141, `docs/design/schema.md`.

use crate::harness;

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
import std::schema::{Rejection, Schema, Shape, Raw, bool, decimal, int, list, optional, refine,
                     secret, string, struct};

pub type Listen = { host: String, port: Int };

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
  struct({ host: string(), port: port() })
}

// Khora has no anonymous record type, so each shape a test combines into gets
// a name; the literal is checked against it, and needs no assembler.
pub type Named_ = { listen: Listen, name: String };
pub type Money = { rate: Decimal, big: Int };
pub type Tokens = { public: Int, token: Redacted<Int> };
pub type Words = { count: Int, phrase: Redacted<String> };
pub type Settings = { listen: Listen, password: Redacted<String>, debug: Option<Bool> };
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
  let outer: Schema<Named_> = struct({{ listen: listen_schema(), name: string() }});
  let input = rec([field(\"listen\", rec([field(\"host\", Raw::Text(\"h\"))])),
                   field(\"name\", Raw::Text(\"svc\"))]);
  print(problems(Schema::decode(outer, input)));

  let items = list(port());
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
  let s: Schema<Money> = struct({{ rate: decimal(), big: int() }});
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
  let s: Schema<Tokens> = struct({{ public: int(), token: secret(int()) }});
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
        "public should be a whole number, and is \"not a number\"; \
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
  let s: Schema<Words> = struct({{ count: int(), phrase: secret(string()) }});
  let input = rec([field(\"count\", Raw::Number(\"1\")),
                   field(\"phrase\", Raw::Text(\"s3cr3t-value\"))]);
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
  let s: Schema<Settings> =
    struct({{ listen: listen_schema(), password: secret(string()), debug: optional(bool()) }});
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
    assert_eq!(out, "absent gives None\nthe value should be a whole number, and is \"eighty\"; \n");
}

/// The wider import for the tests of the second version's surface, kept apart
/// from `HEAD` so the eight above stay word for word what they were.
const HEAD2: &str = "module main;

import std::core::{Dict, List, Option, Pair, Redacted, Result, Show, Validated, print};
import std::decimal::{Decimal};
import std::json::{encode, parse};
import std::schema::{Case, Fields, Rejection, Schema, Shape, Raw, any, between, bool, decimal,
                     default, dict, int, key, list, min_length, non_empty, nullable, one_of,
                     optional, refine, renamed, secret, string, struct};

derive(Show)
pub type Listen = { host: String, port: Int };

pub type Words = { count: Int, phrase: Redacted<String> };

derive(Show)
pub type Mode = | Local | Remote(url: String);

fn mode() -> Schema<Mode> {
  Schema::cases([
    Schema::case(\"Local\", Fields::none(), fn _u => Mode::Local),
    Schema::case(\"Remote\", Fields::of(\"url\", string()), fn u => Mode::Remote(u)),
  ])
}

pub type Tree = { label: String, children: List<Tree> };

fn tree() -> Schema<Tree> {
  Schema::lazy(\"Tree\", fn () => struct({ label: string(), children: list(tree()) }))
}

fn depth(t: Tree) -> Int {
  List::fold(t.children, 0, fn (deepest, child) => {
    let d = depth(child);
    if d > deepest { d } else { deepest }
  }) + 1
}

fn rec(entries: List<Pair<String, Raw>>) -> Raw { Raw::Record(entries) }

fn field(key: String, value: Raw) -> Pair<String, Raw> { { key: key, value: value } }

fn problems<A>(v: Validated<A, Rejection>) -> String {
  match Validated::to_result(v) {
    Result::Ok(_a) => \"no problems\",
    Result::Err(errors) =>
      List::fold(errors, \"\", fn (acc, e) => acc + Rejection::describe(e) + \"; \"),
  }
}

fn plain() -> Schema<Listen> { struct({ host: string(), port: int() }) }

fn shown(v: Validated<Listen, Rejection>) -> String {
  match Validated::to_result(v) {
    Result::Ok(l) => \"${l.host}:${l.port}\",
    Result::Err(errors) =>
      List::fold(errors, \"\", fn (acc, e) => acc + Rejection::describe(e) + \"; \"),
  }
}
";

/// **A primitive is strict where the source could label, and reads `Untyped`
/// where it could not.** A JSON body sending `"8080"` for a port is refused,
/// and the message says the value arrived as text by quoting it; the same
/// text from the environment is read. `null` is not `Absent`.
#[test]
fn strictness_follows_the_source() {
    let out = run(
        "schema_strict",
        &format!(
            "{HEAD2}
fn main() -> Int {{
  print(shown(Schema::decode(plain(), rec([field(\"host\", Raw::Number(\"1\")), field(\"port\", Raw::Text(\"8080\"))]))));
  print(shown(Schema::decode(plain(), rec([field(\"host\", Raw::Untyped(\"h\")), field(\"port\", Raw::Untyped(\"8080\"))]))));
  print(shown(Schema::decode(plain(), rec([field(\"host\", Raw::Null), field(\"port\", Raw::Number(\"1\"))]))));
  print(problems(Schema::decode(nullable(int()), Raw::Absent)));
  match Validated::to_result(Schema::decode(optional(int()), Raw::Null)) {{
    Result::Ok(v) => print(\"${{v}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  print(problems(Schema::decode(bool(), Raw::Untyped(\"yes\"))));
  print(problems(Schema::decode(decimal(), Raw::Text(\"0.10\"))));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "host should be text, and is 1; port should be a whole number, and is \"8080\"; \n\
         h:8080\n\
         host should be text, and is null; \n\
         the value is not set; \n\
         None\n\
         the value should be true or false, and is \"yes\"; \n\
         no problems\n"
    );
}

/// A wire key that is not the field's name, a default that fires on absence
/// only, a closed record, and a renamed key that reports the name the client
/// sent.
#[test]
fn keys_defaults_and_closed_records() {
    let out = run(
        "schema_wire_keys",
        &format!(
            "{HEAD2}
fn main() -> Int {{
  let s: Schema<Listen> = struct({{ host: key(\"Host\", string()), port: default(int(), 8080) }});
  print(shown(Schema::decode(s, rec([field(\"Host\", Raw::Text(\"h\"))]))));
  print(shown(Schema::decode(s, rec([field(\"Host\", Raw::Text(\"h\")), field(\"port\", Raw::Null)]))));
  print(shown(Schema::decode(Schema::closed(s), rec([field(\"Host\", Raw::Text(\"h\")), field(\"verbose\", Raw::Bool(true))]))));
  print(\"${{Shape::keys(s.shape)}}\");
  let r = renamed(plain(), \"hostname\", \"host\");
  print(shown(Schema::decode(r, rec([field(\"hostname\", Raw::Number(\"1\")), field(\"port\", Raw::Number(\"1\"))]))));
  print(shown(Schema::decode(r, rec([field(\"hostname\", Raw::Text(\"h\")), field(\"port\", Raw::Number(\"1\"))]))));
  print(\"${{Shape::keys(r.shape)}}\");
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "h:8080\n\
         port should be a whole number, and is null; \n\
         verbose is not expected; \n\
         [Host, port]\n\
         hostname should be text, and is 1; \n\
         h:1\n\
         [hostname, port]\n"
    );
}

/// **A payload-free case is a bare string and a payload case is an object
/// tagged with `type`**, each decided by the case alone; every way of getting
/// it wrong has its own sentence.
#[test]
fn a_variant_reads_a_bare_string_or_a_tagged_record() {
    let out = run(
        "schema_cases",
        &format!(
            "{HEAD2}
fn show_mode(v: Validated<Mode, Rejection>) -> String {{
  match Validated::to_result(v) {{
    Result::Ok(m) => Show::show(m),
    Result::Err(errors) =>
      List::fold(errors, \"\", fn (acc, e) => acc + Rejection::describe(e) + \"; \"),
  }}
}}
fn main() -> Int {{
  print(show_mode(Schema::decode(mode(), Raw::Text(\"Local\"))));
  print(show_mode(Schema::decode(mode(), rec([field(\"type\", Raw::Text(\"Remote\")), field(\"url\", Raw::Text(\"x\"))]))));
  print(show_mode(Schema::decode(mode(), rec([field(\"type\", Raw::Text(\"Cloud\"))]))));
  print(show_mode(Schema::decode(mode(), Raw::Text(\"Remote\"))));
  print(show_mode(Schema::decode(mode(), rec([]))));
  print(show_mode(Schema::decode(mode(), Raw::Number(\"7\"))));
  print(show_mode(Schema::decode(mode(), rec([field(\"type\", Raw::Text(\"Remote\"))]))));
  print(\"${{mode().shape}}\");
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "Mode::Local\n\
         Mode::Remote(x)\n\
         type should be one of `Local`, `Remote`, and is \"Cloud\"; \n\
         the value should be a record, and is \"Remote\"; \n\
         type is not set; \n\
         the value should be one of `Local`, `Remote`, and is 7; \n\
         url is not set; \n\
         Cases(Local, Remote)\n"
    );
}

/// A named rule carries its bounds, and the sentence is built from them.
#[test]
fn rules_carry_their_sentences() {
    let out = run(
        "schema_rules",
        &format!(
            "{HEAD2}
fn main() -> Int {{
  print(problems(Schema::decode(between(int(), 1, 10), Raw::Number(\"11\"))));
  print(problems(Schema::decode(between(int(), 1, 10), Raw::Number(\"10\"))));
  print(problems(Schema::decode(min_length(string(), 3), Raw::Text(\"ab\"))));
  print(problems(Schema::decode(one_of(string(), [\"gzip\", \"br\"]), Raw::Text(\"zstd\"))));
  print(problems(Schema::decode(non_empty(list(int())), Raw::Sequence([]))));
  let colour = Schema::try_map(string(), \"a colour\", fn s => if s == \"red\" {{ Option::Some(1) }} else {{ Option::None }});
  print(problems(Schema::decode(colour, Raw::Text(\"blue\"))));
  print(problems(Schema::decode(refine(int(), \"even\", fn n => n % 2 == 0), Raw::Number(\"3\"))));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "the value must be between 1 and 10; \n\
         no problems\n\
         the value must be at least 3 characters; \n\
         the value must be one of `gzip`, `br`; \n\
         the value must have at least 1 item; \n\
         the value should be a colour, and is \"blue\"; \n\
         the value must be even; \n"
    );
}

/// **A JSON document and the command line are two sources of one tree.** The
/// document round-trips through `Raw`, and flags become fields.
#[test]
fn json_and_arguments_are_sources() {
    let out = run(
        "schema_sources",
        &format!(
            "{HEAD2}
fn main() -> Int {{
  match parse(`{{\"host\": \"h\", \"port\": 8080, \"tags\": [\"a\", null], \"none\": null}}`) {{
    Result::Ok(document) => {{
      let raw = Raw::of_json(document);
      print(shown(Schema::decode(plain(), raw)));
      print(encode(Raw::to_json(raw)));
    }},
    Result::Err(_e) => print(\"not json\"),
  }};
  let args = Raw::of_arguments([\"in.txt\", \"--host\", \"h\", \"--port=8080\", \"--log-level\", \"debug\", \"--verbose\"]);
  print(shown(Schema::decode(plain(), args)));
  print(Show::show(Raw::field(args, \"log_level\")));
  print(Show::show(Raw::field(args, \"verbose\")));
  print(Show::show(Raw::field(args, \"arguments\")));
  match Validated::to_result(Schema::decode(dict(int()), rec([field(\"a\", Raw::Number(\"1\")), field(\"b\", Raw::Number(\"2\"))]))) {{
    Result::Ok(d) => print(\"${{Dict::size(d)}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  print(problems(Schema::decode(dict(int()), rec([field(\"a\", Raw::Number(\"1\")), field(\"b\", Raw::Text(\"x\"))]))));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "h:8080\n\
         {\"host\":\"h\",\"none\":null,\"port\":8080,\"tags\":[\"a\",null]}\n\
         h:8080\n\
         Raw::Untyped(debug)\n\
         Raw::Bool(true)\n\
         Raw::Sequence([Raw::Untyped(in.txt)])\n\
         2\n\
         b should be a whole number, and is \"x\"; \n"
    );
}

/// A denied value is its own problem, a report is one line per problem, and
/// a type that mentions itself decodes through `Schema::lazy`.
#[test]
fn denied_reports_and_recursion() {
    let out = run(
        "schema_denied",
        &format!(
            "{HEAD2}
fn main() -> Int {{
  let s: Schema<Words> = struct({{ count: int(), phrase: secret(string()) }});
  match Validated::to_result(Schema::decode(s, rec([field(\"count\", Raw::Denied), field(\"phrase\", Raw::Denied)]))) {{
    Result::Ok(_w) => print(\"decoded\"),
    Result::Err(errors) => print(Rejection::report(errors)),
  }};
  print(problems(Schema::decode(optional(int()), Raw::Denied)));
  let leaf = fn label => rec([field(\"label\", Raw::Text(label)), field(\"children\", Raw::Sequence([]))]);
  let input = rec([field(\"label\", Raw::Text(\"root\")),
                   field(\"children\", Raw::Sequence([leaf(\"a\"), rec([field(\"label\", Raw::Text(\"b\")), field(\"children\", Raw::Sequence([leaf(\"c\")]))])]))]);
  match Validated::to_result(Schema::decode(tree(), input)) {{
    Result::Ok(t) => print(\"depth ${{depth(t)}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  print(problems(Schema::decode(tree(), rec([field(\"label\", Raw::Text(\"root\")), field(\"children\", Raw::Sequence([rec([field(\"label\", Raw::Number(\"1\"))])]))]))));
  print(\"${{Shape::keys(tree().shape)}}\");
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "count is not granted\nphrase is not granted\n\
         the value is not granted; \n\
         depth 3\n\
         children[0].label should be text, and is 1; children[0].children is not set; \n\
         [label, children]\n"
    );
}

/// A program that reaches its schemas through the traits.
const HEAD3: &str = "module main;

import std::core::{List, Option, Pair, Redacted, Result, Show, Validated, print};
import std::decimal::{Decimal};
import std::json::{encode};
import std::schema::{Decode, Encode, Rejection, Schema, Shape, Raw, between, decode, int, list,
                     string, struct};
import std::time::{Date};

derive(Show)
pub type Port = Int;

impl Decode for Port {
  fn schema() -> Schema<Port> { Schema::map(between(int(), 1, 65535), fn n => Port(n)) }
}

impl Encode for Port {
  fn encode(self) -> Raw { match self { Port(n) => n.encode() } }
}

pub type Listen = { host: String, port: Port };

impl Decode for Listen {
  fn schema() -> Schema<Listen> { struct({ host: string(), port: Port::schema() }) }
}

impl Encode for Listen {
  fn encode(self) -> Raw {
    Raw::Record([entry(\"host\", self.host.encode()), entry(\"port\", self.port.encode())])
  }
}

fn entry(key: String, value: Raw) -> Pair<String, Raw> { { key: key, value: value } }

fn rec(entries: List<Pair<String, Raw>>) -> Raw { Raw::Record(entries) }

fn problems<A>(v: Validated<A, Rejection>) -> String {
  match Validated::to_result(v) {
    Result::Ok(_a) => \"no problems\",
    Result::Err(errors) =>
      List::fold(errors, \"\", fn (acc, e) => acc + Rejection::describe(e) + \"; \"),
  }
}
";

/// **The schema is reached through the type.** `Decode::schema()` is chosen
/// by an annotation, `Listen::schema()` by name, and `decode(raw)` by the
/// type it is asked for; a hand-written newtype's refinement is picked up by
/// every schema that contains it. `Encode` writes the same values back, and
/// a list of rejections is a body a client can read.
#[test]
fn the_traits_are_selected_by_the_expected_type() {
    let out = run(
        "schema_traits",
        &format!(
            "{HEAD3}
fn main() -> Int {{
  let maybe: Schema<Option<Int>> = Decode::schema();
  match Validated::to_result(Schema::decode(maybe, Raw::Null)) {{
    Result::Ok(v) => print(\"${{v}}\"),
    Result::Err(_e) => print(\"refused\"),
  }};
  print(\"${{Shape::keys(Listen::schema().shape)}}\");
  let ok: Validated<Listen, Rejection> =
    decode(rec([entry(\"host\", Raw::Text(\"h\")), entry(\"port\", Raw::Number(\"8080\"))]));
  match Validated::to_result(ok) {{
    Result::Ok(l) => {{
      print(\"${{l.host}} ${{l.port}}\");
      print(encode(Raw::to_json(l.encode())));
    }},
    Result::Err(errors) => print(Rejection::report(errors)),
  }};
  let bad: Validated<Listen, Rejection> =
    decode(rec([entry(\"host\", Raw::Text(\"h\")), entry(\"port\", Raw::Number(\"70000\"))]));
  match Validated::to_result(bad) {{
    Result::Ok(_l) => print(\"decoded\"),
    Result::Err(errors) => {{
      print(Rejection::report(errors));
      print(encode(Raw::to_json(errors.encode())));
    }},
  }};
  let ports: Validated<List<Port>, Rejection> = decode(Raw::Sequence([Raw::Number(\"1\"), Raw::Number(\"0\")]));
  print(problems(ports));
  let day: Validated<Date, Rejection> = decode(Raw::Text(\"2026-09-02\"));
  match Validated::to_result(day) {{
    Result::Ok(d) => print(encode(Raw::to_json(d.encode()))),
    Result::Err(errors) => print(Rejection::report(errors)),
  }};
  let late: Validated<Date, Rejection> = decode(Raw::Text(\"yesterday\"));
  print(problems(late));
  match Decimal::of_string(\"0.10\") {{
    Option::Some(d) => print(encode(Raw::to_json(d.encode()))),
    Option::None => print(\"no decimal\"),
  }};
  let none: Option<Int> = Option::None;
  print(encode(Raw::to_json(Raw::Record([entry(\"a\", none.encode()), entry(\"b\", 1.encode())]))));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "None\n\
         [host, port]\n\
         h Port(8080)\n\
         {\"host\":\"h\",\"port\":8080}\n\
         port must be between 1 and 65535\n\
         [{\"message\":\"port must be between 1 and 65535\",\"path\":\"port\"}]\n\
         [1] must be between 1 and 65535; \n\
         \"2026-09-02\"\n\
         the value should be an ISO 8601 date, and is \"yesterday\"; \n\
         \"0.10\"\n\
         {\"b\":1}\n"
    );
}

/// A program whose schemas the compiler wrote.
const HEAD4: &str = "module main;

import std::core::{List, Option, Pair, Redacted, Result, Show, Validated, print};
import std::decimal::{Decimal};
import std::json::{encode, parse};
import std::schema::{Decode, Encode, Rejection, Schema, Shape, Raw, between, decode, int};

derive(Show)
pub type Port = Int;

impl Decode for Port {
  fn schema() -> Schema<Port> { Schema::map(between(int(), 1, 65535), fn n => Port(n)) }
}

impl Encode for Port {
  fn encode(self) -> Raw { match self { Port(n) => n.encode() } }
}

derive(Show, Decode, Encode)
pub type Listen = { host: String, port: Port };

derive(Show, Decode, Encode)
pub type Mode = | Local | Remote(url: String);

derive(Show, Decode, Encode)
pub type Level = | Debug | Info;

derive(Show, Decode)
pub type Settings = {
  listen: Listen,
  password: Redacted<String>,
  debug: Option<Bool>,
  rate: Decimal,
  tags: List<String>,
  mode: Mode,
  level: Level,
};

derive(Show, Decode, Encode)
pub type UserId = Int;

derive(Show, Decode, Encode)
pub type Wrapper<A> = { value: A, count: Int };

derive(Show, Decode, Encode)
pub type Tree = { label: String, children: List<Tree> };

derive(Decode)
pub type Branch = { leaves: List<Leaf> };

derive(Decode)
pub type Leaf = { back: Option<Branch>, name: String };

fn leaves(b: Branch) -> Int { List::length(b.leaves) }

fn written<A: Encode>(value: A) -> String { encode(Raw::to_json(value.encode())) }

fn report(text: String) -> String {
  match parse(text) {
    Result::Err(_e) => \"not json\",
    Result::Ok(document) =>
      match Settings::schema().decode(Raw::of_json(document)) {
        Validated::Valid(s) =>
          \"${s.listen.host}:${s.listen.port} ${s.password} ${s.debug} ${s.rate} ${s.tags} ${s.mode} ${s.level}\",
        Validated::Invalid(problems) => Rejection::report(problems),
      },
  }
}
";

/// **The declaration is the schema.** A derived record reads its fields
/// under their names, a nested derived type is found through the trait, a
/// hand-written newtype's refinement is picked up, every problem is reported
/// in declaration order, and the secret is never quoted.
#[test]
fn a_derived_schema_reads_the_declaration() {
    let out = run(
        "schema_derive_record",
        &format!(
            "{HEAD4}
fn main() -> Int {{
  print(report(`{{\"listen\": {{\"host\": \"localhost\", \"port\": 8080}}, \"password\": \"hunter2\", \"rate\": \"0.0725\", \"tags\": [\"a\", \"b\"], \"mode\": {{\"type\": \"Remote\", \"url\": \"https://x\"}}, \"level\": \"Info\"}}`));
  print(report(`{{\"listen\": {{\"port\": 70000}}, \"password\": 42, \"debug\": \"maybe\", \"rate\": \"free\", \"tags\": [\"a\", 7], \"mode\": {{\"type\": \"Cloud\"}}, \"level\": \"Trace\"}}`));
  print(\"${{Shape::keys(Settings::schema().shape)}}\");
  print(\"${{Settings::schema().shape}}\");
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "localhost:Port(8080) <redacted> None 0.0725 [a, b] Mode::Remote(https://x) Level::Info\n\
         listen.host is not set\n\
         listen.port must be between 1 and 65535\n\
         password should be text\n\
         debug should be true or false, and is \"maybe\"\n\
         rate should be an exact decimal, and is \"free\"\n\
         tags[1] should be text, and is 7\n\
         mode.type should be one of `Local`, `Remote`, and is \"Cloud\"\n\
         level should be one of `Debug`, `Info`, and is \"Trace\"\n\
         [listen, password, debug, rate, tags, mode, level]\n\
         Lazy(Settings)\n"
    );
}

/// **A derived encoder writes what the derived schema reads**, for a record,
/// a variant, an enum, a newtype and a generic record; a type that mentions
/// itself, and two that mention each other, need nothing written.
#[test]
fn a_derived_encoder_round_trips() {
    let out = run(
        "schema_derive_encode",
        &format!(
            "{HEAD4}
fn main() -> Int {{
  print(written(Mode::Remote(\"u\")));
  print(written(Mode::Local));
  print(written(Level::Info));
  let l: Listen = {{ host: \"h\", port: Port(1) }};
  print(written(l));
  print(written(UserId(7)));
  let w: Wrapper<String> = {{ value: \"x\", count: 1 }};
  print(written(w));
  let id: Validated<UserId, Rejection> = decode(Raw::Number(\"7\"));
  match Validated::to_result(id) {{
    Result::Ok(v) => print(Show::show(v)),
    Result::Err(problems) => print(Rejection::report(problems)),
  }};
  let wrapped: Validated<Wrapper<Int>, Rejection> =
    decode(Raw::Record([Raw::entry(\"value\", Raw::Number(\"3\")), Raw::entry(\"count\", Raw::Number(\"2\"))]));
  match Validated::to_result(wrapped) {{
    Result::Ok(v) => print(Show::show(v)),
    Result::Err(problems) => print(Rejection::report(problems)),
  }};
  match parse(`{{\"label\": \"r\", \"children\": [{{\"label\": \"c\", \"children\": []}}]}}`) {{
    Result::Err(_e) => print(\"not json\"),
    Result::Ok(document) =>
      match Validated::to_result(Tree::schema().decode(Raw::of_json(document))) {{
        Result::Ok(t) => print(written(t)),
        Result::Err(problems) => print(Rejection::report(problems)),
      }},
  }};
  match parse(`{{\"leaves\": [{{\"name\": \"a\", \"back\": {{\"leaves\": []}}}}, {{\"name\": \"b\"}}]}}`) {{
    Result::Err(_e) => print(\"not json\"),
    Result::Ok(document) =>
      match Validated::to_result(Branch::schema().decode(Raw::of_json(document))) {{
        Result::Ok(b) => print(\"${{leaves(b)}} leaves\"),
        Result::Err(problems) => print(Rejection::report(problems)),
      }},
  }};
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "{\"type\":\"Remote\",\"url\":\"u\"}\n\
         \"Local\"\n\
         \"Info\"\n\
         {\"host\":\"h\",\"port\":1}\n\
         7\n\
         {\"count\":1,\"value\":\"x\"}\n\
         UserId(7)\n\
         Wrapper { value: 3, count: 2 }\n\
         {\"children\":[{\"children\":[],\"label\":\"c\"}],\"label\":\"r\"}\n\
         2 leaves\n"
    );
}

/// **The shape renders as the document a model is prompted with.** A derived
/// type is a `$defs` entry and a `$ref`, a rule is its keyword, a secret is
/// `writeOnly`, an optional field is left out of `required`, a decimal is a
/// string, and a variant is an enum or a `oneOf` over the two forms the
/// decoder reads. A type that mentions itself terminates on its own name.
#[test]
fn a_shape_renders_as_a_json_schema() {
    let out = run(
        "schema_json_schema",
        &format!(
            "{HEAD4}
fn main() -> Int {{
  print(encode(Shape::to_json_schema(Settings::schema().shape)));
  print(encode(Shape::to_json_schema(Tree::schema().shape)));
  print(encode(Shape::to_json_schema(Schema::closed(Listen::schema()).shape)));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        concat!(
            "{\"$defs\":{",
            "\"Level\":{\"enum\":[\"Debug\",\"Info\"]},",
            "\"Listen\":{\"properties\":{\"host\":{\"type\":\"string\"},\"port\":{\"maximum\":65535,\"minimum\":1,\"type\":\"integer\"}},\"required\":[\"host\",\"port\"],\"type\":\"object\"},",
            "\"Mode\":{\"oneOf\":[{\"const\":\"Local\"},{\"properties\":{\"type\":{\"const\":\"Remote\"},\"url\":{\"type\":\"string\"}},\"required\":[\"type\",\"url\"],\"type\":\"object\"}]},",
            "\"Settings\":{\"properties\":{\"debug\":{\"type\":\"boolean\"},\"level\":{\"$ref\":\"#/$defs/Level\"},\"listen\":{\"$ref\":\"#/$defs/Listen\"},\"mode\":{\"$ref\":\"#/$defs/Mode\"},\"password\":{\"type\":\"string\",\"writeOnly\":true},\"rate\":{\"description\":\"an exact decimal, every digit kept\",\"type\":\"string\"},\"tags\":{\"items\":{\"type\":\"string\"},\"type\":\"array\"}},\"required\":[\"listen\",\"password\",\"rate\",\"tags\",\"mode\",\"level\"],\"type\":\"object\"}",
            "},\"$ref\":\"#/$defs/Settings\"}\n",
            "{\"$defs\":{\"Tree\":{\"properties\":{\"children\":{\"items\":{\"$ref\":\"#/$defs/Tree\"},\"type\":\"array\"},\"label\":{\"type\":\"string\"}},\"required\":[\"label\",\"children\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/Tree\"}\n",
            "{\"$defs\":{\"Listen\":{\"properties\":{\"host\":{\"type\":\"string\"},\"port\":{\"maximum\":65535,\"minimum\":1,\"type\":\"integer\"}},\"required\":[\"host\",\"port\"],\"type\":\"object\"}},\"$ref\":\"#/$defs/Listen\",\"additionalProperties\":false}\n",
        )
    );
}

/// The `///` above a type and above each of its fields is the description
/// in the JSON Schema, with nothing written twice.
#[test]
fn a_documented_type_describes_itself() {
    let out = run(
        "schema_documented",
        &format!(
            "{HEAD4}
/// A place to listen.
derive(Decode)
pub type Where = {{
  /// The host name, or an address.
  host: String,
  /// A port, as the OS numbers them.
  port: Int,
}};

fn main() -> Int {{
  print(encode(Shape::to_json_schema(Where::schema().shape)));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        concat!(
            "{\"$defs\":{\"Where\":{\"properties\":{",
            "\"host\":{\"description\":\"The host name, or an address.\",\"type\":\"string\"},",
            "\"port\":{\"description\":\"A port, as the OS numbers them.\",\"type\":\"integer\"}},",
            "\"required\":[\"host\",\"port\"],\"type\":\"object\"}},",
            "\"$ref\":\"#/$defs/Where\",\"description\":\"A place to listen.\"}\n",
        )
    );
}

/// A flag whose field is a `Bool` takes no value when the shape is asked,
/// so `-c .name` is the switch and then the query; the shape-blind reading
/// takes `.name` as the value of `c` and says so.
#[test]
fn a_switch_takes_no_value_when_the_shape_says_so() {
    let out = run(
        "schema_switches",
        &format!(
            "{HEAD2}
type Line = {{ c: Bool, explain: Bool, arguments: List<String> }};

fn main() -> Int {{
  let line: Schema<Line> = struct({{
    c: default(bool(), false),
    explain: default(bool(), false),
    arguments: default(list(string()), List::Nil),
  }});
  let args = List::Cons(\"-c\", List::Cons(\".name\", List::Cons(\"f.json\", List::Cons(\"--explain\", List::Nil))));
  match Validated::to_result(line.decode(Raw::of_arguments_for(line.shape, args))) {{
    Result::Ok(read) => print(
      (if read.c {{ \"c\" }} else {{ \"-\" }}) + \" \" + (if read.explain {{ \"explain\" }} else {{ \"-\" }}) + \" \"
        + Int::to_string(List::length(read.arguments)) + \" \" + Option::unwrap_or(List::head(read.arguments), \"?\")
    ),
    Result::Err(problems) => print(Rejection::report(problems)),
  }};
  match Validated::to_result(line.decode(Raw::of_arguments(args))) {{
    Result::Ok(_read) => print(\"read\"),
    Result::Err(problems) => print(Rejection::report(problems)),
  }};
  0
}}
"
        ),
    );
    assert_eq!(out, "c explain 2 .name\nc should be true or false, and is \".name\"\n");
}

const HEAD5: &str = "module main;

import std::core::{List, Option, Result, Show, Validated, print};
import std::json::{encode};
import std::schema::{Raw, Rejection, Schema, Shape, string, via};

pub type Level = | Low | High;

/// A string on the wire, a variant in the program. The record that holds one
/// does not know the difference, which is the whole point of `via`.
fn level() -> Schema<Level> {
  via(string(), \"Level\", fn t =>
    if t == \"low\" {
      Option::Some(Level::Low)
    } else if t == \"high\" {
      Option::Some(Level::High)
    } else {
      Option::None
    })
}

fn said(v: Validated<Level, Rejection>) -> String {
  match Validated::to_result(v) {
    Result::Ok(l) => match l { Level::Low => \"low\", Level::High => \"high\" },
    Result::Err(errors) =>
      List::fold(errors, \"\", fn (acc, e) => acc + Rejection::describe(e) + \"; \"),
  }
}
";

/// **A value may arrive as one thing and become another.** `refine` narrows a
/// type to itself; `via` changes it, which is what a timestamp travelling as a
/// string needs and what nothing before this could say.
///
/// Three claims. The conversion runs and the program gets the domain type. A
/// conversion that refuses reports like a primitive that did not match, because
/// a caller should not be able to tell whether the reader was built in. And the
/// rendered JSON Schema describes the *wire* form -- a producer has to send a
/// string, not a `Level` -- with the domain name as `format`, which is the
/// keyword JSON Schema has for exactly this.
#[test]
fn a_value_can_arrive_as_one_thing_and_become_another() {
    let out = run(
        "schema_via",
        &format!(
            "{HEAD5}
fn main() -> Int {{
  print(said(Schema::decode(level(), Raw::Text(\"high\"))));
  print(said(Schema::decode(level(), Raw::Text(\"sideways\"))));
  print(encode(Shape::to_json_schema(level().shape)));
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "high\n\
         the value should be Level, and is sideways; \n\
         {\"format\":\"Level\",\"type\":\"string\"}\n"
    );
}
