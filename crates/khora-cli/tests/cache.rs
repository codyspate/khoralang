//! The build cache, from outside.
//!
//! A build cache's failure mode is that it hands you the wrong artifact and
//! everything looks fine, so every test here is either "this misses when it
//! must" or "the thing it handed back is what a build would have produced".
//! Roadmap 14.17.
//!
//! Every test gets its own `KHORA_HOME`, passed to the child rather than set in
//! this process, so they run in parallel and none can see a real cache.
//!
//! These build for real and are therefore slow — a handful of seconds each.
//! That is the point: a test that mocked the compiler would be testing the
//! mock, and what is being claimed here is about actual bytes.
//!
//! # Why the runtime archive is pinned to a copy
//!
//! Because it moves. `pinned` has the whole story; the short version is that
//! `target/debug`'s archive flips between two files while other tests run, the
//! archive is in the cache key -- correctly, since a program linked against a
//! different runtime is a different program -- and so these tests kept
//! missing while the cache was right every time. Errata 51.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

mod pinned;

struct World {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    /// Everything every build in this test said.
    ///
    /// **The assertion message is the only evidence a flake leaves.** These
    /// tests failed intermittently under a full parallel run and passed alone,
    /// and the first three attempts to diagnose that were guesses because the
    /// message carried one build's output and the question was which of two
    /// keys disagreed. `KHORA_CACHE_EXPLAIN` prints the key's ingredients;
    /// this keeps all of them.
    story: std::cell::RefCell<String>,
}

impl World {
    /// Every build so far, for an assertion message.
    fn story(&self) -> String {
        self.story.borrow().clone()
    }
}

/// A one-module program and a private cache.
fn world(body: &str) -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");
    std::fs::write(project.join("khora.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n")
        .expect("a manifest");
    std::fs::write(project.join("src").join("main.kh"), body).expect("a source file");
    World { _tmp: tmp, home, project, story: std::cell::RefCell::new(String::new()) }
}

fn source(answer: i64) -> String {
    format!("module app::main;\n\npub fn main() -> Int {{\n  {answer}\n}}\n")
}

fn khora(w: &World, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if let Some(pinned) = pinned::runtime() {
        command.env("KHORA_RT_LIB", pinned);
    }
    let out = command
        .args(args)
        .current_dir(&w.project)
        .env("KHORA_HOME", &w.home)
        // So that a failure says *why* the cache missed rather than only
        // that it did. These tests are the reason `Miss` has a `Display`.
        .env("KHORA_CACHE_EXPLAIN", "1")
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    w.story.borrow_mut().push_str(&format!("\n--- khora {}\n{text}", args.join(" ")));
    (out.status.success(), text)
}

/// [`khora`], somewhere other than `w.project`.
///
/// Only the test with two projects in one home needs it, and that test is the
/// whole reason the cache's miss classification is a per-target question
/// rather than a per-machine one.
fn khora_in(w: &World, cwd: &Path, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if let Some(pinned) = pinned::runtime() {
        command.env("KHORA_RT_LIB", pinned);
    }
    let out = command
        .args(args)
        .current_dir(cwd)
        .env("KHORA_HOME", &w.home)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    w.story.borrow_mut().push_str(&text);
    (out.status.success(), text)
}

/// Builds, and says whether the cache answered.
fn build(w: &World, extra: &[&str]) -> (bool, String) {
    let mut args = vec!["build", "."];
    args.extend_from_slice(extra);
    khora(w, &args)
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("reading an artifact")
}

/// Where `khora build .` puts an executable in these fixtures.
///
/// `build/` rather than `src/`, and the package's name rather than the entry
/// file's: a build's output stopped landing beside the source it came from.
fn artifact(w: &World) -> PathBuf {
    w.project.join("build").join(format!("app{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn a_second_build_of_the_same_thing_is_reused() {
    let w = world(&source(1));

    let (ok, first) = build(&w, &[]);
    assert!(ok, "{}", w.story());
    assert!(first.contains("built"), "the first build should be a real one: {}", w.story());

    let (ok, second) = build(&w, &[]);
    assert!(ok, "{}", w.story());
    assert!(second.contains("reused"), "{}", w.story());
}

/// **A second project's first build says nothing about a key that moved.**
///
/// The cache is one directory for the whole machine, and the miss used to be
/// classified by asking whether that directory was empty. So the first build
/// of a second project -- the ordinary case for anyone who has built anything
/// before -- opened with `the key moved. Nothing is stored under this one, and
/// the cache holds 1751 other(s)`, in which nothing had moved, no input had
/// changed, and the keys listed belonged to somebody else.
///
/// Found by depending on `packages/postgres` from a project outside this
/// repository, which is the first thing a new user does.
#[test]
fn a_new_project_on_a_used_cache_is_a_quiet_first_build() {
    let w = world(&source(1));
    let (ok, first) = build(&w, &[]);
    assert!(ok, "{}", w.story());
    assert!(first.contains("built"), "{}", w.story());

    // A second project, its own directory and manifest, the same `KHORA_HOME`.
    let other = w.project.parent().expect("a parent").join("other");
    std::fs::create_dir_all(other.join("src")).expect("a src directory");
    std::fs::write(
        other.join("khora.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n",
    )
    .expect("a manifest");
    std::fs::write(
        other.join("src").join("main.kh"),
        "module other::main;\n\npub fn main() -> Int {\n  2\n}\n",
    )
    .expect("a source file");

    let (ok, output) = khora_in(&w, &other, &["build", "."]);
    assert!(ok, "{output}");
    assert!(output.contains("built"), "it still has to build: {output}");
    assert!(
        !output.contains("cache miss"),
        "a project's first build must not report a miss it cannot have had:\n{output}"
    );
}

/// The other half of the same change: a key that really did move still says so,
/// and names what this target built under before.
///
/// Without this the fix above is indistinguishable from deleting the message.
#[test]
fn a_key_that_moves_on_one_target_still_reports_it() {
    let w = world(&source(1));
    build(&w, &[]);

    std::fs::write(w.project.join("src").join("main.kh"), source(2)).expect("an edit");
    let (ok, output) = build(&w, &[]);
    assert!(ok, "{output}");
    assert!(
        output.contains("the key moved") && output.contains("last built under"),
        "an edited target's key moved and the message should name the old key:\n{output}"
    );
}

/// A cleared cache is its own case, and not an anomaly.
///
/// It reads as one otherwise: the key is right, the target has built under it,
/// and there is simply nothing there. Saying `the key moved` would send
/// somebody looking for an input that changed, and none did.
#[test]
fn an_emptied_cache_says_the_entry_is_gone_rather_than_that_the_key_moved() {
    let w = world(&source(1));
    build(&w, &[]);

    let (ok, _) = khora(&w, &["cache", "--clear"]);
    assert!(ok, "{}", w.story());

    let (ok, output) = build(&w, &[]);
    assert!(ok, "{output}");
    assert!(
        output.contains("the entry for this key is gone"),
        "an emptied cache is eviction, not a key that moved:\n{output}"
    );
}

#[test]
fn editing_a_source_misses() {
    let w = world(&source(1));
    build(&w, &[]);

    std::fs::write(w.project.join("src").join("main.kh"), source(2)).expect("an edit");
    let (ok, output) = build(&w, &[]);
    assert!(ok, "{output}");
    assert!(output.contains("built"), "a changed program must not be reused: {output}");
}

#[test]
fn a_change_and_a_change_back_hits_again() {
    // The key is the content, not a timestamp or a generation counter. Undoing
    // an edit should cost nothing, and a cache keyed on mtime would rebuild.
    let w = world(&source(1));
    let (_, first) = build(&w, &[]);
    std::fs::write(w.project.join("src").join("main.kh"), source(2)).expect("an edit");
    let (_, edited) = build(&w, &[]);
    std::fs::write(w.project.join("src").join("main.kh"), source(1)).expect("undone");

    let (ok, output) = build(&w, &[]);
    let _ = (first, edited);
    assert!(ok, "{}", w.story());
    assert!(output.contains("reused"), "{}", w.story());
}

#[test]
fn the_profile_is_in_the_key() {
    let w = world(&source(1));
    build(&w, &[]);

    let (ok, output) = build(&w, &["--release"]);
    assert!(ok, "{}", w.story());
    assert!(output.contains("built"), "a release build is not a debug build: {}", w.story());

    let (ok, output) = build(&w, &["--release"]);
    assert!(output.contains("reused"), "{}", w.story());
    assert!(ok);
}

#[test]
fn debug_information_is_in_the_key() {
    // `KHORA_DEBUG` overrides the profile in both directions, so it changes the
    // output without changing the profile's name. A key that only carried the
    // name would hand back the wrong artifact.
    let w = world(&source(1));
    build(&w, &[]);

    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if let Some(pinned) = pinned::runtime() {
        command.env("KHORA_RT_LIB", pinned);
    }
    let out = command
        .args(["build", "."])
        .current_dir(&w.project)
        .env("KHORA_HOME", &w.home)
        .env("KHORA_DEBUG", "0")
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("built"), "debug information off is a different build: {text}");
}

#[test]
fn a_release_hit_is_byte_for_byte_what_a_build_would_have_produced() {
    // **The claim the whole feature rests on.** 13.10 made release builds
    // bit-for-bit reproducible, so a hit is not "an artifact from the same
    // inputs" -- it is provably the same bytes, and this is the proof rather
    // than the assertion.
    let w = world(&source(7));
    build(&w, &["--release"]);
    let reused = w.project.join("reused.exe");
    let fresh = w.project.join("fresh.exe");

    let (ok, output) = build(&w, &["--release", "-o", reused.to_str().expect("a path")]);
    assert!(ok, "{output}");
    assert!(output.contains("reused"), "{output}");

    let (ok, output) =
        build(&w, &["--release", "--no-cache", "-o", fresh.to_str().expect("a path")]);
    assert!(ok, "{output}");
    assert!(output.contains("built"), "{output}");

    assert_eq!(bytes(&reused), bytes(&fresh), "a release hit must be the same bytes");
}

#[test]
fn no_cache_rebuilds_and_still_records() {
    let w = world(&source(1));
    build(&w, &[]);

    let (ok, output) = build(&w, &["--no-cache"]);
    assert!(ok, "{}", w.story());
    assert!(output.contains("built"), "{}", w.story());

    let (ok, output) = build(&w, &[]);
    assert!(ok, "{}", w.story());
    assert!(output.contains("reused"), "the entry should still be there: {}", w.story());
}

#[test]
fn a_reused_artifact_actually_runs() {
    // A cache that produces a file of the right length and the wrong contents
    // passes every test above. This one runs it.
    let w = world(&source(42));
    build(&w, &[]);
    let (ok, output) = build(&w, &[]);
    assert!(ok && output.contains("reused"), "{}", w.story());

    let status = Command::new(artifact(&w)).status().expect("running the artifact");
    assert_eq!(status.code(), Some(42), "the reused executable should still be the program");
}

#[test]
fn a_corrupt_entry_is_a_miss_rather_than_a_bad_artifact() {
    // The reason a hit re-hashes what it is about to hand over. A rename is
    // atomic so this should not happen, and "should not" is what a cache says
    // right before it hands somebody a truncated binary.
    let w = world(&source(1));
    build(&w, &[]);

    let entries = w.home.join("cache").join("build");
    let entry = std::fs::read_dir(&entries)
        .expect("the cache directory")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("one entry");
    std::fs::write(entry.join("artifact"), b"not an executable").expect("corrupting it");

    let (ok, output) = build(&w, &[]);
    assert!(ok, "{output}");
    assert!(output.contains("built"), "a corrupt entry must not be served: {output}");
}

#[test]
fn clear_empties_it_and_says_how_much() {
    let w = world(&source(1));
    build(&w, &[]);

    let (ok, listing) = khora(&w, &["cache"]);
    assert!(ok, "{listing}");
    assert!(listing.contains("1 entr"), "{listing}");

    let (ok, cleared) = khora(&w, &["cache", "--clear"]);
    assert!(ok, "{cleared}");
    assert!(cleared.contains("cleared 1 entr"), "{cleared}");

    let (ok, output) = build(&w, &[]);
    assert!(ok, "{output}");
    assert!(output.contains("built"), "an emptied cache has nothing to reuse: {output}");
}

#[test]
fn a_library_build_caches_its_header_too() {
    let w = world("module app::lib;\n\npub extern fn answer() -> Int {\n  7\n}\n");

    let (ok, _first) = build(&w, &["--lib"]);
    assert!(ok, "{}", w.story());
    let (ok, second) = build(&w, &["--lib"]);
    assert!(ok, "{}", w.story());
    assert!(second.contains("reused"), "{}", w.story());

    let header = w.project.join("build").join("app.h");
    assert!(header.is_file(), "the header should come back with the library");
    assert!(
        std::fs::read_to_string(&header).expect("the header").contains("answer"),
        "and it should be the real one"
    );
}
