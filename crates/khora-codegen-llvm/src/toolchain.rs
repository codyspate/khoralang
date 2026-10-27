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

/// The C driver used to link, wherever it is.
///
/// **An installed toolchain has no `LLVM_SYS_221_PREFIX`**, and the linker is
/// the one thing a downloaded `khora` cannot bring with it. Everything else is
/// in the archive — LLVM is linked into the binary, the runtime archive sits
/// beside it, `std` is found the same way — but linking needs the platform's
/// own C runtime and system libraries, and the driver that knows where those
/// live belongs to the platform. `rustup` makes the same trade.
///
/// The pinned prefix comes first, so a build from this tree uses the LLVM it
/// was built against; then `PATH`. On Windows the fallbacks are clang only: a
/// MinGW `gcc` produces a different ABI from the MSVC-targeting objects the
/// backend emits, and linking those fails a long way from its cause.
pub fn linker() -> Option<PathBuf> {
    if let Some(pinned) = tool("clang") {
        return Some(pinned);
    }
    let names: &[&str] =
        if cfg!(windows) { &["clang", "clang-cl"] } else { &["clang", "cc", "gcc"] };
    names.iter().find_map(|name| which(name))
}

/// The first `name` on `PATH`, or `None`.
///
/// Written here rather than taken as a dependency: it is a dozen lines, and a
/// compiler reaching for a crate to look at `PATH` is a crate to audit for the
/// sake of a dozen lines.
fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::var_os("PATH")?
        .to_str()?
        .split(if cfg!(windows) { ';' } else { ':' })
        .filter(|entry| !entry.is_empty())
        .map(|entry| Path::new(entry).join(&exe))
        .find(|path| path.is_file())
}

/// What to say when there is no linker.
///
/// Names the command for the platform the reader is actually on. "Install a C
/// toolchain" is advice they have to translate, and this is the first thing
/// anybody meets after downloading a compiler.
fn no_linker() -> String {
    let install = if cfg!(windows) {
        // **Clang by name, because that is what is probed for.** The workload
        // alone does not install one -- its C++ Clang tools are a separate
        // component nobody ticks by accident -- and MSVC's own `cl.exe` is not
        // a substitute here: `link` uses the driver to find the CRT and the
        // system libraries, which `link.exe` does not do for it.
        "install LLVM from https://releases.llvm.org, or add the \"C++ Clang tools for \
         Windows\" component to the Visual Studio Build Tools. The \"Desktop development \
         with C++\" workload on its own does not include one"
    } else if cfg!(target_os = "macos") {
        "run `xcode-select --install`"
    } else {
        "install `clang` or `gcc` from your package manager"
    };
    format!(
        "no linker found. Khora compiles to a native object and needs a C driver to link it \
         against the platform's runtime: {install}. Or set LLVM_SYS_221_PREFIX to an LLVM \
         installation, or put `clang` on PATH."
    )
}

/// Links object files into an executable.
///
/// `clang` is used as the driver rather than invoking a linker directly: it
/// already knows how to find the platform's CRT and system libraries, and it
/// is the same driver that will handle the cross-targets in Phase 6.
pub fn link_executable(objects: &[&Path], out: &Path) -> Result<(), String> {
    drive_clang(objects, &[], out, false, Profile::from_env(), &natives())
}

/// File name of the runtime's static archive, as cargo writes it.
const RUNTIME_ARCHIVE: &str = if cfg!(windows) { "khora_rt.lib" } else { "libkhora_rt.a" };

/// The name every reproducible macOS link writes to before being moved.
///
/// `ld64` puts the output file's name inside the ad-hoc code signature, so the
/// name has to be the same for two links to produce the same bytes. Any fixed
/// string does; this one says why it exists to whoever finds it in a signature.
const RELEASE_IMAGE: &str = "khora-release-image";

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
    let triple = khora_db::target_triple();

    // **A cross build's archive is looked for first, before anything sitting
    // next to the compiler.** The probes below find the *host* archive — it is
    // beside the `khora` binary, which is the common case and the right answer
    // when building for this machine. For another target it is the wrong file
    // with the right name, and `wasm-ld` says so a thousand times over:
    // "archive member is neither Wasm object file nor LLVM bitcode", once per
    // member. Worse, with `--allow-undefined` it had said nothing at all and
    // turned every runtime symbol into a host import.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(triple) = &triple {
        candidates.extend(cross_archive_candidates(triple));
        if let Some(workspace) = manifest_dir.parent().and_then(|c| c.parent()) {
            for profile in ["debug", "release"] {
                candidates.push(
                    workspace.join("target").join(triple).join(profile).join(cross_archive()),
                );
            }
        }
        // **A musl build never falls back to the host's archive.** The host's
        // is built against glibc, and linked statically against musl's `libc.a`
        // it either fails on a glibc-only symbol or, worse, links -- a program
        // carrying two C libraries' assumptions about one process. "Not found,
        // build it with this" is the better message by a distance.
        if is_musl(triple) {
            return candidates.into_iter().find(|p| p.is_file());
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

/// Whether `triple` names a musl target, which links statically.
pub fn is_musl(triple: &str) -> bool {
    triple.ends_with("-musl")
}

/// Where a target's runtime archive can be, relative to the running compiler.
///
/// Two layouts, both found without configuration:
///
/// - **A packaged toolchain** carries other targets' runtimes in
///   `targets/<triple>/` beside `bin/`, so `khora` at `<prefix>/bin/khora`
///   looks in `<prefix>/targets/<triple>/`.
/// - **A cargo build** puts `cargo build -p khora-rt --target <triple>` in
///   `<target-dir>/<triple>/<profile>/`, and the compiler (or a test binary in
///   `deps/`) is under `<target-dir>/<profile>/`. The workspace probe in
///   [`runtime_archive`] covers only `<workspace>/target`, so a build with
///   `CARGO_TARGET_DIR` set elsewhere -- every CI cache, and every agent
///   worktree -- built the archive and then could not find it.
fn cross_archive_candidates(triple: &str) -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    let mut candidates = Vec::new();
    for dir in exe.ancestors().skip(1).take(3) {
        candidates.push(dir.join("targets").join(triple).join(cross_archive()));
        for profile in ["debug", "release"] {
            candidates.push(dir.join(triple).join(profile).join(cross_archive()));
        }
    }
    candidates
}

/// The files a static musl executable is linked from, besides its own objects
/// and the runtime archive.
///
/// They are Rust's: `rustup target add <triple>` installs musl's `libc.a`,
/// its start files, and LLVM's `libunwind.a` in
/// `lib/rustlib/<triple>/lib/self-contained/`, and that is the C library the
/// runtime archive was built against. Taking them from there means the two
/// cannot disagree about the libc.
///
/// **And `crtbegin.o` and `crtend.o`** from the same directory, which are
/// LLVM compiler-rt's `crtbegin.c`/`crtend.c` (Apache-2.0 WITH LLVM-exception,
/// like `libunwind.a`; Rust compiles them with a gcc, which is all the
/// `GCC: 9.4.0` in their `.comment` says). `crtend.o` holds
/// the zero word that ends `.eh_frame`. lld writes one of its own and GNU ld
/// does not, so without it a static program linked by bfd segfaults inside
/// its own trap report when `KHORA_BACKTRACE=1` walks the frames. Rust links
/// the same pair for the same target.
const MUSL_START: &[&str] = &["crt1.o", "crti.o", "crtbegin.o"];
/// What closes a static musl link, in this order: the unwinder and the C
/// library after everything that calls them, then the end files.
const MUSL_END: &[&str] = &["libunwind.a", "libc.a", "crtend.o", "crtn.o"];

/// Where the musl start files and C library are for `triple`.
///
/// **Beside the runtime archive first**, as `sysroot/`, which is how a
/// packaged toolchain carries them: one directory per target holding both, so
/// the libc that ships is the one the runtime it ships beside was built
/// against. Then the Rust toolchain's own, for a build from source.
fn musl_sysroot(triple: &str, runtime: &Path) -> Result<PathBuf, String> {
    let complete = |dir: &Path| MUSL_START.iter().chain(MUSL_END).all(|f| dir.join(f).is_file());
    if let Some(beside) = runtime.parent().map(|dir| dir.join("sysroot")) {
        if complete(&beside) {
            return Ok(beside);
        }
    }
    let rustc = std::process::Command::new("rustc").args(["--print", "sysroot"]).output();
    if let Ok(out) = rustc {
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let dir =
            Path::new(&root).join("lib").join("rustlib").join(triple).join("lib").join("self-contained");
        if out.status.success() && complete(&dir) {
            return Ok(dir);
        }
    }
    Err(format!(
        "no musl C library was found for `{triple}`. A packaged toolchain carries one beside the \
         target's runtime archive, in `sysroot/`; from a source build, run `rustup target add \
         {triple}`, which installs musl's `libc.a` and start files with the Rust toolchain."
    ))
}

/// Runtime symbols a shared library publishes alongside its own exports.
///
/// **A library's control surface comes from the archive rather than from
/// generated code.** Khora functions get `dllexport` where they are built;
/// these are Rust, in a static archive, and nothing in the emitted module
/// refers to them — so on Windows they are absent from the export table and on
/// ELF the archive member holding them is never pulled in. Naming them
/// *explicitly* rather than exporting everything keeps the rest of the runtime,
/// and the Rust standard library it carries, out of the published surface.
///
/// The first three are the contract (`docs/design/c-export.md` §8). The
/// counters are diagnostic — allocation behavior is not part of the language's
/// promise — and are here because a host just told a trap was contained is
/// entitled to check that the memory came back.
///
/// **`khora_enable_counters` comes with them because counting is off until
/// something asks.** A program gets the call emitted into its `main` when it
/// declares a counter; a library has no `main`, so a host that wants a number
/// asks for one first and gets `-1` if it forgets. That is the honest failure:
/// zero is what a leak check hopes to see, and answering it without having
/// counted would be a lie a host could not detect.
const LIBRARY_CONTROL: &[&str] = &[
    "khora_set_trap_policy",
    "khora_trapped",
    "khora_clear_trap",
    "khora_enable_counters",
    "khora_live_count",
    "khora_alloc_count",
];

/// How much a build optimizes, and what it therefore keeps.
///
/// **Two, and no more, on purpose.** A profile is a *name for a set of
/// answers*, and its value is that a person can say which set they want
/// without knowing what is in it. Three is where that stops being true — the
/// third is always "release but with something", and the something is better
/// asked for directly. `docs/design/profiles.md`.
///
/// Here rather than beside the code generator for the reason
/// [`Profile::debug_info`] gives: the linker driver in this module needs the
/// answer in a build with no LLVM in it at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Profile {
    /// Unoptimized, with debug information. What a language being brought up
    /// should do when nobody said otherwise: the crash you are about to have
    /// is readable.
    #[default]
    Debug,
    /// Optimized, without debug information, and bit-for-bit reproducible.
    Release,
}

impl Profile {
    /// The profile `KHORA_PROFILE` names, or [`Profile::Debug`].
    ///
    /// An environment variable as well as `--release` because `khora test` and
    /// `khora bench` have profiles too, and a flag on every subcommand is
    /// three ways to say one thing. Anything unrecognized is the default
    /// rather than an error: this is read deep inside a build, where there is
    /// no channel to complain on, and a typo that silently optimized would be
    /// worse than one that silently did not.
    pub fn from_env() -> Profile {
        match std::env::var("KHORA_PROFILE").as_deref() {
            Ok("release") => Profile::Release,
            _ => Profile::Debug,
        }
    }

    /// What the profile is called, for a message.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }

    /// Whether this build emits debug information.
    ///
    /// The profile decides, and `KHORA_DEBUG` overrides — in both directions,
    /// which is the point. A release build with debug information is what
    /// profiling wants, and a debug build without one is how 12.9 measured
    /// reproducibility before there was a profile to ask for.
    ///
    /// **Here rather than in `debug`, which is where it belongs and cannot
    /// live.** That module is behind the `llvm` feature because it names
    /// inkwell types; this is an environment variable, and the linker driver
    /// below needs it in a build that has no LLVM at all. Defining it there
    /// made `cargo build` fail for anyone working on the front end, which is
    /// the one thing the feature exists to prevent.
    pub fn debug_info(self) -> bool {
        match std::env::var("KHORA_DEBUG").as_deref() {
            Ok("0") | Ok("off") | Ok("false") => false,
            Ok("1") | Ok("on") | Ok("true") => true,
            _ => self == Profile::Debug,
        }
    }

    /// The LLVM pass pipeline to run over the module, if any.
    ///
    /// `None` in debug, and that is what a debug build has always done —
    /// nothing but instruction selection. `default<O2>` in release rather than
    /// `O3`: the difference between them is mostly loop unrolling and
    /// vectorization, which costs compile time and code size for programs that
    /// are not numerical kernels, and there is no measurement here that says
    /// Khora's output is. `O3` is a change to make with a benchmark in hand.
    pub fn pipeline(self) -> Option<&'static str> {
        match self {
            Profile::Debug => None,
            Profile::Release => Some("default<O2>"),
        }
    }
}

/// Whether a build emits debug information, for the profile the environment
/// names.
///
/// The linker driver's view: it has no profile to hand and reads the same
/// answer the code generator did.
pub fn debug_info_wanted() -> bool {
    Profile::from_env().debug_info()
}

/// Native libraries the root package asked to link against.
///
/// **Only the root package's request is honored.** A dependency may ship an
/// archive and declare `extern fn` against it, but it cannot add a flag to the
/// link line: a transitive package quietly linking a native library into a
/// program is a supply-chain hole with no signal at the place that would have
/// to consent. The package does the work and documents one line for the program
/// to add, so every native library a build links is readable from its own
/// manifest.
#[derive(Debug, Default, Clone)]
pub struct Natives {
    /// Library names as the linker spells them: `pq` becomes `-lpq`.
    pub link: Vec<String>,
    /// Absolute directories to search, already resolved against the manifest.
    pub search: Vec<PathBuf>,
}

impl Natives {
    /// Whether anything was asked for.
    pub fn is_empty(&self) -> bool {
        self.link.is_empty() && self.search.is_empty()
    }
}

/// The native libraries this build may link, set once by whoever read the
/// manifest.
///
/// **A global rather than a parameter, for the same reason [`Profile`] reads
/// the environment.** The link happens at the bottom of several public entry
/// points -- `compile`, `compile_with`, `compile_library`, and the library form
/// -- and threading one more argument through all of them to reach a single
/// call site would widen four signatures for one consumer. What matters is that
/// the value is decided *once*, by the CLI, which is the only layer that has a
/// manifest and an error channel to complain on.
static NATIVES: std::sync::OnceLock<Natives> = std::sync::OnceLock::new();

/// Records what the root manifest asked to link. Later calls are ignored.
pub fn set_natives(natives: Natives) {
    let _ = NATIVES.set(natives);
}

/// What was recorded, or nothing.
fn natives() -> Natives {
    NATIVES.get().cloned().unwrap_or_default()
}

/// Links objects with the Khora runtime into an executable.
///
/// What [`crate::compile`] finishes with. The runtime archive goes *after* the
/// objects and the system libraries after that: a static link resolves left to
/// right, so an archive listed before its user contributes nothing.
pub fn link_with_runtime(
    objects: &[&Path],
    out: &Path,
    library: bool,
    exports: &[String],
    profile: Profile,
) -> Result<(), String> {
    let natives = natives();
    let wasm = khora_db::target_triple().is_some_and(|t| t.contains("wasm"));
    let runtime = runtime_archive().ok_or_else(|| {
        let (archive, how) = match khora_db::target_triple() {
            Some(triple) if is_musl(&triple) => (
                cross_archive().to_string(),
                format!("sh scripts/build-musl-runtime.sh {triple}"),
            ),
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
    check_archive_target(&runtime)?;
    if wasm {
        if !natives.is_empty() {
            return Err("linking a native library is not supported for wasm: the module's \
                        imports are resolved by the host, so a `-l` flag has nothing to \
                        resolve against. Remove `build.link` for this target."
                .to_string());
        }
        return drive_wasm_ld(objects, runtime.as_path(), out, exports);
    }
    if let Some(triple) = khora_db::target_triple().filter(|t| is_musl(t)) {
        if library {
            return Err(format!(
                "`{triple}` builds a static executable, and a shared library cannot be one: \
                 the library would carry its own copy of musl's `libc.a` into a host process \
                 that already has a C library. Build the library for `{}` instead.",
                triple.replace("-musl", "-gnu")
            ));
        }
        let sysroot = musl_sysroot(&triple, &runtime)?;
        return drive_static_musl(&triple, objects, &runtime, &sysroot, out, profile, &natives);
    }
    drive_clang(objects, &[runtime.as_path()], out, library, profile, &natives)
}

/// Links a static executable against musl, naming every input.
///
/// **`-nostdlib`, so that nothing comes from the machine.** Without it the
/// driver adds the host's start files and `-lc`, which on a glibc host are
/// glibc's: the link succeeds, and the result is a static executable carrying
/// glibc's `crt1.o` on top of musl's `libc.a`. Every file here is named, in
/// the order a static link needs -- start files, the program, the runtime,
/// what the program asked to link, the unwinder and the C library after all of
/// their callers, end files.
///
/// **`ld.lld` when there is one**, and it is required for another
/// architecture: the host's `ld` reads only its own machine's objects, and
/// said `file in wrong format` about an aarch64 one. For the host's own
/// architecture the default linker works too, so its absence is not an error
/// there.
///
/// **What it costs:** the whole of musl and Rust's standard library are in
/// every executable -- a debug hello world is tens of megabytes, almost all of
/// it debug information that `--release` drops. And musl's `malloc` is not in
/// play, because the runtime brings mimalloc, so the throughput gap between
/// glibc and musl allocators that `khora-rt`'s manifest records does not apply.
fn drive_static_musl(
    triple: &str,
    objects: &[&Path],
    runtime: &Path,
    sysroot: &Path,
    out: &Path,
    profile: Profile,
    natives: &Natives,
) -> Result<(), String> {
    let clang = linker().ok_or_else(no_linker)?;
    let lld = tool("ld.lld").or_else(|| which("ld.lld"));
    let mut cmd = std::process::Command::new(&clang);
    cmd.args(static_musl_flags(triple, &clang, lld.as_deref(), std::env::consts::ARCH)?);
    cmd.args(MUSL_START.iter().map(|f| sysroot.join(f)));
    cmd.args(objects).arg(runtime);
    cmd.args(musl_natives(triple, natives)?);
    cmd.args(MUSL_END.iter().map(|f| sysroot.join(f)));
    if profile.debug_info() {
        cmd.arg("-g");
    }
    cmd.arg("-o").arg(out);

    let output = cmd.output().map_err(|e| format!("running {}: {e}", clang.display()))?;
    if !output.status.success() {
        return Err(format!(
            "static link for `{triple}` failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
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
    //
    // **`--fatal-warnings`, because the warning that mattered was a trap.** A
    // call whose signature differs from the definition's is only a warning to
    // `wasm-ld`, which then links the call to a stub whose body is
    // `unreachable`; and the warning was never shown, because this prints the
    // linker's output only when it fails. A runtime function taking a `usize`
    // where generated code passes an `i64` made every `print` in a wasm module
    // a trap that way. With the flag it is a link error naming the function.
    cmd.arg("--fatal-warnings");
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
    profile: Profile,
    natives: &Natives,
) -> Result<(), String> {
    let clang = linker().ok_or_else(no_linker)?;

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
                //
                // **Spelled the way the object file spells it, which is not
                // the same on both.** Mach-O prefixes every C symbol with an
                // underscore, and `-u` takes the name as it appears in the
                // object -- so `-u khora_alloc_count` asks a macOS linker for
                // a symbol nothing defines, and all six of these came back as
                // `Undefined symbols for architecture arm64 ... referenced
                // from: <initial-undefines>`. That phrasing is the tell: an
                // initial undefine is one *this flag* asked for, not one the
                // program referenced.
                //
                // ELF carries no such prefix, so Linux was always right and
                // only macOS was ever wrong -- which is why nothing found it
                // until the three-platform matrix ran again.
                let named = if cfg!(target_os = "macos") {
                    format!("_{symbol}")
                } else {
                    symbol.to_string()
                };
                cmd.arg(format!("-Wl,-u,{named}"));
            }
        }
    }
    cmd.args(objects).args(archives);
    // **After the program's own objects, before the system libraries.** A
    // static archive only contributes the members something already undefined
    // asks for, so an archive named ahead of the object that needs it
    // contributes nothing and the symbol is still missing at the end.
    for dir in &natives.search {
        cmd.arg(format!("-L{}", dir.display()));
    }
    for name in &natives.link {
        cmd.arg(format!("-l{name}"));
    }
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
    // The *profile's* answer rather than the environment's, because a build
    // may have been asked for with `--release` and never touched a variable.
    // A link that adds `-g` to a module carrying no debug metadata is not
    // merely pointless: on Windows it costs the reproducibility that not
    // emitting debug information is what buys.
    if profile.debug_info() {
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

    // **`main` gets the stack a fiber gets, on Windows too.** A fiber's stack
    // is `khora_rt::stack::FIBER_STACK`, 8 MB, chosen as what `main` gets from
    // a default `ulimit -s` on Linux and from the system on macOS. A Windows
    // executable's main thread gets what its PE header asks for, and the
    // linker's default is 1 MB -- so on Windows the program that recursed
    // fifty thousand frames on a fiber overflowed doing it in `main`. Reserved,
    // not committed: pages are only taken as the stack grows into them.
    // Executables only; a library's host decides its own thread's stack.
    if cfg!(windows) && !library {
        cmd.arg(format!("-Wl,/STACK:{}", khora_rt::FIBER_STACK));
    }

    // **A Mach-O image's reproducibility is decided by the output file's
    // name, and by nothing else here.** Two fields vary with it: `LC_UUID`,
    // which `ld64` hashes over the content *and the path*, and the ad-hoc code
    // signature an `arm64` image carries by default, whose identifier is the
    // file name embedded in the image. Linking one unchanged set of objects to
    // `reused.exe` and to `fresh.exe` differed in both.
    //
    // So the link writes `khora-release-image` in a `.khora-link` directory
    // beside the output and the result is renamed into place. One name for
    // every build makes both fields constant, the caller still gets the path
    // it asked for, and the bytes that were signed are the bytes that ship.
    //
    // **`-no_uuid` is not the fix, and was actively wrong.** It removes the
    // load command, and `dyld` on Apple Silicon then refuses to start the
    // program:
    //
    //     dyld[23193]: missing LC_UUID load command
    //
    // A reproducible binary that will not run is not a trade worth making, and
    // the flag is unnecessary once the name is stable -- measured both ways by
    // cross-linking `arm64` images on Linux and comparing hashes.
    //
    // Release only. Every other platform's link is the command it was before.
    let staged_name = cfg!(target_os = "macos") && !profile.debug_info();
    let staging = staged_name
        .then(|| out.parent().map(|dir| dir.join(".khora-link")))
        .flatten();
    let linked = match &staging {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("creating {}: {e}", dir.display()))?;
            dir.join(RELEASE_IMAGE)
        }
        None => out.to_path_buf(),
    };
    cmd.arg("-o").arg(&linked);

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
                 linker and a sysroot that this build does not yet fetch."
            ),
            None => String::new(),
        };
        return Err(format!(
            "link failed ({}):\n{}{note}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // The staged image into place, and the staging directory away with it. A
    // rename rather than a copy, so the bytes the linker signed are the bytes
    // that ship and nothing can be observed half-written.
    if let Some(dir) = staging {
        std::fs::rename(&linked, out)
            .map_err(|e| format!("moving {} to {}: {e}", linked.display(), out.display()))?;
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(())
}

/// The prefix of the symbol `khora-rt`'s build script defines, followed by
/// its target triple with `-` (and `.`) written `__`.
const BUILT_FOR: &str = "khora_rt_built_for_";

/// The target a runtime archive says it was built for, read from its symbol
/// table.
///
/// `None` for an archive that carries no mark -- one built before the mark
/// existed, or a format whose table this does not read (only the System V /
/// GNU `/` member, which every archive cargo writes for an ELF or wasm target
/// has). The check is a refusal of a known mismatch, never a requirement, so
/// an unreadable archive links as it always did.
pub fn archive_target(archive: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(archive).ok()?;
    let mut header = [0u8; 68];
    file.read_exact(&mut header).ok()?;
    if &header[..8] != b"!<arch>\n" || header[8] != b'/' || header[9] != b' ' {
        return None;
    }
    let size: usize = std::str::from_utf8(&header[56..66]).ok()?.trim().parse().ok()?;
    let mut table = vec![0u8; size];
    file.read_exact(&mut table).ok()?;
    let at = table.windows(BUILT_FOR.len()).position(|w| w == BUILT_FOR.as_bytes())?;
    let rest = &table[at + BUILT_FOR.len()..];
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Some(std::str::from_utf8(&rest[..end]).ok()?.replace("__", "-"))
}

/// Refuses a runtime archive that says it was built for another target.
///
/// What it prevents: `KHORA_RT_LIB` -- one variable for every target -- naming
/// a musl runtime in a glibc build, which linked silently and ran a glibc
/// program carrying a musl-built Rust standard library; and a glibc runtime in
/// a musl build, which failed on `undefined symbol: fstat64` and named no
/// file. Costs one read of the archive's symbol table per link, about 2 MB.
fn check_archive_target(archive: &Path) -> Result<(), String> {
    let Some(built_for) = archive_target(archive) else { return Ok(()) };
    let Some(wanted) = khora_db::target_triple().or_else(host_linux_triple) else {
        return Ok(());
    };
    if built_for == wanted {
        return Ok(());
    }
    let named = if std::env::var_os("KHORA_RT_LIB").is_some() {
        " It is the one `KHORA_RT_LIB` names; unset it, or point it at that target's archive."
    } else {
        ""
    };
    Err(format!(
        "the Khora runtime archive {} was built for `{built_for}`, and this build is for \
         `{wanted}`: a program is linked against the runtime built for its own target.{named}",
        archive.display()
    ))
}

/// The triple a build with no `KHORA_TARGET` is for, where this can say it
/// without a build script: a Linux compiler is always built for the glibc
/// triple of its architecture, which is the only Linux toolchain published.
/// Elsewhere `None`, and a host build is not checked -- the mismatch this
/// guards against is a musl archive, and only Linux has one.
fn host_linux_triple() -> Option<String> {
    cfg!(all(target_os = "linux", target_env = "gnu"))
        .then(|| format!("{}-unknown-linux-gnu", std::env::consts::ARCH))
}

/// Native libraries for a musl link, each one named by its path.
///
/// **Resolved here rather than by the linker**, because `-nostdlib` drops the
/// driver's start files and default libraries but *not* its default search
/// path: `-lz` found the host's glibc-built `/usr/lib/x86_64-linux-gnu/libz.a`
/// and linked it into a "musl" executable, `-lm` found glibc's `libm-2.41.a`
/// and failed on `_dl_x86_cpu_features`, and an aarch64 build of the same
/// manifest failed because that directory holds no arm64 files. A `.a`
/// looked up in `[build] link-search` and passed by path cannot come from
/// anywhere else, under clang, gcc, lld or bfd alike, and needs no `--sysroot`
/// that the gcc fallback would read differently. Static archives only: a
/// static executable has nothing to load a `.so` with.
///
/// The libraries musl folds into `libc.a` -- `m`, `pthread`, `dl`, `rt`,
/// `util`, `crypt`, `resolv`, `xnet` -- are dropped, since asking for them is
/// what every C build recipe does and musl satisfies them already.
fn musl_natives(triple: &str, natives: &Natives) -> Result<Vec<PathBuf>, String> {
    const IN_LIBC: &[&str] = &["c", "m", "pthread", "dl", "rt", "util", "crypt", "resolv", "xnet"];
    let mut found = Vec::new();
    for name in natives.link.iter().filter(|name| !IN_LIBC.contains(&name.as_str())) {
        let file = format!("lib{name}.a");
        match natives.search.iter().map(|dir| dir.join(&file)).find(|path| path.is_file()) {
            Some(path) => found.push(path),
            None => {
                return Err(format!(
                    "`[build] link` names `{name}`, and no `{file}` is in a `[build] link-search` \
                     directory. A `{triple}` build uses no system libraries -- the machine's own \
                     are built against its C library, not musl -- so build `{name}` for \
                     `{triple}` (a musl toolchain, or `CC=\"clang --target={triple}\"`) and put \
                     its `{file}` in a directory `link-search` names."
                ))
            }
        }
    }
    Ok(found)
}

/// The driver flags for a static musl link: everything before the inputs.
///
/// **gcc is not told the target**, because it refuses `--target=` outright
/// (`unrecognized command-line option`) and [`linker`] falls back to it on a
/// machine without clang. For the machine's own architecture it needs nothing
/// else: every input is named and `-nostdlib` keeps the host's away. For
/// another architecture gcc cannot be told the target at all, so that is
/// refused with the driver that can, rather than with gcc's message about a
/// flag the reader never wrote.
fn static_musl_flags(
    triple: &str,
    driver: &Path,
    lld: Option<&Path>,
    host_arch: &str,
) -> Result<Vec<String>, String> {
    let foreign = !triple.starts_with(host_arch);
    let clang = driver.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("clang"));
    if foreign && !clang {
        return Err(format!(
            "linking for `{triple}` from this {host_arch} machine needs clang, which can be \
             told another target; the C compiler found here is {}, which cannot. Install \
             `clang` and `lld` from your package manager.",
            driver.display()
        ));
    }
    if foreign && lld.is_none() {
        return Err(format!(
            "linking for `{triple}` from this {host_arch} machine needs `ld.lld`, which reads \
             every architecture's objects; this machine's own linker reads only its own. \
             Install `lld` from your package manager, or put LLVM's `ld.lld` on PATH."
        ));
    }
    let mut flags = Vec::new();
    if clang {
        flags.push(format!("--target={triple}"));
        if let Some(lld) = lld {
            flags.push(format!("-fuse-ld={}", lld.display()));
        }
    }
    flags.extend(["-static".to_string(), "-nostdlib".to_string()]);
    // The unwinder finds `.eh_frame` through `PT_GNU_EH_FRAME`, and gcc's
    // `-static` spec does not ask the linker for one (clang's does). Without
    // it, `KHORA_BACKTRACE=1` on a trap prints `unsupported backtrace`.
    flags.push("-Wl,--eh-frame-hdr".to_string());
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gcc is a driver [`linker`] accepts on Linux, and it refuses `--target=`
    /// outright: `unrecognized command-line option`. A machine with gcc and no
    /// clang could not build a musl program even for its own architecture,
    /// which needs nothing gcc lacks.
    #[test]
    fn a_gcc_driver_links_musl_for_its_own_architecture() {
        let flags = static_musl_flags(
            "x86_64-unknown-linux-musl",
            Path::new("/usr/bin/gcc"),
            None,
            "x86_64",
        )
        .expect("gcc links for its own architecture");
        assert!(
            flags.iter().all(|f| !f.starts_with("--target")),
            "gcc does not take --target: {flags:?}"
        );
        assert!(flags.iter().any(|f| f == "-static") && flags.iter().any(|f| f == "-nostdlib"));
        assert!(
            flags.iter().any(|f| f == "-Wl,--eh-frame-hdr"),
            "gcc's -static spec omits PT_GNU_EH_FRAME, so the link must ask: {flags:?}"
        );
    }

    /// gcc cannot be told the target, and reads `-fuse-ld=` as a linker name
    /// rather than a path, so it cannot produce another architecture's
    /// executable. The refusal has to name clang, which can.
    #[test]
    fn a_gcc_driver_refuses_another_architecture_by_naming_clang() {
        let refused = static_musl_flags(
            "aarch64-unknown-linux-musl",
            Path::new("/usr/bin/gcc"),
            Some(Path::new("/usr/bin/ld.lld")),
            "x86_64",
        )
        .expect_err("gcc cannot cross-link");
        assert!(refused.contains("clang"), "{refused}");
    }

    /// **`crtend.o` closes the link.** It holds the zero word that ends
    /// `.eh_frame`; GNU ld does not write one, and a backtrace walking a list
    /// with no end runs off it: `KHORA_BACKTRACE=1` on a trapping static
    /// program linked by bfd segfaulted inside the trap report. lld writes its
    /// own terminator, which is why the suite -- linked by lld -- did not see
    /// it. The link must not depend on which linker the machine has.
    #[test]
    fn a_static_musl_link_ends_eh_frame_whichever_linker_runs() {
        assert!(MUSL_START.contains(&"crtbegin.o"), "{MUSL_START:?}");
        assert!(MUSL_END.contains(&"crtend.o"), "{MUSL_END:?}");
        assert_eq!(MUSL_END.last(), Some(&"crtn.o"), "crtn.o is last: {MUSL_END:?}");
    }

    /// clang is told the triple, and given lld by path.
    #[test]
    fn a_clang_driver_is_told_the_target_and_the_linker() {
        let flags = static_musl_flags(
            "aarch64-unknown-linux-musl",
            Path::new("/opt/llvm/bin/clang"),
            Some(Path::new("/opt/llvm/bin/ld.lld")),
            "x86_64",
        )
        .expect("clang cross-links with lld");
        assert!(flags.contains(&"--target=aarch64-unknown-linux-musl".to_string()), "{flags:?}");
        assert!(flags.contains(&"-fuse-ld=/opt/llvm/bin/ld.lld".to_string()), "{flags:?}");
    }
}
