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

/// **A seventy-element list literal used to kill the process.**
///
/// `[1, 2, ..]` desugars to `Cons(1, Cons(2, ..))`, so a literal of *n* items
/// is a tree *n* deep and every pass that walks an expression tree recurses
/// once per level. On the main thread's one megabyte that ran out at
/// sixty-nine items, and the whole of the output was `thread 'main' has
/// overflowed its stack` -- no file, no line, no note. It was found by
/// somebody writing an ordinary test: a hundred copies of `0.11d`.
///
/// The fix is a worker thread with a large stack, which lives in the binary's
/// `main`, so this has to run the binary to see it. A thousand elements is far
/// past the old ceiling and far short of the new one, and it checks the
/// *answer* rather than only the exit status: a compiler that lost elements on
/// the way through would otherwise pass.
#[test]
fn a_long_list_literal_does_not_exhaust_the_stack() {
    let ns = vec!["1"; 1000].join(", ");
    let (ok, output) = check(
        "long_list_literal",
        &format!(
            "module m;\nimport std::core::{{List}};\n\n             pub fn main() -> Int {{ let ns = [{ns}]; List::length(ns) - 1000 }}\n"
        ),
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
            format!(
                "module t::which;\n/// Which target this file is for.\n\
                 pub fn which() -> Int {{ 1 }} // {target}\n"
            ),
        )
        .expect("writing a fixture");
    }
    std::fs::write(
        dir.join("main.kh"),
        "module t::main;\nimport t::which::{which};\n/// The entry point.\n\
         pub fn main() -> Int { which() }\n",
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
    std::fs::write(&path, "module t;\n/// Something to read.\npub fn f() -> Int { 1 }\n")
        .expect("a fixture");

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

/// **A dependency's tests are not modules of the program that depends on it.**
///
/// They were, and it went further than a slower build: a library's `test`
/// module was a module of the consuming program, so `import greet_test::{..}`
/// resolved and reached types the library wrote for its own tests, and
/// `khora test` in the consumer ran every dependency's suite — somebody
/// else's failing test failing your run.
///
/// Both halves are asserted. The dependency's ordinary code still arrives,
/// because a library with tests in it is still a library.
#[test]
fn a_dependencys_tests_are_not_part_of_this_build() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("deps_tests");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).expect("a workspace");
    std::fs::create_dir_all(root.join("greet")).expect("a workspace");

    std::fs::write(
        root.join("greet/greet.kh"),
        "module acme::greet;\npub fn greeting() -> Int { 7 }\n",
    )
    .expect("a fixture");
    // The library's own test module, carrying a `pub` type a consumer must
    // not be able to reach.
    std::fs::write(
        root.join("greet/greet_test.kh"),
        "module acme::greet_test;\n\
         import std::core::{assert};\n\
         import acme::greet::{greeting};\n\
         pub type TestOnly = { secret: Int };\n\
         test \"it greets\" {\n\
         \x20 assert(greeting() == 7);\n\
         }\n",
    )
    .expect("a fixture");
    std::fs::write(
        root.join("app/khora.toml"),
        "[package]
name = \"app\"
version = \"0.1.0\"

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

    assert!(out.status.success(), "the dependency should still work:\n{text}");
    assert_eq!(
        count_of(&text),
        2 + std_files(),
        "the app and its dependency, and not the dependency's test module:\n{text}"
    );

    // And the test module is unreachable by name, which is the half a file
    // count cannot show.
    std::fs::write(
        root.join("app/main.kh"),
        "module app::main;
import acme::greet_test::{TestOnly};
pub fn main() -> Int { 0 }
",
    )
    .expect("a fixture");
    let reaching = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("check")
        .arg(root.join("app"))
        .output()
        .expect("could not run `khora`");
    let reaching_text = String::from_utf8_lossy(&reaching.stdout).into_owned()
        + &String::from_utf8_lossy(&reaching.stderr);
    assert!(
        !reaching.status.success(),
        "a consumer must not reach into a dependency's tests:\n{reaching_text}"
    );
}

/// **A library keeps its tests in the file it publishes, and stays importable.**
///
/// `khora new --lib` writes the API and a `test` block into one `src/lib.kh`,
/// and `reference/testing` teaches exactly that. An earlier attempt at the
/// test-module rule excluded dependency *files* holding a test, which dropped
/// that file -- so every scaffolded library became unimportable, and the error
/// pointed at the consumer's call site rather than at anything the author did.
///
/// The rule is about module paths, not files: `lib.kh` declares the package's
/// own module, so it belongs no matter what else it declares.
#[test]
fn a_library_with_a_test_beside_its_api_is_still_importable() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lib_with_test");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).expect("a workspace");
    std::fs::create_dir_all(root.join("csv")).expect("a workspace");

    // The scaffold's shape: one file, API and test together.
    std::fs::write(
        root.join("csv/lib.kh"),
        "module csv;
import std::core::{assert};

pub fn parse() -> Int { 7 }

test \"parse returns seven\" {
  assert(parse() == 7);
}
",
    )
    .expect("a fixture");
    std::fs::write(
        root.join("app/khora.toml"),
        "[package]
name = \"app\"
version = \"0.1.0\"

[dependencies]
csv = { path = \"../csv\" }
",
    )
    .expect("a manifest");
    std::fs::write(
        root.join("app/main.kh"),
        "module app::main;
import csv::{parse};
pub fn main() -> Int { parse() }
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

    assert!(
        out.status.success(),
        "a library whose tests sit beside its API must still be importable:\n{text}"
    );
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

[dependencies]
\"acme.json\" = { version = \"1.0.0\" }
",
    )
    .expect("a manifest");
    std::fs::write(dir.join("main.kh"), "module app::main;\n/// The entry point.\npub fn main() -> Int { 0 }\n")
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

const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n";

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

/// Runs one of `khora`'s commands over a package written under the scratch
/// directory, and hands back everything it said.
fn command_on_package(name: &str, verb: &str, files: &[(&str, &str)]) -> (bool, String) {
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
        .arg(verb)
        .arg(&dir)
        .output()
        .expect("could not run `khora`");

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// A file with enough text in it to have something at the offsets the *other*
/// file's error lands on. That is the whole fixture: the bug was invisible
/// unless the wrong file happened to be long enough to render.
const PADDING: &str = "module fixture::first;

/// A long doc comment, so this file has text at the byte offsets the error in
/// `main.kh` will be rendered at if anything renders it here by mistake.
/// Another line, to be sure of it.
///
/// And another.
pub fn helper() -> Int { 1 }
";

const BROKEN_MAIN: &str = "module main;
import std::core::{print};
import fixture::first::{helper};

pub fn main() -> Int {
  print(Int::to_string(helper()));
  let wrong: Int = \"not an int\";
  0
}
";

/// **An error is reported against the file it is in.**
///
/// A `HirError` carries a `TextRange` and no file, and `report_build_errors`
/// rendered every one of them against `inputs[0]` -- so a build or a test run
/// showed the right message at the right byte offsets *in the wrong file*,
/// with the caret under a line that had nothing to do with it. One report of
/// this had `khora test` pointing at line 155 of a 154-line file, at doc
/// comments in a module the error was not in.
///
/// `khora check` has always been right, because it asks each file for its own
/// diagnostics. These pin that the other two agree with it.
///
/// `build` and `test` need the backend, so these three run only with it, as
/// the whole of `run.rs` and `binaries.rs` do; without the gate the front-end
/// tier ran them against a `khora` that could only say it had no backend.
#[cfg(feature = "llvm")]
#[test]
fn a_build_error_names_the_file_it_is_in() {
    let files = &[("khora.toml", MANIFEST), ("src/first.kh", PADDING), ("src/main.kh", BROKEN_MAIN)];
    let (ok, output) = command_on_package("wrong_file_build", "build", files);

    assert!(!ok, "the program is broken:\n{output}");
    assert!(output.contains("expected `Int`, found `String`"), "{output}");
    assert!(output.contains("main.kh:7"), "the error is in main.kh, line 7:\n{output}");
    assert!(!output.contains("first.kh"), "and not in first.kh:\n{output}");
}

/// The same for `khora test`, which is where it was found.
#[cfg(feature = "llvm")]
#[test]
fn a_test_error_names_the_file_it_is_in() {
    let files = &[("khora.toml", MANIFEST), ("src/first.kh", PADDING), ("src/main.kh", BROKEN_MAIN)];
    let (ok, output) = command_on_package("wrong_file_test", "test", files);

    assert!(!ok, "the program is broken:\n{output}");
    assert!(output.contains("main.kh:7"), "{output}");
    assert!(!output.contains("first.kh"), "{output}");
}

/// And `khora check` still says exactly the same thing, which is the point of
/// comparison the other two were measured against.
#[test]
fn check_build_and_test_agree_about_where_an_error_is() {
    let files = &[("khora.toml", MANIFEST), ("src/first.kh", PADDING), ("src/main.kh", BROKEN_MAIN)];
    let (_, checked) = command_on_package("wrong_file_check", "check", files);
    assert!(checked.contains("main.kh:7"), "{checked}");
    assert!(!checked.contains("first.kh"), "{checked}");
}

/// **A failing test says which assertion failed.**
///
/// It said only that the test had failed -- no line, no values, no ordinal --
/// so finding out which of six assertions it was meant deleting them one at a
/// time until it passed. Somebody did exactly that.
#[cfg(feature = "llvm")]
#[test]
fn a_failing_assertion_says_which_one_it_was() {
    let (ok, output) = command_on_package(
        "which_assert",
        "test",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;\n\
                 import std::core::{assert, print};\n\
                 \n\
                 pub fn main() -> Int { print(\"hi\"); 0 }\n\
                 \n\
                 test \"four assertions, the third of which does not hold\" {\n  \
                 assert(1 + 1 == 2);\n  \
                 assert(2 + 2 == 4);\n  \
                 assert(3 + 3 == 7);\n  \
                 assert(4 + 4 == 8);\n\
                 }\n",
            ),
        ],
    );

    assert!(!ok, "the third assertion does not hold:\n{output}");
    assert!(output.contains("assertion 3 failed"), "and it says which:\n{output}");
    // **And where.** The ordinal alone meant counting `assert`s to find the
    // one that went; the line is passed as an immediate at the call, so it
    // reads the same in a release build as in a debug one.
    assert!(output.contains("at line"), "and where it was written:\n{output}");
}

/// **A bare relative path finds the workspace root that `./` finds.**
///
/// `khora check src/lib.kh` walked to the empty path and read the manifest as
/// the bare name `khora.toml`, which has no parent to look above -- so a
/// member inheriting anything from its root was told there was no root, with
/// the root sitting one directory up. `khora check ./src/lib.kh` worked, and
/// the difference was two characters. `reference/manifest.md` writes the bare
/// spelling.
#[test]
fn a_bare_relative_path_still_finds_the_workspace_root() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bare_relative_path");
    let _ = std::fs::remove_dir_all(&root);
    let member = root.join("packages").join("alpha");
    std::fs::create_dir_all(member.join("src")).expect("a member directory");
    std::fs::write(
        root.join("khora.toml"),
        format!(
            "[workspace]\nmembers = [\"packages/*\"]\n\n\
             [workspace.lints]\nunused-import = \"warn\"\n\n\
             [toolchain]\nversion = \"{}\"\n",
            khora_toolchain::RUNNING,
        ),
    )
    .expect("a workspace root");
    std::fs::write(
        member.join("khora.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n\n[lints]\nworkspace = true\n",
    )
    .expect("a member manifest");
    std::fs::write(
        member.join("src").join("lib.kh"),
        "module alpha::lib;\n\npub fn go() -> Int {\n    1\n}\n",
    )
    .expect("a member source file");

    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(["check", "src/lib.kh"])
        .current_dir(&member)
        .output()
        .expect("could not run `khora`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !text.contains("no workspace root above"),
        "the root is one directory up:\n{text}"
    );
    assert!(out.status.success(), "{text}");
}

/// **A dependency ships the archive; only the root package may link it.**
///
/// The shape a driver package has to have. A native library cannot be reached
/// from Khora at all unless something puts `-l` on the link line, and the
/// question is who. A dependency doing it would be a supply-chain change with
/// no signal at the place that would have to consent, so the dependency ships
/// the bytes and declares `extern fn`, and the program that depends on it
/// writes one line saying yes.
///
/// Asserted in both directions: the consumer's build fails while the line is
/// absent, and succeeds once it is there. Without the first half this test
/// would pass against a build that linked everything it found.
///
/// **Gated on `llvm`, like every test here that drives a real build.** The
/// default `cargo nextest run --workspace` builds `khora` without a backend,
/// and such a binary refuses every `khora build` with "this `khora` was built
/// without the LLVM backend" -- which arrives as this test's own assertion
/// failing on all three platforms, reading like a defect in the feature.
#[cfg(feature = "llvm")]
#[test]
fn only_the_root_package_may_link_a_native_library() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("native_link");
    let _ = std::fs::remove_dir_all(&root);
    let vendor = root.join("vendor");
    std::fs::create_dir_all(&vendor).expect("a workspace");

    // A C archive, built with the linker the toolchain already requires.
    let c = vendor.join("answer.c");
    std::fs::write(&c, "long doubled(long n) { return n * 2; }\n").expect("the C source");
    let object = vendor.join("answer.o");
    let clang = khora_codegen_llvm::toolchain::linker().expect("a C compiler");
    let compiled = Command::new(&clang)
        .arg("-c")
        .arg(&c)
        .arg("-o")
        .arg(&object)
        .output()
        .expect("running the C compiler");
    assert!(compiled.status.success(), "compiling the fixture library");
    // **Named the way this platform's linker looks for it.** `-lanswer` finds
    // `libanswer.a` where the linker is ELF or Mach-O and `answer.lib` on
    // Windows, and `llvm-ar` writes either -- so a fixture that always wrote
    // the Unix spelling built fine and then failed at the link with
    // `could not open 'answer.lib'`, which reads like a defect in the feature
    // rather than in the test.
    let archive = vendor.join(if cfg!(windows) {
        "answer.lib"
    } else {
        "libanswer.a"
    });
    let ar = khora_codegen_llvm::toolchain::tool("llvm-ar")
        .or_else(|| khora_codegen_llvm::toolchain::tool("ar"))
        .unwrap_or_else(|| PathBuf::from("ar"));
    let archived = Command::new(&ar)
        .arg("rcs")
        .arg(&archive)
        .arg(&object)
        .output()
        .expect("running ar");
    assert!(
        archived.status.success(),
        "archiving the fixture library: {}",
        String::from_utf8_lossy(&archived.stderr)
    );
    assert!(archive.is_file(), "the archive should exist at {archive:?}");

    let app = root.join("app");
    std::fs::create_dir_all(app.join("src")).expect("the package");
    std::fs::write(
        app.join("src").join("main.kh"),
        "module app::main;\n\
         \n\
         import std::core::{print};\n\
         \n\
         extern fn doubled(n: Int) -> Int;\n\
         \n\
         pub fn main() -> Int {\n\
         \x20 print(\"${doubled(21)}\");\n\
         \x20 0\n\
         }\n",
    )
    .expect("the program");

    // Permitted to declare the extern, but naming no library to satisfy it.
    let manifest = app.join("khora.toml");
    let base = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                [toolchain]\nversion = \"0.3.0\"\n\n\
                [permissions]\nextern = [\"app\"]\n";
    std::fs::write(&manifest, base).expect("the manifest");

    let without = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("build")
        .arg(&app)
        .output()
        .expect("could not run `khora`");
    assert!(
        !without.status.success(),
        "a build that names no library must not resolve the symbol"
    );

    // The one line that says yes.
    std::fs::write(
        &manifest,
        format!(
            "{base}\n[build]\nlink = [\"answer\"]\nlink-search = [\"../vendor\"]\n"
        ),
    )
    .expect("the manifest");

    let with = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("build")
        .arg(&app)
        .output()
        .expect("could not run `khora`");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&with.stdout),
        String::from_utf8_lossy(&with.stderr)
    );
    assert!(
        with.status.success(),
        "`build.link` should satisfy the extern:\n{said}"
    );
    assert!(
        !said.contains("unrecognized key"),
        "the manifest audit should know both keys:\n{said}"
    );
}

/// **A row in a `let` annotation was not checked at all.**
///
/// Khora reads a written type through two converters. A signature goes through
/// `type_of_syntax`, which has carried rows since errata 59. A body annotation
/// goes through the HIR echo `TypeRef::of_syntax`, which had no row variant --
/// so `ast::Type::Record` fell into the catch-all beside `Fn | Union | Variant
/// | Forall`, became `TypeRef::Opaque`, and `type_of_ref` read that as
/// `Type::Unknown`.
///
/// `Unknown` unifies with anything, so the row was not merely lost, it was
/// *permissive*: the program below passes a `{ Bad: Bad }` list to a parameter
/// declared `{ Oops: Oops }` and compiled clean. The loud half of the same
/// defect refused valid programs -- `for` over a list of row-carrying values
/// was rejected, blaming `Iterator::next`, a function the source never names.
///
/// **This is the case that goes green-to-red**, which is why it is the test. A
/// test that only asserted the valid program compiles would pass against a
/// build that still dropped the row, because dropping it is what made the
/// invalid program pass too. It is the fourth site of the shape errata 30, 59
/// and 60 describe, and the first outside `khora-types/src/syntax.rs`.
#[test]
fn a_row_written_in_a_let_annotation_is_checked() {
    let (ok, output) = check(
        "row_annotation_mismatch",
        "module m;\n\
         import std::core::{List};\n\
         type Oops = { why: String };\n\
         type Bad = { nope: String };\n\
         type Job<A, 'er> = { done: A };\n\
         fn take(js: List<Job<Int, { Oops: Oops }>>) -> Int { 0 }\n\
         pub fn main() -> Int {\n\
         \x20 let js: List<Job<Int, { Bad: Bad }>> = List::Cons({ done: 1 }, List::Nil);\n\
         \x20 take(js)\n\
         }\n",
    );
    assert!(
        !ok,
        "a row mismatch between the annotation and the parameter has to be \
         refused; the annotation's row was being dropped:\n{output}"
    );
    assert!(
        output.contains("Bad"),
        "the error should name the row entry that is not accounted for:\n{output}"
    );
}

/// The other half: carrying the row must not refuse a program that agrees.
///
/// `for` over a list of row-carrying values is the shape a worker pool takes,
/// and it was rejected outright while the row was `Unknown`.
#[test]
fn a_for_loop_over_a_row_carrying_list_compiles() {
    let (ok, output) = check(
        "row_annotation_for",
        "module m;\n\
         import std::core::{List, Iterator, Step};\n\
         type Oops = { why: String };\n\
         type Job<A, 'er> = { done: A };\n\
         pub fn main() -> Int {\n\
         \x20 let js: List<Job<Int, { Oops: Oops }>> = List::Cons({ done: 1 }, List::Nil);\n\
         \x20 for j in js { let _ = j.done; };\n\
         \x20 0\n\
         }\n",
    );
    assert!(ok, "expected success, got:\n{output}");
}

/// **The correct `raises` row in an annotation replaced a good error with a
/// worse one.**
///
/// A function type carrying either clause was not echoed at all: it fell to
/// `TypeRef::Opaque` and then to `Type::Unknown`, so the list's element type
/// was unknown and the failure surfaced in the `for` desugar, naming
/// `Iterator::next` — a function this source does not mention, three lines
/// from the annotation. Adding the row that fixes the first error is the
/// obvious move, and it was punished.
#[test]
fn a_raises_row_in_a_let_annotation_is_honoured() {
    let (ok, output) = check(
        "let_annotation_raises_row",
        "module m;\n\
         import std::core::{List, Iterator, Step};\n\
         type Boom = | Went;\n\
         pub fn main() -> Int raises Boom {\n\
         \x20 let handlers: List<(Int) -> () raises Boom> =\n\
         \x20   List::Cons(fn (_x: Int) => raise Boom::Went, List::Nil);\n\
         \x20 for h in handlers { h(1)!; };\n\
         \x20 0\n\
         }\n",
    );
    assert!(ok, "expected success, got:\n{output}");
}

/// A row wider than the value that fills it is legal, nested or not.
///
/// **This pins a non-defect, because it looked like one.** A review of the
/// annotation work reported that a wrong clause *inside a type argument* is
/// caught only at the use site rather than at the `let`, and read that as the
/// fix reaching one level down but landing in the wrong place. The `let` is
/// not wrong: a lambda that raises nothing satisfies an annotation that says
/// it may raise `Bang`, exactly as it does when the annotation is direct
/// rather than nested. Widening is sound in both, so there is nothing at the
/// binding to report, and the first genuine disagreement — passing that value
/// where a *different* row is required — is reported where it happens.
///
/// Both halves are here so the next reader sees that the nested case follows
/// the direct one rather than diverging from it. A change that starts refusing
/// either is narrowing the language, and this says so.
#[test]
fn a_row_wider_than_the_value_that_fills_it_is_accepted() {
    let (ok, output) = check(
        "let_annotation_row_widening",
        "module m;\n\
         type Bang = | Off;\n\
         type Wrap<A> = { inner: A };\n\
         pub fn main() -> Int {\n\
         \x20 let direct: (Int) -> () raises Bang = fn (_x: Int) => ();\n\
         \x20 let nested: Wrap<(Int) -> () raises Bang> =\n\
         \x20   { inner: fn (_x: Int) => () };\n\
         \x20 let _ = direct;\n\
         \x20 let _ = nested;\n\
         \x20 0\n\
         }\n",
    );
    assert!(ok, "expected success, got:\n{output}");
}

/// The half that goes green-to-red: an annotation that is only a comment is
/// worse than no annotation, because it is believed.
///
/// The closure raises `Boom` and the annotation says `Bang`. While the row was
/// dropped, `Unknown` agreed with both and this compiled clean. The error has
/// to name the binding, not a line further on: the annotation is what is
/// wrong.
#[test]
fn a_wrong_raises_row_in_a_let_annotation_is_refused_at_the_annotation() {
    let (ok, output) = check(
        "let_annotation_raises_row_wrong",
        "module m;\n\
         type Boom = | Went;\n\
         type Bang = | Off;\n\
         pub fn main() -> Int raises Bang {\n\
         \x20 let f: (Int) -> () raises Bang = fn (_x: Int) => raise Boom::Went;\n\
         \x20 f(1)!;\n\
         \x20 0\n\
         }\n",
    );
    assert!(
        !ok,
        "a closure raising `Boom` against an annotation saying `Bang` has to be \
         refused; the annotation's row was being dropped:\n{output}"
    );
    assert!(
        output.contains("Boom"),
        "the error should name the row entry the annotation does not account \
         for:\n{output}"
    );
    assert!(
        output.contains("let f: (Int) -> () raises Bang"),
        "the error should point at the annotated binding, not at a later use \
         of it:\n{output}"
    );
}

/// The same for `with`, which is a different clause reader: a `with` clause
/// may name a `row` declaration and a `raises` clause may not.
///
/// Written as `with Deps`, so the annotation reaches the splice too — a `row`
/// is structural and its fields replace the clause outright, a lookup only
/// `khora-types` can do and the reason the echo carries the clause rather than
/// reading it.
#[test]
fn a_with_row_in_a_let_annotation_is_honoured() {
    let source = "module m;\n\
         effect Ticks { now: () -> Int }\n\
         row Deps = { ticks: Ticks };\n\
         fn plain(f: (Int) -> Int) -> Int { f(1) }\n\
         fn helper() -> Int with Deps {\n\
         \x20 let f: (Int) -> Int with Deps = fn (x: Int) => x + ticks.now();\n\
         \x20 f(1)\n\
         }\n\
         pub fn main() -> Int { helper() with { ticks: { now: fn () => 7 } } }\n";
    let (ok, output) = check("let_annotation_with_row", source);
    assert!(ok, "expected success, got:\n{output}");

    // And the annotation is load-bearing: handed to a parameter whose row is
    // closed and empty, the same `f` has to be refused. While the clause was
    // dropped this compiled, which is what says the clause is now read.
    let (ok, output) = check(
        "let_annotation_with_row_escapes",
        &source.replace("\x20 f(1)\n", "\x20 plain(f)\n"),
    );
    assert!(
        !ok,
        "a function requiring `ticks` cannot be passed where one requiring \
         nothing is wanted; the annotation's `with` row was being dropped:\n{output}"
    );
    assert!(
        output.contains("ticks"),
        "the error should name the capability the parameter does not \
         supply:\n{output}"
    );
}

/// A row *variable* in a `let` annotation, which is the shape that would need
/// its own scoping if the echo interpreted rows itself.
///
/// It does not: `'er` is resolved by the same `named_type` that resolves it in
/// a signature, against the same `generics` list, so the parameter the
/// enclosing `fn` declares is the one the annotation names. Nothing here is
/// bound by the annotation.
#[test]
fn a_row_variable_in_a_let_annotation_names_the_enclosing_parameter() {
    let (ok, output) = check(
        "let_annotation_row_var",
        "module m;\n\
         fn run<'er>(g: (Int) -> Int raises 'er) -> Int raises 'er {\n\
         \x20 let f: (Int) -> Int raises 'er = g;\n\
         \x20 f(1)!\n\
         }\n\
         pub fn main() -> Int { run(fn (x: Int) => x + 1) }\n",
    );
    assert!(ok, "expected success, got:\n{output}");
}

/// A `main` that waits for a child that cannot fail. `wait` is on line 6.
const WAIT_IN_INFALLIBLE_MAIN: &str = "module main;
import std::core::{Fiber, print};

pub fn main() -> Int {
  let hand = Fiber::spawn(fn () => 1);
  Fiber::wait(hand);
  print(\"waited\");
  0
}
";

/// **`check` refuses a `Fiber::wait` in a function with no `raises` clause.**
///
/// It passed the program and `build` refused it, so the two commands disagreed
/// about one program and the editor, which runs the checker, showed nothing.
/// The row on `wait` is the child's, and over an infallible child it is `{}`,
/// which demands nothing -- but the waiter can be cancelled while it is parked,
/// and a cancellation needs the failure channel whatever the child's row is.
///
/// The build's refusal was also rendered against a standard-library file,
/// because a code-generator error carries no file; see
/// `a_build_names_the_users_file_for_a_wait_with_no_channel` below.
#[test]
fn a_wait_in_a_function_with_no_raises_clause_is_refused_by_check() {
    let (ok, output) = command_on_package(
        "wait_no_raises_check",
        "check",
        &[("khora.toml", MANIFEST), ("src/main.kh", WAIT_IN_INFALLIBLE_MAIN)],
    );

    assert!(!ok, "a wait with nowhere to be cancelled to must not check:\n{output}");
    assert!(output.contains("`Fiber::wait` is a place this function can be cancelled"), "{output}");
    // Why, and what to write -- both repairs, with a type to name.
    assert!(output.contains("cancelled while it is parked"), "{output}");
    assert!(output.contains("-> Int raises String"), "{output}");
    assert!(output.contains("catch { _ => () }"), "{output}");
    assert!(output.contains("main.kh:6:3"), "at the call, in the user's file:\n{output}");
}

/// The method spelling reaches the same rule, under a different key.
#[test]
fn a_wait_spelled_as_a_method_is_refused_the_same_way() {
    let source = WAIT_IN_INFALLIBLE_MAIN.replace("Fiber::wait(hand);", "hand.wait();");
    let (ok, output) = command_on_package(
        "wait_no_raises_method",
        "check",
        &[("khora.toml", MANIFEST), ("src/main.kh", &source)],
    );

    assert!(!ok, "{output}");
    assert!(output.contains("`Fiber::wait` is a place this function can be cancelled"), "{output}");
    assert!(output.contains("main.kh:6:3"), "{output}");
}

/// **A `wait` inside a closure that raises nothing is refused at the call**,
/// and told the two repairs that work in place: a `catch` inside the closure,
/// or a failing type on it. It passed `check` and failed to build with a
/// message about a `raises` clause a closure cannot write.
#[test]
fn a_wait_inside_a_closure_that_raises_nothing_is_a_clear_refusal() {
    let (ok, output) = command_on_package(
        "wait_in_closure",
        "check",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;
import std::core::{Fiber, print};

pub type Oops = | Oops;

pub fn main() -> Int raises Oops {
  let inner = Fiber::spawn(fn () => 1);
  let outer = Fiber::spawn(fn () => { Fiber::wait(inner); 2 });
  Fiber::wait(outer)!;
  print(\"waited\");
  0
}
",
            ),
        ],
    );

    assert!(!ok, "{output}");
    assert!(output.contains("the closure raises nothing"), "{output}");
    assert!(output.contains("Handle it inside the closure"), "{output}");
    assert!(output.contains("give the closure a failing type"), "{output}");
    assert!(output.contains("main.kh:8:39"), "{output}");
}

/// **A `catch` is a channel, and so is a closure that already raises.** Both
/// build and run today; the rule above must not reach them. This passes with
/// the refusal disabled too -- it is the boundary, not the rule.
#[test]
fn a_wait_with_a_channel_is_not_refused() {
    let (ok, output) = command_on_package(
        "wait_with_channel",
        "check",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;
import std::core::{Fiber, print};

pub type Oops = | Oops;

fn boom(n: Int) -> Int raises Oops { if n > 5 { raise Oops::Oops } else { n } }

pub fn main() -> Int {
  let hand = Fiber::spawn(fn () => 1);
  Fiber::wait(hand)! catch { _ => print(\"caught\") };
  let inner = Fiber::spawn(fn () => 1);
  let outer = Fiber::spawn(fn () => { let x = boom(1)!; Fiber::wait(inner)!; x });
  Fiber::wait(outer)! catch { _ => print(\"caught\") };
  0
}
",
            ),
        ],
    );

    assert!(ok, "{output}");
}

/// **The build agrees with `check`, and says so in the user's file.**
///
/// A code-generator error carries a range and no file, and the build rendered
/// it against whichever input sorted first -- a standard-library file, with the
/// caret under a doc comment. The checker's refusal is what makes this one
/// right: the build reports the checker's diagnostics per file, before any code
/// is generated.
#[cfg(feature = "llvm")]
#[test]
fn a_build_names_the_users_file_for_a_wait_with_no_channel() {
    let (ok, output) = command_on_package(
        "wait_no_raises_build",
        "build",
        &[("khora.toml", MANIFEST), ("src/main.kh", WAIT_IN_INFALLIBLE_MAIN)],
    );

    assert!(!ok, "{output}");
    assert!(output.contains("main.kh:6:3"), "{output}");
    assert!(!output.contains("clock_native.kh"), "not in a file the user never opened:\n{output}");
}

/// The same program with a channel checks, builds, runs, and waits.
#[cfg(feature = "llvm")]
#[test]
fn a_wait_in_a_function_with_a_raises_clause_runs() {
    let (ok, output) = command_on_package(
        "wait_with_raises_run",
        "run",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;
import std::core::{Fiber, print};

pub type Oops = | Oops;

pub fn main() -> Int raises Oops {
  let hand = Fiber::spawn(fn () => 1);
  Fiber::wait(hand)!;
  print(\"waited\");
  0
}
",
            ),
        ],
    );

    assert!(ok, "{output}");
    assert!(output.contains("waited"), "{output}");
}

/// **Both repairs the closure message offers work.** A `catch` inside the
/// closure, in a `main` with no channel of its own, and a closure given a
/// failing type by its binding. The message is only honest while these build.
#[cfg(feature = "llvm")]
#[test]
fn the_closure_repairs_the_message_offers_build_and_run() {
    let (ok, output) = command_on_package(
        "wait_closure_repairs",
        "run",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;
import std::core::{Fiber, print};

pub type Oops = | Oops;

pub fn main() -> Int {
  let inner = Fiber::spawn(fn () => 1);
  let outer = Fiber::spawn(fn () => { Fiber::wait(inner)! catch { _ => () }; 2 });
  let hand = Fiber::spawn(fn () => 3);
  let body: () -> Int raises Oops = fn () => { Fiber::wait(hand)!; 4 };
  let n = body()! catch { _ => 0 };
  print(\"${Fiber::join(outer) + n}\");
  0
}
",
            ),
        ],
    );

    assert!(ok, "{output}");
    assert!(output.contains("6"), "{output}");
}

/// **A `catch` handler is not inside its operand.** A `wait` in a handler arm
/// has no enclosing channel, and the code generator refuses it; the checker
/// has to agree.
#[test]
fn a_wait_in_a_catch_handler_is_refused() {
    let (ok, output) = command_on_package(
        "wait_in_handler",
        "check",
        &[
            ("khora.toml", MANIFEST),
            (
                "src/main.kh",
                "module main;
import std::core::{Fiber, print};

pub type Oops = | Oops;

fn boom() -> Int raises Oops { raise Oops::Oops }

pub fn main() -> Int {
  let hand = Fiber::spawn(fn () => 1);
  let n = boom()! catch { _ => { Fiber::wait(hand); 0 } };
  n
}
",
            ),
        ],
    );

    assert!(!ok, "{output}");
    assert!(output.contains("`Fiber::wait` is a place this function can be cancelled"), "{output}");
}

/// **A known gap, pinned so that closing it is noticed.** A generic function
/// whose only `raises` is a row variable passes `check`, and `build` refuses
/// the instantiation at an empty row. Refusing every such function would also
/// refuse the instantiations that build. When this assertion starts failing,
/// the checker has learned to see through the instantiation: update the
/// limitations page and flip the test.
#[cfg(feature = "llvm")]
#[test]
fn a_generic_wait_at_an_empty_row_is_refused_only_by_build() {
    let source = "module main;
import std::core::{Fiber, print};

fn waiter<'er>(f: Fiber<Int, 'er>) -> () raises 'er {
  Fiber::wait(f)!;
}

pub fn main() -> Int {
  let hand = Fiber::spawn(fn () => 1);
  waiter(hand);
  print(\"waited\");
  0
}
";
    let files = [("khora.toml", MANIFEST), ("src/main.kh", source)];
    let (checked, check_out) = command_on_package("wait_generic_gap_check", "check", &files);
    let (built, build_out) = command_on_package("wait_generic_gap_build", "build", &files);

    assert!(checked, "the gap is that check passes:\n{check_out}");
    assert!(!built, "and build refuses:\n{build_out}");
}
