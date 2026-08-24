//! Resolution, hashing, the lockfile and the store.
//!
//! The git tests build a real repository in a temporary directory and resolve
//! against it over a `file://` URL. That exercises everything except the
//! network transport — `git ls-remote`, a shallow fetch, the checkout, the
//! hash, the store, and the lockfile round trip — and it does so without
//! needing anything to exist on the internet, which is what makes it a test
//! rather than a liability.

use std::path::{Path, PathBuf};
use std::process::Command;

use khora_pkg::{resolve, Lockfile, Store};

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a parent directory");
    }
    std::fs::write(path, text).expect("writing a file");
}

/// Runs git in `cwd`, with an identity supplied on the command line.
///
/// The `-c` settings come *before* the subcommand, which is the only place git
/// accepts them. Supplying them at all is so the test does not depend on the
/// machine having a `user.email` configured, which a CI runner does not.
fn git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git on the path");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `git init`, add everything, commit. The three lines every fixture repeats.
fn commit_all(at: &Path) {
    git(&["init", "--quiet", "-b", "main"], at);
    git(&["add", "-A"], at);
    git(&["commit", "--quiet", "-m", "first"], at);
}

/// The full commit id of `HEAD`.
fn head(at: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(at)
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `file://` URL for a directory, with the forward slashes git wants on
/// every platform.
fn url_of(at: &Path) -> String {
    format!("file:///{}", at.display().to_string().replace('\\', "/"))
}

/// A package with one module, committed, and the URL to reach it by.
fn package_repo(at: &Path, name: &str, body: &str) -> (String, String) {
    std::fs::create_dir_all(at).expect("a directory");
    write(
        &at.join("khora.toml"),
        &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    );
    write(&at.join("src").join(format!("{name}.kh")), body);
    commit_all(at);
    (url_of(at), head(at))
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    store: Store,
}

fn world() -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).expect("the app directory");
    let store = Store::at(tmp.path().join("store")).expect("a store");
    World { _tmp: tmp, root, store }
}

// --- hashing ---------------------------------------------------------------

#[test]
fn the_same_tree_hashes_the_same_twice() {
    let w = world();
    write(&w.root.join("khora.toml"), "[package]\nname = \"a\"\nversion = \"0.1.0\"\n");
    write(&w.root.join("src").join("a.kh"), "module a;\n");

    let first = khora_pkg::hash_tree(&w.root).expect("a hash");
    let second = khora_pkg::hash_tree(&w.root).expect("a hash");
    assert_eq!(first, second);
}

#[test]
fn changing_a_source_file_changes_the_hash() {
    let w = world();
    write(&w.root.join("khora.toml"), "[package]\nname = \"a\"\nversion = \"0.1.0\"\n");
    let source = w.root.join("src").join("a.kh");
    write(&source, "module a;\n");
    let before = khora_pkg::hash_tree(&w.root).expect("a hash");

    write(&source, "module a;\nexport fn f() -> Int = { 1 }\n");
    assert_ne!(before, khora_pkg::hash_tree(&w.root).expect("a hash"));
}

/// The separator and length prefix in `hash::tree` exist for this: without
/// them, moving bytes between a path and its contents is invisible.
#[test]
fn renaming_a_file_changes_the_hash() {
    let w = world();
    write(&w.root.join("khora.toml"), "[package]\nname = \"a\"\nversion = \"0.1.0\"\n");
    write(&w.root.join("src").join("a.kh"), "module a;\n");
    let before = khora_pkg::hash_tree(&w.root).expect("a hash");

    std::fs::rename(w.root.join("src").join("a.kh"), w.root.join("src").join("b.kh"))
        .expect("renaming");
    assert_ne!(before, khora_pkg::hash_tree(&w.root).expect("a hash"));
}

/// A README is not part of the package as far as a build is concerned, and a
/// lockfile that churned when one changed would be a lockfile nobody reads.
#[test]
fn a_file_no_build_reads_does_not_change_the_hash() {
    let w = world();
    write(&w.root.join("khora.toml"), "[package]\nname = \"a\"\nversion = \"0.1.0\"\n");
    write(&w.root.join("src").join("a.kh"), "module a;\n");
    let before = khora_pkg::hash_tree(&w.root).expect("a hash");

    write(&w.root.join("README.md"), "# a\n");
    assert_eq!(before, khora_pkg::hash_tree(&w.root).expect("a hash"));
}

// --- resolution ------------------------------------------------------------

#[test]
fn a_git_dependency_is_fetched_hashed_and_locked() {
    let w = world();
    let (url, revision) = package_repo(&w.root.parent().unwrap().join("dep"), "dep", "module dep;\n");

    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ndep = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );

    let resolution =
        resolve(&w.root.join("khora.toml"), &w.store, false).expect("resolution");

    assert_eq!(resolution.packages.len(), 1);
    let dep = &resolution.packages[0];
    assert_eq!(dep.name, "dep");
    assert!(dep.directory.join("src").join("dep.kh").is_file(), "the module should be there");
    assert!(dep.directory.starts_with(w.store.root()), "it should live in the store");

    let locked = resolution.lockfile.get("dep").expect("an entry");
    assert_eq!(locked.source, "git");
    assert_eq!(
        locked.revision.as_deref(),
        Some(revision.as_str()),
        "a branch name must be resolved to a commit id before it is written down"
    );
    assert!(locked.checksum.is_some(), "a git package is pinned by content too");
    assert!(w.root.join("khora.lock").is_file(), "the lockfile should be written");
}

/// The second resolution must decide the same thing and touch nothing. A
/// package manager that rewrites its lockfile on every build has not locked
/// anything.
#[test]
fn resolving_twice_changes_nothing() {
    let w = world();
    let (url, _) = package_repo(&w.root.parent().unwrap().join("dep"), "dep", "module dep;\n");
    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ndep = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );

    let first = resolve(&w.root.join("khora.toml"), &w.store, false).expect("resolution");
    assert!(first.changed, "the first resolution writes the lockfile");

    let text = std::fs::read_to_string(w.root.join("khora.lock")).expect("the lockfile");
    let second = resolve(&w.root.join("khora.toml"), &w.store, false).expect("resolution");

    assert!(!second.changed, "the second should decide the same thing");
    assert_eq!(text, std::fs::read_to_string(w.root.join("khora.lock")).expect("the lockfile"));
}

/// `--locked` is what CI wants: a build needing a new resolution is a build
/// whose lockfile was not committed.
#[test]
fn locked_refuses_to_add_a_dependency() {
    let w = world();
    let (url, _) = package_repo(&w.root.parent().unwrap().join("dep"), "dep", "module dep;\n");
    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ndep = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );

    let error = resolve(&w.root.join("khora.toml"), &w.store, true).expect_err("should refuse");
    assert!(
        format!("{error:#}").contains("lockfile"),
        "the message should say what is wrong: {error:#}"
    );
}

/// The reason a content hash is kept as well as a commit id: they can disagree,
/// and if they ever do the build must stop rather than compile what arrived.
#[test]
fn a_checksum_that_does_not_match_stops_the_build() {
    let w = world();
    let (url, _) = package_repo(&w.root.parent().unwrap().join("dep"), "dep", "module dep;\n");
    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ndep = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );
    resolve(&w.root.join("khora.toml"), &w.store, false).expect("resolution");

    // Rewrite the lockfile with a checksum nothing hashes to, and empty the
    // store so the fetch actually happens again.
    let lock_path = w.root.join("khora.lock");
    let mut lockfile = Lockfile::read(&lock_path).expect("the lockfile");
    lockfile.packages[0].checksum = Some("0".repeat(64));
    lockfile.write(&lock_path).expect("writing");
    std::fs::remove_dir_all(w.store.root()).expect("emptying the store");
    std::fs::create_dir_all(w.store.root()).expect("recreating the store");

    let error = resolve(&lock_path.parent().unwrap().join("khora.toml"), &w.store, false)
        .expect_err("should refuse");
    let message = format!("{error:#}");
    assert!(
        message.contains("does not hash to what the lockfile records"),
        "the message should name the mismatch: {message}"
    );
}

/// A dependency's own manifest is what says what *it* needs, so nobody has to
/// restate a transitive dependency.
#[test]
fn a_dependencys_dependency_is_resolved_too() {
    let w = world();
    let outside = w.root.parent().unwrap().to_path_buf();
    let (inner_url, _) = package_repo(&outside.join("inner"), "inner", "module inner;\n");

    let middle = outside.join("middle");
    std::fs::create_dir_all(&middle).expect("a directory");
    write(
        &middle.join("khora.toml"),
        &format!(
            "[package]\nname = \"middle\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ninner = {{ git = \"{inner_url}\", rev = \"main\" }}\n"
        ),
    );
    write(&middle.join("src").join("middle.kh"), "module middle;\n");
    commit_all(&middle);
    let middle_url = url_of(&middle);

    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nmiddle = {{ git = \"{middle_url}\", rev = \"main\" }}\n"
        ),
    );

    let resolution = resolve(&w.root.join("khora.toml"), &w.store, false).expect("resolution");
    let mut names: Vec<&str> = resolution.packages.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["inner", "middle"], "the transitive dependency should be found");
}

/// Without a version solver there is nothing to choose between two answers, so
/// the disagreement is the error -- and it names both askers, which is what a
/// person needs to fix it.
#[test]
fn two_packages_wanting_different_revisions_is_an_error() {
    let w = world();
    let outside = w.root.parent().unwrap().to_path_buf();
    let (a_url, _) = package_repo(&outside.join("shared_a"), "shared", "module shared;\n");
    let (b_url, _) = package_repo(&outside.join("shared_b"), "shared", "module shared;\n");

    let middle = outside.join("middle");
    std::fs::create_dir_all(&middle).expect("a directory");
    write(
        &middle.join("khora.toml"),
        &format!(
            "[package]\nname = \"middle\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nshared = {{ git = \"{b_url}\", rev = \"main\" }}\n"
        ),
    );
    write(&middle.join("src").join("middle.kh"), "module middle;\n");
    commit_all(&middle);
    let middle_url = url_of(&middle);

    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\n\
             middle = {{ git = \"{middle_url}\", rev = \"main\" }}\n\
             shared = {{ git = \"{a_url}\", rev = \"main\" }}\n"
        ),
    );

    let error = resolve(&w.root.join("khora.toml"), &w.store, false).expect_err("should refuse");
    let message = format!("{error:#}");
    assert!(message.contains("asked for twice"), "unexpected message: {message}");
    assert!(message.contains("middle"), "the message should name who asked: {message}");
}

/// A git dependency with no revision is not reproducible, and quietly taking
/// the default branch is how a build stops meaning anything.
#[test]
fn a_git_dependency_without_a_revision_is_refused() {
    let w = world();
    write(
        &w.root.join("khora.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ndep = { git = \"file:///nowhere\" }\n",
    );

    let error = resolve(&w.root.join("khora.toml"), &w.store, false).expect_err("should refuse");
    assert!(format!("{error:#}").contains("no `rev` or `tag`"), "{error:#}");
}

#[test]
fn a_version_dependency_says_there_is_no_registry() {
    let w = world();
    write(
        &w.root.join("khora.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ndep = { version = \"1.0.0\" }\n",
    );

    let error = resolve(&w.root.join("khora.toml"), &w.store, false).expect_err("should refuse");
    assert!(format!("{error:#}").contains("registry"), "{error:#}");
}

/// A package answering to a different name than it was asked for means every
/// import of it is wrong, and the failure would otherwise be a confusing
/// "no such module" much later.
#[test]
fn a_package_that_calls_itself_something_else_is_refused() {
    let w = world();
    let (url, _) =
        package_repo(&w.root.parent().unwrap().join("dep"), "actually", "module actually;\n");
    write(
        &w.root.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nexpected = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );

    let error = resolve(&w.root.join("khora.toml"), &w.store, false).expect_err("should refuse");
    assert!(format!("{error:#}").contains("calls itself"), "{error:#}");
}

// --- the store -------------------------------------------------------------

/// Two identical packages are one directory, whatever they are called.
#[test]
fn identical_contents_share_one_directory() {
    let w = world();
    let outside = w.root.parent().unwrap().to_path_buf();

    let one = outside.join("one");
    let two = outside.join("two");
    for dir in [&one, &two] {
        std::fs::create_dir_all(dir).expect("a directory");
        write(&dir.join("khora.toml"), "[package]\nname = \"same\"\nversion = \"0.1.0\"\n");
        write(&dir.join("src").join("same.kh"), "module same;\n");
    }

    assert_eq!(
        khora_pkg::hash_tree(&one).expect("a hash"),
        khora_pkg::hash_tree(&two).expect("a hash")
    );
}
