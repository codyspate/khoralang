//! What every test that *runs* a compiled program needs first.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::SystemTime;

/// A command that runs `exe` under a stack limit of `kb` kilobytes.
///
/// **Through `sh`, never with `setrlimit` in `pre_exec`.** Lowering
/// `RLIMIT_STACK` from a `pre_exec` hook worked on Linux and failed on macOS
/// with `EINVAL` before the program ran: the child of a fork is a copy of the
/// thread that forked, which under the test harness is not the main thread,
/// and macOS refuses to lower the stack limit from anywhere but the main
/// thread's own stack (xnu `kern_resource.c`, "not on the main stack:
/// reject"). A shell's main thread sets it with `ulimit -s`, then `exec`s the
/// program in the same process, so the child's pid and pipes are the
/// program's. Callers add the program's arguments as usual.
///
/// `allow(dead_code)` for the reason [`python`] gives: only `suite` calls it.
#[cfg(unix)]
#[allow(dead_code)]
pub fn with_stack_limit(exe: &Path, kb: u64) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("ulimit -s {kb} && exec \"$0\" \"$@\""))
        .arg(exe);
    command
}

/// The Python interpreter's name on this machine.
///
/// **`python` is not a command on a current Debian or Ubuntu.** The name was
/// freed when Python 2 was removed and comes back only if somebody installs
/// `python-is-python3`, which a clean container does not have. Two TLS tests
/// drove their client with `Command::new("python")`, so on Linux they failed
/// at `.expect("python should run")` -- and the message that reached the log
/// was `the python client failed:`, which reads like the Khora server refused
/// a handshake rather than like a missing interpreter.
///
/// Looked up once and remembered: this runs per test, and the tests that use
/// it are the ones already paying for a TLS handshake.
/// **`#[allow(dead_code)]` because this module has four homes.** `suite`,
/// `debugging`, `portability` and `targets` each declare `mod harness;`, so
/// every one of them compiles this file, and only `suite` drives a Python
/// client. Without the allow, `clippy -D warnings` fails on the three that do
/// not -- which the gate runs and `cargo test` does not.
#[allow(dead_code)]
pub fn python() -> &'static str {
    static FOUND: OnceLock<&'static str> = OnceLock::new();
    FOUND.get_or_init(|| {
        for name in ["python3", "python"] {
            let ran = Command::new(name).arg("--version").output();
            if ran.is_ok_and(|out| out.status.success()) {
                return name;
            }
        }
        panic!("neither `python3` nor `python` is on PATH; the TLS tests drive their client with it")
    })
}

/// The `std` file that tells `std::permissions` whether `\` separates path
/// segments, for the host: its name and its text.
///
/// **What it prevents: a hand-listed `std` that stops compiling.** A test
/// that compiles `permissions.kh` from a list rather than by walking `std`
/// has to bring this module too, and the right one of the three -- picking
/// it by the same rule the build uses, rather than naming one, keeps the
/// list right on every runner.
#[allow(dead_code)]
pub fn separator_source() -> (&'static str, String) {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    for name in ["separator_posix.kh", "separator_windows.kh", "separator_wasm.kh"] {
        let path = std.join(name);
        if khora_db::selected_for_target(&path, khora_db::host_target()) {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            return (name, text);
        }
    }
    panic!("no `std/separator_*.kh` is selected for `{}`", khora_db::host_target())
}

/// The cargo target directory this test's artifacts were built into: where
/// `khora` and every `libkhora_rt.a` cargo wrote are.
///
/// **A stopgap, until cargo tells a test where its workspace's artifacts
/// are.** This walked up from the test binary, assuming it sat in
/// `<target>/<profile>/deps/`. A separate build directory moves test
/// binaries and leaves artifacts where they were: with
/// `build.build-dir = "b"`, the binary is in `b/debug/deps/` and
/// `CARGO_TARGET_TMPDIR` is `b/tmp`, while `khora` is still in
/// `<target>/debug/` (measured on 1.99). Rust 1.100 moves test binaries by
/// default. So no single path is right in both, and this tries, in order:
/// `CARGO_TARGET_DIR` as the run was given it, `CARGO_TARGET_TMPDIR`'s parent
/// (the target directory when there is no separate build directory), and the
/// workspace's own `target/`. The first that holds a `khora` or a
/// `libkhora_rt.a` wins. It costs a wrong answer only when two of those each
/// hold one and the first is stale, which a test cannot tell from here.
#[allow(dead_code)]
pub fn target_dir() -> Option<std::path::PathBuf> {
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let khora = format!("khora{}", std::env::consts::EXE_SUFFIX);
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let candidates = [
        std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from),
        Path::new(env!("CARGO_TARGET_TMPDIR")).parent().map(Path::to_path_buf),
        Some(workspace.join("target")),
    ];
    let runtime = if cfg!(windows) { "khora_rt.lib" } else { "libkhora_rt.a" };
    candidates.into_iter().flatten().find(|dir| {
        let built = dir.join(profile);
        built.join(&khora).is_file() || built.join(runtime).is_file()
    })
}

/// The `khora` executable cargo built for this run.
///
/// **What it prevents: a test that cannot find the compiler and passes.**
/// The cross-process checks in `profiles` and `reproducible` asked for
/// `CARGO_BIN_EXE_khora`, which cargo sets only in the tests of the crate that
/// owns the binary -- `khora-cli`, not this one. So all three returned early,
/// counted as passed in a dozen milliseconds, in every run there had ever
/// been. A missing binary is a failure here, naming the command that builds
/// it. Found through [`target_dir`].
///
/// A workspace run builds it; `-p khora-codegen-llvm` alone does not, and
/// then the binary is whatever the last build left. Nothing here can tell a
/// stale one from a fresh one without rebuilding it in the middle of a run,
/// which other tests that hash it forbid.
#[allow(dead_code)]
pub fn khora() -> std::path::PathBuf {
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let Some(target) = target_dir() else {
        panic!(
            "no `khora` in any target directory this test could find (`CARGO_TARGET_DIR`, \
             `CARGO_TARGET_TMPDIR`'s parent, the workspace's `target/`); build it with \
             `cargo build -p khora-cli --features llvm` and run again"
        )
    };
    target.join(profile).join(format!("khora{}", std::env::consts::EXE_SUFFIX))
}

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
        // The runtime archive is in the target directory, which the test
        // binary is not beside under a separate build directory; see
        // `set_test_target_dir` for why this is not `KHORA_RT_LIB`.
        if let Some(target) = target_dir() {
            khora_codegen_llvm::toolchain::set_test_target_dir(target);
        }
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
