#![cfg(feature = "llvm")]

//! `std::env`: what a program was told, and what time it is.
//!
//! The smallest thing separating a program from a demo. Until this existed
//! nothing a Khora program did could depend on anything outside its own
//! source — every path, port and setting was compiled in.

mod harness;

use std::path::PathBuf;

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

fn build(name: &str, main: &str) -> PathBuf {
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
    exe
}

const HEAD: &str = "module demo::main;
import std::core::{List, Option};
import std::env::{Clock, Env, variable_or};

fn print(value: String);
extern fn khora_print_int(value: Int);

fn show_all(items: List<String>) -> () {
  match items {
    List::Nil => (),
    List::Cons(head, rest) => { print(head); show_all(rest) },
  }
}
";

/// **The program's own name first**, then what it was invoked with — the same
/// convention C, Rust and Go all follow, and the one every caller expects.
#[test]
fn a_program_can_read_its_arguments() {
    let exe = build(
        "env_arguments",
        &format!(
            "{HEAD}
fn work() -> Int with {{ env: Env }} {{ show_all(env.arguments()); 0 }}

fn main() -> Int {{
  with {{ env: Env::real() }} {{ khora_print_int(work()); }}
  0
}}
"
        ),
    );

    let out = std::process::Command::new(&exe)
        .args(["alpha", "beta gamma"])
        .output()
        .expect("the program should run");
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines.len(), 4, "argv[0] plus two, then the return value: {text:?}");
    assert!(lines[0].ends_with("program.exe") || lines[0].ends_with("program"), "{text:?}");
    assert_eq!(lines[1], "alpha");
    assert_eq!(lines[2], "beta gamma", "an argument with a space is still one argument");
    assert_eq!(lines[3], "0");
}

/// A program with no arguments still has its own name.
#[test]
fn a_program_with_no_arguments_has_one() {
    let exe = build(
        "env_no_arguments",
        &format!(
            "{HEAD}
fn count(items: List<String>) -> Int {{
  match items {{ List::Nil => 0, List::Cons(head, rest) => 1 + count(rest) }}
}}

fn work() -> Int with {{ env: Env }} {{ count(env.arguments()) }}

fn main() -> Int {{
  with {{ env: Env::real() }} {{ khora_print_int(work()); }}
  0
}}
"
        ),
    );
    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1");
}

#[test]
fn a_program_can_read_the_environment() {
    let exe = build(
        "env_variables",
        &format!(
            "{HEAD}
fn work() -> Int with {{ env: Env }} {{
  print(variable_or(\"KHORA_TEST_VALUE\", \"unset\"));
  print(variable_or(\"KHORA_TEST_ABSENT\", \"fallback\"));
  match env.variable(\"KHORA_TEST_ABSENT\") {{
    Option::None => print(\"really absent\"),
    Option::Some(found) => print(found),
  }};
  0
}}

fn main() -> Int {{
  with {{ env: Env::real() }} {{ khora_print_int(work()); }}
  0
}}
"
        ),
    );

    let out = std::process::Command::new(&exe)
        .env("KHORA_TEST_VALUE", "from the environment")
        .env_remove("KHORA_TEST_ABSENT")
        .output()
        .expect("the program should run");
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(text, "from the environment\nfallback\nreally absent\n0\n");
}

/// Whole seconds since 1970, which is what ISO C's `time` offers. A finer
/// clock is not portable and the roadmap carries the gap.
#[test]
fn a_program_can_ask_the_time() {
    let exe = build(
        "env_clock",
        &format!(
            "{HEAD}
fn work() -> Int with {{ clock: Clock }} {{ clock.unix_seconds() }}

fn main() -> Int {{
  with {{ clock: Clock::real() }} {{ khora_print_int(work()); }}
  0
}}
"
        ),
    );
    let out = std::process::Command::new(&exe).output().expect("the program should run");
    let seconds: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("a number");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs() as i64;
    assert!((seconds - now).abs() < 120, "the clock said {seconds}, the host says {now}");
}

/// **The seam.** A test hands the code under test a different environment and
/// it cannot tell — which is the whole reason these are capabilities rather
/// than plain functions. Reading the environment is exactly the hidden input
/// that makes a program hard to test.
#[test]
fn the_environment_can_be_replaced_wholesale() {
    let exe = build(
        "env_mocked",
        &format!(
            "{HEAD}
/// Ordinary code. It has no idea whether a real environment exists.
fn work() -> Int with {{ env: Env }} {{
  print(variable_or(\"HOME\", \"nowhere\"));
  show_all(env.arguments());
  0
}}

fn main() -> Int {{
  with {{ env: handler for Env {{
    variable: fn name => Option::Some(\"/pretend\"),
    arguments: fn () => List::Cons(\"fake\", List::Nil),
  }} }} {{
    khora_print_int(work());
  }}
  0
}}
"
        ),
    );
    let out = std::process::Command::new(&exe)
        .args(["ignored"])
        .output()
        .expect("the program should run");
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(
        text, "/pretend\nfake\n0\n",
        "the real arguments never reached it"
    );
}

/// And the permission half: without the capability there is no way to ask.
#[test]
fn nothing_reads_the_environment_without_the_capability() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("env_denied");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(
        &db,
        dir.join("main.kh"),
        "module demo::main;
import std::env::{variable_or};

extern fn khora_print_int(value: Int);

fn main() -> Int {
  khora_print_int(String::byte_length(variable_or(\"HOME\", \"\")));
  0
}
"
        .to_string(),
    ));
    let root = SourceRoot::new(&db, files);
    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("reading the environment without `env` should be refused");
    let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("env")),
        "expected the missing capability to be named, got {messages:?}"
    );
}
