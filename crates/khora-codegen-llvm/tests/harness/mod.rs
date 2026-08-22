//! What every test that *runs* a compiled program needs first.

use std::process::Command;
use std::sync::OnceLock;

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
pub fn ensure_runtime() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
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
