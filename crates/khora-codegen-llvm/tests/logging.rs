#![cfg(feature = "llvm")]

//! `std::log`: saying what happened, on the stream meant for it.
//!
//! **The stream is the whole subject.** Khora had no way to write to standard
//! error at all, so a program's diagnostics and its answer shared one stream
//! and the first `> out.txt` anybody typed swallowed the diagnostics. Two
//! independent evaluators building ordinary command-line tools reported it.
//!
//! So every test here reads the two streams separately and asserts which one a
//! thing arrived on. A test that captured them together would pass on the bug
//! this module exists to fix.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_sources(db: &KhoraDatabase) -> Vec<SourceFile> {
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
    out
}

/// The program's stdout and stderr, kept apart.
struct Ran {
    out: String,
    err: String,
}

fn run(name: &str, main: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(&db, dir.join("main.kh"), main.to_string()));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        out: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        err: String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n"),
    }
}

/// A clock that always says the same thing, so a log line can be compared.
///
/// **This is the reason the clock is a parameter.** A logger holding the real
/// one emits a different byte string every run and can only be asserted against
/// with a regular expression, which is a test of the regular expression.
const FIXED: &str = r#"
  let fixed = handler for Clock {
    unix_seconds: fn () => 1757021376,
    unix_millis: fn () => 1757021376817,
    monotonic_millis: fn () => 0,
    sleep: fn _ms => (),
  };
"#;

/// **The point of the module, in one assertion.** The answer goes to stdout and
/// the diagnostic to stderr, so redirecting one does not take the other.
#[test]
fn a_diagnostic_goes_to_standard_error_and_the_answer_does_not() {
    let ran = run(
        "log_streams",
        "module demo::main;
import std::log::{eprint};

fn print(value: String);

pub fn main() -> Int {
  print(\"the answer\");
  eprint(\"the diagnostic\");
  0
}
",
    );
    assert_eq!(ran.out, "the answer\n", "stdout should carry only the answer");
    assert_eq!(ran.err, "the diagnostic\n", "stderr should carry only the diagnostic");
}

/// One object per line, with `timestamp`, `level` and `message` in that order.
///
/// The order is asserted whole rather than by three `contains` checks, because
/// the order is the property: a log read with `head` or diffed between runs
/// depends on it, and a `Map` would not have one.
#[test]
fn a_record_is_one_json_object_per_line() {
    let ran = run(
        "log_json",
        &format!(
            "module demo::main;
import std::clock::{{Clock}};
import std::log::{{Severity, Log, info, warn}};

fn work() -> () with {{ log: Log }} {{
  info(\"starting\");
  warn(\"nearly done\");
}}

pub fn main() -> Int {{{FIXED}
  with {{ log: Log::json_using(Severity::Info, fixed) }} {{ work() }};
  0
}}
"
        ),
    );
    assert_eq!(
        ran.err,
        "{\"timestamp\":1757021376817,\"level\":\"info\",\"message\":\"starting\"}\n\
         {\"timestamp\":1757021376817,\"level\":\"warn\",\"message\":\"nearly done\"}\n",
        "stdout was {:?}",
        ran.out
    );
}

/// **A message is arbitrary text and will eventually contain a quote.** A
/// logger that emits broken JSON on that line is worse than one that emits
/// none, so the escaping goes through `std::json` rather than being written by
/// hand here.
#[test]
fn a_message_with_quotes_and_newlines_is_escaped() {
    let ran = run(
        "log_escaping",
        &format!(
            "module demo::main;
import std::clock::{{Clock}};
import std::log::{{Severity, Log, error}};

fn work() -> () with {{ log: Log }} {{
  error(\"she said \\\"no\\\" and left\");
}}

pub fn main() -> Int {{{FIXED}
  with {{ log: Log::json_using(Severity::Trace, fixed) }} {{ work() }};
  0
}}
"
        ),
    );
    assert!(
        ran.err.contains(r#""message":"she said \"no\" and left""#),
        "the quotes should be escaped, not emitted raw: {:?}",
        ran.err
    );
}

/// Attributes become fields of the same object, keeping their JSON types: a
/// number is a number and a flag is a bool, so a collector can filter on them
/// without parsing strings back.
#[test]
fn attributes_become_typed_fields() {
    let ran = run(
        "log_attributes",
        &format!(
            "module demo::main;
import std::clock::{{Clock}};
import std::core::{{List}};
import std::log::{{Severity, Log}};
import std::trace::{{flag, number, text}};

fn work() -> () with {{ log: Log }} {{
  log.record(Severity::Error, \"failed\", [text(\"job\", \"abc\"), number(\"attempt\", 2), flag(\"retry\", true)]);
}}

pub fn main() -> Int {{{FIXED}
  with {{ log: Log::json_using(Severity::Trace, fixed) }} {{ work() }};
  0
}}
"
        ),
    );
    assert!(
        ran.err.contains(r#""job":"abc","attempt":2,"retry":true"#),
        "typed, in the order given: {:?}",
        ran.err
    );
}

/// **The handler drops what is below the minimum, not the caller.** A caller
/// that checked the level itself would have to know the configuration, which is
/// what the capability exists to keep away from it.
#[test]
fn a_level_below_the_minimum_says_nothing() {
    let ran = run(
        "log_levels",
        &format!(
            "module demo::main;
import std::clock::{{Clock}};
import std::log::{{Severity, Log, debug, error, info, trace, warn}};

fn work() -> () with {{ log: Log }} {{
  trace(\"t\");
  debug(\"d\");
  info(\"i\");
  warn(\"w\");
  error(\"e\");
}}

pub fn main() -> Int {{{FIXED}
  with {{ log: Log::json_using(Severity::Warn, fixed) }} {{ work() }};
  0
}}
"
        ),
    );
    let lines: Vec<&str> = ran.err.lines().collect();
    assert_eq!(lines.len(), 2, "only warn and error should survive: {:?}", ran.err);
    assert!(lines[0].contains(r#""level":"warn""#), "{:?}", ran.err);
    assert!(lines[1].contains(r#""level":"error""#), "{:?}", ran.err);
}

/// The default has to be nearly free: if logging costs when it is off it gets
/// turned off, and then it does not exist.
#[test]
fn the_silent_logger_says_nothing_at_all() {
    let ran = run(
        "log_none",
        "module demo::main;
import std::log::{Log, error, info};

fn work() -> () with { log: Log } {
  info(\"i\");
  error(\"e\");
}

pub fn main() -> Int {
  with { log: Log::none() } { work() };
  0
}
",
    );
    assert_eq!(ran.err, "", "a silent logger writes nothing: {:?}", ran.err);
}
