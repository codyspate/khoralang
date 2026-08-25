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

    // **A cross build's archive is looked for first, before anything sitting
    // next to the compiler.** The probes below find the *host* archive — it is
    // beside the `khora` binary, which is the common case and the right answer
    // when building for this machine. For another target it is the wrong file
    // with the right name, and `wasm-ld` says so a thousand times over:
    // "archive member is neither Wasm object file nor LLVM bitcode", once per
    // member. Worse, with `--allow-undefined` it had said nothing at all and
    // turned every runtime symbol into a host import.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let (Some(triple), Some(workspace)) =
        (khora_db::target_triple(), manifest_dir.parent().and_then(|c| c.parent()))
    {
        for profile in ["debug", "release"] {
            candidates.push(
                workspace.join("target").join(&triple).join(profile).join(cross_archive()),
            );
        }
    }

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

/// What cargo calls the archive when it builds for another target.
///
/// Not [`RUNTIME_ARCHIVE`], which is named for the *host*: cargo uses the
/// target's own convention, so a wasm or Linux cross build produces
/// `libkhora_rt.a` even when the compiler doing the building runs on Windows.
fn cross_archive() -> &'static str {
    match khora_db::target_triple() {
        Some(triple) if triple.contains("windows") => "khora_rt.lib",
        _ => "libkhora_rt.a",
    }
}

/// Links objects with the Khora runtime into an executable.
///
/// This is what [`crate::compile`] finishes with. The runtime archive goes
/// *after* the objects and the system libraries after that: a static link
/// resolves left to right, so an archive listed before its user contributes
/// nothing.
/// Runtime symbols a shared library publishes alongside its own exports.
///
/// **A library needs a control surface, and it comes from the archive rather
/// than from generated code.** The Khora functions get `dllexport` where they
/// are built; these are Rust, in a static archive, and nothing in the emitted
/// module refers to them — so on Windows they are absent from the export table
/// and on ELF the archive member holding them is never pulled in at all.
/// Naming them here fixes both, and naming them *explicitly* rather than
/// exporting everything keeps the rest of the runtime — and the Rust standard
/// library it carries — out of the published surface.
///
/// The first three are the contract: `docs/design/c-export.md` §8. The
/// counters are diagnostic, and `docs/design/compatibility.md` is clear that
/// allocation behaviour is not part of the language's promise — they are here
/// because a host that has just been told a trap was contained is entitled to
/// check that the memory actually came back.
const LIBRARY_CONTROL: &[&str] = &[
    "khora_set_trap_policy",
    "khora_trapped",
    "khora_clear_trap",
    "khora_live_count",
    "khora_alloc_count",
];

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

pub fn link_with_runtime(
    objects: &[&Path],
    out: &Path,
    library: bool,
    exports: &[String],
) -> Result<(), String> {
    let wasm = khora_db::target_triple().is_some_and(|t| t.contains("wasm"));
    let runtime = runtime_archive().ok_or_else(|| {
        let (archive, how) = match khora_db::target_triple() {
            Some(triple) => (
                cross_archive().to_string(),
                format!("cargo build -p khora-rt --target {triple}"),
            ),
            None => (RUNTIME_ARCHIVE.to_string(), "cargo build -p khora-rt".to_string()),
        };
        format!(
            "the Khora runtime archive ({archive}) was not found for this target. Build it \
             with `{how}`, or point KHORA_RT_LIB at it."
        )
    })?;
    if wasm {
        return drive_wasm_ld(objects, runtime.as_path(), out, exports);
    }
    drive_clang(objects, &[runtime.as_path()], out, library)
}

/// Links a WebAssembly module with `wasm-ld`.
///
/// **Not through `clang`**, which drives the *host* linker: `lld-link` reads a
/// perfectly good wasm object and says "unknown file type", which is true and
/// unhelpful. `wasm-ld` ships in the same LLVM toolchain the rest of this uses.
///
/// A wasm module needs no sysroot, which is why this target is reachable while
/// `aarch64-unknown-linux-gnu` is not: there is no libc to find and no CRT to
/// link, so the runtime archive and the emitted object are the whole input.
///
/// **Exports are named one at a time rather than with `--export-dynamic`.**
/// Exporting everything published 1,255 symbols from a three-function library
/// and defeated `wasm-ld`'s own dead-code elimination, which matters on a
/// platform with a size limit — `docs/design/targets.md` §"Which wasm, though"
/// puts Cloudflare Workers first, and a Worker is measured in megabytes.
fn drive_wasm_ld(
    objects: &[&Path],
    runtime: &Path,
    out: &Path,
    exports: &[String],
) -> Result<(), String> {
    let linker = tool("wasm-ld").ok_or_else(|| {
        "wasm-ld not found in the LLVM toolchain; set LLVM_SYS_221_PREFIX \
         (see docs/llvm-setup.md)"
            .to_string()
    })?;

    let mut cmd = std::process::Command::new(&linker);
    // No `_start`: a Khora library is called through its exports, and a wasm
    // module with an entry point the host never calls is a module that fails
    // to instantiate for want of one.
    cmd.arg("--no-entry");
    for symbol in exports.iter().map(String::as_str).chain(LIBRARY_CONTROL.iter().copied()) {
        cmd.arg(format!("--export={symbol}"));
    }
    // **No `--allow-undefined`, and that was a real bug rather than a
    // preference.** It reads like "let the host supply what is missing", and
    // what it actually does is stop resolving: `khora_overflow`,
    // `khora_contain_enabled` and `khora_export_call` are all *defined in the
    // archive on the command line*, and every one of them came out as an
    // `env.` import the embedder would have had to provide. The module linked,
    // validated, exported exactly the right two names, and would have failed
    // to instantiate on a Worker for want of four functions it already had.
    //
    // A link error naming a missing symbol is the better failure by a wide
    // margin. When a Worker genuinely needs a host import — entropy, a clock —
    // it will be declared, and declared imports do not need this flag.
    cmd.args(objects).arg(runtime).arg("-o").arg(out);

    let output = cmd.output().map_err(|e| format!("running {}: {e}", linker.display()))?;
    if !output.status.success() {
        return Err(format!(
            "wasm-ld failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
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
        for symbol in LIBRARY_CONTROL {
            if cfg!(windows) {
                // Publishes it *and* pulls the archive member in, which are
                // two problems with one flag.
                cmd.arg(format!("-Wl,/EXPORT:{symbol}"));
            } else {
                // `-u` forces the member to be linked; a shared object's
                // symbols are visible by default once it is.
                cmd.arg(format!("-Wl,-u,{symbol}"));
            }
        }
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

    // **No timestamp in the artifact.** `docs/project.md` §6.1 asks for
    // bit-for-bit reproducible builds, and a PE header carries a
    // `TimeDateStamp` that is the clock unless the linker is told otherwise.
    // `/Brepro` replaces it with a hash of the content, which is what the flag
    // is for.
    //
    // **It is not sufficient on Windows with debug information**, and that is
    // measured rather than assumed: relinking one unchanged object twice gives
    // identical bytes without `-g` and different bytes with it, `/Brepro` or
    // not. What varies is inside lld-link's PDB emission. Recorded in
    // `docs/roadmap.md` 12.9 rather than worked around, because a build that
    // is reproducible only without debug information is a real limit and
    // pretending otherwise would be worse than naming it.
    if cfg!(windows) {
        cmd.arg("-Wl,-Brepro");
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
