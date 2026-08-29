//! `khora check` from the outside.
//!
//! The command spent its early life reporting only syntax errors while
//! announcing that it had "checked" the file, which is the worst possible
//! failure mode for a command named `check`: a clean exit on a broken program.
//! These tests run the real binary, because that gap was invisible to every
//! library-level test.

use std::path::PathBuf;
use std::process::Command;

/// Writes `source` to a scratch file and runs `khora check` over it.
fn check(name: &str, source: &str) -> (bool, String) {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.kh"));
    std::fs::write(&path, source).expect("could not write the fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("could not run `khora`");

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn a_correct_program_passes() {
    let (ok, output) = check(
        "good",
        "module m;\nfn double(x: Int) -> Int { x + x }\npub fn main() -> Int { double(21) }\n",
    );
    assert!(ok, "expected success, got:\n{output}");
    assert!(output.contains("no errors"), "{output}");
}

#[test]
fn a_syntax_error_fails() {
    let (ok, output) = check("syntax", "module m;\nfn f( -> Int { 1 }\n");
    assert!(!ok, "a broken parse must not exit zero:\n{output}");
    assert!(output.contains("error"), "{output}");
}

/// The regression this file exists for.
#[test]
fn a_type_error_fails() {
    let (ok, output) = check("types", "module m;\nfn f() -> Int { true }\n");
    assert!(!ok, "a type error must not exit zero:\n{output}");
    assert!(
        output.contains("returns `Int`") && output.contains("`Bool`"),
        "expected the mismatch to be named, got:\n{output}"
    );
}

/// A type error is reported where it happened, not at the top of the file.
#[test]
fn a_type_error_points_at_the_offending_line() {
    let (_, output) = check(
        "span",
        "module m;\nfn a() -> Int { 1 }\nfn b() -> Int { false }\nfn c() -> Int { 3 }\n",
    );
    assert!(output.contains(":3:"), "expected a line 3 span, got:\n{output}");
    assert!(output.contains("^"), "expected a caret, got:\n{output}");
}

/// Nothing invented on top of a parse failure: one broken construct should not
/// produce a page of consequential type errors.
#[test]
fn a_file_that_does_not_parse_reports_only_syntax_errors() {
    let (ok, output) = check("only_syntax", "module m;\nfn f( -> Int { 1 }\n");
    assert!(!ok);
    assert!(!output.contains("this function returns"), "{output}");
}

// --- one target's files at a time -------------------------------------------

/// Two files declaring the same module, one per target, and only one of them
/// is ever read.
///
/// The rule is in the file's name — `khora_db::selected_for_target` — and this
/// is the test that `khora` itself applies it. Without the rule the two would
/// be a duplicate-module error; with it they are how a `std::net::socket`
/// exists on Windows and on POSIX at the same time.
#[test]
fn only_this_targets_files_are_read() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets");
    std::fs::create_dir_all(&dir).expect("a workspace");
    for name in ["which_windows.kh", "which_linux.kh", "which_macos.kh"] {
        let target = name
            .trim_start_matches("which_")
            .trim_end_matches(".kh")
            .to_string();
        std::fs::write(
            dir.join(name),
            format!("module t::which;\npub fn which() -> Int {{ 1 }} // {target}\n"),
        )
        .expect("writing a fixture");
    }
    std::fs::write(
        dir.join("main.kh"),
        "module t::main;\nimport t::which::{which};\npub fn main() -> Int { which() }\n",
    )
    .expect("writing a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "expected success, got:\n{text}");
    // Two of the four fixtures belong in this build, plus the standard library,
    // which every build gets without asking. The count is what proves the other
    // two targets' files were never read.
    assert_eq!(count_of(&text), 2 + std_files(), "got:\n{text}");
}

/// A file named on the command line is read whichever target it names. Asking
/// for a file by name is asking for it, and refusing would leave no way to
/// check the other target's version at all.
#[test]
fn a_file_named_outright_is_read_anyway() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets_named");
    std::fs::create_dir_all(&dir).expect("a workspace");
    // A name that cannot be this host's, whichever host that is.
    let other = if cfg!(windows) { "linux" } else { "windows" };
    let path = dir.join(format!("only_{other}.kh"));
    std::fs::write(&path, "module t;\npub fn f() -> Int { 1 }\n").expect("a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "{text}");
    assert_eq!(count_of(&text), 1 + std_files(), "{text}");
}

/// How many files `khora check` said it checked.
fn count_of(output: &str) -> usize {
    let at = output.find("checked ").expect("a count in the output");
    output[at + "checked ".len()..]
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a number after `checked`")
}

/// How many files the standard library contributes to every build.
///
/// Counted rather than written down: `std` grows, and a test that has to be
/// edited every time a module is added to it is a test nobody trusts.
fn std_files() -> usize {
    fn walk(dir: &std::path::Path, seen: &mut usize) {
        for entry in std::fs::read_dir(dir).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(&path, seen);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                *seen += 1;
            }
        }
    }
    let root = khora_db::standard_library().expect("a standard library beside the compiler");
    let mut seen = 0;
    walk(&root, &mut seen);
    seen
}

// --- the command line names an entry point; the manifest names the rest -----

/// A package is built against what its manifest says, not against what the
/// invocation remembers to mention.
///
/// `khora build ./app` is the whole of what a developer should have to say.
/// Which packages it is built against is a property of the package, and
/// repeating it at every call is how the two come to disagree.
#[test]
fn a_path_dependency_comes_from_the_manifest() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("deps");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).expect("a workspace");
    std::fs::create_dir_all(root.join("greet")).expect("a workspace");

    std::fs::write(
        root.join("greet/greet.kh"),
        "module acme::greet;\npub fn greeting() -> Int { 7 }\n",
    )
    .expect("a fixture");
    std::fs::write(
        root.join("app/khora.toml"),
        "[package]
name = \"app\"
version = \"0.1.0\"
edition = \"2026\"

[dependencies]
\"acme.greet\" = { path = \"../greet\" }
",
    )
    .expect("a manifest");
    std::fs::write(
        root.join("app/main.kh"),
        "module app::main;
import acme::greet::{greeting};
pub fn main() -> Int { greeting() }
",
    )
    .expect("a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(root.join("app"))
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "the dependency should have been found:\n{text}");
    assert_eq!(count_of(&text), 2 + std_files(), "the app and its dependency:\n{text}");
}

/// The standard library is there without being declared, the way `rustc` finds
/// its sysroot. A program that has never written a manifest still has one.
#[test]
fn the_standard_library_needs_no_declaring() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("implicit_std");
    std::fs::create_dir_all(&dir).expect("a workspace");
    std::fs::write(
        dir.join("main.kh"),
        "module app::main;
import std::core::{Option};
pub fn main() -> Int { Option::Some(41).unwrap_or(0) + 1 }
",
    )
    .expect("a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "no manifest, and `std` is still there:\n{text}");
}

/// A version needs a registry, which does not exist. Saying so beats resolving
/// to nothing and failing somewhere further along.
#[test]
fn a_version_dependency_says_what_is_missing() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("versioned");
    std::fs::create_dir_all(&dir).expect("a workspace");
    std::fs::write(
        dir.join("khora.toml"),
        "[package]
name = \"app\"
version = \"0.1.0\"
edition = \"2026\"

[dependencies]
\"acme.json\" = { version = \"1.0.0\" }
",
    )
    .expect("a manifest");
    std::fs::write(dir.join("main.kh"), "module app::main;\npub fn main() -> Int { 0 }\n")
        .expect("a fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "{text}");
    assert!(text.contains("registry"), "expected the missing registry to be named:\n{text}");
}

/// The manifest audit reaches a person.
///
/// It did not until 14.20b: `khora-manifest` produced a `Warning` per
/// unrecognized key and every caller dropped the vector. A whole module
/// arriving nowhere.
#[test]
fn a_manifest_warning_is_printed_and_is_not_fatal() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("manifest_warning");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("a package");
    std::fs::write(
        dir.join("khora.toml"),
        "[package]{n}name = {q}warned{q}{n}version = {q}0.1.0{q}{n}{n}[fmt]{n}         explicit-semicolons = true{n}future-knob = 3{n}"
            .replace("{n}", "
")
            .replace("{q}", "\""),
    )
    .expect("a manifest");
    std::fs::write(
        dir.join("src").join("lib.kh"),
        "module warned::lib;{n}{n}pub fn go() -> Int {{{n}  1{n}}}{n}"
            .replace("{n}", "
"),
    )
    .expect("a module");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "a warning is not a failure: {text}");
    assert!(text.contains("removed key"), "{text}");
    assert!(text.contains("never a choice"), "the reason, not just the fact: {text}");
    assert!(text.contains("unrecognized key"), "the other kind still works: {text}");
}

/// Writes a package under the scratch directory and runs `khora check` on it.
///
/// `files` is `(relative path, contents)`. Directories are created as needed,
/// so a nested package is written by naming a path with a `khora.toml` in it.
fn check_package(name: &str, files: &[(&str, &str)]) -> (bool, String) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (relative, contents) in files {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("could not make the fixture directory");
        }
        std::fs::write(&path, contents).expect("could not write the fixture");
    }

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(&dir)
        .output()
        .expect("could not run `khora`");

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2026\"\n";

/// **Two files may not declare the same module, and `check` says so.**
///
/// It said nothing. The check existed in the module graph and nothing ever
/// read its errors, so a helper whose name already existed in a sibling file
/// silently changed which one the program called -- the later file won, and
/// `khora check` reported no errors at all. Found by somebody's second
/// program, where the name in question was `main`.
#[test]
fn two_files_may_not_declare_the_same_module() {
    let (ok, output) = check_package(
        "dup_module",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;\nimport std::core::{print};\n\
                 fn shared_name() -> Int { 1 }\n\
                 pub fn main() { print(Int::to_string(shared_name())); }\n",
            ),
            ("src/second.kh", "module main;\nfn shared_name() -> Int { 2 }\n"),
        ],
    );

    assert!(!ok, "two files claiming one module must not pass:\n{output}");
    assert!(output.contains("already declared"), "{output}");
    // Named after the file that has the module, so the other half is findable.
    assert!(output.contains("main.kh"), "{output}");
    // And pointed at the offending `module` line rather than at byte zero.
    assert!(output.contains("second.kh:1:1"), "{output}");
}

/// **A package nested inside another is a different package.**
///
/// A walk collected every `.kh` under the directory it was given, manifest or
/// no manifest, so a scratch reproducer with its own `khora.toml` was absorbed
/// into its parent's compilation: its `fn main` competed with the parent's,
/// its errors were reported against the parent, and `khora run` on the parent
/// wrote the executable under the *nested* package's path and ran the wrong
/// program. `collect_sources` already said the package is the manifest's
/// directory; the walk did not stop there.
#[test]
fn a_nested_package_is_not_absorbed_by_its_parent() {
    let files = &[
        ("khora.toml", MANIFEST),
        ("src/main.kh", "module main;\nimport std::core::{print};\npub fn main() { print(\"outer\"); }\n"),
        ("repro/khora.toml", MANIFEST),
        (
            "repro/src/main.kh",
            "module main;\nimport std::core::{print};\npub fn main() { print(\"inner\"); }\n",
        ),
    ];
    let (ok, output) = check_package("nested_package", files);

    // Two `module main;` in one compilation would be the error above; the
    // point is that they are not in one compilation.
    assert!(ok, "the nested package must not join its parent:\n{output}");
    assert!(!output.contains("already declared"), "{output}");
}

/// The nested package still checks perfectly well on its own, which is the
/// half that makes the rule a boundary rather than an exclusion.
#[test]
fn a_nested_package_still_checks_on_its_own() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("nested_package_alone");
    let _ = std::fs::remove_dir_all(&dir);
    for (relative, contents) in [
        ("khora.toml", MANIFEST),
        ("src/main.kh", "module main;\nimport std::core::{print};\npub fn main() { print(\"outer\"); }\n"),
        ("repro/khora.toml", MANIFEST),
        (
            "repro/src/main.kh",
            "module main;\nimport std::core::{print};\npub fn main() { print(\"inner\"); }\n",
        ),
    ] {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("could not make the fixture directory");
        }
        std::fs::write(&path, contents).expect("could not write the fixture");
    }

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(dir.join("repro"))
        .output()
        .expect("could not run `khora`");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(out.status.success(), "{text}");
    assert!(text.contains("no errors"), "{text}");
}
