#![cfg(feature = "llvm")]

//! `khora build --target`: a program for a triple other than the host's.
//!
//! The library-level tests in `khora-codegen-llvm/tests/targets.rs` drive
//! `KHORA_TARGET` directly. These drive the flag through the real binary,
//! because the flag is what a person types and its whole job is to reach the
//! same place the variable does.

use std::path::PathBuf;
use std::process::Command;

/// A package whose program prints one line.
fn project() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");
    std::fs::write(
        project.join("khora.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{}\"\n",
            khora_toolchain::RUNNING,
        ),
    )
    .expect("a manifest");
    std::fs::write(
        project.join("src").join("main.kh"),
        "module main;\nimport std::core::{print};\nfn main() -> Int { print(\"targeted\"); 0 }\n",
    )
    .expect("a program");
    (tmp, project)
}

fn khora(project: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(project)
        .env("KHORA_HOME", project.join("home"))
        .env_remove("KHORA_TARGET")
        .env_remove("KHORA_RT_LIB")
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// **`--target` with a family name is refused, rather than meaning "here".**
///
/// What this prevents: `--target linux` building for the host. `KHORA_TARGET`
/// accepts `linux`, `macos` and `windows` because it began as a `std`-file
/// selector, and those keep the host's architecture; a flag spelled `--target`
/// that quietly did the same would say it had done something it had not.
#[test]
fn a_target_must_be_a_triple() {
    let (_tmp, project) = project();
    let (ok, out) = khora(&project, &["build", ".", "--target", "linux"]);
    assert!(!ok, "a family name is not a target: {out}");
    assert!(out.contains("not a target triple"), "the refusal should say why: {out}");
}

/// **`--target x86_64-unknown-linux-musl` builds a static executable.**
///
/// What this prevents: a flag that is accepted and ignored -- the program
/// would build and run, for the host, dynamically linked, and only the
/// program headers say otherwise. So they are what is checked. x86-64 Linux
/// only: the program is run, and the musl runtime it needs is built for this
/// architecture by `scripts/build-musl-runtime.sh`, which CI runs first; a
/// machine without it skips, and CI does not.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_musl_target_builds_a_static_program() {
    let triple = "x86_64-unknown-linux-musl";
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_khora"));
    let archive = exe.parent().and_then(|d| d.parent()).map(|t| t.join(triple).join("debug").join("libkhora_rt.a"));
    if !archive.is_some_and(|a| a.is_file()) {
        assert!(std::env::var_os("CI").is_none(), "no {triple} khora-rt; CI must build it, not skip");
        eprintln!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`; skipping");
        return;
    }

    let (_tmp, project) = project();
    let (ok, out) = khora(&project, &["build", ".", "--target", triple]);
    assert!(ok, "{out}");
    let built = project.join("build").join("app");
    let bytes = std::fs::read(&built).expect("the executable");

    // PT_INTERP (3) and PT_DYNAMIC (2) are what a dynamic executable has.
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let phoff = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
    let kinds: Vec<u32> = (0..u16_at(56))
        .map(|i| {
            let at = phoff + i * u16_at(54);
            u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
        })
        .collect();
    assert!(!kinds.contains(&3) && !kinds.contains(&2), "a static executable: {kinds:?}");

    let ran = Command::new(&built).env_clear().output().expect("running it");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "targeted\n");
}

// --- native libraries in a musl build -----------------------------------

/// Whether the x86-64 musl runtime is built, failing rather than skipping in CI.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn musl_runtime_ready() -> bool {
    let triple = "x86_64-unknown-linux-musl";
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_khora"));
    let archive = exe
        .parent()
        .and_then(|d| d.parent())
        .map(|t| t.join(triple).join("debug").join("libkhora_rt.a"));
    if archive.is_some_and(|a| a.is_file()) {
        return true;
    }
    assert!(std::env::var_os("CI").is_none(), "no {triple} khora-rt; CI must build it, not skip");
    eprintln!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`; skipping");
    false
}

/// A package that calls `extern fn doubled`, with `[build]` lines appended to
/// its manifest.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn native_project(build: &str, source: &str) -> (tempfile::TempDir, PathBuf) {
    let (tmp, project) = project();
    std::fs::write(
        project.join("khora.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{}\"\n\n\
             [permissions]\nextern = [\"app\"]\n\n[build]\n{build}",
            khora_toolchain::RUNNING,
        ),
    )
    .expect("a manifest");
    std::fs::write(project.join("src").join("main.kh"), source).expect("a program");
    (tmp, project)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const DOUBLED: &str = "module main;\nimport std::core::{print};\nextern fn doubled(n: Int) -> Int;\n\
                       fn main() -> Int { print(\"${doubled(21)}\"); 0 }\n";

/// **A musl build does not take a native library from the host.**
///
/// What this prevents: `link = ["z"]` resolving to the machine's glibc-built
/// `/usr/lib/x86_64-linux-gnu/libz.a`, which the driver's default search path
/// finds even under `-nostdlib`. The program linked and ran, a "static musl"
/// executable carrying a library compiled against glibc's headers, and the
/// same manifest failed for aarch64, which has no such directory. Refused, with
/// the way out.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_musl_build_takes_no_native_library_from_the_host() {
    if !musl_runtime_ready() {
        return;
    }
    let (_tmp, project) = native_project("link = [\"z\"]\n", DOUBLED);
    let (ok, out) = khora(&project, &["build", ".", "--target", "x86_64-unknown-linux-musl"]);
    assert!(!ok, "`z` is in no `link-search` directory, so nothing may satisfy it: {out}");
    assert!(
        out.contains("libz.a") && out.contains("link-search") && out.contains("system"),
        "the refusal should say musl builds use no system libraries, and where one goes: {out}"
    );
}

/// **A library in `link-search` is the one linked.**
///
/// Named `libz.a` on purpose: the host has one too, and the program prints 42
/// only if this directory's was taken. Not a guard against the host leak --
/// `-L vendor` ahead of the default path finds this one either way -- but
/// against the fix refusing what it should accept.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_musl_build_links_the_library_in_link_search() {
    if !musl_runtime_ready() {
        return;
    }
    let (_tmp, project) =
        native_project("link = [\"z\"]\nlink-search = [\"vendor\"]\n", DOUBLED);
    let vendor = project.join("vendor");
    std::fs::create_dir_all(&vendor).expect("a vendor directory");
    let c = vendor.join("doubled.c");
    std::fs::write(&c, "long doubled(long n) { return n * 2; }\n").expect("the C source");
    let object = vendor.join("doubled.o");
    let clang = khora_codegen_llvm::toolchain::linker().expect("a C compiler");
    let compiled = Command::new(&clang).arg("-c").arg(&c).arg("-o").arg(&object).output();
    assert!(compiled.is_ok_and(|o| o.status.success()), "compiling the fixture");
    let ar = khora_codegen_llvm::toolchain::tool("llvm-ar").unwrap_or_else(|| PathBuf::from("ar"));
    let archived = Command::new(&ar).arg("rcs").arg(vendor.join("libz.a")).arg(&object).output();
    assert!(archived.is_ok_and(|o| o.status.success()), "archiving the fixture");

    let (ok, out) = khora(&project, &["build", ".", "--target", "x86_64-unknown-linux-musl"]);
    assert!(ok, "{out}");
    let ran = Command::new(project.join("build").join("app")).env_clear().output().expect("running it");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "42\n");
}

/// **`link = ["m"]` builds for musl**, where the maths library is inside
/// `libc.a`.
///
/// What this prevents: `-lm` reaching glibc's `libm-2.41.a` through the host
/// search path and failing on `_dl_x86_cpu_features`, a glibc internal. Every
/// C build recipe adds `m`, `pthread` or `dl`, so this is the common case.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_musl_build_finds_libm_in_libc() {
    if !musl_runtime_ready() {
        return;
    }
    let (_tmp, project) = native_project(
        "link = [\"m\", \"pthread\", \"dl\"]\n",
        "module main;\nimport std::core::{print};\nextern fn cbrt(x: Float) -> Float;\n\
         extern fn sin(x: Float) -> Float;\nextern fn exp(x: Float) -> Float;\n\
         fn main() -> Int { print(\"${cbrt(27.0)} ${sin(0.0)} ${exp(0.0)}\"); 0 }\n",
    );
    let (ok, out) = khora(&project, &["build", ".", "--target", "x86_64-unknown-linux-musl"]);
    assert!(ok, "{out}");
    let ran = Command::new(project.join("build").join("app")).env_clear().output().expect("running it");
    assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "3 0 1", "{}", String::from_utf8_lossy(&ran.stderr));
}

// --- a runtime archive for the wrong C library ---------------------------

/// `khora` with `KHORA_RT_LIB` set, which [`khora`] removes.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn khora_with_runtime(project: &std::path::Path, archive: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(project)
        .env("KHORA_HOME", project.join("home"))
        .env_remove("KHORA_TARGET")
        .env("KHORA_RT_LIB", archive)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// **`KHORA_RT_LIB` naming another target's runtime is refused by name.**
///
/// What this prevents, in both directions: a musl archive linked silently
/// into a glibc build (it ran, a glibc program carrying a musl-built Rust
/// standard library), and a glibc archive in a musl build failing on
/// `undefined symbol: fstat64`, which says nothing about which file is wrong.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_runtime_archive_for_another_target_is_refused() {
    if !musl_runtime_ready() {
        return;
    }
    let target = PathBuf::from(env!("CARGO_BIN_EXE_khora"));
    let target = target.parent().and_then(|d| d.parent()).expect("the target directory");
    let musl = target.join("x86_64-unknown-linux-musl").join("debug").join("libkhora_rt.a");
    let glibc = PathBuf::from(env!("CARGO_BIN_EXE_khora")).with_file_name("libkhora_rt.a");

    let (_tmp, project) = project();
    let (ok, out) = khora_with_runtime(&project, &musl, &["build", ".", "--no-cache"]);
    assert!(!ok, "a musl runtime in a glibc build: {out}");
    assert!(
        out.contains("x86_64-unknown-linux-musl") && out.contains("KHORA_RT_LIB"),
        "the refusal should name the archive's target and the variable: {out}"
    );

    let (ok, out) = khora_with_runtime(
        &project,
        &glibc,
        &["build", ".", "--no-cache", "--target", "x86_64-unknown-linux-musl"],
    );
    assert!(!ok, "a glibc runtime in a musl build: {out}");
    assert!(
        out.contains("x86_64-unknown-linux-gnu") && out.contains("KHORA_RT_LIB"),
        "the refusal should name the archive's target and the variable: {out}"
    );
}
