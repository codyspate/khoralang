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
//! - an absent key answers with `default`: open with no `default` or with
//!   `default = "allow"`, closed to everything but `std` under
//!   `default = "deny"` -- including the root package and its `src/bin`
//!   programs.

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

/// `default = "deny"` with no `extern` key refuses a dependency's `extern fn`
/// as it refuses every other category the table leaves out.
#[test]
fn deny_without_an_extern_key_refuses_a_dependency() {
    let (ok, output) = check_with("[permissions]\ndefault = \"deny\"\n");
    assert!(!ok, "`default = \"deny\"` should refuse the dependency:\n{output}");
    assert!(output.contains("fopen"), "the message should name the declaration:\n{output}");
    assert!(output.contains("extern = [\"reaching\"]"), "and the line to add:\n{output}");
}

/// Tightening is opt-in everywhere else in this table, and this key is no
/// different. A project that has never thought about it -- a table with no
/// `default` -- is not punished.
#[test]
fn an_absent_key_grants_every_package() {
    let (ok, output) = check_with("[permissions]\nnetwork = [\"*\"]\n");
    assert!(ok, "an absent `extern` key without `deny` should grant everything:\n{output}");
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
///
/// Every caller builds and runs a program, so all of them are gated on the
/// backend -- and without it this was a function nothing called, which the
/// no-backend build has been warning about.
#[cfg(feature = "llvm")]
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

/// **A path with a character outside ASCII goes through the checker intact.**
///
/// The normalizer walked the path a byte at a time and sliced it a character
/// at a time, so the first byte of any multi-byte character cut one in half
/// and trapped: `this slice cuts a character in half, so it is not a String`,
/// exit 134, from inside `std` with no path named and nothing of the caller's
/// in the backtrace. Every filesystem call passes through here, so one
/// accented filename in a directory took the program down -- and `attempt`
/// could not catch it, a trap not being a failure.
///
/// Both halves are asserted. That the program survives is the bug; that the
/// grant still refuses what it should is the fix not having been an
/// amputation.
#[cfg(feature = "llvm")]
#[test]
fn a_path_outside_ascii_is_checked_rather_than_halved() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("perm_utf8");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("a src directory");
    std::fs::create_dir_all(root.join("data")).expect("a data directory");
    std::fs::write(
        root.join("khora.toml"),
        pinned(
            "[package]\nname = \"utf8paths\"\nversion = \"0.1.0\"\n\n\
             [permissions.fs]\nread = [\"data/**\"]\nwrite = [\"data/**\"]\n",
        ),
    )
    .expect("a manifest");
    std::fs::write(
        root.join("src").join("main.kh"),
        "module main;\n\n\
         import std::core::{print};\n\
         import std::fs::{FsRead, FsWrite, IoError, read_text, write_text};\n\n\
         fn look(label: String, path: String) -> () with { reads: FsRead } {\n\
         \x20 let text = read_text(path)! catch {\n\
         \x20   IoError::Denied(p) => \"DENIED ${p}\",\n\
         \x20   IoError::NotFound(p) => \"missing ${p}\",\n\
         \x20   IoError::Failed(p) => \"failed ${p}\",\n\
         \x20 };\n\
         \x20 print(\"${label}: ${text}\");\n\
         }\n\n\
         fn main() -> Int {\n\
         \x20 with { reads: FsRead::real(), writes: FsWrite::real() } {\n\
         \x20   write_text(\"data/café.txt\", \"accented\")! catch {\n\
         \x20     IoError::Denied(p) => print(\"DENIED ${p}\"),\n\
         \x20     IoError::NotFound(p) => print(\"missing ${p}\"),\n\
         \x20     IoError::Failed(p) => print(\"failed ${p}\"),\n\
         \x20   };\n\
         \x20   look(\"granted\", \"data/café.txt\");\n\
         \x20   look(\"refused\", \"secrets/café.txt\");\n\
         \x20   print(\"survived\");\n\
         \x20 }\n\
         \x20 0\n\
         }\n",
    )
    .expect("a source file");

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

    assert!(
        !text.contains("cuts a character in half"),
        "an accented path must not trap in the permission checker: {text}"
    );
    assert!(
        text.contains("granted: accented"),
        "the granted accented path has to read back: {text}"
    );
    assert!(
        text.contains("DENIED secrets/café.txt"),
        "and one outside the grant is still refused: {text}"
    );
    assert!(text.contains("survived"), "the program has to reach its end: {text}");
    assert_eq!(out.status.code(), Some(0), "and exit normally: {text}");
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

/// **`default = "deny"` covers `extern` too.** Every other category left out
/// of a `deny` table grants nothing, so an absent `extern` key granting every
/// package made the strict posture a door with the frame removed: the root
/// package's own `extern fn getpid` compiled and ran beside a manifest that
/// denied the network, the disk and the environment.
#[test]
fn deny_without_an_extern_key_refuses_the_package_s_own_extern() {
    let (ok, output) = build_alone("[permissions]\ndefault = \"deny\"\n");
    assert!(!ok, "`default = \"deny\"` should refuse an unlisted `extern fn`:\n{output}");
    assert!(
        output.contains("khora_live_count"),
        "the message should name the declaration:\n{output}"
    );
}

/// The refusal points at the declaration, so the reader opens the right line
/// rather than searching a package for the word `extern`.
#[test]
fn the_deny_refusal_points_at_the_declaration_and_names_the_key() {
    let (_, output) = build_alone("[permissions]\ndefault = \"deny\"\n");
    assert!(
        output.contains("main.kh:3:1"),
        "the message should give the declaration's line and column:\n{output}"
    );
    assert!(
        output.contains("extern = [\"app\"]"),
        "the message should name the key to add:\n{output}"
    );
}

/// Under `deny`, a program that wants its own `extern fn` lists itself, like
/// any other package.
#[test]
fn deny_with_the_package_listed_may_declare_extern() {
    let (ok, output) = build_alone("[permissions]\ndefault = \"deny\"\nextern = [\"app\"]\n");
    assert!(ok, "listing itself should be enough under `deny`:\n{output}");
}

/// `allow` spelled out is the same as no table: every package may.
#[test]
fn allow_without_an_extern_key_grants_every_package() {
    let (ok, output) = build_alone("[permissions]\ndefault = \"allow\"\n");
    assert!(ok, "`default = \"allow\"` should leave `extern` open:\n{output}");
}

/// A package whose only `extern fn` is in a `src/bin` program, under `deny`.
///
/// **What this guards: a program the scan never looked at.** The walk that
/// collects a package's sources skips `src/bin`, because each file there is a
/// program of its own; the `extern` scan used the same walk, so a
/// `src/bin/tool.kh` declaring `extern fn getpid` passed `check` and `build`
/// beside a manifest that allowed `extern fn` in `std` alone.
fn bin_package(tmp: &Path) -> PathBuf {
    let app = tmp.join("app");
    write(
        &app.join("khora.toml"),
        &pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[permissions]\ndefault = \"deny\"\n"),
    );
    write(&app.join("src").join("main.kh"), "module app::main;\n\npub fn main() -> Int { 0 }\n");
    write(
        &app.join("src").join("bin").join("tool.kh"),
        "module app::tool;\n\nextern fn getpid() -> I32;\n\npub fn main() -> Int { I32::to_int(getpid()) }\n",
    );
    app
}

/// Runs `khora <args>` in `app`, answering whether it succeeded and what it said.
fn khora_in(app: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(app)
        .env("KHORA_HOME", home)
        .output()
        .expect("running khora");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn deny_refuses_an_extern_in_a_src_bin_program_at_check() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = bin_package(tmp.path());
    let home = tmp.path().join("home");
    for args in [&["check"][..], &["check", "."][..], &["check", "src/bin/tool.kh"][..]] {
        let (ok, output) = khora_in(&app, &home, args);
        assert!(!ok, "`khora {}` should refuse the `src/bin` extern:\n{output}", args.join(" "));
        assert!(output.contains("getpid"), "`khora {}` should name it:\n{output}", args.join(" "));
        assert!(output.contains("tool.kh:3:1"), "`khora {}` should point at it:\n{output}", args.join(" "));
    }
}

#[cfg(feature = "llvm")]
#[test]
fn deny_refuses_an_extern_in_a_src_bin_program_at_build() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = bin_package(tmp.path());
    let home = tmp.path().join("home");
    for args in [&["build"][..], &["build", "."][..], &["build", "src/bin/tool.kh"][..]] {
        let (ok, output) = khora_in(&app, &home, args);
        assert!(!ok, "`khora {}` should refuse the `src/bin` extern:\n{output}", args.join(" "));
        assert!(output.contains("getpid"), "`khora {}` should name it:\n{output}", args.join(" "));
    }
    assert!(!app.join("build").join("tool").exists(), "nothing should have been built");
}

/// A file named on the command line that the package walk would not reach --
/// here one under `target/`, which the walk skips -- is compiled under the
/// package's manifest, so the rule covers it too.
#[test]
fn deny_refuses_an_extern_in_a_named_file_the_walk_skips() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = bin_package(tmp.path());
    std::fs::remove_file(app.join("src").join("bin").join("tool.kh")).expect("no bin program");
    write(
        &app.join("target").join("scratch.kh"),
        "module app::scratch;\n\nextern fn getpid() -> I32;\n\npub fn main() -> Int { I32::to_int(getpid()) }\n",
    );
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check", "target/scratch.kh"]);
    assert!(!ok, "a named file should be scanned:\n{output}");
    assert!(output.contains("scratch.kh:3:1"), "and pointed at:\n{output}");
}

/// **`khora check` refuses a `network` grant ending in `:`** and says which
/// entry and what to write instead.
#[test]
fn check_refuses_a_network_grant_with_an_empty_port() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = tmp.path().join("app");
    write(
        &app.join("khora.toml"),
        &pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[permissions]\nnetwork = [\"[::1]:\"]\n"),
    );
    write(&app.join("src").join("main.kh"), "module app::main;\n\npub fn main() -> Int { 0 }\n");
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check"]);
    assert!(!ok, "`[::1]:` should be refused:\n{output}");
    assert!(output.contains("`[::1]:`") && output.contains("`[::1]:*`"), "{output}");
}

/// **`khora check` refuses a `listen` entry that names an interface, and its
/// advice is a shape that loads.** The path this closes: `localhost:8080`
/// was refused with advice to write `127.0.0.1:8080`, which loaded, passed
/// `khora check`, and was refused at run time, because a listener binds
/// every interface and so never asks about loopback. Each interface shape
/// is refused here, under `deny`, and the port the advice offers passes.
#[test]
fn check_refuses_a_listen_entry_that_names_an_interface() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = tmp.path().join("app");
    write(&app.join("src").join("main.kh"), "module app::main;\n\npub fn main() -> Int { 0 }\n");
    let manifest = |entry: &str| {
        pinned(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[permissions]\ndefault = \"deny\"\nlisten = [\"{entry}\"]\n"
        ))
    };
    for entry in ["localhost:8080", "127.0.0.1:8080", "0.0.0.0:8080", "[::1]:*"] {
        write(&app.join("khora.toml"), &manifest(entry));
        let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check"]);
        assert!(!ok, "`{entry}` should be refused:\n{output}");
        assert!(output.contains("permissions.listen"), "{output}");
        assert!(output.contains(&format!("`{entry}`")), "names the entry:\n{output}");
        assert!(output.contains("every interface"), "says why:\n{output}");
        assert!(output.contains("`8080`") && output.contains("`8000-8099`"), "offers the shapes:\n{output}");
        assert!(!output.contains("`127.0.0.1:9000`"), "offers no interface shape:\n{output}");
    }
    write(&app.join("khora.toml"), &manifest("8080"));
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check"]);
    assert!(ok, "the advice, `8080`, should load:\n{output}");
}

/// A program that tries `listen_on` on port 0 and then on port 1 -- which
/// no test grant names -- and prints what each did.
#[cfg(feature = "llvm")]
const LISTENS: &str = "module main;\n\n\
     import std::core::{print};\n\
     import std::net::socket::{SocketError, listen_on, shut, start};\n\n\
     fn tried(port: Int) -> String raises SocketError {\n\
     \x20 let socket = listen_on(port)!;\n\
     \x20 shut(socket);\n\
     \x20 if socket < 0 { \"failed\" } else { \"bound\" }\n\
     }\n\n\
     pub fn main() -> Int {\n\
     \x20 start();\n\
     \x20 print(\"zero \" + (tried(0)! catch { e => \"${e}\" }));\n\
     \x20 print(\"one \" + (tried(1)! catch { e => \"${e}\" }));\n\
     \x20 0\n\
     }\n";

/// **`khora run` writes `[permissions] listen` into the program.** Under
/// `deny` with no `listen` key both listens are refused; with `listen =
/// ["0"]` port 0 binds and port 1 is still refused; with no table both are
/// attempted, and port 1, which needs privileges this test does not have
/// or is taken, is at worst a failed bind rather than a refusal.
#[cfg(feature = "llvm")]
#[test]
fn a_listen_grant_reaches_the_built_program() {
    for (name, table, zero, one) in [
        ("perm_listen_deny", "\n[permissions]\ndefault = \"deny\"\n", "zero SocketError::Denied(0.0.0.0:0)", "one SocketError::Denied(0.0.0.0:1)"),
        ("perm_listen_zero", "\n[permissions]\ndefault = \"deny\"\nlisten = [\"0\"]\n", "zero bound", "one SocketError::Denied(0.0.0.0:1)"),
        ("perm_listen_open", "", "zero bound", "one "),
    ] {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("a src directory");
        std::fs::write(
            root.join("khora.toml"),
            pinned(&format!("[package]\nname = \"listens\"\nversion = \"0.1.0\"\n{table}")),
        )
        .expect("a manifest");
        std::fs::write(root.join("src").join("main.kh"), LISTENS).expect("a source file");
        let text = ran(&root);
        assert!(text.contains(zero), "{name}: expected {zero:?}:\n{text}");
        assert!(text.contains(one), "{name}: expected {one:?}:\n{text}");
        if table.is_empty() {
            assert!(!text.contains("Denied"), "{name}: no table refuses nothing:\n{text}");
        }
    }
}

/// **A dependency's `src/bin` programs are not this build's code.** They are
/// never compiled into the program, so an `extern fn` in one is not refused
/// under `deny`. Refusing it forced `extern = ["dep"]`, which would then have
/// let the dependency's *library* declare one too.
#[test]
fn a_dependencys_src_bin_extern_is_not_this_programs() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let dep = tmp.path().join("dep");
    write(
        &dep.join("khora.toml"),
        &pinned("[package]\nname = \"dep\"\nversion = \"0.1.0\"\npublish = true\n"),
    );
    write(&dep.join("src").join("lib.kh"), "module dep::lib;\n\npub fn seven() -> Int { 7 }\n");
    write(
        &dep.join("src").join("bin").join("x.kh"),
        "module dep::x;\n\nextern fn getpid() -> I32;\n\npub fn main() -> Int { I32::to_int(getpid()) }\n",
    );
    let app = tmp.path().join("app");
    write(
        &app.join("khora.toml"),
        &pinned(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n\n\
             [permissions]\ndefault = \"deny\"\n",
        ),
    );
    write(
        &app.join("src").join("main.kh"),
        "module app::main;\n\nimport dep::lib::{seven};\n\npub fn main() -> Int { seven() - 7 }\n",
    );
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check"]);
    assert!(ok, "the dependency's own tool is not part of this program:\n{output}");
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

/// Builds and runs `source` in a project whose manifest ends with `table`,
/// answering whether it ran and what it printed.
#[cfg(feature = "llvm")]
fn run_with_table(name: &str, table: &str, source: &str) -> (bool, String) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("khora.toml"), &pinned(&format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n{table}")));
    write(&root.join("src").join("main.kh"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["run", "."])
        .current_dir(&root)
        .env("KHORA_HOME", root.join("home"))
        .output()
        .expect("running khora");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// **A thousand entries in every list key build, and the last one is
/// granted.** Each list was written into std's grants module as one nested
/// expression, one level per entry, and the parser refuses anything nested
/// past 256: a `network` list of 260 hosts made std's own file fail to parse.
/// The entry one past each list is asked too, so a list that is cut short or
/// that grants everything fails here.
#[cfg(feature = "llvm")]
#[test]
fn a_thousand_grants_in_every_list_build_and_the_last_one_holds() {
    let list = |shape: &dyn Fn(usize) -> String| -> String {
        (0..1000).map(|i| format!("\"{}\"", shape(i))).collect::<Vec<_>>().join(", ")
    };
    let mut externs: Vec<String> = (0..999).map(|i| format!("\"pkg{i}\"")).collect();
    externs.push("\"app\"".to_string());
    let table = format!(
        "[permissions]\ndefault = \"deny\"\n\
         network = [{}]\nlisten = [{}]\nenv = [{}]\nprocess = [{}]\nextern = [{}]\n\n\
         [permissions.fs]\nread = [{}]\nwrite = [{}]\n",
        list(&|i| format!("h{i}.example.com")),
        list(&|i| format!("{}", 20000 + i)),
        list(&|i| format!("VAR_{i}")),
        list(&|i| format!("tool{i}")),
        externs.join(", "),
        list(&|i| format!("data{i}/**")),
        list(&|i| format!("out{i}/**")),
    );
    let source = "module main;\n\n\
        import std::core::{List, print};\n\
        import std::permissions::{granted, granted_host, granted_listen, granted_name};\n\
        import std::permissions::grants::{env, fs_read, fs_write, listen, network, process};\n\n\
        extern fn khora_live_count() -> Int;\n\n\
        fn line(key: String, all: List<String>, last: Bool, past: Bool) -> () {\n  \
          print(\"${key} ${List::length(all)} ${last} ${past}\");\n\
        }\n\n\
        pub fn main() -> Int {\n  \
          let last_host = \"h999.example.com:443\";\n  \
          let past_host = \"h1000.example.com:443\";\n  \
          line(\"network\", network(), granted_host(network(), last_host), granted_host(network(), past_host));\n  \
          let last_port = \"0.0.0.0:20999\";\n  \
          let past_port = \"0.0.0.0:21000\";\n  \
          line(\"listen\", listen(), granted_listen(listen(), last_port), granted_listen(listen(), past_port));\n  \
          line(\"env\", env(), granted_name(env(), \"VAR_999\"), granted_name(env(), \"VAR_1000\"));\n  \
          line(\"process\", process(), granted_name(process(), \"tool999\"), granted_name(process(), \"tool1000\"));\n  \
          line(\"read\", fs_read(), granted(fs_read(), \"data999/x\"), granted(fs_read(), \"data1000/x\"));\n  \
          line(\"write\", fs_write(), granted(fs_write(), \"out999/x\"), granted(fs_write(), \"out1000/x\"));\n  \
          let live = khora_live_count();\n  \
          print(\"extern ${live >= 0}\");\n  \
          0\n\
        }\n";
    let (ok, text) = run_with_table("perm_thousand", &table, source);
    assert!(ok, "a thousand grants per key should build and run:\n{text}");
    for line in [
        "network 1000 true false",
        "listen 1000 true false",
        "env 1000 true false",
        "process 1000 true false",
        "read 1000 true false",
        "write 1000 true false",
        "extern true",
    ] {
        assert!(text.contains(line), "expected {line:?}:\n{text}");
    }
}

/// **A grant pattern is text, whatever characters it holds.** The patterns
/// were pasted into std's grants module with only `"` and `\` escaped, so a
/// `${x}` in one was an interpolation in a file std's gate counts as std's
/// own. Each of `${..}`, `"` and `\` is matched as written, and only as
/// written: the pattern `A${x}"\B` grants a variable named exactly that and
/// not one named after what `${x}` would have printed.
#[cfg(feature = "llvm")]
#[test]
fn a_grant_pattern_is_matched_as_the_text_it_is() {
    let table = "[permissions]\ndefault = \"deny\"\nenv = ['A${x}\"\\B']\n";
    let source = "module main;\n\n\
        import std::core::{List, Option, print};\n\
        import std::permissions::{granted_name};\n\
        import std::permissions::grants::{env};\n\n\
        pub fn main() -> Int {\n  \
          let x = \"x\";\n  \
          let written = \"A\\${x}\\\"\\\\B\";\n  \
          let interpolated = \"A${x}\\\"\\\\B\";\n  \
          let unquoted = \"A\\${x}B\";\n  \
          let shown = match List::head(env()) {\n    \
            Option::Some(only) => only,\n    \
            Option::None => \"none\",\n  \
          };\n  \
          print(\"pattern ${shown}\");\n  \
          print(\"length ${List::length(env())}\");\n  \
          print(\"exact ${granted_name(env(), written)}\");\n  \
          print(\"interpolated ${granted_name(env(), interpolated)}\");\n  \
          print(\"unquoted ${granted_name(env(), unquoted)}\");\n  \
          0\n\
        }\n";
    let (ok, text) = run_with_table("perm_escaped", table, source);
    assert!(ok, "a pattern with `${{..}}`, `\"` and `\\` should build:\n{text}");
    assert!(text.contains("pattern A${x}\"\\B"), "{text}");
    assert!(text.contains("length 1"), "{text}");
    assert!(text.contains("exact true"), "{text}");
    assert!(text.contains("interpolated false"), "{text}");
    assert!(text.contains("unquoted false"), "{text}");
}

// ---------------------------------------------------------------------------
// `default`, which is the key that decides every category nobody wrote down.
//
// **These build and run a program rather than inspecting a manifest**, and that
// is the whole point of them. `Permissions::grants` had this rule right from
// the day it was written and ten assertions in `khora-manifest` proved it; what
// nothing checked is that a *build* asks. It did not -- `granted_source` read
// `fs`, `env` and `network`, never `default`, and returned `None` when all
// three were absent, leaving the permissive `std/grants.kh` in place. So
// `default = "deny"`, which the manifest reference calls "the strict posture",
// granted everything, and the tests were green throughout. Roadmap 16.1.
//
// A test that cannot fail when the wiring is cut is not protecting the wiring,
// so every one below goes through the binary and reads what the program did.
#[cfg(feature = "llvm")]
mod default_key {
    use super::{pinned, write};
    use std::process::Command;

    /// Reads a file inside the project and one outside it, printing `ok` or
    /// `refused` for each, then the same question for an environment variable.
    const PROBE: &str = "module app::main;\n\n\
         import std::core::{print};\n\
         import std::fs::{read_text, FsRead};\n\
         import std::env::{Env, variable_or};\n\n\
         fn work() -> () with { reads: FsRead, env: Env } {\n  \
         print(\"inside=\" + (read_text(\"./data/ok.txt\")! catch { e => \"refused\" }));\n  \
         print(\"outside=\" + (read_text(\"../secret.txt\")! catch { e => \"refused\" }));\n  \
         print(\"env=\" + (variable_or(\"KHORA_TEST_VALUE\", \"refused\")! catch { e => \"refused\" }));\n\
         }\n\n\
         pub fn main() -> Int {\n  \
         with { reads: FsRead::real(), env: Env::real() } { work() };\n  \
         0\n\
         }\n";

    /// Builds and runs `PROBE` under the given `[permissions]`, answering with
    /// what the program printed.
    fn probe(permissions: &str) -> String {
        let tmp = tempfile::tempdir().expect("a temporary directory");
        let project = tmp.path().join("project");
        write(&project.join("khora.toml"), &pinned(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n{permissions}\n"
        )));
        write(&project.join("src").join("main.kh"), PROBE);
        write(&project.join("data").join("ok.txt"), "inside-the-grant");
        write(&tmp.path().join("secret.txt"), "outside-the-grant");

        let out = Command::new(env!("CARGO_BIN_EXE_khora"))
            .args(["run", "."])
            .current_dir(&project)
            .env("KHORA_HOME", tmp.path().join("home"))
            .env("KHORA_TEST_VALUE", "read-it")
            .output()
            .expect("running khora");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the program should have built and run:\n{text}");
        text
    }

    /// The bug, exactly: one line, and it used to grant everything.
    #[test]
    fn deny_alone_refuses_every_category() {
        let out = probe("[permissions]\ndefault = \"deny\"\n");
        assert!(out.contains("inside=refused"), "fs should be denied:\n{out}");
        assert!(out.contains("outside=refused"), "fs should be denied:\n{out}");
        assert!(out.contains("env=refused"), "env should be denied:\n{out}");
    }

    /// And the other direction, which is the rule that makes tightening
    /// possible: naming one category says nothing about the others.
    #[test]
    fn narrowing_one_category_leaves_the_rest_alone() {
        let out = probe("[permissions.fs]\nread = [\"./data/**\"]\n");
        assert!(out.contains("inside=inside-the-grant"), "the grant should hold:\n{out}");
        assert!(out.contains("outside=refused"), "and stop at its edge:\n{out}");
        assert!(out.contains("env=read-it"), "an unmentioned category is unrestricted:\n{out}");
    }

    /// `deny` plus one grant is the posture the reference recommends, and the
    /// combination is where an off-by-one in the fallback would show.
    #[test]
    fn deny_plus_a_grant_allows_exactly_the_grant() {
        let out = probe("[permissions]\ndefault = \"deny\"\n\n[permissions.fs]\nread = [\"./data/**\"]\n");
        assert!(out.contains("inside=inside-the-grant"), "the named path should be readable:\n{out}");
        assert!(out.contains("outside=refused"), "and nothing else:\n{out}");
        assert!(out.contains("env=refused"), "an unmentioned category still takes `deny`:\n{out}");
    }

    /// The default default. A program that has never heard of permissions is
    /// not asked to think about them.
    #[test]
    fn no_table_at_all_grants_everything() {
        let out = probe("");
        assert!(out.contains("inside=inside-the-grant"), "{out}");
        assert!(out.contains("outside=outside-the-grant"), "{out}");
        assert!(out.contains("env=read-it"), "{out}");
    }

    /// Spelled out, because `allow` is the default and writing it should not
    /// be the same as writing nothing by accident rather than by rule.
    #[test]
    fn allow_spelled_out_grants_everything() {
        let out = probe("[permissions]\ndefault = \"allow\"\n");
        assert!(out.contains("inside=inside-the-grant"), "{out}");
        assert!(out.contains("outside=outside-the-grant"), "{out}");
        assert!(out.contains("env=read-it"), "{out}");
    }
}
