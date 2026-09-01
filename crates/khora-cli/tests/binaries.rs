#![cfg(feature = "llvm")]

//! `src/bin`: a package's other programs.
//!
//! One file per program, built with the package's modules and not with each
//! other. What is tested here is the separation, because that is the whole
//! feature: `src/main.kh` and `src/bin/*.kh` each become their own executable
//! out of one shared library of modules, and a `main` in one is not a `main`
//! in another.
//!
//! # Why this file exists at all
//!
//! For a while `src/bin` was recommended by a lint, exempted by that lint, and
//! refused by the backend — which compiled every `main` it found into one
//! program. `khora check` passed on the layout the message suggested and
//! `khora build` then failed with the error the message was trying to help
//! with. The exemption was withdrawn and the layout built properly; these are
//! what stop the three from drifting apart again.

use std::path::{Path, PathBuf};
use std::process::Command;

mod pinned;

struct World {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

/// A package with a shared module, a `main`, and the named programs in
/// `src/bin`.
fn world(programs: &[&str]) -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src").join("bin")).expect("a src/bin directory");
    std::fs::write(
        project.join("khora.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .expect("a manifest");
    std::fs::write(
        project.join("src").join("shared.kh"),
        "module app::shared;\n\npub fn who(name: String) -> String { \"I am \" + name }\n",
    )
    .expect("the shared module");
    std::fs::write(project.join("src").join("main.kh"), program("main", "the package"))
        .expect("the package's own program");
    for name in programs {
        std::fs::write(project.join("src").join("bin").join(format!("{name}.kh")), program(name, name))
            .expect("a program");
    }
    World { _tmp: tmp, home, project }
}

/// A program that prints through the package's shared module.
///
/// **Through `app::shared` on purpose.** A program that imported nothing would
/// pass whether or not the package's modules reached it, and "each of these is
/// built with the package" is half of what is under test.
fn program(module: &str, says: &str) -> String {
    format!(
        "module app::{module};\n\n\
         import std::core::{{print}};\n\
         import app::shared::{{who}};\n\n\
         pub fn main() -> () {{ print(who(\"{says}\")) }}\n"
    )
}

fn khora(w: &World, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if let Some(archive) = pinned::runtime() {
        command.env("KHORA_RT_LIB", archive);
    }
    let out = command
        .args(args)
        .current_dir(&w.project)
        .env("KHORA_HOME", &w.home)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn built(w: &World, name: &str) -> PathBuf {
    w.project.join("build").join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() })
}

fn output_of(exe: &Path) -> String {
    let out = Command::new(exe).output().expect("the program should run");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// **One `khora build`, every program in the package.**
///
/// Cargo's rule, and the one that makes the directory worth having: a
/// maintenance program only built when somebody remembers to name it is a
/// maintenance program that has not compiled in six months.
#[test]
fn building_a_package_builds_every_program_in_it() {
    let w = world(&["backfill", "report"]);
    let (ok, out) = khora(&w, &["build", "."]);
    assert!(ok, "{out}");

    for (name, says) in [("app", "the package"), ("backfill", "backfill"), ("report", "report")] {
        let exe = built(&w, name);
        assert!(exe.is_file(), "`{name}` should have been built:\n{out}");
        assert_eq!(
            output_of(&exe),
            format!("I am {says}\n"),
            "each program is its own, and reaches the package's modules"
        );
    }
}

/// **A program in `src/bin` does not take the package's `main` with it.**
///
/// The gather that assembles a compilation pulls in the whole package, because
/// a program is more than its entry file — and `src/main.kh` is a *different*
/// program. Leaving it in put two `main`s in one compilation, and the backend
/// duly refused, naming the file the user had just asked for.
#[test]
fn a_program_in_src_bin_is_built_alone() {
    let w = world(&["backfill"]);
    let (ok, out) = khora(&w, &["build", "src/bin/backfill.kh"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("more than one module"),
        "the package's own `main` should not be in this compilation:\n{out}"
    );
    assert_eq!(output_of(&built(&w, "backfill")), "I am backfill\n");
}

/// **`khora check` sees them, or it is a check/build split.**
///
/// The walk that assembles a *build* skips `src/bin` so that one program comes
/// out. A check inheriting that skip would pass on a program that does not
/// parse, and `khora build` would then fail on it — which is the shape this
/// repository has fixed twice and reopened once, by the commit that made
/// `src/bin` real.
#[test]
fn check_sees_a_program_in_src_bin() {
    let w = world(&["backfill"]);
    std::fs::write(
        w.project.join("src").join("bin").join("backfill.kh"),
        "module app::backfill;\n\nthis is not khora at all\n",
    )
    .expect("a broken program");

    let (ok, out) = khora(&w, &["check", "."]);
    assert!(!ok, "a program that does not parse must fail `check`:\n{out}");
    assert!(out.contains("backfill.kh"), "and be named:\n{out}");
}

/// A package whose programs are all in `src/bin` has no default one, and says
/// which it has rather than reporting that it has none.
#[test]
fn running_a_package_with_no_main_names_its_programs() {
    let w = world(&["backfill", "report"]);
    std::fs::remove_file(w.project.join("src").join("main.kh")).expect("no default program");

    let (ok, out) = khora(&w, &["run", "."]);
    assert!(!ok, "there is no one program to run:\n{out}");
    assert!(out.contains("backfill"), "it should name them:\n{out}");
    assert!(out.contains("report"), "both of them:\n{out}");
    assert!(
        !out.contains("no `main` function"),
        "and not claim the package has no programs:\n{out}"
    );
}

/// Two programs both named `main` in one *file* is still the error it was.
///
/// `src/bin` separates programs by file; it does not make a second `main`
/// inside one compilation acceptable.
#[test]
fn two_mains_in_one_program_are_still_refused() {
    let w = world(&[]);
    std::fs::write(
        w.project.join("src").join("second.kh"),
        program("second", "the second"),
    )
    .expect("a second main outside src/bin");

    let (ok, out) = khora(&w, &["build", "."]);
    assert!(!ok, "two `main`s in one program:\n{out}");
    assert!(out.contains("more than one module"), "{out}");
    assert!(out.contains("src/bin"), "and it should say where the other one goes:\n{out}");
}
