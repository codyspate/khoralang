//! One runtime archive that nothing else rebuilds.
//!
//! **Why a test has to think about this at all.**
//! `khora-codegen-llvm`'s harness runs `cargo build -p khora-rt` from inside a
//! test, so that an edit to the runtime cannot leave a stale archive behind.
//! That build resolves a different feature set from the one
//! `cargo nextest --features llvm` resolved, so cargo relinks the staticlib
//! and `target/debug`'s archive flips between two files **while other tests
//! are running**.
//!
//! The archive is an input to every compiled program, and 14.17's build cache
//! keys on it — correctly, because a program linked against a different
//! runtime is a different program. So any test that expects two builds to
//! agree has to be looking at one archive. Errata 51; the underlying problem
//! is roadmap 14.33.
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
