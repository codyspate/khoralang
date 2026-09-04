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
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n");
    write(&w.src.join("b.kh"), "module p::deep::beta;\n//! Beta.\npub fn g() -> Int { 1 }\n");

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
        "module p::sock;\n//! Sockets.\n/// Opens one.\npub fn open() -> Int { 1 }\n",
    );
    write(
        &w.src.join("sock_windows.kh"),
        "module p::sock;\n/// Opens one.\npub fn open() -> Int { 1 }\n\
         /// Windows only.\npub fn startup() -> Int { 2 }\n",
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
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n");
    write(&w.src.join("b.kh"), "module p::deep::beta;\n//! Beta.\npub fn g() -> Int { 1 }\n");
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
        "module p::alpha;\n//! Alpha.\n/// Adds.\npub fn f(a: Int) -> Int { a }\n",
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
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n");
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);

    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n\
         /// New.\npub fn g() -> Int { 2 }\n");
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
    write(&w.src.join("a.kh"), "module p::alpha;\npub fn f() -> Int { 1 }\n");

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
    write(&w.src.join("a.kh"), "module p::alpha;\npub fn (((\n");

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
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n");
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

/// **The sweep deletes pages, and a page is not every markdown file.** The
/// stale sweep used to take every `.md` under `--out`, which is right when the
/// directory is a generated tree and destroys somebody's work when it is not.
/// With the old default sending it into a path the caller never named, that
/// was a way to lose a file by running a documentation command.
#[test]
fn a_file_this_did_not_write_is_left_alone() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n");
    write(&w.out.join("by-hand.md"), "# Mine\n\nWritten by a person.\n");
    write(&w.out.join("deep").join("also-mine.md"), "# Mine too\n");

    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(
        std::fs::read_to_string(w.out.join("by-hand.md")).expect("the hand-written page"),
        "# Mine\n\nWritten by a person.\n",
        "a file this command did not write is not its to delete"
    );
    assert!(w.out.join("deep").join("also-mine.md").is_file(), "nor one in a subdirectory");
    assert!(w.out.join("alpha.md").is_file(), "and the page it did write is there");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("was not written by `khora doc`"),
        "and it says so rather than passing over it silently: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Adopting a tree is not the same as owning every file in it: a second run
/// still deletes what the first one wrote, and still leaves the rest.
#[test]
fn the_sweep_stays_scoped_across_runs() {
    let w = world();
    write(&w.src.join("a.kh"), "module p::alpha;\n//! Alpha.\npub fn f() -> Int { 1 }\n");
    write(&w.src.join("b.kh"), "module p::beta;\n//! Beta.\npub fn g() -> Int { 1 }\n");
    write(&w.out.join("by-hand.md"), "# Mine\n");
    run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(w.out.join("beta.md").is_file());

    std::fs::remove_file(w.src.join("b.kh")).expect("removing the module");
    let out = run(&["doc", w.src.to_str().unwrap(), "--out", w.out.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert!(!w.out.join("beta.md").exists(), "the page whose module went away is gone");
    assert!(w.out.join("by-hand.md").is_file(), "the file it never wrote is still there");
}

/// `khora doc` in a package documents *that package*, into a directory beside
/// its manifest. The defaults used to be this repository's own layout, so the
/// command meant something different everywhere else than it did here.
#[test]
fn a_package_documents_itself_by_default() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let root = tmp.path();
    write(&root.join("khora.toml"), &pinned("[package]\nname = \"probe\"\nversion = \"0.1.0\"\n"));
    write(&root.join("src").join("main.kh"), "module probe::main;\n//! A probe.\npub fn f() -> Int { 1 }\n");

    let out = Command::new(khora())
        .arg("doc")
        .current_dir(root)
        .output()
        .expect("khora should run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let page = root.join("docs").join("api").join("main.md");
    assert!(page.is_file(), "the page goes beside the manifest, not into a path from another repository");
    assert!(
        std::fs::read_to_string(&page).expect("the page").contains("title: probe::main"),
        "and it documents this package"
    );
}

/// Outside a package and with nothing named, there is nothing to document, and
/// the refusal says what to type instead of inventing a target.
#[test]
fn nothing_to_document_is_a_refusal_that_says_what_to_do() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let out = Command::new(khora())
        .arg("doc")
        .current_dir(tmp.path())
        .output()
        .expect("khora should run");

    assert!(!out.status.success(), "there is no package here");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("no `khora.toml`"), "it says why: {said}");
    assert!(said.contains("khora doc src"), "and what to type: {said}");
}

/// A manifest with the `[toolchain]` table every project now needs.
///
/// **A pin is required, and these fixtures live in the system temporary
/// directory** -- unlike the suites under `CARGO_TARGET_TMPDIR`, which sit
/// inside this repository and inherit its pin by the same upward walk a real
/// project uses. There is nothing above these, so they carry their own.
///
/// It names the version under test. That is the binary these tests are about to
/// run, so the pin resolves to "the one already running" and no handover
/// happens -- which is what keeps this a fixture detail rather than a thing the
/// tests have to think about.
fn pinned(manifest: &str) -> String {
    format!("{manifest}\n[toolchain]\nversion = \"{}\"\n", khora_toolchain::RUNNING)
}
