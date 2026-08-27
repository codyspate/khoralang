//! What every test that *runs* a compiled program needs first.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::SystemTime;

/// Makes sure the runtime archive exists and is current.
///
/// `cargo test -p khora-codegen-llvm` builds `khora-rt`'s *rlib*, because that
/// is what a dependency needs. It does not build the `staticlib` crate type,
/// which is what generated executables link against — so an edit to the
/// runtime leaves a stale archive on disk and every compiled program links
/// against the previous version of it.
///
/// **This must be called by every test binary that links a program**, not just
/// one of them. Cargo runs test binaries in parallel, so a single binary
/// building the archive is a race the others lose: they link whatever was
/// there when they got to it. The failure that taught this looked exactly like
/// a code generator bug — a program calling a five-argument runtime function
/// that had four parameters in the archive, reading an argument as a flag it
/// was not, and dropping an integer as if it were a pointer.
///
/// Calling it from every binary is safe and nearly free: cargo takes its own
/// build lock, and the second caller finds the work already done.
///
/// # It only builds when the archive is actually stale
///
/// **`cargo build -p khora-rt` does not produce the same archive the enclosing
/// run did.** Measured: `-p khora-rt` writes 98,725,916 bytes and
/// `cargo build --workspace --features llvm` writes 98,490,170, because the
/// two resolve their dependencies' features differently. So a harness that
/// rebuilt unconditionally *replaced* the archive the run had already built —
/// from inside a test, while fifty other test binaries were linking against
/// it. Two `khora build` invocations seconds apart got different runtimes.
///
/// Nothing noticed until 14.17's build cache, which keys on the archive and
/// was correctly missing. Errata 51, roadmap 14.33.
///
/// The staleness question the doc comment above is really asking is "is the
/// archive older than the runtime's sources", and that is answerable without
/// building anything. When the enclosing build already wrote a current
/// archive — which is every workspace run — this now does nothing at all.
pub fn ensure_runtime() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        if archive_is_current() {
            return;
        }
        let built = Command::new("cargo")
            .args(["build", "-p", "khora-rt"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output();
        match built {
            Ok(output) if output.status.success() => {}
            other => {
                // Only fatal if there is no archive to fall back on: a
                // developer may have built one by hand, or be running from a
                // packaged toolchain with no cargo at all.
                assert!(
                    khora_codegen_llvm::toolchain::runtime_archive().is_some(),
                    "could not build the khora-rt archive and none is on disk: {other:?}"
                );
            }
        }
    });
}

/// Whether the archive on disk is newer than every runtime source.
///
/// Conservative in the direction that costs time rather than correctness: no
/// archive, an unreadable timestamp or an unreadable source directory all
/// answer "not current", and the build runs.
fn archive_is_current() -> bool {
    let Some(archive) = khora_codegen_llvm::toolchain::runtime_archive() else {
        return false;
    };
    let Some(built) = modified(&archive) else { return false };

    // The runtime's own tree, and its manifest. Not `Cargo.lock`: a dependency
    // bump changes it and cargo will have rebuilt the archive anyway, so
    // reading it here would only make this answer "stale" for a build that has
    // already happened.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(|c| c.join("khora-rt"));
    let Some(runtime) = runtime else { return false };
    match newest_under(&runtime) {
        Some(edited) => built > edited,
        None => false,
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// The most recent modification time anywhere under `directory`.
fn newest_under(directory: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![directory.to_path_buf()];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).ok()? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.is_dir() {
                // `target` under a crate directory is build output, and its
                // timestamps are downstream of the sources rather than part
                // of them.
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if let Some(when) = modified(&path) {
                newest = Some(newest.map_or(when, |seen| seen.max(when)));
            }
        }
    }
    newest
}
