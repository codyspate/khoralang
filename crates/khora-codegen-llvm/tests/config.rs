#![cfg(feature = "llvm")]

//! `std::config`, against a fake environment.
//!
//! Every test here installs a `handler for Env` rather than setting a real
//! variable, which is the argument the module makes in its own header: the
//! provider is a capability, so a test can swap it. If these tests needed
//! `std::env::set_var` to exist, the argument would be wrong.

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

const HEAD: &str = r#"module demo::main;
import std::core::{Eq, List, Option, Redacted, Result, Show, Validated, print};
import std::env::{Env, EnvError};
import std::config::{read, report, variables};
import std::decimal::{Decimal};
import std::schema::{Decode, Rejection, Schema, default, int, string, struct};

/// An environment that holds exactly what a test says it does.
///
/// `denied` is the list the manifest would have refused, so that a denial can
/// be reached without a manifest.
fn fake(pairs: List<(String, String)>, denied: List<String>) -> Env {
  handler for Env {
    variable: fn name =>
      if member(denied, name) {
        raise EnvError::Denied(name)
      } else {
        lookup(pairs, name)
      },
    arguments: fn () => List::Nil,
  }
}

fn member(names: List<String>, wanted: String) -> Bool {
  match names {
    List::Nil => false,
    List::Cons(head, tail) => if head.eq(wanted) { true } else { member(tail, wanted) },
  }
}

fn lookup(pairs: List<(String, String)>, wanted: String) -> Option<String> {
  match pairs {
    List::Nil => Option::None,
    List::Cons(head, tail) => {
      let (name, value) = head;
      if name.eq(wanted) { Option::Some(value) } else { lookup(tail, wanted) }
    }
  }
}

/// Whatever a read answered, as one line -- or the report, one per line.
fn said<A>(answer: Validated<A, Rejection>, shown: (A) -> String) -> String {
  match answer {
    Validated::Valid(value) => shown(value),
    Validated::Invalid(problems) => report(problems),
  }
}

derive(Show, Decode)
pub type Listen = { host: String, port: Int };

derive(Show, Decode)
pub type Mode = | Local | Remote(url: String);

derive(Show, Decode)
pub type Settings = {
  listen: Listen,
  password: Redacted<String>,
  debug: Option<Bool>,
  region: String,
  workers: Int,
  tags: List<String>,
  mode: Mode,
};

derive(Show, Decode)
pub type Flags = { a: Bool, b: Bool, c: Bool };

derive(Show, Decode)
pub type Money = { rate: Decimal, fee_cap: Decimal, minimum: Decimal };

derive(Show, Decode)
pub type Spread = { spread: Decimal };

derive(Show, Decode)
pub type Keys = { secret_key: String };

derive(Show, Decode)
pub type Password = { db_password: Redacted<String> };

derive(Show, Decode)
pub type Nested = { primary: Listen, backup: Option<Listen> };

fn shown_settings(s: Settings) -> String {
  "${s.listen.host}:${s.listen.port} ${s.password} ${s.debug} ${s.region} x${s.workers} ${s.tags} ${s.mode}"
}
"#;

fn program(name: &str, body: &str) -> (PathBuf, KhoraDatabase, SourceRoot) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    // `std::config` is a source for `std::schema`, so the schema module and
    // everything it imports come too.
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("permissions.kh"), std_source("permissions.kh")),
        SourceFile::new(&db, dir.join("grants.kh"), std_source("grants.kh")),
        SourceFile::new(&db, dir.join("env_native.kh"), std_source("env_native.kh")),
        SourceFile::new(&db, dir.join("decimal.kh"), std_source("decimal.kh")),
        SourceFile::new(&db, dir.join("json.kh"), std_source("json.kh")),
        SourceFile::new(&db, dir.join("time.kh"), std_source("time.kh")),
        SourceFile::new(&db, dir.join("schema.kh"), std_source("schema.kh")),
        SourceFile::new(&db, dir.join("config.kh"), std_source("config_native.kh")),
        SourceFile::new(&db, dir.join("main.kh"), format!("{HEAD}\n{body}\n")),
    ];
    let root = SourceRoot::new(&db, files);
    (exe, db, root)
}

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

/// **Everything wrong, in one pass.**
///
/// The whole reason a read answers `Validated` rather than `Result`: a
/// service that stops at the first missing key is a person running it once per
/// key to be told what it knew the first time.
#[test]
fn every_missing_setting_is_reported_together() {
    let out = run(
        "config_all_at_once",
        r#"fn main() -> () {
  with { env: fake(List::Nil, List::Nil) } {
    print(said(read(Listen::schema()), fn (l) => l.host));
  }
}"#,
    );

    assert_eq!(out, "HOST is not set\nPORT is not set\n");
}

/// **A default fires on missing and never on malformed.**
///
/// `PORT=eighty` quietly becoming `80` is the bug this module exists to stop,
/// and it is the one a fallback written the obvious way introduces.
#[test]
fn a_default_covers_absence_and_not_a_typo() {
    let out = run(
        "config_default",
        r#"fn main() -> () {
  let s: Schema<Listen> = struct({ host: default(string(), "0.0.0.0"), port: int() });
  with { env: fake(List::Cons(("PORT", "eighty"), List::Nil), List::Nil) } {
    print(said(read(s), fn (l) => l.host + ":" + Int::to_string(l.port)));
  };
  with { env: fake(List::Cons(("PORT", "8080"), List::Nil), List::Nil) } {
    print(said(read(s), fn (l) => l.host + ":" + Int::to_string(l.port)));
  }
}"#,
    );

    assert_eq!(out, "PORT should be a whole number, and is \"eighty\"\n0.0.0.0:8080\n");
}

/// A denial is its own line, because the file to open is `khora.toml` rather
/// than a deployment script -- and a default does not cover it, because
/// nobody asked for a fallback password.
#[test]
fn a_denied_variable_says_which_file_to_edit() {
    let out = run(
        "config_denied",
        r#"fn main() -> () {
  with { env: fake(List::Nil, List::Cons("SECRET_KEY", List::Nil)) } {
    print(said(read(Keys::schema()), fn (k) => k.secret_key));
    let s: Schema<Keys> = struct({ secret_key: default(string(), "fallback") });
    print(said(read(s), fn (k) => k.secret_key));
  }
}"#,
    );

    assert_eq!(
        out,
        "SECRET_KEY is not granted -- add it to `[permissions] env` in khora.toml\n\
         SECRET_KEY is not granted -- add it to `[permissions] env` in khora.toml\n"
    );
}

/// A read secret is still a secret.
#[test]
fn a_secret_read_from_the_environment_cannot_be_printed() {
    let out = run(
        "config_secret",
        r#"fn main() -> () {
  with { env: fake(List::Cons(("DB_PASSWORD", "hunter2"), List::Nil), List::Nil) } {
    print(said(read(Password::schema()), fn (p) => p.db_password.show()));
    print(said(read(Password::schema()), fn (p) => Redacted::expose(p.db_password)));
  }
}"#,
    );

    assert_eq!(out, "<redacted>\nhunter2\n");
}

/// **Only the four spellings**, because a config reader that takes `yes` and
/// `on` also takes a typo as a `false`.
#[test]
fn a_flag_takes_four_spellings_and_refuses_the_rest() {
    let out = run(
        "config_boolean",
        r#"fn main() -> () {
  let bad = List::Cons(("A", "true"), List::Cons(("B", "0"), List::Cons(("C", "yes"), List::Nil)));
  with { env: fake(bad, List::Nil) } {
    print(said(read(Flags::schema()), fn (f) => "${f.a} ${f.b} ${f.c}"));
  };
  let good = List::Cons(("A", "true"), List::Cons(("B", "0"), List::Cons(("C", "false"), List::Nil)));
  with { env: fake(good, List::Nil) } {
    print(said(read(Flags::schema()), fn (f) => "${f.a} ${f.b} ${f.c}"));
  }
}"#,
    );

    assert_eq!(out, "C should be true or false, and is \"yes\"\ntrue false false\n");
}

/// **One shape, every variable.** A nested record is joined with `_`, a list
/// is split on commas, a variant is its case with its payload beside it, and
/// `variables` says all of that without reading anything.
#[test]
fn settings_come_from_one_shape() {
    let out = run(
        "config_shape",
        r#"fn main() -> () {
  let set = List::Cons(("LISTEN_HOST", "db.internal"),
    List::Cons(("LISTEN_PORT", "5432"), List::Cons(("PASSWORD", "hunter2"),
      List::Cons(("REGION", "eu-west"), List::Cons(("WORKERS", "4"),
        List::Cons(("TAGS", "a,b"), List::Cons(("MODE", "Remote"),
          List::Cons(("MODE_URL", "https://x"), List::Nil))))))));
  with { env: fake(set, List::Nil) } {
    print(said(read(Settings::schema()), shown_settings));
  };
  print("${variables(Settings::schema().shape)}");
  let local = List::Cons(("LISTEN_HOST", "h"), List::Cons(("LISTEN_PORT", "1"),
    List::Cons(("PASSWORD", "p"), List::Cons(("REGION", "r"), List::Cons(("WORKERS", "1"),
      List::Cons(("TAGS", "one"), List::Cons(("MODE", "Cloud"), List::Nil)))))));
  with { env: fake(local, List::Nil) } {
    print(said(read(Settings::schema()), shown_settings));
  }
}"#,
    );

    assert_eq!(
        out,
        "db.internal:5432 <redacted> None eu-west x4 [a, b] Mode::Remote(https://x)\n\
         [LISTEN_HOST, LISTEN_PORT, PASSWORD, DEBUG, REGION, WORKERS, TAGS, MODE, MODE_URL]\n\
         MODE should be one of `Local`, `Remote`, and is \"Cloud\"\n"
    );
}

/// **Declaration order, whichever variables are wrong** -- and the ones that
/// were fine do not appear. A reader is matching the list against the record
/// they just wrote.
#[test]
fn only_the_settings_that_are_wrong_are_reported() {
    let out = run(
        "config_partial",
        r#"fn main() -> () {
  let set = List::Cons(("LISTEN_HOST", "db.internal"),
    List::Cons(("REGION", "eu-west"), List::Cons(("DEBUG", "true"),
      List::Cons(("MODE", "Local"), List::Nil))));
  with { env: fake(set, List::Nil) } {
    print(said(read(Settings::schema()), shown_settings));
  }
}"#,
    );

    assert_eq!(
        out,
        "LISTEN_PORT is not set\nPASSWORD is not set\nWORKERS is not set\nTAGS is not set\n"
    );
}

/// **This is the language for money and a setting can be one.** The scale is
/// whatever was written: `1250.00` reads at two places, which is what makes a
/// total built from it print the way the person who set it expected.
#[test]
fn a_rate_can_be_configured_exactly() {
    let out = run(
        "config_decimal",
        r#"fn main() -> () {
  let set = List::Cons(("RATE", "0.0125"),
    List::Cons(("FEE_CAP", "1250.00"), List::Cons(("MINIMUM", "-3"), List::Nil)));
  with { env: fake(set, List::Nil) } {
    print(said(read(Money::schema()), fn (m) => "${m.rate} ${m.fee_cap} ${m.minimum}"));
    print(said(read(Spread::schema()), fn (s) => "${s.spread}"));
  }
}"#,
    );

    assert_eq!(out, "0.0125 1250.00 -3\nSPREAD is not set\n");
}

/// **Exponent notation is refused**, the way `Decimal::of_string` refuses it:
/// a number arriving as `1e-3` has been through a float somewhere, and a
/// configuration file is exactly where that would go unnoticed. A numeral
/// too long to hold is the same answer, which is what keeps one bad line in
/// one file from stopping the process.
#[test]
fn a_decimal_setting_refuses_what_is_not_one() {
    let out = run(
        "config_decimal_bad",
        r#"derive(Show, Decode)
pub type Bad = { rate: Decimal, cap: Decimal, huge: Decimal };

fn main() -> () {
  let set = List::Cons(("RATE", "1e-3"),
    List::Cons(("CAP", "twelve"), List::Cons(("HUGE",
      "99999999999999999999999999999999999999999999"), List::Nil)));
  with { env: fake(set, List::Nil) } {
    print(said(read(Bad::schema()), fn (b) => "${b.rate}"));
  }
}"#,
    );

    assert_eq!(
        out,
        "RATE should be an exact decimal, and is \"1e-3\"\n\
         CAP should be an exact decimal, and is \"twelve\"\n\
         HUGE should be an exact decimal, and is \
         \"99999999999999999999999999999999999999999999\"\n"
    );
}

/// An optional nested record is there when any of its variables is, and then
/// it is held to the whole of its shape.
#[test]
fn an_optional_nested_record_is_present_when_any_of_its_variables_is() {
    let out = run(
        "config_nested_optional",
        r#"fn main() -> () {
  let primary = List::Cons(("PRIMARY_HOST", "a"), List::Cons(("PRIMARY_PORT", "1"), List::Nil));
  with { env: fake(primary, List::Nil) } {
    print(said(read(Nested::schema()), fn (n) => "${n.primary.host} ${n.backup}"));
  };
  let half = List::Cons(("BACKUP_HOST", "b"), primary);
  with { env: fake(half, List::Nil) } {
    print(said(read(Nested::schema()), fn (n) => "${n.primary.host} ${n.backup}"));
  };
  let whole = List::Cons(("BACKUP_PORT", "2"), half);
  with { env: fake(whole, List::Nil) } {
    print(said(read(Nested::schema()), fn (n) => "${n.primary.host} ${n.backup}"));
  }
}"#,
    );

    assert_eq!(
        out,
        "a None\nBACKUP_PORT is not set\na Some(Listen { host: b, port: 2 })\n"
    );
}
