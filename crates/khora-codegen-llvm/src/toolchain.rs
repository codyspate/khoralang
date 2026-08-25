//! Locating the pinned LLVM toolchain, and the runtime it links against.
//!
//! The backend does not rely on LLVM being on `PATH`. It resolves everything
//! from the same prefix `llvm-sys` was built against, so the linker and the
//! codegen library can never drift apart.
//!
//! The same problem applies to `khora-rt`: every generated executable links
//! against its static archive, and the archive that gets linked has to be the
//! one built from the source this compiler was built against.
//! [`runtime_archive`] searches for it rather than taking it as an argument,
//! because `compile` has no other place to put it.

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
    drive_clang(objects, &[], out, false)
}

/// File name of the runtime's static archive, as cargo writes it.
const RUNTIME_ARCHIVE: &str = if cfg!(windows) { "khora_rt.lib" } else { "libkhora_rt.a" };

/// System libraries `khora-rt` needs, because it carries Rust's `std` with it.
///
/// Not guessed: it is what
/// `cargo rustc -p khora-rt --crate-type staticlib -- --print native-static-libs`
/// reports, which is the only authority on the question. If a future `std`
/// version adds one, that command says so and this list is where it goes.
#[cfg(windows)]
const SYSTEM_LIBS: &[&str] = &[
    // `bcrypt` and `advapi32` arrived with TLS: `rustls` needs the operating
    // system's random number generator, and on Windows that is
    // `BCryptGenRandom` with `SystemFunction036` behind it. Re-derived with the
    // command above rather than guessed, which is the only reason the list is
    // in this order.
    "-lbcrypt",
    "-ladvapi32",
    "-lkernel32",
    "-lntdll",
    "-luserenv",
    "-lws2_32",
    "-ldbghelp",
];
#[cfg(target_os = "linux")]
const SYSTEM_LIBS: &[&str] = &["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc"];
#[cfg(target_os = "macos")]
const SYSTEM_LIBS: &[&str] = &[
    // The two frameworks are TLS, the same way `bcrypt` is on Windows:
    // `rustls` reaches the system trust store through `security-framework`,
    // which is Apple's `Security`, which is built on `CoreFoundation`.
    //
    // This list was a placeholder -- `-lSystem -lc -lm` -- until CI ran the
    // backend on a Mac for the first time and every generated program failed
    // to link with a page of undefined `_$s...CFError...` symbols. Nothing had
    // ever exercised it.
    "-lobjc",
    "-liconv",
    "-framework",
    "CoreFoundation",
    "-framework",
    "Security",
    "-lSystem",
    "-lc",
    "-lm",
];
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
const SYSTEM_LIBS: &[&str] = &["-lSystem", "-lc", "-lm"];

/// The runtime archive generated executables link against.
///
/// Searched rather than configured, in this order:
///
/// 1. `KHORA_RT_LIB`, for a packaged toolchain or an unusual layout.
/// 2. Beside the running executable, and one directory up — which covers both
///    an installed `khora` and a `cargo test` binary in `target/*/deps`.
/// 3. This crate's source tree, for a workspace build driven from elsewhere.
///
/// Returns `None` if nothing plausible is on disk, so the caller can say which
/// command produces it rather than failing inside the linker.
pub fn runtime_archive() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("KHORA_RT_LIB") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(RUNTIME_ARCHIVE));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(RUNTIME_ARCHIVE));
            }
        }
    }

    // `CARGO_MANIFEST_DIR` is baked in at build time, so this only helps a
    // compiler that is still sitting in the tree it was built from — which is
    // exactly the case where the first two probes can miss.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace) = manifest.parent().and_then(|c| c.parent()) {
        for profile in ["debug", "release"] {
            candidates.push(workspace.join("target").join(profile).join(RUNTIME_ARCHIVE));
        }
    }

    candidates.into_iter().find(|p| p.is_file())
}

/// Links objects with the Khora runtime into an executable.
///
/// This is what [`crate::compile`] finishes with. The runtime archive goes
/// *after* the objects and the system libraries after that: a static link
/// resolves left to right, so an archive listed before its user contributes
/// nothing.
/// Whether a build emits debug information.
///
/// On by default, and off with `KHORA_DEBUG=0`. There is no release mode to
/// hang this off yet, and the default that serves a language being brought up
/// is the one where a crash can be read. When there is an optimization level to
/// speak of, this becomes part of it.
///
/// **Here rather than in `debug`, which is where it belongs and cannot live.**
/// That module is behind the `llvm` feature because it names inkwell types;
/// this is an environment variable, and the linker driver below needs it in a
/// build that has no LLVM at all. Defining it there made `cargo build` fail for
/// anyone working on the front end, which is the one thing the feature exists
/// to prevent.
pub fn debug_info_wanted() -> bool {
    !matches!(std::env::var("KHORA_DEBUG").as_deref(), Ok("0") | Ok("off") | Ok("false"))
}

pub fn link_with_runtime(objects: &[&Path], out: &Path, library: bool) -> Result<(), String> {
    let runtime = runtime_archive().ok_or_else(|| {
        format!(
            "the Khora runtime archive ({RUNTIME_ARCHIVE}) was not found. Build it with \
             `cargo build -p khora-rt`, or point KHORA_RT_LIB at it."
        )
    })?;
    drive_clang(objects, &[runtime.as_path()], out, library)
}

fn drive_clang(
    objects: &[&Path],
    archives: &[&Path],
    out: &Path,
    library: bool,
) -> Result<(), String> {
    let clang = tool("clang").ok_or_else(|| {
        "clang not found in the LLVM toolchain; set LLVM_SYS_221_PREFIX \
         (see docs/llvm-setup.md)"
            .to_string()
    })?;

    let mut cmd = std::process::Command::new(&clang);
    // A shared library rather than an executable: no entry point, and the
    // exported symbols are the whole interface. `docs/design/c-export.md`.
    if library {
        cmd.arg("-shared");
    }
    cmd.args(objects).args(archives);
    if !archives.is_empty() {
        cmd.args(SYSTEM_LIBS);
    }
    // **The link needs telling too.** The object carried `.debug$S` and
    // `.debug$T` all along and the executable had neither, because a linker
    // does not keep debug sections it was not asked for — on Windows that
    // means no PDB is written at all. Emitting perfect metadata into an
    // artifact that discards it is the failure mode worth guarding against
    // here: everything verifies, nothing is wrong, and no debugger can read
    // the program.
    if debug_info_wanted() {
        cmd.arg("-g");
    }
    cmd.arg("-o").arg(out);

    let output = cmd.output().map_err(|e| format!("running {}: {e}", clang.display()))?;
    if !output.status.success() {
        // **A cross build reaches here on its way to a linker that cannot help
        // it**, and the raw message is baffling: `lld-link` says "unknown file
        // type" about a perfectly good aarch64 object, because it is an ELF
        // and the Windows linker reads COFF. Code generation succeeded; what
        // is missing is a linker and a sysroot for the target, which
        // `docs/design/targets.md` lists as steps three and four and this is
        // step one. Saying so beats letting somebody debug their program.
        let crossing = khora_db::target_triple();
        let note = match &crossing {
            Some(triple) => format!(
                "\n\nThe object was generated for `{triple}` and this host's linker cannot \
                 read it. Code generation for another target works; linking for one needs a \
                 linker and a sysroot that this build does not yet fetch — \
                 `docs/design/targets.md`."
            ),
            None => String::new(),
        };
        return Err(format!(
            "link failed ({}):\n{}{note}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
