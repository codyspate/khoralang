#![cfg(feature = "llvm")]

//! **Phase 10.2's exit criterion: a package from outside this repository.**
//!
//! Builds a real git repository holding a real Khora package, resolves it
//! through `khora.lock` into the content-addressed store, compiles a program
//! that imports it, and runs the program.
//!
//! # Why the package is built here rather than committed
//!
//! An example whose manifest names a `file:///C:/Users/...` URL builds on one
//! machine, and an example that only builds on one machine is worse than none.
//! Everything the test needs is therefore made in a temporary directory: the
//! repository, the commit, the manifest, the app. Nothing reaches the network,
//! and the only step that is skipped is the transport — `git ls-remote`, the
//! shallow fetch, the checkout, the hash, the store and the lockfile all run
//! exactly as they would against a remote.
//!
//! The human-facing counterpart lives outside the repository at
//! `~/dev/khora-uuid`: a real RFC 4122 package with eleven of its own tests. It
//! is what the exit criterion is *about*; this is what keeps it working.
//!
//! # What the package is chosen to prove
//!
//! Not that a file can be copied. The package declares a type, an `impl` of a
//! *standard library* trait for it, and a generic function — so a passing test
//! says that a dependency's types are visible, that its trait impls are found
//! by the consumer's checker, and that monomorphization crosses a package
//! boundary.

mod harness;

use std::path::{Path, PathBuf};
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// A package with a type, a trait impl and a generic — the three things that
/// have to cross a package boundary for a dependency to be worth having.
const PACKAGE: &str = r#"module tally;

import std::core::{Show};

/// A count of something, so that the consumer has a type it did not declare.
export type Tally = {
  count: Int,
};

impl Tally {
  export fn of(count: Int) -> Tally {
    { count: count }
  }

  export fn plus(self, other: Tally) -> Tally {
    { count: self.count + other.count }
  }
}

/// An impl of a *standard library* trait, for a type this package declares.
/// The consumer must find it without importing anything but `Tally`.
impl Show for Tally {
  fn show(self) -> String {
    "tally(" + Int::to_string(self.count) + ")"
  }
}

/// A generic, so that monomorphization has to reach across the boundary: the
/// body lives here and the specialization is demanded there.
export fn twice<A>(value: A, join: (A, A) -> A) -> A {
  join(value, value)
}
"#;

const APP: &str = r#"module app::main;

import std::core::{Show};
import tally::{Tally, twice};

fn print(value: String);

export fn main() -> () {
  let one = Tally::of(21);
  print(twice(one, fn (a, b) => Tally::plus(a, b)).show());
}
"#;

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

/// Every `.kh` file of `std`, plus whatever directories the resolution found.
fn sources(db: &KhoraDatabase, extra: &[PathBuf]) -> Vec<SourceFile> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut out = Vec::new();
    let mut stack = vec![repo.join("std")];
    stack.extend(extra.iter().cloned());
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out
}

#[test]
fn a_package_from_a_git_repository_is_resolved_compiled_and_run() {
    harness::ensure_runtime();
    let tmp = tempfile::tempdir().expect("a temporary directory");

    // 1. A package, in a repository of its own, outside anything being built.
    let package = tmp.path().join("tally");
    write(&package.join("khora.toml"), "[package]\nname = \"tally\"\nversion = \"0.1.0\"\npublish = true\n");
    write(&package.join("src").join("tally.kh"), PACKAGE);
    git(&["init", "--quiet", "-b", "main"], &package);
    git(&["add", "-A"], &package);
    git(&["commit", "--quiet", "-m", "tally 0.1.0"], &package);
    let url = format!("file:///{}", package.display().to_string().replace('\\', "/"));

    // 2. An application that depends on it by revision, not by path.
    let app = tmp.path().join("app");
    write(
        &app.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ntally = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );
    write(&app.join("src").join("main.kh"), APP);

    // 3. Resolve. This is the part under test.
    let store = khora_pkg::Store::at(tmp.path().join("store")).expect("a store");
    let resolution =
        khora_pkg::resolve(&app.join("khora.toml"), &store, false).expect("resolution");

    let locked = resolution.lockfile.get("tally").expect("a locked entry");
    assert_eq!(locked.source, "git");
    assert_eq!(locked.revision.as_ref().map(String::len), Some(40), "a full commit id");
    assert!(locked.checksum.is_some(), "pinned by content as well as by revision");
    assert!(
        resolution.packages[0].directory.starts_with(store.root()),
        "the dependency should be compiled out of the store, not out of its working copy"
    );

    // 4. Compile the application together with what resolution found.
    let mut directories = resolution.directories();
    directories.push(app.clone());

    let db = KhoraDatabase::new();
    let files = sources(&db, &directories);
    let root = SourceRoot::new(&db, files);
    // Not `app`: that is the source directory's name, and on anything but
    // Windows the executable would have no extension to tell them apart. `ld`
    // says `cannot open output file ...: Is a directory`, which is a clear
    // message about a confusing mistake.
    let exe = tmp.path().join(if cfg!(windows) { "program.exe" } else { "program" });
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling against a git dependency failed:\n  {}", messages.join("\n  "));
    }

    // 5. Run it. `twice` is the dependency's generic, `plus` its method and
    //    `show` its impl of a std trait, so the answer only comes out right if
    //    all three crossed the boundary.
    let out = Command::new(&exe).output().expect("running the program");
    assert!(out.status.success(), "the program failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "tally(42)");
}

/// The lockfile is the point, so a second build must use it rather than ask
/// the repository again.
///
/// Checked by making the repository unreachable between the two resolutions:
/// if anything still needed it, this fails.
#[test]
fn a_locked_build_does_not_need_the_repository_again() {
    let tmp = tempfile::tempdir().expect("a temporary directory");

    let package = tmp.path().join("tally");
    write(&package.join("khora.toml"), "[package]\nname = \"tally\"\nversion = \"0.1.0\"\npublish = true\n");
    write(&package.join("src").join("tally.kh"), "module tally;\n");
    git(&["init", "--quiet", "-b", "main"], &package);
    git(&["add", "-A"], &package);
    git(&["commit", "--quiet", "-m", "first"], &package);
    let url = format!("file:///{}", package.display().to_string().replace('\\', "/"));

    let app = tmp.path().join("app");
    write(
        &app.join("khora.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ntally = {{ git = \"{url}\", rev = \"main\" }}\n"
        ),
    );

    let store = khora_pkg::Store::at(tmp.path().join("store")).expect("a store");
    khora_pkg::resolve(&app.join("khora.toml"), &store, false).expect("the first resolution");

    // Gone. A resolution that still reaches for it now fails.
    std::fs::remove_dir_all(&package).expect("removing the repository");

    let again = khora_pkg::resolve(&app.join("khora.toml"), &store, true)
        .expect("the second resolution should be served from the lockfile and the store");
    assert!(!again.changed, "a locked resolution must not rewrite the lockfile");
    assert!(again.packages[0].directory.is_dir(), "the store still holds the package");
}
