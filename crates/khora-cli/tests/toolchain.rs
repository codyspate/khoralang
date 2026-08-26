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
    assert!(output.contains("nothing installed here"), "{output}");
    // Both ways of getting one, because somebody with neither a release nor a
    // build of their own needs to be told which of the two they want.
    assert!(output.contains("khora toolchain install"), "{output}");
    assert!(output.contains("khora toolchain link"), "{output}");
}

#[test]
fn which_reports_no_pin_outside_a_project() {
    let w = world(None);
    let (ok, output) = khora(&w, &w.project, &["toolchain", "which"]);
    assert!(ok, "{output}");
    assert!(output.contains("no pin here and no default"), "{output}");
}

#[test]
fn remove_forgets_and_says_so() {
    let w = world(None);
    link(&w, "0.99.0");

    // `unlink` is what this was called before `install` existed, and is kept
    // as an alias so a script written against it still runs.
    let (ok, output) = khora(&w, &w.project, &["toolchain", "unlink", "0.99.0"]);
    assert!(ok, "{output}");
    assert!(output.contains("removed Khora 0.99.0"), "{output}");

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

// --- the default ------------------------------------------------------------

/// **A default is obeyed where there is no pin**, which is what makes
/// `khora update` mean anything: the bootstrap toolchain stays on the path, and
/// this is how a newer one installed beside it gets used.
#[test]
fn a_default_is_used_when_the_project_pins_nothing() {
    let w = world(None);
    link(&w, "0.99.0");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "default", "0.99.0"]);
    assert!(ok, "{output}");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "which"]);
    assert!(ok, "{output}");
    assert!(output.contains("0.99.0"), "{output}");
    assert!(output.contains("it is your default"), "{output}");
    assert!(output.contains("would hand over"), "{output}");
}

/// **A pin beats a default**, because a default is a preference somebody
/// expressed once and a pin is what the project requires every time.
#[test]
fn a_pin_wins_over_a_default() {
    let w = world(Some("9.9.9"));
    link(&w, "9.9.9");
    link(&w, "0.99.0");
    let (ok, output) = khora(&w, &w.project, &["toolchain", "default", "0.99.0"]);
    assert!(ok, "{output}");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "which"]);
    assert!(ok, "{output}");
    assert!(output.contains("this project pins it"), "{output}");
    assert!(output.contains("9.9.9"), "{output}");
    assert!(!output.contains("0.99.0"), "the default should not appear: {output}");
}

/// And the refusal names the *pin*, not the default that lost to it.
///
/// A message naming the wrong version sends somebody to install a toolchain
/// they already have.
#[test]
fn a_missing_pin_is_reported_rather_than_the_default() {
    let w = world(Some("9.9.9"));
    link(&w, "0.99.0");
    let (ok, _) = khora(&w, &w.project, &["toolchain", "default", "0.99.0"]);
    assert!(ok);

    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(!ok, "a missing pin must stop the build: {output}");
    assert!(output.contains("pins Khora 9.9.9"), "{output}");
}

/// **A default naming something that is gone warns and carries on.**
///
/// The opposite of what a missing *pin* does, and deliberately: a default may
/// name a toolchain removed months ago, and refusing every command in every
/// unpinned directory would be a machine somebody has to repair before they can
/// use it — including with the command that repairs it.
#[test]
fn a_default_that_is_not_installed_does_not_stop_anything() {
    let w = world(None);
    std::fs::create_dir_all(&w.home).expect("the home directory");
    std::fs::write(w.home.join("default"), "9.9.9\n").expect("the default");

    let (ok, output) = khora(&w, &w.project, &["check", "src/main.kh"]);
    assert!(ok, "a missing default must not stop a build: {output}");
    assert!(output.contains("your default is Khora 9.9.9"), "{output}");
    assert!(output.contains("toolchain default --none"), "{output}");
}

/// Removing the default toolchain clears the default with it.
///
/// Otherwise the machine is left naming a version that cannot be run, which is
/// a state nothing else here can produce and nobody would expect.
#[test]
fn removing_the_default_toolchain_clears_the_default() {
    let w = world(None);
    link(&w, "0.99.0");
    let (ok, _) = khora(&w, &w.project, &["toolchain", "default", "0.99.0"]);
    assert!(ok);

    let (ok, output) = khora(&w, &w.project, &["toolchain", "remove", "0.99.0"]);
    assert!(ok, "{output}");
    assert!(output.contains("which was the default"), "{output}");

    let (ok, output) = khora(&w, &w.project, &["toolchain", "default"]);
    assert!(ok, "{output}");
    assert!(output.contains("no default"), "{output}");
}

/// A default naming a version nobody has is refused at the point of setting it,
/// where the person is still there to read why.
#[test]
fn a_default_must_name_something_that_exists() {
    let w = world(None);
    let (ok, output) = khora(&w, &w.project, &["toolchain", "default", "9.9.9"]);
    assert!(!ok, "{output}");
    assert!(output.contains("khora toolchain install 9.9.9"), "{output}");
}

/// **`khora toolchain` and `khora update` never hand over.**
///
/// They are how a broken default or a missing pin is repaired, so a handover
/// that refused to run them would make the situation they exist for
/// unrecoverable.
#[test]
fn the_commands_that_repair_a_toolchain_are_never_handed_over() {
    let w = world(Some("9.9.9"));
    for command in [["toolchain", "list"], ["toolchain", "default"]] {
        let (ok, output) = khora(&w, &w.project, &command);
        assert!(ok, "{command:?} should run despite the missing pin: {output}");
    }
}

/// The bootstrap toolchain is listed even though it is not under `toolchains/`.
///
/// `install.sh` unpacks it into `~/.khora` directly, and everything installed
/// later sits beside it — so leaving it out of the list makes "go back to the
/// one I had" look impossible after the first update.
#[test]
fn the_toolchain_on_the_path_is_listed_too() {
    let w = world(None);
    link(&w, "0.99.0");
    let (ok, output) = khora(&w, &w.project, &["toolchain", "list"]);
    assert!(ok, "{output}");
    assert!(output.contains("on your path"), "{output}");
    assert!(output.contains("0.99.0"), "{output}");
}
