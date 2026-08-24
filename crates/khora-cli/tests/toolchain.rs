//! The shim, from outside.
//!
//! `khora-toolchain` unit-tests the deciding; this drives the real executable,
//! because the part worth testing is what happens *before* argument parsing.
//! Every test gets its own `KHORA_HOME`, passed to the child rather than set in
//! this process, so they can run in parallel and none can see a real
//! installation.

use std::path::Path;
use std::process::Command;

struct World {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
}

/// A project, optionally pinned, and a private toolchain directory.
fn world(pin: Option<&str>) -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");

    let mut manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n".to_string();
    if let Some(version) = pin {
        manifest.push_str(&format!("\n[toolchain]\nversion = \"{version}\"\n"));
    }
    std::fs::write(project.join("khora.toml"), manifest).expect("writing the manifest");
    std::fs::write(project.join("src").join("main.kh"), "module app::main;\n")
        .expect("writing the source");

    World { _tmp: tmp, home, project }
}

/// Runs `khora` in `cwd` with its own toolchain home.
fn khora(w: &World, cwd: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(cwd)
        .env("KHORA_HOME", &w.home)
        .output()
        .expect("running khora");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Registers this very executable under `version`.
fn link(w: &World, version: &str) {
    let (ok, output) =
        khora(w, w.project.as_path(), &["toolchain", "link", version, env!("CARGO_BIN_EXE_khora")]);
    assert!(ok, "linking {version}: {output}");
}

/// The version this test binary's `khora` reports itself as.
const RUNNING: &str = env!("CARGO_PKG_VERSION");

#[test]
fn with_no_pin_the_running_toolchain_is_used() {
    let w = world(None);
    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(ok, "{output}");
    assert!(output.contains("no errors"), "{output}");
}

#[test]
fn a_pin_naming_the_running_version_just_runs() {
    let w = world(Some(RUNNING));
    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(ok, "{output}");
    assert!(output.contains("no errors"), "{output}");
}

/// The whole feature: a pin naming something that is not installed stops,
/// rather than quietly compiling with whatever is on the path.
#[test]
fn a_missing_pinned_version_refuses_to_build() {
    let w = world(Some("9.9.9"));
    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(!ok, "it should refuse: {output}");
    assert!(output.contains("pins Khora 9.9.9"), "{output}");
    assert!(output.contains("will not fall back"), "the reason should be there: {output}");
    assert!(output.contains("khora toolchain link 9.9.9"), "and the fix: {output}");
}

/// A build handed to another toolchain still has to work — and has to
/// terminate, which is the failure a loop guard exists to prevent.
#[test]
fn a_build_hands_over_to_the_pinned_toolchain() {
    let w = world(Some("0.99.0"));
    link(&w, "0.99.0");

    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(ok, "the handover should build: {output}");
    assert!(output.contains("no errors"), "{output}");
}

/// The situation the feature exists for is also the one it could make
/// unrecoverable: standing in a project whose pinned version is missing, unable
/// to run the command that installs it.
#[test]
fn toolchain_commands_work_inside_a_project_with_a_missing_pin() {
    let w = world(Some("9.9.9"));

    let (ok, output) = khora(&w, &w.project, &["toolchain", "which"]);
    assert!(ok, "`which` must answer rather than be handed over: {output}");
    assert!(output.contains("9.9.9"), "{output}");

    let (ok, output) =
        khora(&w, &w.project, &["toolchain", "link", "9.9.9", env!("CARGO_BIN_EXE_khora")]);
    assert!(ok, "`link` must work from in here, or the pin is a trap: {output}");

    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(ok, "and then the build works: {output}");
}

#[test]
fn list_says_which_one_is_running() {
    let w = world(None);
    link(&w, RUNNING);
    link(&w, "0.99.0");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "list"]);
    assert!(ok, "{output}");
    assert!(output.contains(&format!("{RUNNING}  (running)")), "{output}");
    assert!(output.contains("0.99.0"), "{output}");
}

#[test]
fn list_with_nothing_registered_says_what_to_do() {
    let w = world(None);
    let (ok, output) = khora(&w, &w.project, &["toolchain", "list"]);
    assert!(ok, "{output}");
    assert!(output.contains("no toolchains registered"), "{output}");
    assert!(output.contains("khora toolchain link"), "{output}");
}

#[test]
fn which_reports_no_pin_outside_a_project() {
    let w = world(None);
    let (ok, output) = khora(&w, &w.project, &["toolchain", "which"]);
    assert!(ok, "{output}");
    assert!(output.contains("no pin here"), "{output}");
}

#[test]
fn unlink_forgets_and_says_so() {
    let w = world(None);
    link(&w, "0.99.0");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "unlink", "0.99.0"]);
    assert!(ok, "{output}");
    assert!(output.contains("forgot Khora 0.99.0"), "{output}");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "list"]);
    assert!(ok, "{output}");
    assert!(!output.contains("0.99.0"), "{output}");
}

/// The pin is found from wherever the command is run, not only from the root.
#[test]
fn a_pin_applies_from_a_subdirectory() {
    let w = world(Some("9.9.9"));
    let (ok, output) = khora(&w, &w.project.join("src"), &["check", "main.kh"]);
    assert!(!ok, "the pin should still apply one level down: {output}");
    assert!(output.contains("pins Khora 9.9.9"), "{output}");
}
