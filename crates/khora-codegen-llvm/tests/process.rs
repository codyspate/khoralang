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

    let output = Command::new(&exe).output().expect("the program should run");
    assert!(output.status.success(), "`{name}` exited with {:?}", output.status.code());
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// A shell run *as a program*, which is how a test gets a chosen exit status
/// without one shell builtin being spelled two ways.
///
/// It also exercises the thing under test rather than working around it: the
/// flag and the command cross as a list of arguments, and nothing parses them
/// on the way.
fn failing(code: &str) -> String {
    let (program, flag) = if cfg!(windows) { ("cmd", "/c") } else { ("sh", "-c") };
    format!("\"{program}\", [\"{flag}\", \"exit {code}\"]")
}

/// The head every program below shares.
///
/// `attempt` rather than `catch` throughout: `catch` has to name constructors
/// and be exhaustive over each type it names, which is three arms of noise in
/// a test whose subject is not the error type. These programs also have to
/// reach the end and print a live-object count whatever happened, so a `main`
/// with a `raises` row would turn a wrong answer into a crash.
const HEAD: &str = "module demo::main;
import std::core::{List, Result, attempt, print};
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
  match attempt(fn () => process.shell(\"echo hello\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => \"[\" + done.text + \"] status \" + Int::to_string(done.status),
  }}
}}

pub fn main() -> () {{
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
  match attempt(fn () => process.shell(\"exit 3\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => Int::to_string(done.status),
  }}
}}

pub fn main() -> () {{
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
    let failing_seven = failing("7");
    let out = run(
        "process_checked",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => checked_output({failing_seven})!) {{
    Result::Err(error) => reason(error),
    Result::Ok(text) => \"unexpectedly fine: \" + text,
  }}
}}

pub fn main() -> () {{
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
  match attempt(fn () => process.shell(\"{loop_command}\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => Int::to_string(String::byte_length(done.text)),
  }}
}}

pub fn main() -> () {{
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
    let failing_nine = failing("9");
    let out = run(
        "process_leaks",
        &format!(
            "{HEAD}
fn work() -> Int with {{ process: Process }} {{
  let captured = match attempt(fn () => process.shell(\"echo one\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => done.text,
  }};
  let refused = match attempt(fn () => checked_output({failing_nine})!) {{
    Result::Err(error) => reason(error),
    Result::Ok(text) => text,
  }};
  String::byte_length(captured) + String::byte_length(refused)
}}

pub fn main() -> () {{
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
  match attempt(fn () => checked_output(\"git\", [\"describe\", \"--tags\"])!) {{
    Result::Err(error) => reason(error),
    Result::Ok(text) => text,
  }}
}}

pub fn main() -> () {{
  with {{ process: handler for Process {{
    run: fn (program, arguments) => 0,
    // The arguments are a *value* the handler can look at, which is the
    // assertion a real subprocess makes impossible to write: not that
    // something plausible came back, but that this is what was asked for.
    output: fn (program, arguments) =>
      {{ status: 0, text: \"asked: \" + program + \" \" + String::join(arguments, \" \") }},
    shell: fn command => {{ status: 0, text: \"asked a shell: \" + command }},
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
  match attempt(fn () => checked_output(\"echo\", [\"hi\"])!) {
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

/// **The reason the argument list exists**: punctuation in an argument stays
/// in the argument.
///
/// `a & b ; c` through a shell is three commands. Through `output` it is one
/// argument that happens to have an ampersand in it, because there is no shell
/// left to reinterpret it — which is what makes a file name, a search term or
/// anything else from outside the program safe to pass.
///
/// The two halves of this test are the same string down two paths, which is
/// the only way to say the difference is the path.
#[test]
fn punctuation_in_an_argument_is_not_a_second_command() {
    // **The two platforms need different argument lists to ask the same
    // question, and this asked `sh` the Windows one.** `cmd /c` takes what
    // follows as one command line, so `cmd /c echo "a & b ; c"` echoes the
    // punctuation. `sh -c` does not: the argument after the flag is the
    // *program text* and everything after it binds to `$0`, `$1`... -- so
    // `sh -c echo "a & b ; c"` runs `echo` with no arguments and prints an
    // empty line. The first half came back empty and the failure read
    // "one argument, punctuation and all: " with nothing after the colon,
    // which looks like a bug in `output` and is a bug in the test. It had
    // never passed on Linux or macOS.
    //
    // On POSIX the honest spelling is no shell at all, which is what the doc
    // comment above says is being demonstrated: hand `/bin/echo` one argument
    // and watch it come back whole. `sh -c` stays out of the half of the test
    // that is about there being no shell.
    let (program, arguments) = if cfg!(windows) {
        ("cmd", "[\"/c\", \"echo\", \"a & b ; c\"]")
    } else {
        ("/bin/echo", "[\"a & b ; c\"]")
    };
    let out = run(
        "process_injection",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  // One argument, whatever is in it.
  let safe = match attempt(fn () =>
    process.output(\"{program}\", {arguments})!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => String::trim(done.text),
  }};

  // The same text as part of a *line*, where the shell does read it.
  let through_a_shell = match attempt(fn () => process.shell(\"echo a & echo b\")!) {{
    Result::Err(error) => reason(error),
    Result::Ok(done) => String::trim(done.text),
  }};

  safe + \" | \" + String::replace(through_a_shell, \"\\n\", \"+\")
}}

pub fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );

    // The first half kept the ampersand and the semicolon; the second ran two
    // commands, which is the shell doing its job and the reason not to hand it
    // anything that came from outside.
    let (safe, shelled) = out.trim_end().split_once(" | ").expect("both halves");
    assert!(safe.contains('&') && safe.contains(';'), "one argument, punctuation and all: {safe}");
    assert!(!safe.contains('+'), "and one command, so no second line: {safe}");
    assert!(shelled.contains('+'), "the shell really does split on `&`: {shelled}");
}

/// A program that is not there is `NotStarted`, which is a different thing
/// from a program that ran and exited non-zero.
///
/// The distinction the whole module rests on, now that there is no shell in
/// between to blur it: `system` reported both as a number, and a shell that
/// cannot find a command starts perfectly well and exits 127.
#[test]
fn a_program_that_does_not_exist_never_started() {
    let out = run(
        "process_missing",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => process.run(\"khora-no-such-program-exists\", [])!) {{
    Result::Err(error) => reason(error),
    Result::Ok(code) => \"unexpectedly ran: \" + Int::to_string(code),
  }}
}}

pub fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );
    assert_eq!(out, "not started: khora-no-such-program-exists\n");
}

/// An exit status comes back as itself, through the argv path.
#[test]
fn an_exit_status_survives_the_argument_list() {
    let seven = failing("7");
    let out = run(
        "process_argv_status",
        &format!(
            "{HEAD}
fn work() -> String with {{ process: Process }} {{
  match attempt(fn () => process.run({seven})!) {{
    Result::Err(error) => reason(error),
    Result::Ok(code) => Int::to_string(code),
  }}
}}

pub fn main() -> () {{
  with {{ process: Process::real() }} {{ print(work()) }}
}}
"
        ),
    );
    assert_eq!(out, "7\n");
}
