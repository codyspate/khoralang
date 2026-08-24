//! Finding a pin, and finding what is installed.
//!
//! The deciding is unit-tested in the crate itself, because it is pure. These
//! are the parts that touch a filesystem: walking up to a manifest, and reading
//! `~/.khora/toolchains`.
//!
//! Every test sets `KHORA_HOME`, so nothing here can see or disturb a real
//! installation.

use std::path::Path;

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a parent directory");
    }
    std::fs::write(path, text).expect("writing");
}

fn manifest(version: Option<&str>) -> String {
    let mut text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n".to_string();
    if let Some(v) = version {
        text.push_str(&format!("\n[toolchain]\nversion = \"{v}\"\n"));
    }
    text
}

/// `KHORA_HOME` is process-wide, so these run one at a time.
///
/// Each `tests/*.rs` is its own binary and Rust runs the tests in one binary on
/// several threads, so a test that sets an environment variable has to be the
/// only one doing it. A mutex is cheaper than splitting this into six files.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_home<T>(home: &Path, body: impl FnOnce() -> T) -> T {
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("KHORA_HOME", home);
    let out = body();
    std::env::remove_var("KHORA_HOME");
    out
}

/// Registers `version` as a toolchain, with a file standing in for a compiler.
fn install(home: &Path, version: &str) {
    let fake = home.join("source").join(format!("khora{}", std::env::consts::EXE_SUFFIX));
    write(&fake, "not really a compiler");
    with_home(home, || {
        khora_toolchain::link(version, &fake).expect("linking");
    });
}

// --- the pin ---------------------------------------------------------------

#[test]
fn a_manifest_with_no_toolchain_table_pins_nothing() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    write(&tmp.path().join("khora.toml"), &manifest(None));
    assert_eq!(khora_toolchain::pinned_version(tmp.path()), None);
}

#[test]
fn a_pin_is_read_from_the_manifest() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    write(&tmp.path().join("khora.toml"), &manifest(Some("0.4.2")));
    assert_eq!(khora_toolchain::pinned_version(tmp.path()).as_deref(), Some("0.4.2"));
}

/// Found from a file deep in the project, not only from the root — which is
/// where `khora build src/main.kh` is run from.
#[test]
fn a_pin_is_found_from_a_nested_file() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    write(&tmp.path().join("khora.toml"), &manifest(Some("0.4.2")));
    let deep = tmp.path().join("src").join("net").join("main.kh");
    write(&deep, "module app::main;\n");
    assert_eq!(khora_toolchain::pinned_version(&deep).as_deref(), Some("0.4.2"));
}

#[test]
fn no_manifest_anywhere_pins_nothing() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let loose = tmp.path().join("scratch.kh");
    write(&loose, "module scratch;\n");
    assert_eq!(khora_toolchain::pinned_version(&loose), None);
}

/// A manifest that does not parse contributes no pin rather than stopping the
/// compiler. `khora check` on the manifest is what reports it, and refusing to
/// start would mean that error could never be shown.
#[test]
fn an_unparseable_manifest_pins_nothing_rather_than_failing() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    write(&tmp.path().join("khora.toml"), "[package\nname =");
    assert_eq!(khora_toolchain::pinned_version(tmp.path()), None);
}

// --- what is installed -----------------------------------------------------

#[test]
fn nothing_is_installed_in_an_empty_home() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let found = with_home(tmp.path(), || khora_toolchain::installed().expect("listing"));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_linked_toolchain_is_listed() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    install(tmp.path(), "0.2.0");

    let found = with_home(tmp.path(), || khora_toolchain::installed().expect("listing"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].version, "0.2.0");
    assert!(found[0].binary.is_file(), "the executable should be there: {found:?}");
}

/// Copied, not symlinked: a link into somebody's `target/debug` breaks on the
/// next `cargo clean`, and breaks by pointing at nothing rather than by saying
/// so.
#[test]
fn linking_copies_rather_than_pointing() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    install(tmp.path(), "0.2.0");
    std::fs::remove_dir_all(tmp.path().join("source")).expect("removing the original");

    let found = with_home(tmp.path(), || khora_toolchain::installed().expect("listing"));
    assert!(found[0].binary.is_file(), "the copy should survive its source: {found:?}");
}

#[test]
fn several_toolchains_are_listed_in_order() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    for v in ["0.3.0", "0.1.0", "0.2.0"] {
        install(tmp.path(), v);
    }
    let found = with_home(tmp.path(), || khora_toolchain::installed().expect("listing"));
    let versions: Vec<&str> = found.iter().map(|t| t.version.as_str()).collect();
    assert_eq!(versions, ["0.1.0", "0.2.0", "0.3.0"]);
}

#[test]
fn unlinking_forgets_a_toolchain() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    install(tmp.path(), "0.2.0");
    with_home(tmp.path(), || {
        khora_toolchain::unlink("0.2.0").expect("unlinking");
        assert!(khora_toolchain::installed().expect("listing").is_empty());
        assert!(khora_toolchain::unlink("0.2.0").is_err(), "twice is an error");
    });
}

/// A directory under `toolchains` with no executable in it is what an
/// interrupted `link` leaves behind. It is skipped rather than reported.
#[test]
fn a_half_registered_toolchain_is_ignored() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir_all(tmp.path().join("toolchains").join("0.9.0").join("bin"))
        .expect("a directory");
    let found = with_home(tmp.path(), || khora_toolchain::installed().expect("listing"));
    assert!(found.is_empty(), "{found:?}");
}

/// The directory a toolchain is filed under is a version, so a typo is caught
/// when it is made rather than when somebody pins it.
#[test]
fn linking_something_that_is_not_a_version_is_refused() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let fake = tmp.path().join("khora");
    write(&fake, "x");
    with_home(tmp.path(), || {
        assert!(khora_toolchain::link("latest", &fake).is_err());
        assert!(khora_toolchain::link("v1.0.0", &fake).is_err());
        assert!(khora_toolchain::link("1.0.0", &fake).is_ok());
    });
}
