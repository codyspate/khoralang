//! `khora sbom` from the outside.
//!
//! `docs/positioning.md` names an audit-heavy buyer, and a bill of materials is
//! the first artifact that buyer asks for. Roadmap 12.9.
//!
//! Run against the real binary rather than the renderer, because most of what
//! can go wrong here is between the two: finding the manifest, resolving the
//! dependencies rather than trusting a lockfile on disk, and putting the bytes
//! somewhere. The document's *shape* is tested in `khora-pkg`.

use std::path::PathBuf;
use std::process::Command;

/// A package directory with a manifest, a source file, and whatever
/// dependencies the caller names.
fn package(root: &std::path::Path, name: &str, version: &str, dependencies: &str) {
    std::fs::create_dir_all(root.join("src")).expect("a workspace");
    std::fs::write(
        root.join("khora.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\
             {dependencies}"
        ),
    )
    .expect("writing a manifest");
    std::fs::write(
        root.join("src").join("lib.kh"),
        format!("module {name}::lib;\npub fn go() -> Int {{ 1 }}\n"),
    )
    .expect("writing a source file");
}

fn sbom(dir: &std::path::Path, extra: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("sbom")
        .arg(dir)
        .args(extra)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A package with no dependencies still gets a document.
///
/// An empty bill of materials is a fact about the package. A tool that prints
/// nothing cannot be told apart from one that failed.
#[test]
fn a_package_with_no_dependencies_still_gets_a_document() {
    let dir = scratch("sbom_bare");
    package(&dir, "solo", "1.2.3", "");

    let (ok, out) = sbom(&dir, &[]);
    assert!(ok, "it should succeed: {out}");
    assert!(out.contains("\"bomFormat\": \"CycloneDX\""), "{out}");
    assert!(out.contains("\"name\": \"solo\""), "the root is named: {out}");
    assert!(out.contains("\"version\": \"1.2.3\""), "and versioned: {out}");
}

#[test]
fn a_dependency_becomes_a_component_and_an_edge() {
    let dir = scratch("sbom_with_dep");
    package(&dir.join("router"), "router", "0.2.0", "");
    package(
        &dir.join("app"),
        "app",
        "1.0.0",
        "\n[dependencies]\nrouter = { path = \"../router\" }\n",
    );

    let (ok, out) = sbom(&dir.join("app"), &[]);
    assert!(ok, "it should succeed: {out}");
    assert!(out.contains("\"bom-ref\": \"router\""), "the dependency is listed: {out}");
    // The edge, not just the list. A consumer asking what the application pulls
    // in reads the graph rather than inferring one.
    assert!(out.contains("\"dependsOn\": [\n        \"router\"\n      ]"), "{out}");
    // A path dependency has nothing immutable to hash, and the document says so
    // rather than leaving a reader to wonder why a component has no hash.
    assert!(out.contains("khora:unpinned"), "{out}");
}

/// **The same input twice is the same bytes.** There is no timestamp, which is
/// the point: §6.1 asks for reproducible builds, and a generated artifact that
/// embeds a clock cannot be diffed against the one before it.
#[test]
fn two_runs_over_unchanged_input_agree() {
    let dir = scratch("sbom_stable");
    package(&dir, "steady", "1.0.0", "");

    let one = dir.join("one.json");
    let two = dir.join("two.json");
    assert!(sbom(&dir, &["-o", one.to_str().expect("utf-8")]).0);
    assert!(sbom(&dir, &["-o", two.to_str().expect("utf-8")]).0);

    let one = std::fs::read(&one).expect("the first was written");
    let two = std::fs::read(&two).expect("the second was written");
    assert_eq!(one, two, "identical bytes, or it cannot be diffed");
    assert!(
        !String::from_utf8_lossy(&one).contains("timestamp"),
        "no clock in a reproducible artifact"
    );
}

/// Somewhere with no `khora.toml` is not a package, and the message says that
/// rather than producing an empty document about nothing.
#[test]
fn a_directory_that_is_not_a_package_is_refused() {
    let dir = scratch("sbom_no_manifest");
    std::fs::create_dir_all(&dir).expect("a workspace");

    let (ok, out) = sbom(&dir, &[]);
    assert!(!ok, "it should fail: {out}");
    assert!(out.contains("khora.toml"), "and say what is missing: {out}");
}

/// **`--locked` is a flag this command takes.**
///
/// `getting-started/installation.md` documents it, `locked_requested` reads
/// it, and the parser rejected it -- so the documented command did not run at
/// all. The refusal it buys is `khora-pkg`'s; what is tested here is that the
/// flag reaches it.
#[test]
fn locked_is_accepted() {
    let dir = scratch("sbom_locked");
    package(&dir, "audited", "1.0.0", "");

    let (ok, out) = sbom(&dir, &["--locked"]);
    assert!(ok, "`--locked` should be a flag rather than an error:\n{out}");
    assert!(!out.contains("unexpected argument"), "{out}");
    assert!(out.contains("CycloneDX"), "and it should still write the document:\n{out}");
}

/// And it refuses a lockfile that no longer matches the manifest, rather than
/// absorbing the difference into a document somebody may hand to an auditor.
#[test]
fn locked_refuses_a_stale_lockfile() {
    let dir = scratch("sbom_locked_stale");
    package(&dir.join("router"), "router", "0.2.0", "");
    package(
        &dir,
        "service",
        "1.0.0",
        "\n[dependencies]\nrouter = { path = \"router\" }\n",
    );

    // One clean run writes the lockfile the second is asked to trust.
    let (ok, out) = sbom(&dir, &[]);
    assert!(ok, "{out}");
    let lock = dir.join("khora.lock");
    let recorded = std::fs::read_to_string(&lock).expect("a lockfile");
    std::fs::write(&lock, recorded.replace("\"router\"", "\"router_gone\""))
        .expect("staling it");

    let (ok, out) = sbom(&dir, &["--locked"]);
    assert!(!ok, "a stale lockfile should be refused:\n{out}");
    assert!(out.contains("--locked"), "and the message should name the flag:\n{out}");
}
