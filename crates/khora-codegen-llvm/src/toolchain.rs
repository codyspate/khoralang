//! Locating the pinned LLVM toolchain.
//!
//! The backend does not rely on LLVM being on `PATH`. It resolves everything
//! from the same prefix `llvm-sys` was built against, so the linker and the
//! codegen library can never drift apart.

use std::path::{Path, PathBuf};

/// The prefix `llvm-sys` was configured with at build time.
///
/// `llvm-sys` reads `LLVM_SYS_221_PREFIX` from the environment in its build
/// script; we capture the same value so runtime tool lookup agrees with it.
pub fn prefix() -> Option<PathBuf> {
    option_env!("LLVM_SYS_221_PREFIX").map(PathBuf::from).or_else(|| {
        std::env::var_os("LLVM_SYS_221_PREFIX").map(PathBuf::from)
    })
}

/// Path to a tool in the toolchain's `bin` directory, e.g. `clang` or
/// `lld-link`. Returns `None` if the toolchain prefix is unknown or the tool
/// is missing.
pub fn tool(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let path = prefix()?.join("bin").join(exe);
    path.is_file().then_some(path)
}

/// Links object files into an executable.
///
/// `clang` is used as the driver rather than invoking a linker directly: it
/// already knows how to find the platform's CRT and system libraries, and it
/// is the same driver that will handle the cross-targets in Phase 6.
pub fn link_executable(objects: &[&Path], out: &Path) -> Result<(), String> {
    let clang = tool("clang").ok_or_else(|| {
        "clang not found in the LLVM toolchain; set LLVM_SYS_221_PREFIX \
         (see docs/llvm-setup.md)"
            .to_string()
    })?;

    let mut cmd = std::process::Command::new(&clang);
    cmd.args(objects).arg("-o").arg(out);

    let output = cmd.output().map_err(|e| format!("running {}: {e}", clang.display()))?;
    if !output.status.success() {
        return Err(format!(
            "link failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
