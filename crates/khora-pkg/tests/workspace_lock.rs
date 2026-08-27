//! One lockfile for a workspace, and one version of each dependency in it.
//!
//! The single-version rule is most of what makes a monorepo coherent rather
//! than a directory of projects, and it only works if resolving *any* member
//! resolves *every* member — otherwise two members quietly hold two revisions
//! of a shared package and nothing notices until one of them is deployed.
//! Roadmap 14.15.

use std::path::{Path, PathBuf};

use khora_pkg::{resolve, Store};

/// A workspace root with `packages/*` as its members.
fn workspace(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ws_lock").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("writing the root manifest");
    root
}

/// A package directory holding a manifest and one module.
fn package(at: &Path, name: &str, dependencies: &str) {
    std::fs::create_dir_all(at.join("src")).expect("a package directory");
    std::fs::write(
        at.join("khora.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\npublish = true\n{dependencies}"
        ),
    )
    .expect("writing a manifest");
    std::fs::write(
        at.join("src").join("lib.kh"),
        format!("module {name}::lib;\npub fn go() -> Int {{ 1 }}\n"),
    )
    .expect("writing a module");
}

#[test]
fn the_lockfile_lands_at_the_root_and_not_beside_the_member() {
    let root = workspace("at_the_root");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n",
    );

    let store = Store::open().expect("a store");
    let member = root.join("packages").join("alpha").join("khora.toml");
    resolve(&member, &store, false).expect("it resolves");

    assert!(root.join("khora.lock").is_file(), "the root should hold the lockfile");
    assert!(
        !root.join("packages").join("alpha").join("khora.lock").is_file(),
        "the member should not have one of its own"
    );
}

#[test]
fn the_lock_covers_the_workspace_even_when_one_member_is_built() {
    // `beta` is not in `alpha`'s graph at all. Its dependency still has to be
    // in the lock, or the lock is not a record of what the workspace resolves
    // to -- it is a record of whatever was built last.
    let root = workspace("covers_all");
    package(&root.join("vendor").join("one"), "one", "");
    package(&root.join("vendor").join("two"), "two", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\none = { path = \"../../vendor/one\" }\n",
    );
    package(
        &root.join("packages").join("beta"),
        "beta",
        "\n[dependencies]\ntwo = { path = \"../../vendor/two\" }\n",
    );

    let store = Store::open().expect("a store");
    let member = root.join("packages").join("alpha").join("khora.toml");
    let resolution = resolve(&member, &store, false).expect("it resolves");

    let locked: Vec<&str> =
        resolution.lockfile.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(locked, ["one", "two"], "the lock should cover both members");
}

#[test]
fn what_comes_back_is_only_what_the_member_reaches() {
    // The other half of the same test, and the one that matters more: handing
    // `alpha` every package in the workspace would compile `beta`'s
    // dependencies into it, and a package that builds only because a sibling
    // depends on something is one that stops building the day the sibling
    // stops.
    let root = workspace("only_reachable");
    package(&root.join("vendor").join("one"), "one", "");
    package(&root.join("vendor").join("two"), "two", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\none = { path = \"../../vendor/one\" }\n",
    );
    package(
        &root.join("packages").join("beta"),
        "beta",
        "\n[dependencies]\ntwo = { path = \"../../vendor/two\" }\n",
    );

    let store = Store::open().expect("a store");
    let member = root.join("packages").join("alpha").join("khora.toml");
    let resolution = resolve(&member, &store, false).expect("it resolves");

    let compiled: Vec<&str> = resolution.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(compiled, ["one"], "`beta`'s dependency should not reach `alpha`'s build");
}

#[test]
fn a_transitive_dependency_still_comes_back() {
    let root = workspace("transitive");
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

    let store = Store::open().expect("a store");
    let member = root.join("packages").join("alpha").join("khora.toml");
    let resolution = resolve(&member, &store, false).expect("it resolves");

    let mut compiled: Vec<&str> = resolution.packages.iter().map(|p| p.name.as_str()).collect();
    compiled.sort_unstable();
    assert_eq!(compiled, ["deep", "shallow"]);
}

#[test]
fn two_members_wanting_two_different_things_is_an_error() {
    // The whole point. Before one lock, each member resolved alone and each
    // was individually consistent.
    let root = workspace("disagree");
    package(&root.join("vendor").join("first"), "shared", "");
    package(&root.join("vendor").join("second"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/first\" }\n",
    );
    package(
        &root.join("packages").join("beta"),
        "beta",
        "\n[dependencies]\nshared = { path = \"../../vendor/second\" }\n",
    );

    let store = Store::open().expect("a store");
    let member = root.join("packages").join("alpha").join("khora.toml");
    let why = resolve(&member, &store, false)
        .expect_err("two revisions of one package should be refused")
        .to_string();
    assert!(why.contains("asked for twice and differently"), "{why}");
    assert!(why.contains("alpha") && why.contains("beta"), "both askers named: {why}");
}

#[test]
fn a_member_lockfile_left_behind_is_reported() {
    let root = workspace("stray");
    package(&root.join("vendor").join("shared"), "shared", "");
    let member_dir = root.join("packages").join("alpha");
    package(&member_dir, "alpha", "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n");
    std::fs::write(member_dir.join("khora.lock"), "version = 1\n").expect("a stale lockfile");

    let store = Store::open().expect("a store");
    let resolution =
        resolve(&member_dir.join("khora.toml"), &store, false).expect("it resolves");

    assert_eq!(resolution.stray_locks.len(), 1, "{:?}", resolution.stray_locks);
    assert!(member_dir.join("khora.lock").is_file(), "reported, not deleted");
}

#[test]
fn a_path_dependency_is_written_relative_to_the_lockfile() {
    // Reached through a member, a path dependency arrives as
    // `packages/alpha/../../vendor/shared`, which is correct, depends on which
    // member asked, and is a sentence nobody should read in a committed file.
    let root = workspace("relative");
    package(&root.join("vendor").join("shared"), "shared", "");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        "\n[dependencies]\nshared = { path = \"../../vendor/shared\" }\n",
    );

    let store = Store::open().expect("a store");
    let member = root.join("packages").join("alpha").join("khora.toml");
    let resolution = resolve(&member, &store, false).expect("it resolves");

    let entry = &resolution.lockfile.packages[0];
    assert_eq!(entry.path.as_deref(), Some("vendor/shared"), "{entry:?}");
}

#[test]
fn a_package_outside_any_workspace_keeps_its_own_lockfile() {
    let solo = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ws_lock").join("solo");
    let _ = std::fs::remove_dir_all(&solo);
    package(&solo.join("vendor").join("shared"), "shared", "");
    package(
        &solo.join("app"),
        "app",
        "\n[dependencies]\nshared = { path = \"../vendor/shared\" }\n",
    );

    let store = Store::open().expect("a store");
    resolve(&solo.join("app").join("khora.toml"), &store, false).expect("it resolves");

    assert!(solo.join("app").join("khora.lock").is_file(), "beside the package it describes");
}
