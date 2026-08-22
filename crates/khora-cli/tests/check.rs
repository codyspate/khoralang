//! `khora check` from the outside.
//!
//! The command spent its early life reporting only syntax errors while
//! announcing that it had "checked" the file, which is the worst possible
//! failure mode for a command named `check`: a clean exit on a broken program.
//! These tests run the real binary, because that gap was invisible to every
//! library-level test.

use std::path::PathBuf;
use std::process::Command;

/// Writes `source` to a scratch file and runs `khora check` over it.
fn check(name: &str, source: &str) -> (bool, String) {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.kh"));
    std::fs::write(&path, source).expect("could not write the fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("could not run `khora`");

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn a_correct_program_passes() {
    let (ok, output) = check(
        "good",
        "module m;\nfn double(x: Int) -> Int { x + x }\nexport fn main() -> Int { double(21) }\n",
    );
    assert!(ok, "expected success, got:\n{output}");
    assert!(output.contains("no errors"), "{output}");
}

#[test]
fn a_syntax_error_fails() {
    let (ok, output) = check("syntax", "module m;\nfn f( -> Int { 1 }\n");
    assert!(!ok, "a broken parse must not exit zero:\n{output}");
    assert!(output.contains("error"), "{output}");
}

/// The regression this file exists for.
#[test]
fn a_type_error_fails() {
    let (ok, output) = check("types", "module m;\nfn f() -> Int { true }\n");
    assert!(!ok, "a type error must not exit zero:\n{output}");
    assert!(
        output.contains("returns `Int`") && output.contains("`Bool`"),
        "expected the mismatch to be named, got:\n{output}"
    );
}

/// A type error is reported where it happened, not at the top of the file.
#[test]
fn a_type_error_points_at_the_offending_line() {
    let (_, output) = check(
        "span",
        "module m;\nfn a() -> Int { 1 }\nfn b() -> Int { false }\nfn c() -> Int { 3 }\n",
    );
    assert!(output.contains(":3:"), "expected a line 3 span, got:\n{output}");
    assert!(output.contains("^"), "expected a caret, got:\n{output}");
}

/// Nothing invented on top of a parse failure: one broken construct should not
/// produce a page of consequential type errors.
#[test]
fn a_file_that_does_not_parse_reports_only_syntax_errors() {
    let (ok, output) = check("only_syntax", "module m;\nfn f( -> Int { 1 }\n");
    assert!(!ok);
    assert!(!output.contains("this function returns"), "{output}");
}

// --- one target's files at a time -------------------------------------------

/// Two files declaring the same module, one per target, and only one of them
/// is ever read.
///
/// The rule is in the file's name — `khora_db::selected_for_target` — and this
/// is the test that `khora` itself applies it. Without the rule the two would
/// be a duplicate-module error; with it they are how a `std::net::socket`
/// exists on Windows and on POSIX at the same time.
#[test]
fn only_this_targets_files_are_read() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets");
    std::fs::create_dir_all(&dir).expect("a workspace");
    for name in ["which_windows.kh", "which_linux.kh", "which_macos.kh"] {
        let target = name
            .trim_start_matches("which_")
            .trim_end_matches(".kh")
            .to_string();
        std::fs::write(
            dir.join(name),
            format!("module t::which;\nexport fn which() -> Int {{ 1 }} // {target}\n"),
        )
        .expect("writing a fixture");
    }
    std::fs::write(
        dir.join("main.kh"),
        "module t::main;\nimport t::which::{which};\nexport fn main() -> Int { which() }\n",
    )
    .expect("writing a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "expected success, got:\n{text}");
    assert!(
        text.contains("checked 2 file(s)"),
        "two of the four files belong in this build, got:\n{text}"
    );
}

/// A file named on the command line is read whichever target it names. Asking
/// for a file by name is asking for it, and refusing would leave no way to
/// check the other target's version at all.
#[test]
fn a_file_named_outright_is_read_anyway() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets_named");
    std::fs::create_dir_all(&dir).expect("a workspace");
    // A name that cannot be this host's, whichever host that is.
    let other = if cfg!(windows) { "linux" } else { "windows" };
    let path = dir.join(format!("only_{other}.kh"));
    std::fs::write(&path, "module t;\nexport fn f() -> Int { 1 }\n").expect("a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "{text}");
    assert!(text.contains("checked 1 file(s)"), "{text}");
}
