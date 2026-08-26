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

use std::path::Path;
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
         export fn open_anything(path: Ptr, mode: Ptr) -> Ptr {\n  \
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
    write(&app.join("src").join("main.kh"), "module app::main;\n\nexport fn main() -> () {}\n");

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
