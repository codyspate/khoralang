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
        "\n[workspace.package]\nversion = \"0.4.0\"\n\
         \n[workspace.fmt]\nindent-width = 2\n",
    );

    let (ok, output) = run(&root, &["new", "packages/alpha"]);
    assert!(ok, "{output}");

    let manifest = std::fs::read_to_string(root.join("packages").join("alpha").join("khora.toml"))
        .expect("the new manifest");
    assert!(manifest.contains("version.workspace = true"), "{manifest}");
    assert!(manifest.contains("[fmt]\nworkspace = true"), "{manifest}");
    assert!(!manifest.contains("0.4.0"), "the value should not be copied: {manifest}");
}

/// A package with nothing above it writes its own version and its own pin.
///
/// **The pin is the field a project cannot do without**, so the scaffold is
/// where a newcomer must first meet it: `[toolchain]` is required, and a
/// starting manifest that omitted it would fail the first command run against
/// it. It names the version doing the scaffolding, because that is the one
/// known to work with the project just written.
#[test]
fn new_outside_a_workspace_writes_a_version_and_a_pin() {
    // **Not `scratch`, which is under `CARGO_TARGET_TMPDIR` and therefore
    // inside this repository** -- whose root manifest pins a toolchain, which
    // the walk would find, which is the whole thing this test is about not
    // finding. "Outside a workspace" has to mean outside this one too.
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let root = tmp.path().to_path_buf();

    let (ok, output) = run(&root, &["new", "solo"]);
    assert!(ok, "{output}");
    let manifest =
        std::fs::read_to_string(root.join("solo").join("khora.toml")).expect("the new manifest");
    assert!(manifest.contains("version = \"0.1.0\""), "{manifest}");
    assert!(manifest.contains("[toolchain]"), "{manifest}");
    assert!(
        manifest.contains(&format!("version = \"{}\"", khora_toolchain::RUNNING)),
        "{manifest}"
    );
    assert!(!manifest.contains("workspace"), "{manifest}");
    assert!(!manifest.contains("edition"), "edition is gone: {manifest}");
}

/// A member does not repeat the pin the workspace above it already carries.
///
/// Two places to write one answer is two places for it to disagree, and the
/// walk that finds a pin passes through the member's manifest on its way to the
/// root -- so the member saying nothing is the member agreeing.
#[test]
fn new_inside_a_pinned_workspace_does_not_repeat_the_pin() {
    let root = scratch("new_pinned");
    std::fs::create_dir_all(root.join("packages")).expect("a workspace");
    std::fs::write(
        root.join("khora.toml"),
        "[workspace]\nmembers = [\"packages/*\"]\n\n[toolchain]\nversion = \"0.2.0\"\n",
    )
    .expect("a root manifest");

    let (ok, output) = run(&root, &["new", "packages/member"]);
    assert!(ok, "{output}");
    let manifest = std::fs::read_to_string(root.join("packages").join("member").join("khora.toml"))
        .expect("the new manifest");
    assert!(!manifest.contains("[toolchain]"), "the root already pins one: {manifest}");
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

/// **A note about what listing buys, not a warning that something is wrong.**
///
/// The old wording — "does not list this directory. Add it to `members`." —
/// read as an error, so the first thing a newcomer did was go and edit a file.
/// Nothing was broken: `build`, `check`, `test` and `run` all take a path and
/// all work on a package the workspace has never heard of, which the second
/// half of this test is here to say out loud. What membership changes is
/// whether the *workspace-wide* commands sweep it up, which is a choice rather
/// than a repair.
#[test]
fn new_says_what_listing_a_package_would_buy() {
    let root = workspace("new_unlisted", "");

    let (ok, output) = run(&root, &["new", "tools/helper"]);
    assert!(ok, "creating it is still the right thing: {output}");
    assert!(output.contains("works as it is"), "{output}");
    assert!(output.contains("members"), "{output}");

    // And it does work as it is, unlisted, which is the claim the line makes.
    let (checked, output) = run(&root, &["check", "tools/helper"]);
    assert!(checked, "an unlisted package should still check: {output}");
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

/// **A build lands beside the source, and nothing said so.**
///
/// A compiled program goes into `src/` next to the `.kh` it came from, with its
/// object file and, on Windows, its debug information — so the first
/// `git status` after a first build is three files nobody recognises. Nothing
/// documented it and nothing ignored it. This repository has carried the
/// patterns since somebody hit it here first; a package made with `khora new`
/// got to work them out again.
///
/// Written by the scaffold rather than documented, because a list of patterns
/// belongs in a file rather than in a paragraph somebody has to find and copy.
#[test]
fn new_writes_a_gitignore_for_what_a_build_leaves() {
    let root = scratch("new_ignore");
    run(&root, &["new", "tidy"]);

    let ignore = std::fs::read_to_string(root.join("tidy").join(".gitignore"))
        .expect("a .gitignore");
    assert!(ignore.contains("build/"), "the build directory should be ignored:\n{ignore}");
    // The source itself obviously must not be.
    assert!(!ignore.contains("*.kh"), "the source is the point:\n{ignore}");
}
