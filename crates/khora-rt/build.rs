//! Compiles the setjmp shim, where there is a setjmp to compile.
//!
//! `csrc/guard.c` exists because Rust has no portable `setjmp`, and because
//! the frame that owns a `jmp_buf` has to be the frame that calls the guarded
//! body — see the comment at the top of that file.
//!
//! `cc` is the only build dependency this adds, and it needs a C compiler,
//! which `docs/llvm-setup.md` already requires: a Khora checkout builds with a
//! Rust toolchain and clang.

fn main() {
    println!("cargo:rerun-if-changed=csrc/guard.c");

    // **A mark naming the target this archive was built for**, read back by
    // `khora-codegen-llvm`'s `toolchain::archive_target` from the archive's
    // symbol table. Without it a runtime for the wrong C library is found out
    // only by the linker, if at all: a musl archive linked silently into a
    // glibc program, and a glibc one in a musl build failed on `fstat64`. The
    // triple's hyphens become `__`, because `x86_64` already holds a `_`.
    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    let mark = target.replace(['-', '.'], "__");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(
        out.join("built_for.rs"),
        format!(
            "/// The target this archive was built for; see `build.rs`.\n\
             #[no_mangle]\n#[used]\n#[allow(non_upper_case_globals)]\n\
             pub static khora_rt_built_for_{mark}: u8 = 0;\n"
        ),
    )
    .expect("writing built_for.rs");

    // **Not on WebAssembly, which has no `setjmp` to bind.** A non-local exit
    // needs either the exception-handling proposal or Emscripten's emulation,
    // and `wasm32-unknown-unknown` has neither.
    //
    // Nothing is lost. Trap containment exists because a library inside
    // somebody else's process has no supervisor to restart it
    // (`docs/design/traps.md` §6), and a Worker is the one place where that is
    // false by construction: the isolate *is* the boundary and the platform
    // restarts it. This is §4's third answer working, rather than a gap.
    if std::env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|f| f.split(',').any(|f| f == "wasm")) {
        return;
    }

    cc::Build::new().file("csrc/guard.c").compile("khora_guard");
}
