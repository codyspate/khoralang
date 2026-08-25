//! Compiles the setjmp shim.
//!
//! `csrc/guard.c` exists because Rust has no portable `setjmp`, and because
//! the frame that owns a `jmp_buf` has to be the frame that calls the guarded
//! body — see the comment at the top of that file. Twelve lines of C is the
//! smallest honest answer.
//!
//! `cc` is the only build dependency this adds, and it needs a C compiler,
//! which `docs/llvm-setup.md` already requires: a Khora checkout builds with a
//! Rust toolchain and clang.

fn main() {
    println!("cargo:rerun-if-changed=csrc/guard.c");
    cc::Build::new().file("csrc/guard.c").compile("khora_guard");
}
