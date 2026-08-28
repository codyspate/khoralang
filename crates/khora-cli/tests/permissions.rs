//! `[permissions] extern`: the gate that makes the rest of the table mean
//! something.
//!
//! From `docs/design/testing.md`'s list of promises that had no test.
//! `permissions.md` says the compile-time gate over Khora code is total and
//! that `extern fn` goes around it — a documented hole. A documented hole is a
//! decision; an undocumented one is a vulnerability, and only a test tells them
//! apart. So these assert that the hole is exactly where it is said to be and
//! no wider:
//!
//! - a dependency declaring `extern fn` is refused when the list excludes it;
//! - `std` is never refused, because a `std` that could not declare `fopen`
//!   could not offer `Fs`;
//! - an absent key still grants everything, like the rest of the table.

use std::path::{Path, PathBuf};
use std::process::Command;

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a parent directory");
    }
    std::fs::write(path, text).expect("writing");
}

fn git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git on the path");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A dependency that reaches the operating system directly, in a repository of
/// its own, and the URL to reach it by.
fn reaching_package(at: &Path) -> String {
    // `publish = true` because a git dependency on a package that has not
    // offered itself is refused before permissions are ever consulted, and this
    // fixture is standing in for a library somebody published.
    write(
        &at.join("khora.toml"),
        "[package]\nname = \"reaching\"\nversion = \"0.1.0\"\npublish = true\n",
    );
    write(
        &at.join("src").join("reaching.kh"),
        "module reaching;\n\n\
         import std::core::{Ptr};\n\n\
         // Nothing in this signature says it touches a filesystem.\n\
         extern fn fopen(path: Ptr, mode: Ptr) -> Ptr;\n\n\
         pub fn open_anything(path: Ptr, mode: Ptr) -> Ptr {\n  \
         fopen(path, mode)\n\
         }\n",
    );
    git(&["init", "--quiet", "-b", "main"], at);
    git(&["add", "-A"], at);
    git(&["commit", "--quiet", "-m", "first"], at);
    format!("file:///{}", at.display().to_string().replace('\\', "/"))
}

/// Runs `khora check` on an app whose manifest has the given `[permissions]`.
fn check_with(permissions: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let url = reaching_package(&tmp.path().join("reaching"));

    let app = tmp.path().join("app");
    write(
        &app.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             {permissions}\n\
             [dependencies]\nreaching = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );
    write(&app.join("src").join("main.kh"), "module app::main;\n\npub fn main() -> () {}\n");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["check", app.join("src").join("main.kh").to_str().expect("a path")])
        // Its own store, so one test cannot see what another fetched.
        .env("KHORA_HOME", tmp.path().join("home"))
        .output()
        .expect("running khora");

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn a_dependency_declaring_extern_is_refused_when_the_list_excludes_it() {
    let (ok, output) = check_with("[permissions]\nextern = []\n");
    assert!(!ok, "the build should have been refused:\n{output}");
    assert!(output.contains("fopen"), "the message should name the declaration:\n{output}");
    assert!(output.contains("reaching"), "and the package it is in:\n{output}");
}

/// The message has to say what to write, because the alternative is a person
/// guessing at TOML.
#[test]
fn the_refusal_says_what_to_add() {
    let (_, output) = check_with("[permissions]\nextern = []\n");
    assert!(
        output.contains("extern = [\"reaching\"]"),
        "the message should suggest the exact line:\n{output}"
    );
}

#[test]
fn a_listed_package_may_declare_extern() {
    let (ok, output) = check_with("[permissions]\nextern = [\"reaching\"]\n");
    assert!(ok, "an allowed package should build:\n{output}");
}

/// Tightening is opt-in everywhere else in this table, and this key is no
/// different. A project that has never thought about it is not punished.
#[test]
fn an_absent_key_grants_every_package() {
    let (ok, output) = check_with("[permissions]\nnetwork = [\"*\"]\n");
    assert!(ok, "an absent `extern` key should grant everything:\n{output}");
}

#[test]
fn no_permissions_table_at_all_grants_every_package() {
    let (ok, output) = check_with("");
    assert!(ok, "no table should grant everything:\n{output}");
}

/// The one exception, and it is the design rather than a hole in it: everything
/// reaching outside Khora is supposed to go through functions whose signatures
/// carry capability rows, and those live in `std`.
#[test]
fn std_may_always_declare_extern() {
    use khora_manifest::Permissions;
    let locked = Permissions { extern_: Some(Vec::new()), ..Permissions::default() };
    assert!(locked.may_declare_extern("std"));
    assert!(!locked.may_declare_extern("anything_else"));
}

// --- what the manifest actually stops --------------------------------------
//
// Until now `[permissions.fs]` was parsed, matched by a tested `granted_path`,
// and consulted by nothing. These are the tests that say it reaches a running
// program. `docs/design/permissions.md` promised exactly this: the paths are
// "given to `Fs::real()` at build time -- as data compiled into the program --
// and it refuses a read outside them, raising `IoError`".

/// A project whose manifest grants only `data/**`.
fn granted_project(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("a src directory");
    std::fs::create_dir_all(root.join("data")).expect("a data directory");
    std::fs::write(
        root.join("khora.toml"),
        "[package]\nname = \"granted\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [permissions.fs]\nread = [\"data/**\"]\nwrite = [\"data/**\"]\n",
    )
    .expect("a manifest");
    std::fs::write(
        root.join("src").join("main.kh"),
        "module main;\n\n\
         import std::core::{print};\n\
         import std::fs::{FsRead, FsWrite, IoError, read_text, write_text};\n\n\
         fn attempt(path: String) -> () with { writes: FsWrite } {\n\
         \x20 let outcome = {\n\
         \x20   write_text(path, \"hello\")!;\n\
         \x20   \"wrote ${path}\"\n\
         \x20 } catch {\n\
         \x20   IoError::Denied(p) => \"DENIED ${p}\",\n\
         \x20   IoError::NotFound(p) => \"missing ${p}\",\n\
         \x20   IoError::Failed(p) => \"failed ${p}\",\n\
         \x20 };\n\
         \x20 print(outcome);\n\
         }\n\n\
         fn look(path: String) -> () with { reads: FsRead } {\n\
         \x20 print(read_text(path)! catch {\n\
         \x20   IoError::Denied(p) => \"DENIED ${p}\",\n\
         \x20   IoError::NotFound(p) => \"missing ${p}\",\n\
         \x20   IoError::Failed(p) => \"failed ${p}\",\n\
         \x20 });\n\
         }\n\n\
         fn main() -> Int {\n\
         \x20 with { reads: FsRead::real(), writes: FsWrite::real() } {\n\
         \x20   attempt(\"data/allowed.txt\");\n\
         \x20   attempt(\"secrets/stolen.txt\");\n\
         \x20   look(\"data/allowed.txt\");\n\
         \x20   look(\"secrets/stolen.txt\");\n\
         \x20 }\n\
         \x20 0\n\
         }\n",
    )
    .expect("a source file");
    root
}

/// **The grant reaches the running program.** A path inside it is written and
/// read back; a path outside it is refused, by the program itself, with the
/// case that names the manifest rather than the disk.
#[test]
fn a_path_outside_the_grant_is_refused_at_run_time() {
    let root = granted_project("perm_enforced");
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["run", "."])
        .current_dir(&root)
        .output()
        .expect("could not run `khora`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(text.contains("wrote data/allowed.txt"), "{text}");
    assert!(text.contains("DENIED secrets/stolen.txt"), "{text}");
    assert!(text.contains("hello"), "the granted file has to read back: {text}");
    // Twice: once for the write, once for the read.
    assert_eq!(
        text.matches("DENIED secrets/stolen.txt").count(),
        2,
        "both halves of the grant are enforced: {text}"
    );
}

/// **A manifest with no `[permissions]` grants everything**, which is the rule
/// that keeps this from being a tax on starting. The same program, with the
/// table removed, writes wherever it likes.
#[test]
fn no_permissions_table_grants_everything() {
    let root = granted_project("perm_absent");
    std::fs::write(
        root.join("khora.toml"),
        "[package]\nname = \"granted\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("a manifest without permissions");
    std::fs::create_dir_all(root.join("secrets")).expect("a secrets directory");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["run", "."])
        .current_dir(&root)
        .output()
        .expect("could not run `khora`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(text.contains("wrote data/allowed.txt"), "{text}");
    assert!(text.contains("wrote secrets/stolen.txt"), "nothing is denied: {text}");
    assert!(!text.contains("DENIED"), "no grant means no refusal: {text}");
}
