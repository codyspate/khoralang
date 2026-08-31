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
//! **And it has to be pinned per process, at a name that is never rewritten.**
//! The first two attempts were not. Copying once per test *run* into one fixed
//! name, and re-copying whenever the real archive looked newer, meant the
//! answer was recomputed before *every* `khora` invocation — so a test that
//! runs the compiler twice could be handed one archive and then a different
//! one, which is precisely the thing this module exists to prevent. That is
//! #115: `the_second_run_comes_from_the_cache` failed about one baseline in
//! two, and the cache was right every time. Both runs were told the truth
//! about a different runtime:
//!
//! ```text
//! khora: key from compiler 7a2f08c81301 linker 8bbe086dfb0f runtime 655dd165f859
//! khora: key from compiler 7a2f08c81301 linker 8bbe086dfb0f runtime 334a4f2499dd
//! ```
//!
//! So the copy is filed under what the archive *was* when this process first
//! looked — its size and modification time — and a file at that name is
//! written once and never replaced. A rebuild produces a new name and leaves
//! every process already using the old one alone. The [`OnceLock`] is the
//! other half: within one process the question is asked once, so the answer
//! cannot change between two calls even if the directory gains a newer entry.
//!
//! Copies are tens of megabytes and one accumulates per distinct build, under
//! `CARGO_TARGET_TMPDIR`, which `cargo clean` removes.

use std::path::PathBuf;
use std::sync::OnceLock;

/// A private copy of the runtime archive, or `None` if there is none to copy.
///
/// Resolved once per process; see the module comment for why that matters more
/// than it looks.
pub fn runtime() -> Option<PathBuf> {
    static PINNED: OnceLock<Option<PathBuf>> = OnceLock::new();
    PINNED.get_or_init(pin).clone()
}

/// Makes the copy, or finds the one a previous process made.
fn pin() -> Option<PathBuf> {
    let real = beside_the_compiler()?;
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pinned-rt").join(stamp(&real)?);
    std::fs::create_dir_all(&directory).ok()?;
    let pinned = directory.join(real.file_name()?);
    // Never overwritten: the name says which build this is, so a file already
    // there is the same bytes and may be in use by somebody else.
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

/// A name for one build of the archive: when it was written, and how big.
///
/// Not a digest of the contents, which would be a hundred megabytes of hashing
/// per test process to answer a question the file's own metadata already
/// answers. Two different builds sharing a size *and* a modification time to
/// the nanosecond is not a case worth the cost.
fn stamp(real: &std::path::Path) -> Option<String> {
    let meta = std::fs::metadata(real).ok()?;
    let written = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{:x}-{:x}", written.as_nanos(), meta.len()))
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
