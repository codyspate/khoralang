//! Proves the backend toolchain works on this host: emit an object with LLVM,
//! link it, run it, check the exit code.
//!
//! Requires `--features llvm` and a configured LLVM 22.1 prefix; without them
//! the whole file compiles to nothing so the default `cargo test` stays green.
#![cfg(feature = "llvm")]

#[test]
fn emits_links_and_runs_a_native_executable() {
    // `CARGO_TARGET_TMPDIR` rather than `/tmp`: one shared path belongs to
    // whoever made it first, and a second user gets a permission error that
    // reads like a code generator bug. `compile.rs` has the account.
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("spike");
    std::fs::create_dir_all(&dir).expect("creating temp dir");
    let obj = dir.join("spike.o");
    let exe = dir.join(if cfg!(windows) { "spike.exe" } else { "spike" });
    let _ = std::fs::remove_file(&exe);

    khora_codegen_llvm::spike::emit_constant_main(&obj, 42).expect("emitting object");
    assert!(obj.is_file(), "no object file produced");

    khora_codegen_llvm::toolchain::link_executable(&[&obj], &exe).expect("linking");
    assert!(exe.is_file(), "no executable produced");

    let status = std::process::Command::new(&exe).status().expect("running executable");
    assert_eq!(status.code(), Some(42), "wrong exit code from generated binary");
}

#[test]
fn toolchain_is_discoverable() {
    assert!(
        khora_codegen_llvm::toolchain::tool("clang").is_some(),
        "clang not found under LLVM_SYS_221_PREFIX; see docs/llvm-setup.md"
    );
}
