//! A dependency cannot make a real capability; the package being built can.
//!
//! **What these guard, through the binary: the CLI telling the checker which
//! files are a dependency's.** The checker's own tests
//! (`khora-types/tests/forging.rs`) hand it that list directly; here it comes
//! from the package resolver, as it does for a user, so a driver that forgot
//! to declare it -- and fell back on reading packages off `module` lines --
//! is caught by the dependency that names itself after the package that
//! depends on it.

use std::path::Path;
use std::process::Command;

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a parent directory");
    }
    std::fs::write(path, text).expect("writing");
}

/// The manifest's `[toolchain]` pin, naming the binary under test.
fn pinned(manifest: &str) -> String {
    format!("{manifest}\n[toolchain]\nversion = \"{}\"\n", khora_toolchain::RUNNING)
}

/// Runs `khora <args>` in `dir`, answering whether it succeeded and what it said.
fn khora_in(dir: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(dir)
        .env("KHORA_HOME", home)
        .output()
        .expect("running khora");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// An app, `app`, depending on a package named `dep_name` at `../dep`, whose
/// `lib.kh` is `dep_source`.
fn project(at: &Path, dep_name: &str, dep_source: &str) -> std::path::PathBuf {
    let dep = at.join("dep");
    write(&dep.join("khora.toml"), &pinned(&format!("[package]\nname = \"{dep_name}\"\nversion = \"0.1.0\"\n")));
    write(&dep.join("src").join("lib.kh"), dep_source);
    let app = at.join("app");
    write(
        &app.join("khora.toml"),
        &pinned(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dep_name} = {{ path = \"../dep\" }}\n"
        )),
    );
    write(
        &app.join("src").join("main.kh"),
        &format!(
            "module app::main;\n\nimport std::core::{{print}};\nimport {dep_name}::lib::{{sneak}};\n\n\
             pub fn main() -> Int {{\n  print(\"${{sneak()}}\");\n  0\n}}\n"
        ),
    );
    app
}

/// A dependency that reads the clock with no row.
fn forging(dep_name: &str) -> String {
    format!(
        "module {dep_name}::lib;\n\nimport std::clock::{{Clock}};\n\n\
         pub fn sneak() -> Int {{\n  with {{ clock: Clock::real() }} {{ clock.unix_millis() }}\n}}\n"
    )
}

/// Requires `output` to be the refusal, at the dependency's line.
fn refused_in_dependency(output: &str) {
    assert!(
        output.contains("`Clock::real` makes a real `Clock`, and only the package being built may make one"),
        "expected the refusal:\n{output}"
    );
    assert!(output.contains("with { clock: Clock }"), "and the row to take instead:\n{output}");
    assert!(output.contains("lib.kh:6"), "at the dependency's line:\n{output}");
}

#[test]
fn a_dependency_making_a_clock_is_refused_at_check() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = project(tmp.path(), "dep", &forging("dep"));
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check"]);
    assert!(!ok, "a dependency forging a clock must not check:\n{output}");
    refused_in_dependency(&output);
}

/// **A dependency named like the package that depends on it is still a
/// dependency.** Its files say `module app::..`, so a rule that read the
/// package off the module path would take it for the root.
fn root_named_project(at: &Path) -> std::path::PathBuf {
    let dep = at.join("dep");
    write(&dep.join("khora.toml"), &pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n"));
    write(
        &dep.join("src").join("evil.kh"),
        "module app::evil;\n\nimport std::clock::{Clock};\n\n\
         pub fn sneak() -> Int {\n  with { clock: Clock::real() } { clock.unix_millis() }\n}\n",
    );
    let app = at.join("app");
    write(
        &app.join("khora.toml"),
        &pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\napp = { path = \"../dep\" }\n"),
    );
    write(
        &app.join("src").join("main.kh"),
        "module app::main;\n\nimport std::core::{print};\nimport app::evil::{sneak};\n\n\
         pub fn main() -> Int {\n  print(\"${sneak()}\");\n  0\n}\n",
    );
    app
}

#[test]
fn a_dependency_with_the_roots_name_is_refused_at_check() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = root_named_project(tmp.path());
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["check"]);
    assert!(!ok, "a dependency calling itself `app` is still a dependency:\n{output}");
    assert!(output.contains("makes a real `Clock`"), "{output}");
    assert!(output.contains("evil.kh"), "{output}");
}

/// **`build` declares the dependencies as `check` does.** It loads the
/// program by its own road, and a build that forgot the declaration would
/// fall back on the imports, which count a dependency called `app` as the
/// root -- and build it.
#[cfg(feature = "llvm")]
#[test]
fn a_dependency_with_the_roots_name_is_refused_at_build() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = root_named_project(tmp.path());
    let (ok, output) = khora_in(&app, &tmp.path().join("home"), &["build"]);
    assert!(!ok, "a dependency calling itself `app` is still a dependency:\n{output}");
    assert!(output.contains("makes a real `Clock`") && output.contains("evil.kh"), "{output}");
}

/// **A shell with no `Process` row, refused from the package being built
/// too.** `std::process::shell` is the half of `std::process` each target
/// writes; nothing outside `std` may name it.
#[test]
fn naming_std_process_shell_is_refused_in_the_root_and_in_a_dependency() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let dep = "module dep::lib;\n\nimport std::process::shell::{run};\n\n\
               pub fn sneak() -> Int { run(\"echo PWNED >&2\") }\n";
    let app = project(tmp.path(), "dep", dep);
    let home = tmp.path().join("home");
    let (ok, output) = khora_in(&app, &home, &["check"]);
    assert!(!ok, "{output}");
    assert!(output.contains("`std::process::shell` is part of `std`'s own implementation"), "{output}");
    assert!(output.contains("lib.kh"), "{output}");

    write(&tmp.path().join("dep").join("src").join("lib.kh"), "module dep::lib;\n\npub fn sneak() -> Int { 1 }\n");
    write(
        &app.join("src").join("main.kh"),
        "module app::main;\n\nimport std::core::{print};\nimport std::process::shell::{run};\n\n\
         pub fn main() -> Int {\n  let status = run(\"true\");\n  print(\"${status}\");\n  0\n}\n",
    );
    let (ok, output) = khora_in(&app, &home, &["check"]);
    assert!(!ok, "{output}");
    assert!(output.contains("`std::process::shell` is part of `std`'s own implementation"), "{output}");
    assert!(output.contains("main.kh"), "{output}");
}

/// **Every driver that makes a `SourceRoot` declares the dependencies with
/// it.** `check`, `build` and `--fix` each read the program their own way;
/// one that made the root and not the declaration would fall back on the
/// imports, and the only programs that tells apart are a dependency named
/// `std` or named after the root -- for `--fix`, nothing a test of its output
/// can see, because it never writes a dependency's file. So the source is
/// held to it: `Dependencies::root` makes both, and nothing else in the
/// driver makes a `SourceRoot`.
#[test]
fn every_source_root_the_driver_makes_comes_with_its_dependencies() {
    let main = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs"))
        .expect("the driver's source");
    let made: Vec<&str> = main.lines().filter(|l| l.contains("SourceRoot::new(")).collect();
    assert_eq!(made.len(), 1, "a `SourceRoot` made outside `Dependencies::root`: {made:#?}");
    assert!(made[0].contains("let root = SourceRoot::new(db, inputs.clone());"), "{made:#?}");
    let calls = main.matches("dependencies.root(&db,").count();
    assert_eq!(calls, 3, "`check`, `check --fix` and `load` each make their root through it");
}

/// **The package being built may make one, in its tests too.** And when the
/// dependency is the one being tested, it is the package being built.
#[test]
fn the_package_being_built_may_make_one_and_its_tests_may() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let source = "module dep::lib;\n\nimport std::clock::{Clock};\n\n\
                  pub fn sneak() -> Int with { clock: Clock } { clock.unix_millis() }\n\n\
                  pub fn own() -> Int {\n  with { clock: Clock::real() } { clock.unix_millis() }\n}\n";
    let app = project(tmp.path(), "dep", source);
    let home = tmp.path().join("home");
    // The dependency checked on its own: it is the package being built.
    let (ok, output) = khora_in(&tmp.path().join("dep"), &home, &["check"]);
    assert!(ok, "a package checked on its own may make a clock:\n{output}");
    // From the app it is a dependency, and `own` is refused.
    let (ok, output) = khora_in(&app, &home, &["check"]);
    assert!(!ok, "{output}");
    assert!(output.contains("makes a real `Clock`") && output.contains("lib.kh:8"), "{output}");

    // The app's own module making one, and its test.
    write(
        &tmp.path().join("dep").join("src").join("lib.kh"),
        "module dep::lib;\n\nimport std::clock::{Clock};\n\n\
         pub fn sneak() -> Int with { clock: Clock } { clock.unix_millis() }\n",
    );
    write(
        &app.join("src").join("main.kh"),
        "module app::main;\n\nimport std::core::{print};\nimport std::clock::{Clock};\nimport dep::lib::{sneak};\n\n\
         pub fn main() -> Int {\n  with { clock: Clock::real() } { print(\"${sneak() > 0}\") };\n  0\n}\n",
    );
    write(
        &app.join("src").join("clocks.kh"),
        "module app::clocks;\n\nimport std::core::{assert};\nimport std::clock::{Clock};\n\n\
         pub fn now() -> Int {\n  with { clock: Clock::real() } { clock.unix_millis() }\n}\n\n\
         test \"the clock moves\" {\n  let t = with { clock: Clock::real() } { clock.unix_millis() };\n  assert(t > 0);\n}\n",
    );
    let (ok, output) = khora_in(&app, &home, &["check"]);
    assert!(ok, "the package being built may make a clock in any module:\n{output}");
}

/// `khora build` refuses what `khora check` does, and builds once it is fixed.
#[cfg(feature = "llvm")]
#[test]
fn a_dependency_making_a_clock_is_refused_at_build() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let app = project(tmp.path(), "dep", &forging("dep"));
    let home = tmp.path().join("home");
    let (ok, output) = khora_in(&app, &home, &["build"]);
    assert!(!ok, "a dependency forging a clock must not build:\n{output}");
    refused_in_dependency(&output);
}

/// **A dependency's own tests may make one, when it is the package tested.**
/// `khora test` in the dependency builds and runs the test that makes a
/// clock; the same package checks clean as a dependency, because a
/// dependency's tests are never part of the build in hand.
#[cfg(feature = "llvm")]
#[test]
fn a_dependency_tested_on_its_own_may_make_one_in_its_tests() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let source = "module dep::lib;\n\nimport std::core::{assert};\nimport std::clock::{Clock};\n\n\
                  pub fn sneak() -> Int with { clock: Clock } { clock.unix_millis() }\n\n\
                  test \"the clock moves\" {\n  let t = with { clock: Clock::real() } { sneak() };\n  assert(t > 0);\n}\n";
    let app = project(tmp.path(), "dep", source);
    write(
        &app.join("src").join("main.kh"),
        "module app::main;\n\nimport std::core::{print};\nimport std::clock::{Clock};\nimport dep::lib::{sneak};\n\n\
         pub fn main() -> Int {\n  with { clock: Clock::real() } { print(\"${sneak() > 0}\") };\n  0\n}\n",
    );
    let home = tmp.path().join("home");
    let (ok, output) = khora_in(&tmp.path().join("dep"), &home, &["test"]);
    assert!(ok && output.contains("1 passed"), "the dependency's own test runs:\n{output}");
    let (ok, output) = khora_in(&app, &home, &["check"]);
    assert!(ok, "the dependency's test is not the app's code:\n{output}");
}
