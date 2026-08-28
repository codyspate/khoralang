//! `khora check` and `khora fmt` over a workspace root.
//!
//! The feature exists because `scripts/baseline.sh` had a `for` loop over
//! `examples/*/` with a comment explaining that each example is its own package
//! and one walk over the directory would resolve one manifest for several
//! programs. These tests are about the two properties that made the loop worth
//! replacing rather than just moving: **every member runs**, and **a failure in
//! one does not hide the rest**. Roadmap 14.13.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A workspace root with `packages/*` as its members.
fn workspace(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("writing the root manifest");
    root
}

/// A member package holding one source file.
fn member(root: &Path, name: &str, source: &str) {
    let directory = root.join("packages").join(name);
    std::fs::create_dir_all(directory.join("src")).expect("a member directory");
    std::fs::write(
        directory.join("khora.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"),
    )
    .expect("writing a member manifest");
    std::fs::write(directory.join("src").join("lib.kh"), source)
        .expect("writing a member source file");
}

/// Runs `khora` with `args` in `root`, returning success and merged output.
fn run(root: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(root)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// A well-formed, correctly formatted module.
fn good(name: &str) -> String {
    format!("module {name}::lib;\n\npub fn go() -> Int {{\n    1\n}}\n")
}

#[test]
fn a_root_checks_every_member() {
    let root = workspace("ws_check_all");
    member(&root, "alpha", &good("alpha"));
    member(&root, "beta", &good("beta"));

    let (ok, output) = run(&root, &["check", "."]);
    assert!(ok, "expected a clean workspace, got:\n{output}");
    assert!(output.contains("alpha"), "alpha was never mentioned:\n{output}");
    assert!(output.contains("beta"), "beta was never mentioned:\n{output}");
    assert!(output.contains("2 member(s) clean"), "{output}");
}

#[test]
fn a_broken_member_does_not_stop_the_others() {
    // The loop this replaced stopped at the first failure, which made fixing a
    // monorepo a sequence of runs. The status still has to say it failed.
    let root = workspace("ws_check_broken");
    member(&root, "alpha", "module alpha::lib;\n\npub fn go() -> Int {\n    \"not an Int\"\n}\n");
    member(&root, "beta", &good("beta"));

    let (ok, output) = run(&root, &["check", "."]);
    assert!(!ok, "a type error should fail the run:\n{output}");
    assert!(output.contains("beta"), "beta was skipped after alpha failed:\n{output}");
    assert!(output.contains("1 of 2 member(s) failed"), "{output}");
}

#[test]
fn a_member_with_no_workspace_of_its_own_is_checked_directly() {
    // Only a root named *directly* fans out. `khora check .` inside one member
    // is a request about that member.
    let root = workspace("ws_check_member");
    member(&root, "alpha", &good("alpha"));
    member(&root, "beta", &good("beta"));

    let (ok, output) = run(&root.join("packages").join("alpha"), &["check", "."]);
    assert!(ok, "{output}");
    assert!(!output.contains("beta"), "checking one member reached another:\n{output}");
    assert!(!output.contains("member(s)"), "one package reported as a workspace:\n{output}");
}

#[test]
fn two_paths_are_never_fanned_out() {
    // `khora check a b` is a request about two things; fanning either of them
    // out would make the report incomprehensible.
    let root = workspace("ws_check_two");
    member(&root, "alpha", &good("alpha"));

    let (ok, output) = run(&root, &["check", ".", "packages/alpha"]);
    assert!(ok, "{output}");
    assert!(!output.contains("member(s)"), "two paths fanned out:\n{output}");
}

#[test]
fn a_root_formats_every_member() {
    // Deliberately not spelling out what canonical form *is*: this is a test
    // about reaching both members, and pinning the formatter's indent would
    // give it a second, unrelated reason to fail.
    let root = workspace("ws_fmt_all");
    member(&root, "alpha", "module alpha::lib;\npub fn go() -> Int {     1 }\n");
    member(&root, "beta", "module beta::lib;\npub fn  go() -> Int {  2  }\n");

    let (ok, output) = run(&root, &["fmt", ".", "--check"]);
    assert!(!ok, "unformatted members should fail --check:\n{output}");
    assert!(output.contains("2 of 2 member(s) failed"), "{output}");

    let (ok, output) = run(&root, &["fmt", "."]);
    assert!(ok, "{output}");
    assert!(output.contains("2 member(s) clean"), "{output}");

    let (ok, output) = run(&root, &["fmt", ".", "--check"]);
    assert!(ok, "formatting the workspace did not make it formatted:\n{output}");
}

#[test]
fn a_root_with_no_members_is_not_a_workspace_to_fan_out_over() {
    // An empty `members` is a root that has not been filled in yet. Falling
    // through to the ordinary walk is a better answer than reporting that
    // zero members were clean.
    let root = workspace("ws_empty");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = []\n")
        .expect("writing the root manifest");
    std::fs::create_dir_all(root.join("src")).expect("a source directory");
    std::fs::write(root.join("src").join("lib.kh"), good("root"))
        .expect("writing a source file");

    let (_, output) = run(&root, &["check", "."]);
    assert!(!output.contains("member(s)"), "an empty workspace fanned out:\n{output}");
}

// --- the one lockfile ------------------------------------------------------

/// A member that depends on another by path, so a resolution has something to
/// record and a lockfile exists at all.
fn member_needing(root: &Path, name: &str, needs: &str) {
    let directory = root.join("packages").join(name);
    std::fs::create_dir_all(directory.join("src")).expect("a member directory");
    std::fs::write(
        directory.join("khora.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n{needs} = {{ path = \"../{needs}\" }}\n"
        ),
    )
    .expect("writing a member manifest");
    std::fs::write(directory.join("src").join("lib.kh"), good(name))
        .expect("writing a member source file");
}

/// **A command aimed at a directory that is not a member must not empty the
/// workspace's lockfile.**
///
/// `resolve` took `manifest_path.parent()` as the workspace directory, and for
/// a manifest named without a directory that is `Some("")` rather than `None`
/// -- so the `unwrap_or(".")` beside it never ran. `""` does not canonicalize,
/// `same` fell back to comparing text, the manifest was judged not to be its
/// own workspace root, and the member seeding was skipped. What got written
/// was one manifest's dependencies, which for a workspace root is none.
///
/// In this repository `khora check std` discarded `postgres`. Against a git
/// dependency it would have discarded a pinned revision. Errata 57.
#[test]
fn a_command_outside_the_members_keeps_the_lockfile() {
    let root = workspace("lock_kept");
    member(&root, "beta", &good("beta"));
    member_needing(&root, "alpha", "beta");

    // A directory inside the workspace root that is *not* a member: it has no
    // manifest of its own, so the nearest one is the root's.
    let outside = root.join("notes");
    std::fs::create_dir_all(&outside).expect("a directory");
    std::fs::write(outside.join("stray.kh"), good("notes")).expect("a stray source");

    let (ok, output) = run(&root, &["check", "."]);
    assert!(ok, "{output}");
    let after_root = std::fs::read_to_string(root.join("khora.lock")).expect("a lockfile");
    assert!(after_root.contains("beta"), "the fixture has to lock something: {after_root}");

    let (ok, output) = run(&root, &["check", "notes"]);
    assert!(ok, "{output}");
    let after_stray = std::fs::read_to_string(root.join("khora.lock")).expect("a lockfile");

    assert_eq!(
        after_root, after_stray,
        "checking a directory that is not a member rewrote the workspace lockfile"
    );
}

/// And the lockfile a member's build writes is the one at the root -- which is
/// what "one lock, one version" means when two commands disagree about which
/// directory they were pointed at.
#[test]
fn a_member_and_the_root_agree_about_the_lockfile() {
    let root = workspace("lock_agree");
    member(&root, "beta", &good("beta"));
    member_needing(&root, "alpha", "beta");

    let (ok, output) = run(&root, &["check", "."]);
    assert!(ok, "{output}");
    let from_root = std::fs::read_to_string(root.join("khora.lock")).expect("a lockfile");

    let (ok, output) = run(&root, &["check", "packages/alpha"]);
    assert!(ok, "{output}");
    let from_member = std::fs::read_to_string(root.join("khora.lock")).expect("a lockfile");

    assert_eq!(from_root, from_member, "one workspace, one lockfile");
    assert!(
        !root.join("packages").join("alpha").join("khora.lock").is_file(),
        "a member must not grow a lockfile of its own"
    );
}
