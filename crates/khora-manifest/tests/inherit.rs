//! `field.workspace = true`.
//!
//! The resolved [`Manifest`] has no trace of where a value came from, which is
//! the design: a `version` is a `String`, so nothing downstream has to cope
//! with one that has not arrived. These tests are therefore all about the two
//! places the trace still exists — the *file*, and the *error* when a value
//! cannot be found. Roadmap 14.14.

use std::path::{Path, PathBuf};

use khora_manifest::{LintLevel, Manifest};

/// A scratch workspace root with `packages/*` as its members.
fn workspace(name: &str, root_body: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("inherit").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    std::fs::write(
        root.join("khora.toml"),
        format!("[workspace]\nmembers = [\"packages/*\"]\n{root_body}"),
    )
    .expect("writing the root manifest");
    root
}

/// Writes a member manifest and returns its path.
fn member(root: &Path, name: &str, body: &str) -> PathBuf {
    let directory = root.join("packages").join(name);
    std::fs::create_dir_all(&directory).expect("a member directory");
    let manifest = directory.join("khora.toml");
    std::fs::write(&manifest, body).expect("writing a member manifest");
    manifest
}

/// The message from loading a manifest that should not load.
fn failure(manifest: &Path) -> String {
    match Manifest::load(manifest) {
        Ok(_) => panic!("expected {} to fail to load", manifest.display()),
        Err(why) => why.to_string(),
    }
}

#[test]
fn a_member_takes_every_shared_field() {
    let root = workspace(
        "all_fields",
        "\n[workspace.package]\nversion = \"0.4.0\"\n\
         authors = [\"A Name <a@example.com>\"]\npublish = true\n",
    );
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion.workspace = true\n\
         authors.workspace = true\npublish.workspace = true\n",
    );

    let package = Manifest::load(&manifest)
        .expect("the member loads")
        .manifest
        .package
        .expect("a package");
    assert_eq!(package.name, "alpha");
    assert_eq!(package.version, "0.4.0");
    assert_eq!(package.authors, vec!["A Name <a@example.com>".to_string()]);
    assert_eq!(package.publish, Some(true));
}

#[test]
fn a_value_written_here_is_not_overwritten_by_the_root() {
    let root = workspace("own_wins", "\n[workspace.package]\nversion = \"0.4.0\"\n");
    let manifest = member(&root, "alpha", "[package]\nname = \"alpha\"\nversion = \"9.9.9\"\n");

    let package = Manifest::load(&manifest).expect("it loads").manifest.package.expect("a package");
    assert_eq!(package.version, "9.9.9");
}

#[test]
fn asking_a_root_for_a_field_it_does_not_set_names_both_halves() {
    let root = workspace("no_field", "\n[workspace.package]\nversion = \"0.4.0\"\n");
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion.workspace = true\npublish.workspace = true\n",
    );

    let why = failure(&manifest);
    assert!(why.contains("package.publish"), "{why}");
    assert!(why.contains("[workspace.package]"), "the fix should name the table: {why}");
}

#[test]
fn inheriting_with_no_workspace_above_says_so() {
    // Not a workspace at all: a lone package that copied a line from somewhere.
    //
    // Outside the repository, deliberately: `CARGO_TARGET_TMPDIR` is under
    // `target/`, which is under this repository's own workspace root, so a
    // fixture written there would find *that* one and the test would pass for
    // the wrong reason.
    let directory = std::env::temp_dir().join("khora-inherit-no-root");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    let manifest = directory.join("khora.toml");
    std::fs::write(&manifest, "[package]\nname = \"alpha\"\nversion.workspace = true\n")
        .expect("writing a manifest");

    let why = failure(&manifest);
    assert!(why.contains("no workspace root"), "{why}");
}

#[test]
fn being_under_a_root_is_not_being_in_it() {
    // The member patterns say `packages/*`, and this is not under `packages`.
    // Taking a version from a workspace you are not part of is the kind of
    // thing that is only noticed once it is published.
    let root = workspace("not_a_member", "\n[workspace.package]\nversion = \"0.4.0\"\n");
    let directory = root.join("tools");
    std::fs::create_dir_all(&directory).expect("a directory");
    let manifest = directory.join("khora.toml");
    std::fs::write(&manifest, "[package]\nname = \"tool\"\nversion.workspace = true\n")
        .expect("writing a manifest");

    let why = failure(&manifest);
    assert!(why.contains("does not list it as a member"), "{why}");
}

#[test]
fn workspace_false_is_refused_rather_than_read_as_no() {
    let root = workspace("false", "\n[workspace.package]\nversion = \"0.4.0\"\n");
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\npublish.workspace = false\n",
    );

    let why = failure(&manifest);
    assert!(why.contains("workspace = false"), "{why}");
}

#[test]
fn a_member_takes_the_lints_whole() {
    let root = workspace(
        "lints",
        "\n[workspace.lints]\nunused-capabilities = \"deny\"\ncyclomatic-complexity = \"warn\"\n",
    );
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n\n[lints]\nworkspace = true\n",
    );

    let lints = Manifest::load(&manifest).expect("it loads").manifest.lints;
    assert_eq!(lints["unused-capabilities"].level, LintLevel::Deny);
    assert_eq!(lints["cyclomatic-complexity"].level, LintLevel::Warn);
}

#[test]
fn a_lint_beside_the_flag_is_an_error_rather_than_a_silent_loss() {
    let root = workspace("lints_plus", "\n[workspace.lints]\nunused-capabilities = \"deny\"\n");
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n\n[lints]\nworkspace = true\n\
         cyclomatic-complexity = \"warn\"\n",
    );

    let why = failure(&manifest);
    assert!(why.contains("silently dropped"), "{why}");
}

#[test]
fn a_member_takes_the_permissions_whole() {
    let root = workspace(
        "permissions",
        "\n[workspace.permissions]\ndefault = \"deny\"\nnetwork = [\"api.example.com\"]\n",
    );
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n\n[permissions]\nworkspace = true\n",
    );

    let permissions = Manifest::load(&manifest).expect("it loads").manifest.permissions;
    assert_eq!(permissions.network.as_deref(), Some(["api.example.com".to_string()].as_slice()));
    assert!(!permissions.grants(khora_manifest::Category::Fs), "`default = \"deny\"` came too");
}

#[test]
fn a_grant_beside_the_flag_is_an_error() {
    let root = workspace("permissions_plus", "\n[workspace.permissions]\nenv = [\"PORT\"]\n");
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n\n[permissions]\nworkspace = true\n\
         network = [\"*\"]\n",
    );

    let why = failure(&manifest);
    assert!(why.contains("silently dropped"), "{why}");
}

#[test]
fn a_member_takes_the_formatter_settings() {
    let root = workspace("fmt", "\n[workspace.fmt]\nindent-width = 2\nindent-style = \"space\"\n");
    let manifest = member(
        &root,
        "alpha",
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n\n[fmt]\nworkspace = true\n",
    );

    let fmt = Manifest::load(&manifest).expect("it loads").manifest.fmt.expect("a `[fmt]` table");
    assert_eq!(fmt.indent_width, Some(2));
    assert_eq!(fmt.indent_style, Some(khora_manifest::IndentStyle::Space));
}

#[test]
fn parsing_without_a_path_refuses_to_guess() {
    // The language server's door. Text alone cannot find a root, and a made-up
    // version is worse than a refusal that says why.
    let why = Manifest::parse("[package]\nname = \"alpha\"\nversion.workspace = true\n")
        .expect_err("no root can be found from text")
        .to_string();
    assert!(why.contains("no workspace root"), "{why}");
}

#[test]
fn a_manifest_that_inherits_nothing_still_parses_from_text() {
    let parsed = Manifest::parse("[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n")
        .expect("a manifest that asks for nothing");
    assert_eq!(parsed.manifest.package.expect("a package").version, "1.0.0");
}

#[test]
fn the_root_tables_are_not_unknown_keys() {
    // The audit reads the document a second time against a schema written by
    // hand, so a table added to the model and not to the schema warns at every
    // manifest that uses it.
    let parsed = Manifest::parse(
        "[workspace]\nmembers = [\"packages/*\"]\n\n\
         [workspace.package]\nversion = \"0.4.0\"\nauthors = []\n\
         publish = false\n\n\
         [workspace.permissions]\ndefault = \"deny\"\n\n\
         [workspace.fmt]\nindent-width = 2\n\n\
         [workspace.lints]\nunused-capabilities = \"deny\"\n",
    )
    .expect("a workspace root");
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
}

#[test]
fn the_member_side_flags_are_not_unknown_keys_either() {
    let parsed = Manifest::parse(
        "[package]\nname = \"alpha\"\nversion = \"1.0.0\"\n\n\
         [permissions]\nworkspace = true\n\n\
         [fmt]\nworkspace = true\n",
    )
    .expect_err("`workspace = true` with no root");
    // It fails for the right reason -- no root -- rather than on an unknown
    // key, which is what this is really checking.
    assert!(parsed.to_string().contains("no workspace root"), "{parsed}");
}
