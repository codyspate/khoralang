//! `khora doc` — the half of documentation generation that touches the disk.
//!
//! `khora-doc`'s own tests are strings in and strings out. What is left for
//! here is everything they cannot see: where a page lands, that two files of
//! one module become one page, that a page for a deleted module is deleted
//! too, and that `--check` fails on a stale tree rather than fixing it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn khora() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!("khora{}", std::env::consts::EXE_SUFFIX))
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a parent directory");
    }
    std::fs::write(path, text).expect("writing a file");
}

fn run(args: &[&str]) -> Output {
    Command::new(khora()).args(args).output().expect("khora should run")
}

fn page(out: &Path, name: &str) -> String {
    std::fs::read_to_string(out.join(name))
        .unwrap_or_else(|_| panic!("expected a page at {}", out.join(name).display()))
}

struct World {
    _tmp: tempfile::TempDir,
    src: PathBuf,
    out: PathBuf,
}

fn world() -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let src = tmp.path().join("src");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&src).expect("a source directory");
    World { _tmp: tmp, src, out }
}

/// The module path decides the file path, so the sidebar nests the way the
/// modules do. The first segment is the package and names the directory this
/// writes into, so it is not repeated inside it.
#[test]
fn a_module_path_becomes_a_file_path() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\nexport fn f() -> Int { 1 }\n");
    write(&w.src.join("b.kh"), "module p::deep::beta;\n//! Beta.\nexport fn g() -> Int { 1 }\n");

    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert!(page(&w.out, "alpha.md").contains("title: p::alpha"));
    assert!(page(&w.out, "deep/beta.md").contains("title: p::deep::beta"));
}

/// `std::net::socket` is three files and one module.
#[test]
fn platform_variants_become_one_page() {
    let w = world();
    write(
        &w.src.join("sock_linux.kh"),
        "module p::sock;\n//! Sockets.\n/// Opens one.\nexport fn open() -> Int { 1 }\n",
    );
    write(
        &w.src.join("sock_windows.kh"),
        "module p::sock;\n/// Opens one.\nexport fn open() -> Int { 1 }\n\
         /// Windows only.\nexport fn startup() -> Int { 2 }\n",
    );

    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let page = page(&w.out, "sock.md");
    assert_eq!(page.matches("### open").count(), 1, "not once per file: {page}");
    assert!(page.contains("### startup"), "a function only one platform has: {page}");
    assert!(page.contains("Sockets."), "the one module block that exists: {page}");
}

/// A page for a module somebody deleted is worse than no page: it describes
/// code that is gone, and nothing in the site says so.
#[test]
fn a_page_for_a_deleted_module_is_deleted_too() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\nexport fn f() -> Int { 1 }\n");
    write(&w.src.join("b.kh"), "module p::deep::beta;\n//! Beta.\nexport fn g() -> Int { 1 }\n");
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(w.out.join("deep").join("beta.md").is_file());

    std::fs::remove_file(w.src.join("b.kh")).expect("removing the module");
    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert!(!w.out.join("deep").join("beta.md").exists(), "the stale page should be gone");
    assert!(!w.out.join("deep").exists(), "and so should the directory it was alone in");
    assert!(w.out.join("alpha.md").is_file(), "the other page should survive");
}

/// The property the baseline depends on: regenerating after no change produces
/// no diff. A generated tree that always differs from itself cannot be
/// reviewed, and `--check` would fail on every commit.
#[test]
fn regenerating_an_unchanged_tree_changes_nothing() {
    let w = world();
    write(
        &w.src.join("a.kh"),
        "module p::alpha;\n//! Alpha.\n/// Adds.\nexport fn f(a: Int) -> Int { a }\n",
    );
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    let first = page(&w.out, "alpha.md");
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert_eq!(first, page(&w.out, "alpha.md"));

    let checked = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap(), "--check"]);
    assert!(checked.status.success(), "{}", String::from_utf8_lossy(&checked.stdout));
}

#[test]
fn check_fails_on_a_stale_tree_and_writes_nothing() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\nexport fn f() -> Int { 1 }\n");
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);

    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\nexport fn f() -> Int { 1 }\n\
         /// New.\nexport fn g() -> Int { 2 }\n");
    let before = page(&w.out, "alpha.md");

    let checked = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap(), "--check"]);
    assert!(!checked.status.success(), "a stale tree should fail");
    let said = String::from_utf8_lossy(&checked.stdout);
    assert!(said.contains("out of date"), "{said}");
    assert!(said.contains("khora doc"), "the message should say how to fix it: {said}");
    assert_eq!(before, page(&w.out, "alpha.md"), "`--check` should not have written");
}

/// A module with no `//!` block still gets a page -- its items are still the
/// API -- but the warning is how somebody finds out it has no introduction.
#[test]
fn a_module_with_no_introduction_is_a_warning_and_not_an_error() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\nexport fn f() -> Int { 1 }\n");

    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("has no `//!` block"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(page(&w.out, "alpha.md").contains("### f"));
}

/// Documentation of a file that does not parse would be documentation of
/// whatever the parser managed to recover, which is worse than a refusal.
#[test]
fn a_file_that_does_not_parse_is_refused_by_name() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\nexport fn (((\n");

    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("a.kh"), "the message should name the file: {said}");
    assert!(said.contains("khora check"), "and the command that explains it: {said}");
}

/// **A checkout must not make every page stale.** Pages are written with `\n`;
/// a Windows checkout with `core.autocrlf` set hands them back with `\r\n`. A
/// byte comparison then reports all of them out of date while `git diff` shows
/// nothing, which is how a gate stops being believed and then gets switched
/// off. Found exactly that way, immediately after a rebase.
#[test]
fn a_page_with_windows_line_endings_is_not_stale() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\nexport fn f() -> Int { 1 }\n");
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);

    let page = page(&w.out, "alpha.md");
    assert!(!page.contains("\r\n"), "written with `\\n`: {page:?}");
    std::fs::write(w.out.join("alpha.md"), page.replace('\n', "\r\n"))
        .expect("rewriting the page as a checkout would");

    let checked =
        run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap(), "--check"]);
    assert!(
        checked.status.success(),
        "the same content is not stale: {}",
        String::from_utf8_lossy(&checked.stdout)
    );
}
