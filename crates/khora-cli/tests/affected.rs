//! `khora check . --since <rev>`.
//!
//! Every case here is a claim about *which members ran*, because the failure
//! mode of an affected-only build is silent: it skips something it should not
//! have, everything passes, and the thing it skipped is broken. Roadmap 14.16.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs git in `at`, with an identity on the command line so the test does not
/// need the machine to have one configured.
fn git(args: &[&str], at: &Path) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(at)
        .output()
        .expect("git on the path");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A committed workspace: two members, one of which depends on a vendored
/// package, and a lockfile already written and committed.
fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("affected").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");

    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("the root manifest");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n",
    );
    package(&root.join("packages").join("beta"), "beta", "");

    git(&["init", "--quiet", "-b", "main"], &root);
    // Resolve once before committing, so the lockfile is in the tree. An
    // uncommitted lockfile is itself a change, and would select everything --
    // correctly, and not what these tests are about.
    run(&root, &["check", "."]);
    git(&["add", "-A"], &root);
    git(&["commit", "--quiet", "-m", "start"], &root);
    root
}

fn package(at: &Path, name: &str, dependencies: &str) {
    std::fs::create_dir_all(at.join("src")).expect("a package directory");
    std::fs::write(
        at.join("khora.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\npublish = true\n{dependencies}"
        ),
    )
    .expect("a manifest");
    std::fs::write(
        at.join("src").join("lib.kh"),
        format!("module {name}::lib;\n\npub fn go() -> Int {{\n  1\n}}\n"),
    )
    .expect("a module");
}

fn run(at: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(at)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Appends a line, which is a change git can see and the compiler cannot.
fn touch(path: &Path) {
    let mut text = std::fs::read_to_string(path).expect("reading a fixture");
    text.push('\n');
    std::fs::write(path, text).expect("writing a fixture");
}

#[test]
fn a_change_in_one_member_runs_only_that_member() {
    let root = fixture("one_member");
    touch(&root.join("packages").join("beta").join("src").join("lib.kh"));

    let (ok, output) = run(&root, &["check", ".", "--since", "HEAD"]);
    assert!(ok, "{output}");
    assert!(output.contains("beta"), "{output}");
    assert!(output.contains("skipping"), "alpha should have been skipped: {output}");
    assert!(!output.contains("== check .\\packages\\alpha"), "alpha ran: {output}");
}

#[test]
fn a_change_in_a_dependency_runs_the_member_that_reaches_it() {
    // The half a heuristic gets wrong. `shared` is not a member; it is a
    // directory `alpha` compiles, and the resolver is what knows that.
    let root = fixture("dependency");
    touch(&root.join("vendor").join("shared").join("src").join("lib.kh"));

    let (ok, output) = run(&root, &["check", ".", "--since", "HEAD"]);
    assert!(ok, "{output}");
    assert!(output.contains("1 of 2 member(s) affected"), "{output}");
    assert!(output.contains("skipping"), "{output}");
    assert!(output.contains("beta"), "beta is the one skipped: {output}");
}

#[test]
fn a_change_nobody_owns_runs_everything_and_says_which_file() {
    // The rule that makes this safe to trust. A tool that answers "nothing was
    // affected" because it did not recognise a file is worse than no tool.
    let root = fixture("unowned");
    std::fs::write(root.join("build.sh"), "#!/bin/sh\necho hello\n").expect("a stray file");

    let (ok, output) = run(&root, &["check", ".", "--since", "HEAD"]);
    assert!(ok, "{output}");
    assert!(output.contains("every member"), "{output}");
    assert!(output.contains("build.sh"), "the file should be named: {output}");
    assert!(output.contains("2 member(s) clean"), "{output}");
}

#[test]
fn a_change_to_the_root_manifest_runs_everything() {
    let root = fixture("root_manifest");
    touch(&root.join("khora.toml"));

    let (_, output) = run(&root, &["check", ".", "--since", "HEAD"]);
    assert!(output.contains("every member"), "{output}");
    assert!(output.contains("khora.toml"), "{output}");
}

#[test]
fn nothing_changed_runs_nothing_and_says_so() {
    let root = fixture("nothing");

    let (ok, output) = run(&root, &["check", ".", "--since", "HEAD"]);
    assert!(ok, "an empty selection is not a failure: {output}");
    assert!(output.contains("no member is affected"), "{output}");
}

#[test]
fn an_untracked_source_file_counts_as_a_change() {
    // The change most likely to be the one being tested is the file nobody has
    // run `git add` on yet.
    let root = fixture("untracked");
    std::fs::write(
        root.join("packages").join("beta").join("src").join("more.kh"),
        "module beta::more;\n\npub fn extra() -> Int {\n  2\n}\n",
    )
    .expect("a new module");

    let (ok, output) = run(&root, &["check", ".", "--since", "HEAD"]);
    assert!(ok, "{output}");
    assert!(output.contains("1 of 2 member(s) affected"), "{output}");
}

#[test]
fn since_outside_a_workspace_says_what_it_is_for() {
    let root = fixture("not_a_root");
    let member = root.join("packages").join("beta");

    let (ok, output) = run(&member, &["check", ".", "--since", "HEAD"]);
    assert!(!ok, "{output}");
    assert!(output.contains("not a workspace root"), "{output}");
}

#[test]
fn fmt_narrows_the_same_way() {
    let root = fixture("fmt");
    touch(&root.join("packages").join("beta").join("src").join("lib.kh"));

    let (_, output) = run(&root, &["fmt", ".", "--check", "--since", "HEAD"]);
    assert!(output.contains("1 of 2 member(s) affected"), "{output}");
}
