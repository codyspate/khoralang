//! Finding a workspace and its members.
//!
//! Every case here is about *which directories are members*, because that is
//! the only question this layer answers and every one of its wrong answers is
//! quiet: a member left out is a package that stops being checked, and nothing
//! says so. Roadmap 14.13.

use std::path::{Path, PathBuf};

/// A scratch directory, unique to the test that asks for one.
fn scratch(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    root
}

/// Writes a package manifest into `directory`, creating it.
fn package(directory: &Path, name: &str) {
    std::fs::create_dir_all(directory).expect("a package directory");
    std::fs::write(
        directory.join("khora.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"),
    )
    .expect("writing a package manifest");
}

/// Writes a workspace root manifest into `directory`.
fn root(directory: &Path, body: &str) {
    std::fs::create_dir_all(directory).expect("a root directory");
    std::fs::write(directory.join("khora.toml"), format!("[workspace]\n{body}"))
        .expect("writing a root manifest");
}

/// The member directory names, sorted, for comparing against a literal.
fn names(members: &[PathBuf]) -> Vec<String> {
    members
        .iter()
        .map(|path| path.file_name().expect("a named directory").to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_package_manifest_is_not_a_workspace() {
    let root = scratch("not_a_workspace");
    package(&root, "solo");

    let found = khora_manifest::read_workspace(&root.join("khora.toml"))
        .expect("the manifest parses");
    assert!(found.is_none(), "a plain package should not read as a workspace");
}

#[test]
fn a_trailing_star_names_every_directory_under_it() {
    let dir = scratch("star");
    root(&dir, "members = [\"packages/*\"]\n");
    package(&dir.join("packages").join("alpha"), "alpha");
    package(&dir.join("packages").join("beta"), "beta");

    let found = khora_manifest::read_workspace(&dir.join("khora.toml"))
        .expect("the manifest parses")
        .expect("a workspace");
    assert_eq!(names(&found.members), vec!["alpha", "beta"]);
}

#[test]
fn a_directory_without_a_manifest_is_quietly_not_a_member() {
    // The case the doc comment promises: somebody leaves notes beside the
    // packages and the workspace keeps working.
    let dir = scratch("no_manifest");
    root(&dir, "members = [\"packages/*\"]\n");
    package(&dir.join("packages").join("alpha"), "alpha");
    std::fs::create_dir_all(dir.join("packages").join("notes")).expect("a stray directory");

    let found = khora_manifest::read_workspace(&dir.join("khora.toml"))
        .expect("the manifest parses")
        .expect("a workspace");
    assert_eq!(names(&found.members), vec!["alpha"]);
}

#[test]
fn exclude_removes_a_member_that_does_have_a_manifest() {
    let dir = scratch("exclude");
    root(&dir, "members = [\"packages/*\"]\nexclude = [\"packages/beta\"]\n");
    package(&dir.join("packages").join("alpha"), "alpha");
    package(&dir.join("packages").join("beta"), "beta");

    let found = khora_manifest::read_workspace(&dir.join("khora.toml"))
        .expect("the manifest parses")
        .expect("a workspace");
    assert_eq!(names(&found.members), vec!["alpha"]);
}

#[test]
fn a_member_named_twice_is_one_member() {
    // `packages/*` and `packages/alpha` overlap, which is what a workspace
    // writes when one member needs saying explicitly. Running it twice would
    // double every diagnostic it reports.
    let dir = scratch("twice");
    root(&dir, "members = [\"packages/*\", \"packages/alpha\"]\n");
    package(&dir.join("packages").join("alpha"), "alpha");

    let found = khora_manifest::read_workspace(&dir.join("khora.toml"))
        .expect("the manifest parses")
        .expect("a workspace");
    assert_eq!(names(&found.members), vec!["alpha"]);
}

#[test]
fn a_member_is_listed_by_name_when_no_pattern_fits() {
    let dir = scratch("by_name");
    root(&dir, "members = [\"tools/cli\"]\n");
    package(&dir.join("tools").join("cli"), "cli");

    let found = khora_manifest::read_workspace(&dir.join("khora.toml"))
        .expect("the manifest parses")
        .expect("a workspace");
    assert_eq!(names(&found.members), vec!["cli"]);
}

#[test]
fn a_member_finds_the_workspace_above_it() {
    // Upwards from the member: the member's own manifest is passed over rather
    // than stopping the walk.
    let dir = scratch("enclosing");
    root(&dir, "members = [\"packages/*\"]\n");
    let member = dir.join("packages").join("alpha");
    package(&member, "alpha");

    let found = khora_manifest::enclosing(&member).expect("a workspace above the member");
    assert_eq!(names(&found.members), vec!["alpha"]);
    assert_eq!(found.root.canonicalize().ok(), dir.canonicalize().ok());
}
