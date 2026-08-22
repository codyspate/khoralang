#![cfg(feature = "llvm")]

//! `khora test`, end to end.
//!
//! Phase 5's exit criterion says the runner gives each test a fiber of its own,
//! which is the first thing anyone writes that is embarrassingly parallel — and
//! a test that only passes when it runs alone is a test that is lying. What
//! these pin is the visible half of that: every test runs, each one's verdict
//! is its own, and the suite's exit status is whether they all passed.

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run_tests(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "tests.exe" } else { "tests" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    if let Err(errors) = khora_codegen_llvm::compile_tests(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the suite should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

const PRELUDE: &str = "module t;
fn assert(condition: Bool);
fn print(value: Int);

export type Oops = | Bad;
fn halve(n: Int) -> Int raises Oops {
  if n % 2 == 0 { n / 2 } else { raise Oops::Bad }
}
";

/// A suite that passes says so, and exits 0.
#[test]
fn a_passing_suite_reports_every_test() {
    let ran = run_tests(
        "suite_pass",
        &format!(
            "{PRELUDE}
test \"halving an even number\" {{ assert(halve(8)! == 4); }}
test \"halving zero\" {{ assert(halve(0)! == 0); }}
"
        ),
    );
    assert!(ran.stdout.contains("test halving an even number ... ok"), "{:?}", ran.stdout);
    assert!(ran.stdout.contains("test halving zero ... ok"), "{:?}", ran.stdout);
    assert!(ran.stdout.contains("2 passed, 0 failed"), "{:?}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// A failed assertion ends the test it is in and nothing else. The verdict is
/// per test, which is what running them apart buys.
#[test]
fn a_failed_assertion_fails_only_its_own_test() {
    let ran = run_tests(
        "suite_fail",
        &format!(
            "{PRELUDE}
test \"this one is right\" {{ assert(halve(8)! == 4); }}
test \"this one is wrong\" {{ assert(halve(8)! == 5); print(99); }}
test \"this one is right too\" {{ assert(halve(4)! == 2); }}
"
        ),
    );
    assert!(ran.stdout.contains("test this one is wrong ... FAILED"), "{:?}", ran.stdout);
    assert!(ran.stdout.contains("test this one is right ... ok"), "{:?}", ran.stdout);
    assert!(ran.stdout.contains("test this one is right too ... ok"), "{:?}", ran.stdout);
    assert!(ran.stdout.contains("2 passed, 1 failed"), "{:?}", ran.stdout);
    assert!(!ran.stdout.contains("99"), "the failed test stopped at the assertion");
    assert_eq!(ran.code, Some(1), "a suite with a failure is a failing exit");
}

/// An error escaping a test is a failing test, not a program that does not
/// compile. A test's error row is open for exactly this.
#[test]
fn an_error_escaping_a_test_fails_it() {
    let ran = run_tests(
        "suite_raise",
        &format!(
            "{PRELUDE}
test \"an odd number has no half\" {{ assert(halve(7)! == 3); }}
"
        ),
    );
    assert!(ran.stdout.contains("... raised"), "{:?}", ran.stdout);
    assert!(ran.stdout.contains("0 passed, 1 failed"), "{:?}", ran.stdout);
    assert_eq!(ran.code, Some(1));
}

/// A program with no tests is not a failure.
#[test]
fn a_suite_with_no_tests_passes() {
    let ran = run_tests("suite_empty", PRELUDE);
    assert_eq!(ran.stdout, "no tests\n");
    assert_eq!(ran.code, Some(0));
}

/// Every test, and every one of them once. The count is the claim; which order
/// the fibers finish in is not something to assert.
#[test]
fn every_test_runs_exactly_once() {
    let ran = run_tests(
        "suite_many",
        &format!(
            "{PRELUDE}
test \"a\" {{ assert(halve(2)! == 1); }}
test \"b\" {{ assert(halve(4)! == 2); }}
test \"c\" {{ assert(halve(6)! == 3); }}
test \"d\" {{ assert(halve(8)! == 4); }}
"
        ),
    );
    assert_eq!(ran.stdout.matches("... ok").count(), 4, "{:?}", ran.stdout);
    assert!(ran.stdout.contains("4 passed, 0 failed"), "{:?}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// `assert` is a test's business. Outside one there is no test to fail, and
/// `raise` says the same thing while saying where it goes.
#[test]
fn assert_outside_a_test_is_refused() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("suite_assert_outside");
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let file = SourceFile::new(
        &db,
        dir.join("main.kh"),
        format!("{PRELUDE}fn main() -> Int {{ assert(1 == 1); 0 }}\n"),
    );
    let root = SourceRoot::new(&db, vec![file]);

    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("`assert` outside a test should be refused");
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("only allowed inside a `test` block")),
        "{messages:?}"
    );
}
