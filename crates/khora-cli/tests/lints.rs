//! `[lints]`: whether a finding is silent, a warning, or a failure.
//!
//! What each lint *finds* is `khora-lint`'s own test. This is the plumbing
//! between that and a manifest — which is where the interesting mistake lives,
//! because a build that prints `error:` and then exits zero teaches people that
//! the word means nothing.

use std::path::Path;
use std::process::Command;

/// A project whose one function trips both lints.
fn project(at: &Path, lints: &str) {
    std::fs::create_dir_all(at.join("src")).expect("a src directory");
    std::fs::write(
        at.join("khora.toml"),
        format!("[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n{lints}"),
    )
    .expect("writing the manifest");
    std::fs::write(
        at.join("src").join("main.kh"),
        "module demo::main;\n\n\
         export effect Clock {\n  now: () -> Int,\n}\n\n\
         fn area(r: Int) -> Int with { clock: Clock } {\n  r * r;\n  r * r\n}\n\n\
         export fn main() -> () {}\n",
    )
    .expect("writing the source");
}

/// Runs `khora check` and returns (succeeded, everything it printed).
fn check(lints: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    project(tmp.path(), lints);

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["check", tmp.path().join("src").join("main.kh").to_str().expect("a path")])
        .output()
        .expect("running khora");

    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Both lints are quiet enough to be worth hearing about and neither is worth
/// failing a build over, so an unmentioned lint warns.
#[test]
fn an_unmentioned_lint_warns_and_the_build_succeeds() {
    let (ok, output) = check("");
    assert!(ok, "a warning must not fail the build:\n{output}");
    assert!(output.contains("warning:"), "{output}");
    assert!(output.contains("[unused-capability]"), "{output}");
    assert!(output.contains("[dangling-expression]"), "{output}");
    assert!(output.contains("2 warning(s)"), "{output}");
}

/// The mistake worth a test of its own: a warning that prints as an error.
#[test]
fn a_warning_does_not_print_as_an_error() {
    let (_, output) = check("");
    assert!(
        !output.contains("error:"),
        "nothing here is an error, so nothing may say so:\n{output}"
    );
}

#[test]
fn deny_prints_an_error_and_fails_the_build() {
    let (ok, output) = check("[lints]\ndangling-expression = \"deny\"\n");
    assert!(!ok, "`deny` must fail the build:\n{output}");
    assert!(output.contains("error:"), "{output}");
    assert!(output.contains("[dangling-expression]"), "{output}");
}

#[test]
fn allow_says_nothing_at_all() {
    let (ok, output) = check(
        "[lints]\ndangling-expression = \"allow\"\nunused-capability = \"allow\"\n",
    );
    assert!(ok, "{output}");
    assert!(!output.contains("[dangling-expression]"), "{output}");
    assert!(!output.contains("[unused-capability]"), "{output}");
    assert!(output.contains("no errors"), "{output}");
}

/// One denied, one allowed. The levels are per lint, not a single switch.
#[test]
fn each_lint_has_its_own_level() {
    let (ok, output) = check(
        "[lints]\ndangling-expression = \"deny\"\nunused-capability = \"allow\"\n",
    );
    assert!(!ok, "{output}");
    assert!(output.contains("[dangling-expression]"), "{output}");
    assert!(!output.contains("[unused-capability]"), "the allowed one should be silent:\n{output}");
}

/// A file with no manifest anywhere is a scratch file, and `khora check
/// scratch.kh` has to work. Defaults apply and nothing complains about the
/// missing manifest.
#[test]
fn a_file_outside_any_package_still_checks() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let file = tmp.path().join("scratch.kh");
    std::fs::write(&file, "module scratch;\n\nfn f(x: Int) -> Int { x + 1; x }\n")
        .expect("writing");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["check", file.to_str().expect("a path")])
        .output()
        .expect("running khora");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(out.status.success(), "{text}");
    assert!(text.contains("[dangling-expression]"), "{text}");
    assert!(!text.contains("khora.toml"), "no complaint about a manifest nobody wrote:\n{text}");
}
