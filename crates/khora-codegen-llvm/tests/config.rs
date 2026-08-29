#![cfg(feature = "llvm")]

//! `std::config`, against a fake environment.
//!
//! Every test here installs a `handler for Env` rather than setting a real
//! variable, which is the argument the module makes in its own header: the
//! reason Khora needs no `Config<A>` description type is that the provider is
//! already a capability, so a test can swap it. If these tests needed
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
import std::config::{ConfigError, boolean, decimal, integer, or_default, report, secret, string};
import std::decimal::{Decimal};

/// An environment that holds exactly what a test says it does.
///
/// `denied` is the list the manifest would have refused, so that
/// `ConfigError::Denied` can be reached without a manifest.
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

/// Whatever a reader answered, as one line.
fn said<A>(answer: Validated<A, ConfigError>, shown: (A) -> String) -> String {
  match answer {
    Validated::Valid(value) => shown(value),
    Validated::Invalid(errors) => report(errors),
  }
}
"#;

fn program(name: &str, body: &str) -> (PathBuf, KhoraDatabase, SourceRoot) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("permissions.kh"), std_source("permissions.kh")),
        SourceFile::new(&db, dir.join("grants.kh"), std_source("grants.kh")),
        SourceFile::new(&db, dir.join("env_native.kh"), std_source("env_native.kh")),
        // `std::config` reads a `Decimal` now, so its module comes too.
        SourceFile::new(&db, dir.join("decimal.kh"), std_source("decimal.kh")),
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
/// The whole reason the module answers `Validated` rather than `Result`: a
/// service that stops at the first missing key is a person running it once per
/// key to be told what it knew the first time.
#[test]
fn every_missing_setting_is_reported_together() {
    let out = run(
        "config_all_at_once",
        r#"fn main() -> () {
  with { env: fake(List::Nil, List::Nil) } {
    let both = Validated::map2(
      string("HOST"),
      integer("PORT"),
      fn (host, port) => host + ":" + Int::to_string(port),
    );
    print(said(both, fn (line) => line));
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
  with { env: fake(List::Cons(("PORT", "eighty"), List::Nil), List::Nil) } {
    print(said(or_default(integer("HOST_PORT"), 8080), Int::to_string));
    print(said(or_default(integer("PORT"), 8080), Int::to_string));
  }
}"#,
    );

    assert_eq!(out, "8080\nPORT should be a whole number, and is `eighty`\n");
}

/// A denial is its own line, because the file to open is `khora.toml` rather
/// than a deployment script.
#[test]
fn a_denied_variable_says_which_file_to_edit() {
    let out = run(
        "config_denied",
        r#"fn main() -> () {
  with { env: fake(List::Nil, List::Cons("SECRET_KEY", List::Nil)) } {
    print(said(string("SECRET_KEY"), fn (line) => line));
    // And a default does *not* cover it: nobody asked for a fallback password.
    print(said(or_default(string("SECRET_KEY"), "fallback"), fn (line) => line));
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
    print(said(secret("DB_PASSWORD"), fn (key) => key.show()));
    print(said(secret("DB_PASSWORD"), fn (key) => Redacted::expose(key)));
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
  let pairs = List::Cons(("A", "true"),
              List::Cons(("B", "0"),
              List::Cons(("C", "yes"), List::Nil)));
  with { env: fake(pairs, List::Nil) } {
    print(said(boolean("A"), fn (flag) => flag.show()));
    print(said(boolean("B"), fn (flag) => flag.show()));
    print(said(boolean("C"), fn (flag) => flag.show()));
  }
}"#,
    );

    assert_eq!(out, "true\nfalse\nC should be `true` or `false`, and is `yes`\n");
}


/// **`map2` stopped at two and a settings block has five keys.**
///
/// Building a record out of four or five validated fields meant nesting
/// `map2` and carrying a tuple through it, or hand-rolling the accumulation —
/// which two of the four review programs did, separately. `List::traverse`
/// does the unbounded version and would have done the whole job, except that
/// `Validated` is deliberately not an `Applicative`, so the one type in `std`
/// built for accumulating errors cannot use it.
///
/// The point of all of them is the same as `map2`'s: **one pass**. A service
/// that stops at the first missing key is a person running it once per key to
/// be told what it knew the first time.
#[test]
fn five_settings_are_reported_in_one_pass() {
    let out = run(
        "config_map5",
        r#"fn main() -> () {
  with { env: fake(List::Nil, List::Nil) } {
    let all = Validated::map5(
      string("HOST"),
      integer("PORT"),
      string("REGION"),
      integer("WORKERS"),
      boolean("DEBUG"),
      fn (host, port, region, workers, debug) =>
        host + ":" + Int::to_string(port) + " " + region
          + " x" + Int::to_string(workers) + " " + Bool::to_string(debug),
    );
    print(said(all, fn (line) => line));
  }
}"#,
    );

    assert_eq!(
        out,
        "HOST is not set\nPORT is not set\nREGION is not set\nWORKERS is not set\n\
         DEBUG is not set\n"
    );
}

/// **Left to right, whichever sides failed** — and the ones that succeeded do
/// not appear.
///
/// The order is the argument order rather than the order the failures were
/// noticed, because a reader is matching the list against the block they just
/// wrote.
#[test]
fn only_the_settings_that_are_wrong_are_reported() {
    let out = run(
        "config_map5_partial",
        r#"fn main() -> () {
  let set = List::Cons(("HOST", "db.internal"),
    List::Cons(("REGION", "eu-west"), List::Cons(("DEBUG", "true"), List::Nil)));
  with { env: fake(set, List::Nil) } {
    // The second and fourth are missing; the first, third and fifth are not.
    let all = Validated::map5(
      string("HOST"),
      integer("PORT"),
      string("REGION"),
      integer("WORKERS"),
      boolean("DEBUG"),
      fn (host, port, region, workers, debug) =>
        host + ":" + Int::to_string(port) + " " + region
          + " x" + Int::to_string(workers) + " " + Bool::to_string(debug),
    );
    print(said(all, fn (line) => line));
  }
}"#,
    );

    assert_eq!(out, "PORT is not set\nWORKERS is not set\n");
}

/// `map3` and `map4`, and the case that has to work for either to be worth
/// having: everything valid, and `f` run once with all of them.
#[test]
fn three_and_four_settings_come_together() {
    let out = run(
        "config_map34",
        r#"fn main() -> () {
  let set = List::Cons(("HOST", "db.internal"),
    List::Cons(("PORT", "5432"), List::Cons(("REGION", "eu-west"),
      List::Cons(("WORKERS", "4"), List::Nil))));
  with { env: fake(set, List::Nil) } {
    let three = Validated::map3(
      string("HOST"),
      integer("PORT"),
      string("REGION"),
      fn (host, port, region) => host + ":" + Int::to_string(port) + " " + region,
    );
    print(said(three, fn (line) => line));

    let four = Validated::map4(
      string("HOST"),
      integer("PORT"),
      string("REGION"),
      integer("WORKERS"),
      fn (host, port, region, workers) =>
        host + ":" + Int::to_string(port) + " " + region + " x" + Int::to_string(workers),
    );
    print(said(four, fn (line) => line));

    // And the first of three missing, which is the arm that gathers the rest.
    let missing = Validated::map3(
      string("NOPE"),
      integer("PORT"),
      string("ALSO_NOPE"),
      fn (a, b, c) => a + Int::to_string(b) + c,
    );
    print(said(missing, fn (line) => line));
  }
}"#,
    );

    assert_eq!(
        out,
        "db.internal:5432 eu-west\n\
         db.internal:5432 eu-west x4\n\
         NOPE is not set\nALSO_NOPE is not set\n"
    );
}


/// **This is the language for money and the config reader could not read
/// any.**
///
/// A rate, a threshold, a fee cap and a currency amount are all settings, and
/// the readers were `Int`, `Bool` and `String` — so configuring a rate meant
/// reading text and parsing it again at the call site, or reading an `Int` of
/// basis points and hoping everybody downstream remembered the scale.
///
/// The scale is whatever was written: `0.10` reads at two places, which is
/// what makes a total built from it print the way the person who set it
/// expected.
#[test]
fn a_rate_can_be_configured_exactly() {
    let out = run(
        "config_decimal",
        r#"fn main() -> () {
  let set = List::Cons(("RATE", "0.0125"),
    List::Cons(("FEE_CAP", "1250.00"), List::Cons(("MINIMUM", "-3"), List::Nil)));
  with { env: fake(set, List::Nil) } {
    print(said(decimal("RATE"), Decimal::show));
    print(said(decimal("FEE_CAP"), Decimal::show));
    print(said(decimal("MINIMUM"), Decimal::show));
    // Not set is the same shape as every other reader's.
    print(said(decimal("SPREAD"), Decimal::show));
  }
}"#,
    );

    assert_eq!(out, "0.0125\n1250.00\n-3\nSPREAD is not set\n");
}

/// **Exponent notation is refused**, the way `Decimal::of_string` refuses it:
/// a number arriving as `1e-3` has been through a float somewhere, and a
/// configuration file is exactly where that would go unnoticed.
///
/// A numeral too long to hold is the same answer, which is what keeps one bad
/// line in one file from stopping the process.
#[test]
fn a_decimal_setting_refuses_what_is_not_one() {
    let out = run(
        "config_decimal_bad",
        r#"fn main() -> () {
  let set = List::Cons(("RATE", "1e-3"),
    List::Cons(("CAP", "twelve"), List::Cons(("HUGE",
      "99999999999999999999999999999999999999999999"), List::Nil)));
  with { env: fake(set, List::Nil) } {
    print(said(decimal("RATE"), Decimal::show));
    print(said(decimal("CAP"), Decimal::show));
    print(said(decimal("HUGE"), Decimal::show));
  }
}"#,
    );

    assert_eq!(
        out,
        "RATE should be an exact decimal, and is `1e-3`\n\
         CAP should be an exact decimal, and is `twelve`\n\
         HUGE should be an exact decimal, and is \
         `99999999999999999999999999999999999999999999`\n"
    );
}
