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
        &pinned("[package]\nname = \"reaching\"\nversion = \"0.1.0\"\npublish = true\n"),
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
        &pinned(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             {permissions}\n\
             [dependencies]\nreaching = {{ git = \"{url}\", rev = \"main\" }}\n"
        )),
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
        pinned("[package]\nname = \"granted\"\nversion = \"0.1.0\"\n\n\
         [permissions.fs]\nread = [\"data/**\"]\nwrite = [\"data/**\"]\n"),
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
#[cfg(feature = "llvm")]
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
#[cfg(feature = "llvm")]
#[test]
fn no_permissions_table_grants_everything() {
    let root = granted_project("perm_absent");
    std::fs::write(
        root.join("khora.toml"),
        pinned("[package]\nname = \"granted\"\nversion = \"0.1.0\"\n"),
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

/// A project granting one env prefix and one host.
fn narrowed_project(name: &str, table: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("a src directory");
    std::fs::write(
        root.join("khora.toml"),
        pinned(&format!("[package]\nname = \"narrowed\"\nversion = \"0.1.0\"\n{table}")),
    )
    .expect("a manifest");
    std::fs::write(
        root.join("src").join("main.kh"),
        "module main;\n\n\
         import std::core::{Result, print};\n\
         import std::env::{Env, EnvError, variable_or};\n\
         import std::net::http::{Call, CallError, HttpClient};\n\n\
         fn look(name: String) -> () with { env: Env } {\n\
         \x20 print(variable_or(name, \"(unset)\")! catch {\n\
         \x20   EnvError::Denied(n) => \"DENIED ${n}\",\n\
         \x20 });\n\
         }\n\n\
         fn fetch(url: String) -> () with { http: HttpClient } {\n\
         \x20 print(match http.send(Call::get(url)) {\n\
         \x20   Result::Ok(_) => \"reached\",\n\
         \x20   Result::Err(CallError::Denied(host)) => \"DENIED ${host}\",\n\
         \x20   Result::Err(_) => \"tried\",\n\
         \x20 });\n\
         }\n\n\
         fn main() -> Int {\n\
         \x20 with { env: Env::real(), http: HttpClient::real() } {\n\
         \x20   look(\"APP_NAME\");\n\
         \x20   look(\"SECRET_KEY\");\n\
         \x20   fetch(\"http://api.example.com/health\");\n\
         \x20   fetch(\"http://evil.example.net/steal\");\n\
         \x20 }\n\
         \x20 0\n\
         }\n",
    )
    .expect("a source file");
    root
}

fn ran(root: &PathBuf) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["run", "."])
        .current_dir(root)
        .output()
        .expect("could not run `khora`");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// **`env` and `network` are enforced, and a grant is not a denial.**
///
/// `APP_NAME` is granted and simply not set, which is `(unset)` rather than a
/// refusal -- the distinction `EnvError::Denied` exists for. `api.example.com`
/// is granted, so it is *attempted*: it fails at the network, which is the
/// right failure and a different one.
#[cfg(feature = "llvm")]
#[test]
fn env_and_network_grants_are_enforced_at_run_time() {
    let text = ran(&narrowed_project(
        "perm_env_net",
        "\n[permissions]\nenv = [\"APP_*\"]\nnetwork = [\"api.example.com\"]\n",
    ));

    assert!(text.contains("(unset)"), "a granted variable that is unset is not denied: {text}");
    assert!(text.contains("DENIED SECRET_KEY"), "{text}");
    assert!(text.contains("DENIED evil.example.net:80"), "{text}");
    assert!(
        !text.contains("DENIED api.example.com"),
        "a granted host must be attempted, not refused: {text}"
    );
}

/// **Narrowing one category does not narrow another.** A manifest that
/// mentions only `fs` still grants every variable and every host, which is the
/// rule the table has always claimed and the one that makes tightening one
/// thing at a time possible.
#[test]
fn narrowing_one_category_leaves_the_others_alone() {
    let text = ran(&narrowed_project(
        "perm_one_category",
        "\n[permissions.fs]\nread = [\"data/**\"]\n",
    ));

    assert!(!text.contains("DENIED"), "only `fs` was narrowed: {text}");
}

/// The package being built, with no dependency involved at all.
///
/// **This is the one the check did not look at.** `check_extern_allowlist`
/// walked the resolved *dependencies*, so every package was covered except the
/// one whose source somebody is writing — which is the one most likely to reach
/// for `extern fn`, because it is the one being changed. A package could write
/// `[permissions] extern = []` in its own manifest, declare an `extern fn` on
/// the next screen, and build.
///
/// The rule was never about dependencies: `may_declare_extern` is documented as
/// "packages that may declare `extern fn`", and a package is no less itself for
/// being the one at the root of the build.
fn build_alone(permissions: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = tmp.path().join("app");
    write(
        &app.join("khora.toml"),
        &pinned(&format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n{permissions}")),
    );
    write(
        &app.join("src").join("main.kh"),
        "module app::main;\n\nextern fn khora_live_count() -> Int;\n\n\
         pub fn main() -> Int { khora_live_count() }\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["check", app.join("src").join("main.kh").to_str().expect("a path")])
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
fn a_package_declaring_extern_in_its_own_source_is_refused() {
    let (ok, output) = build_alone("[permissions]\nextern = []\n");
    assert!(!ok, "its own `extern = []` should refuse it:\n{output}");
    assert!(
        output.contains("khora_live_count"),
        "the message should name the declaration:\n{output}"
    );
}

/// And the suggestion has to name it, or it reads `extern = []` — which is what
/// the manifest already says, and so is no advice at all.
#[test]
fn the_suggestion_names_the_package_being_built() {
    let (_, output) = build_alone("[permissions]\nextern = []\n");
    assert!(
        output.contains("extern = [\"app\"]"),
        "the suggestion should name the package:\n{output}"
    );
}

#[test]
fn a_package_that_lists_itself_may_declare_extern() {
    let (ok, output) = build_alone("[permissions]\nextern = [\"app\"]\n");
    assert!(ok, "listing itself should be enough:\n{output}");
}

/// Absent still grants, here as everywhere else in the table. Tightening is
/// opt-in, and this change does not make it otherwise — it makes the opting-in
/// mean what it says.
#[test]
fn a_package_with_no_permissions_table_may_still_declare_extern() {
    let (ok, output) = build_alone("");
    assert!(ok, "no table should grant everything:\n{output}");
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
