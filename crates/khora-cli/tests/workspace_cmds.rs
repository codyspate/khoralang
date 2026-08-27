//! `khora new`, `khora why`, `khora graph`.
//!
//! Roadmap 14.21. Small commands, and the thing worth testing about each is
//! the same: that it tells the truth about a repository it did not write.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ws_cmds").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    root
}

/// A workspace root, with whatever shared table the caller wants.
fn workspace(name: &str, shared: &str) -> PathBuf {
    let root = scratch(name);
    std::fs::write(
        root.join("khora.toml"),
        format!("[workspace]\nmembers = [\"packages/*\"]\n{shared}"),
    )
    .expect("the root manifest");
    root
}

fn package(at: &Path, name: &str, extra: &str) {
    std::fs::create_dir_all(at.join("src")).expect("a package directory");
    std::fs::write(
        at.join("khora.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\npublish = true\n{extra}"),
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

#[test]
fn new_inside_a_sharing_workspace_inherits() {
    // The whole reason this command needs to exist rather than being a
    // copy-paste: a scaffold that hard-codes `version = "0.1.0"` into a
    // monorepo where everything else inherits has created the drift the
    // inheritance existed to prevent.
    let root = workspace(
        "new_inherits",
        "\n[workspace.package]\nversion = \"0.4.0\"\nedition = \"2026\"\n\
         \n[workspace.fmt]\nindent-width = 2\n",
    );

    let (ok, output) = run(&root, &["new", "packages/alpha"]);
    assert!(ok, "{output}");

    let manifest = std::fs::read_to_string(root.join("packages").join("alpha").join("khora.toml"))
        .expect("the new manifest");
    assert!(manifest.contains("version.workspace = true"), "{manifest}");
    assert!(manifest.contains("edition.workspace = true"), "{manifest}");
    assert!(manifest.contains("[fmt]\nworkspace = true"), "{manifest}");
    assert!(!manifest.contains("0.4.0"), "the value should not be copied: {manifest}");
}

#[test]
fn new_outside_a_workspace_writes_a_version() {
    let root = scratch("new_solo");

    let (ok, output) = run(&root, &["new", "solo"]);
    assert!(ok, "{output}");
    let manifest =
        std::fs::read_to_string(root.join("solo").join("khora.toml")).expect("the new manifest");
    assert!(manifest.contains("version = \"0.1.0\""), "{manifest}");
    assert!(!manifest.contains("workspace"), "{manifest}");
}

#[test]
fn new_scaffolds_a_program_by_default_and_a_library_on_request() {
    let root = scratch("new_kinds");

    run(&root, &["new", "app"]);
    assert!(root.join("app").join("src").join("main.kh").is_file());
    assert!(!std::fs::read_to_string(root.join("app").join("khora.toml"))
        .expect("a manifest")
        .contains("publish"));

    run(&root, &["new", "kit", "--lib"]);
    assert!(root.join("kit").join("src").join("lib.kh").is_file());
    assert!(std::fs::read_to_string(root.join("kit").join("khora.toml"))
        .expect("a manifest")
        .contains("publish = true"));
}

#[test]
fn what_new_writes_checks_and_is_already_formatted() {
    // A scaffold that produces a file the toolchain complains about is a
    // scaffold nobody trusts on the second day.
    let root = scratch("new_clean");
    run(&root, &["new", "fresh", "--lib"]);

    let (ok, output) = run(&root.join("fresh"), &["check", "."]);
    assert!(ok, "{output}");
    let (ok, output) = run(&root.join("fresh"), &["fmt", ".", "--check"]);
    assert!(ok, "{output}");
}

#[test]
fn new_refuses_a_name_that_is_not_an_identifier() {
    let root = scratch("new_bad_name");

    let (ok, output) = run(&root, &["new", "my-package"]);
    assert!(!ok, "{output}");
    assert!(output.contains("not a package name"), "{output}");
    assert!(!root.join("my-package").exists(), "nothing should have been created: {output}");
}

#[test]
fn new_refuses_a_directory_with_something_in_it() {
    let root = scratch("new_occupied");
    std::fs::create_dir_all(root.join("taken")).expect("a directory");
    std::fs::write(root.join("taken").join("notes.txt"), "mine").expect("a file");

    let (ok, output) = run(&root, &["new", "taken"]);
    assert!(!ok, "{output}");
    assert!(output.contains("not empty"), "{output}");
    assert!(root.join("taken").join("notes.txt").is_file(), "the file survived");
}

#[test]
fn new_says_when_the_workspace_will_not_pick_it_up() {
    let root = workspace("new_unlisted", "");

    let (ok, output) = run(&root, &["new", "tools/helper"]);
    assert!(ok, "creating it is still the right thing: {output}");
    assert!(output.contains("does not list this directory"), "{output}");
    assert!(output.contains("members"), "{output}");
}

#[test]
fn why_names_the_chain_and_not_just_the_package() {
    let root = workspace("why_chain", "");
    package(&root.join("vendor").join("deep"), "deep", "");
    package(
        &root.join("vendor").join("shallow"),
        "shallow",
        "\n[dependencies]\ndeep = { path = \"../deep\" }\n",
    );
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshallow = { path = \"../../vendor/shallow\" }\n",
    );

    let (ok, output) = run(&root.join("packages").join("alpha"), &["why", "deep"]);
    assert!(ok, "{output}");
    assert!(output.contains("alpha -> shallow -> deep"), "{output}");
}

#[test]
fn why_prints_every_reason_a_package_is_here() {
    // Printing one of three reasons is how somebody removes a dependency and
    // finds the package still there.
    let root = workspace("why_many", "");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("vendor").join("middle"),
        "middle",
        "\n[dependencies]\nshared = { path = \"../shared\" }\n",
    );
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nmiddle = { path = \"../../vendor/middle\" }\n\
         shared = { path = \"../../vendor/shared\" }\n",
    );

    let (ok, output) = run(&root.join("packages").join("alpha"), &["why", "shared"]);
    assert!(ok, "{output}");
    assert!(output.contains("alpha -> shared"), "the direct reason: {output}");
    assert!(output.contains("alpha -> middle -> shared"), "the indirect one: {output}");
}

#[test]
fn why_about_something_absent_says_what_is_present() {
    let root = workspace("why_absent", "");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n",
    );

    let (ok, output) = run(&root.join("packages").join("alpha"), &["why", "nonsense"]);
    assert!(!ok, "{output}");
    assert!(output.contains("not in this build"), "{output}");
    assert!(output.contains("shared"), "it should say what is: {output}");
}

#[test]
fn graph_draws_the_members_and_their_direct_dependencies() {
    let root = workspace("graph_tree", "");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n",
    );
    package(&root.join("packages").join("beta"), "beta", "");

    let (ok, output) = run(&root, &["graph"]);
    assert!(ok, "{output}");
    assert!(output.contains("alpha"), "{output}");
    assert!(output.contains("shared"), "{output}");
    assert!(output.contains("beta"), "{output}");
}

#[test]
fn graph_emits_dot_on_request() {
    let root = workspace("graph_dot", "");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n",
    );

    let (ok, output) = run(&root, &["graph", "--dot"]);
    assert!(ok, "{output}");
    assert!(output.contains("digraph khora {"), "{output}");
    assert!(output.contains("\"alpha\" -> \"shared\""), "{output}");
}

#[test]
fn graph_of_a_workspace_with_no_edges_says_so() {
    let root = workspace("graph_bare", "");
    package(&root.join("packages").join("alpha"), "alpha", "");

    let (ok, output) = run(&root, &["graph"]);
    assert!(ok, "{output}");
    assert!(output.contains("nothing depends on anything yet"), "{output}");
}
