//! `[workspace.policy]`: a cap a member cannot opt out of.
//!
//! The failure mode of a cap is that it silently does not apply, so most of
//! these are about the cases where it would be tempting to let something
//! through: a name that matches nothing, a member that grants nothing, a
//! sibling being read for somebody else's build. Roadmap 14.19.

use std::path::{Path, PathBuf};

use khora_manifest::Manifest;

fn workspace(name: &str, policy: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("policy").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    std::fs::write(
        root.join("khora.toml"),
        format!("[workspace]\nmembers = [\"packages/*\"]\n{policy}"),
    )
    .expect("the root manifest");
    root
}

fn member(root: &Path, name: &str, permissions: &str) -> PathBuf {
    let directory = root.join("packages").join(name);
    std::fs::create_dir_all(&directory).expect("a member directory");
    let manifest = directory.join("khora.toml");
    std::fs::write(
        &manifest,
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{permissions}"),
    )
    .expect("a member manifest");
    manifest
}

fn failure(manifest: &Path) -> String {
    match Manifest::load(manifest) {
        Ok(_) => panic!("expected {} to be refused", manifest.display()),
        Err(why) => why.to_string(),
    }
}

#[test]
fn a_member_the_policy_does_not_list_may_not_grant() {
    let root = workspace("capped", "\n[workspace.policy]\nnetwork = [\"gateway\"]\n");
    member(&root, "gateway", "\n[permissions]\nnetwork = [\"0.0.0.0:8080\"]\n");
    let worker = member(&root, "worker", "\n[permissions]\nnetwork = [\"evil.example.com\"]\n");

    let why = failure(&worker);
    assert!(why.contains("`worker` is not allowed to grant `network`"), "{why}");
    assert!(why.contains("caps it to gateway"), "the fix needs to name who may: {why}");
}

#[test]
fn a_member_the_policy_lists_still_may() {
    let root = workspace("allowed", "\n[workspace.policy]\nnetwork = [\"gateway\"]\n");
    let gateway = member(&root, "gateway", "\n[permissions]\nnetwork = [\"0.0.0.0:8080\"]\n");

    let parsed = Manifest::load(&gateway).expect("the listed member loads");
    assert_eq!(
        parsed.manifest.permissions.network.as_deref(),
        Some(["0.0.0.0:8080".to_string()].as_slice())
    );
}

#[test]
fn a_category_the_policy_does_not_mention_is_uncapped() {
    // Tightening is opt-in, the same rule the per-package table follows: a
    // workspace that has not thought about the filesystem should not be
    // punished for it.
    let root = workspace("partial", "\n[workspace.policy]\nnetwork = [\"gateway\"]\n");
    member(&root, "gateway", "");
    let worker = member(&root, "worker", "\n[permissions.fs]\nread = [\"./data\"]\n");

    Manifest::load(&worker).expect("`fs` is not capped here");
}

#[test]
fn an_empty_list_caps_everybody() {
    let root = workspace("nobody", "\n[workspace.policy]\nnetwork = []\n");
    let worker = member(&root, "worker", "\n[permissions]\nnetwork = [\"example.com\"]\n");

    let why = failure(&worker);
    assert!(why.contains("no member at all"), "{why}");
}

#[test]
fn a_member_that_grants_nothing_is_not_refused() {
    let root = workspace("quiet", "\n[workspace.policy]\nnetwork = []\n");
    let worker = member(&root, "worker", "");

    Manifest::load(&worker).expect("a package that asks for nothing cannot exceed a cap");
}

#[test]
fn an_empty_grant_is_a_decision_and_not_a_request() {
    // `network = []` in a member is a package that has thought about the
    // network and decided on none. Refusing it would refuse a manifest that is
    // being careful.
    let root = workspace("empty_grant", "\n[workspace.policy]\nnetwork = []\n");
    let worker = member(&root, "worker", "\n[permissions]\nnetwork = []\n");

    Manifest::load(&worker).expect("granting nothing is not asking for something");
}

#[test]
fn a_policy_naming_a_package_that_is_not_there_is_refused() {
    // A typo in a cap is a cap that does not apply, and it fails *open*. So
    // the mistake has to be loud, at the first manifest that reads it.
    let root = workspace("typo", "\n[workspace.policy]\nnetwork = [\"getway\"]\n");
    let gateway = member(&root, "gateway", "\n[permissions]\nnetwork = [\"0.0.0.0:8080\"]\n");

    let why = failure(&gateway);
    assert!(why.contains("`getway` is not a member"), "{why}");
    assert!(why.contains("gateway"), "the real members should be listed: {why}");
}

#[test]
fn the_extern_key_is_capped_too() {
    // `[permissions] extern` is a package handing the door out to a
    // dependency. A member that may not extend it cannot extend it for
    // somebody else either.
    let root = workspace("extern", "\n[workspace.policy]\nextern = []\n");
    let worker = member(&root, "worker", "\n[permissions]\nextern = [\"anything\"]\n");

    let why = failure(&worker);
    assert!(why.contains("not allowed to grant `extern`"), "{why}");
}

#[test]
fn a_package_outside_the_workspace_is_not_capped_by_it() {
    let root = workspace("outside", "\n[workspace.policy]\nnetwork = []\n");
    let directory = root.join("tools");
    std::fs::create_dir_all(&directory).expect("a directory");
    let manifest = directory.join("khora.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n\n[permissions]\nnetwork = [\"*\"]\n",
    )
    .expect("a manifest");

    Manifest::load(&manifest).expect("not a member, so not capped");
}

#[test]
fn reading_a_sibling_for_a_resolution_does_not_enforce_the_cap() {
    // One lockfile means every member's manifest is read to build one member.
    // Enforcing the policy there would report a violation in `worker` while
    // somebody was building `gateway` -- and then report it again, correctly,
    // when `worker`'s own turn came.
    let root = workspace("sibling", "\n[workspace.policy]\nnetwork = [\"gateway\"]\n");
    let worker = member(&root, "worker", "\n[permissions]\nnetwork = [\"evil.example.com\"]\n");

    Manifest::load_for_resolution(&worker).expect("a sibling read is not a policy check");
    failure(&worker);
}

#[test]
fn parsing_from_text_alone_does_not_enforce_a_cap_it_cannot_find() {
    // `Manifest::parse` has no path, so it has no root, so there is no policy
    // to apply. Nothing to test but that it does not invent one.
    let parsed = Manifest::parse(
        "[package]\nname = \"worker\"\nversion = \"0.1.0\"\n\n[permissions]\nnetwork = [\"*\"]\n",
    )
    .expect("text with no workspace above it");
    assert!(parsed.manifest.permissions.network.is_some());
}

#[test]
fn the_policy_table_is_not_an_unknown_key() {
    let parsed = Manifest::parse(
        "[workspace]\nmembers = []\n\n[workspace.policy]\n\
         network = []\nfs = []\nenv = []\nextern = []\n",
    )
    .expect("a workspace root");
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
}
