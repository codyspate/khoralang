#![cfg(feature = "llvm")]

//! `std::process`: a Khora program running another program.
//!
//! Against the real `std`, and against a real shell — these actually start
//! `cmd.exe` or `/bin/sh` and wait for it, because the thing worth testing is
//! the C boundary and a double would test the double. The commands are all
//! shell builtins (`echo`, `exit`, a counted loop) so nothing here depends on
//! what happens to be installed on the machine running it.
//!
//! The two tests that start nothing are the last two, and they are the point
//! of the capability: one substitutes a handler and the code under test cannot
//! tell, the other shows that without the row there is nothing to substitute.

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

    let output = Command::new(&exe).output().expect("the program should run");
    assert!(output.status.success(), "`{name}` exited with {:?}", output.status.code());
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// The head every program below shares.
///
/// `attempt` rather than `catch` throughout: `catch` has to name constructors
/// and be exhaustive over each type it names, which is three arms of noise in
/// a test whose subject is not the error type. These programs also have to
/// reach the end and print a live-object count whatever happened, so a `main`
/// with a `raises` row would turn a wrong answer into a crash.
const HEAD: &str = "module demo::main;
import std::core::{Result, attempt, print};
import std::process::{Completed, Process, ProcessError, checked_output};

extern fn khora_live_count() -> Int;

fn reason(error: ProcessError) -> String {
  match error {
    ProcessError::NotStarted(command) => \"not started: \" + command,
    ProcessError::NotText(command) => \"not text: \" + command,
    ProcessError::Failed(command, code) => \"failed \" + Int::to_string(code),
  }
}
";

/// A command that prints, and every byte of it comes back.
///
/// Bracketed so the newline the shell's `echo` adds is visible in the
/// assertion rather than something a reader has to remember. On Windows the
/// pipe is opened in text mode, which is why that is one byte here and not the
/// CRLF `cmd` actually wrote.
#[test]
fn a_command_that_prints_has_its_output_captured() {
    let out = run(
        "process_capture",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => process.capture(\"echo hello\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => \"[\" + done.text + \"] status \" + Int::to_string(done.status),
  }}
}}

export fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );
    assert_eq!(out, "[hello\n] status 0\n");
}

/// A command that fails says so with its status, and that is *not* an error.
///
/// `exit 3` is a builtin of both shells, so this needs nothing installed. The
/// distinction the whole module rests on is here: the shell ran, so there is
/// nothing to raise. Three is the answer.
#[test]
fn a_non_zero_exit_is_reported_as_a_status() {
    let out = run(
        "process_status",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => process.status(\"exit 3\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(code) => Int::to_string(code),
  }}
}}

export fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );
    assert_eq!(out, "3\n", "the shell ran and exited three; nothing was raised");
}

/// `checked_output` is the other reading of the same run, for the caller who
/// only wanted the text of something that was supposed to work.
#[test]
fn checked_output_turns_a_failing_command_into_an_error() {
    let out = run(
        "process_checked",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => checked_output(\"exit 7\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(text) => \"unexpectedly fine: \" + text,
  }}
}}

export fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );
    assert_eq!(out, "failed 7\n");
}

/// Output far past the point a recursion would have died at.
///
/// Fifty-four kilobytes, from a counted loop in the shell. `String::slice` used
/// to recurse once per byte and took the process out at around nine thousand,
/// so a per-chunk or per-line recursion here would be the same bug with a
/// bigger constant. The read is a `while` and the buffer doubles, and the exact
/// byte count is the assertion that no chunk was dropped or written twice.
#[test]
fn a_large_amount_of_output_neither_truncates_nor_overflows() {
    let loop_command = if cfg!(windows) {
        "for /l %i in (1,1,2000) do @echo abcdefghijklmnopqrstuvwxyz"
    } else {
        "i=0; while [ $i -lt 2000 ]; do echo abcdefghijklmnopqrstuvwxyz; i=$((i+1)); done"
    };
    let out = run(
        "process_large",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => process.capture(\"{loop_command}\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => Int::to_string(String::byte_length(done.text)),
  }}
}}

export fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );
    // Twenty-six letters and a newline, two thousand times.
    assert_eq!(out, "54000\n", "every chunk landed, and none of them landed twice");
}

/// Nothing left alive after two real subprocesses, one of which failed.
///
/// **The count goes into a local before anything is built for printing.** A
/// string literal being concatenated is itself a live object, so reading the
/// count inside the expression that formats it reports a leak that is not
/// there — a mistake this repository has made before.
#[test]
fn running_commands_leaks_nothing() {
    let out = run(
        "process_leaks",
        &format!(
            "{HEAD}
fn work() -> Int with {{ process: Process }} {{
  let captured = match attempt(fn () => process.capture(\"echo one\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => done.text,
  }};
  let refused = match attempt(fn () => checked_output(\"exit 9\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(text) => text,
  }};
  String::byte_length(captured) + String::byte_length(refused)
}}

export fn main() -> () {{
  let total = with {{ process: Process::real() }} {{ work() }};
  let live = khora_live_count();
  print(Int::to_string(total));
  print(Int::to_string(live))
}}
"
        ),
    );
    assert_eq!(out, "12\n0\n", "\"one\\n\" is four bytes and \"failed 9\" is eight; then the leak check");
}

/// **The seam.** A substituted handler, and the code under test cannot tell —
/// which is the whole reason this is a capability rather than two functions.
///
/// Note what the fake makes possible that a real subprocess does not: the
/// command line the caller built is a value the handler can look at, so a test
/// can assert that the right thing was asked for rather than only that
/// something plausible came back.
#[test]
fn a_test_can_substitute_a_handler_and_start_nothing() {
    let out = run(
        "process_double",
        &format!(
            "{HEAD}
/// Ordinary code. It has no idea whether a shell exists.
fn version() -> String with {{ process: Process }} {{
  match attempt(fn () => checked_output(\"git describe --tags\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(text) => text,
  }}
}}

export fn main() -> () {{
  with {{ process: handler for Process {{
    status: fn command => 0,
    capture: fn command => {{ status: 0, text: \"asked: \" + command }},
  }} }} {{
    print(version())
  }}
}}
"
        ),
    );
    assert_eq!(
        out, "asked: git describe --tags\n",
        "no subprocess ran, and the double saw the command line that was built"
    );
}

/// And the permission half: with no `Process` in the row there is nothing to
/// start anything with, and the compiler says so rather than the program
/// discovering it.
#[test]
fn nothing_starts_a_program_without_the_capability() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("process_denied");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let main = "module demo::main;
import std::core::{Result, attempt};
import std::process::{checked_output};

extern fn khora_print_int(value: Int);

fn main() -> Int {
  match attempt(fn () => checked_output(\"echo hi\")!) {
    Result::Err(error) => khora_print_int(0 - 1),
    Result::Ok(text) => khora_print_int(String::byte_length(text)),
  };
  0
}
";
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("running a command without `process` should be refused");
    let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("process")),
        "expected the missing capability to be named, got {messages:?}"
    );
}
