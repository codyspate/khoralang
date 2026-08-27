//! One runtime archive that nothing else rebuilds.
//!
//! **Why a test has to think about this at all.**
//! The archive is an input to every compiled program, and 14.17's build cache
//! keys on it — correctly, because a program linked against a different
//! runtime is a different program. So any test that expects two builds to
//! agree has to be looking at one archive.
//!
//! The specific bug that made this necessary is fixed: `khora-codegen-llvm`'s
//! harness used to run `cargo build -p khora-rt` unconditionally from inside a
//! test, which resolved a different feature set from the enclosing run and
//! replaced the archive while fifty other test binaries were linking against
//! it. It now builds only when the archive is older than the runtime's
//! sources. Errata 51, roadmap 14.33.
//!
//! **The pin stays anyway**, and not out of superstition. Anything that
//! rebuilds `khora-rt` while these tests run moves an input underneath them —
//! a future harness, a `cargo build` in another terminal, an editor's
//! save-and-build. A test whose whole claim is "two builds agree" should not
//! depend on nobody doing that, and one copy of a file is a cheap way not
//! to.
//!
//! Copied once per test *run* rather than once per test: the archive is tens
//! of megabytes.

use std::path::PathBuf;

/// A private copy of the runtime archive, or `None` if there is none to copy.
pub fn runtime() -> Option<PathBuf> {
    let real = beside_the_compiler()?;
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pinned-rt");
    std::fs::create_dir_all(&directory).ok()?;
    let pinned = directory.join(real.file_name()?);
    if pinned.is_file() {
        return Some(pinned);
    }
    // Written under a temporary name and renamed, so two test processes
    // racing both end up pointing at a complete file.
    let staged = directory.join(format!("tmp-{}", std::process::id()));
    std::fs::copy(&real, &staged).ok()?;
    if std::fs::rename(&staged, &pinned).is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    pinned.is_file().then_some(pinned)
}

/// The archive `khora build` would have found on its own.
///
/// `CARGO_BIN_EXE_khora` is `target/<profile>/khora`, and the archive sits
/// beside it — the first place `toolchain::runtime_archive` looks.
fn beside_the_compiler() -> Option<PathBuf> {
    let name = if cfg!(windows) { "khora_rt.lib" } else { "libkhora_rt.a" };
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_khora"));
    let beside = exe.parent()?.join(name);
    beside.is_file().then_some(beside)
}
