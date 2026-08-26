#![cfg(feature = "llvm")]

//! Generates code for every target, from whichever host this runs on.
//!
//! `khora-types/tests/portability.rs` type-checks `std` for all three targets
//! and that was not enough. Which files a build selects is a per-target
//! decision, so a bug can live in a *combination* of modules that only one
//! platform ever compiles together, and be invisible to the type checker
//! because each module is fine on its own.
//!
//! That is exactly what happened. `std/fs_native.kh` declares `fn close(file:
//! Ptr)`. `socket_linux.kh` and `socket_macos.kh` declare `extern fn close(handle:
//! I32)` — POSIX's. `socket_windows.kh` calls the same operation
//! `closesocket`, so on Windows the two names never met, and every POSIX build
//! emitted `call void @kh$std$fs$close(i32 %handle)` and was rejected by LLVM.
//! It survived until CI ran the backend on a Mac for the first time, and could
//! not then be reproduced by anyone working on Windows.
//!
//! **The limit is honest.** This stops after verification, which is the last
//! step that means the same thing on every host. Writing an object needs a
//! machine that can encode for the target and linking needs that target's
//! libraries, so an undefined symbol or a wrong calling convention still needs
//! the real platform. CI builds and runs on all three; this catches the class
//! of bug that only shows up when the wrong modules are in a build together,
//! and it catches it in under a second on a laptop.

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The program is deliberately dull. What is under test is `std` — every module
/// the target selects, compiled together — not this.
const MAIN: &str = "module demo::main;

fn print(value: String);

pub fn main() -> () {
  print(\"ok\");
}
";

fn std_sources(db: &KhoraDatabase, target: &str) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, target)
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out
}

/// One test, not three, and it must stay that way.
///
/// `KHORA_TARGET` is read from the process environment, and Rust runs the tests
/// in one binary on several threads. Three tests setting it would race. Each
/// `tests/*.rs` is its own binary, so being the only test in this one is what
/// makes writing the variable safe.
#[test]
fn every_target_generates_a_valid_module() {
    let mut failures: Vec<String> = Vec::new();

    // **WebAssembly is in this list and is not like the others.** The other
    // three select an operating system's `std`; `wasm` selects the subset that
    // does not call one — no sockets, no filesystem, no process, no `getenv`.
    // What this proves is that the subset is *coherent*: that removing those
    // modules leaves the rest with no dangling import, which is exactly what
    // it did not before `_native` existed, and what nobody would have noticed
    // until a Worker build was attempted.
    for target in ["windows", "linux", "macos", "wasm"] {
        // Safe here for the reason in this function's doc comment, and reset
        // before the next iteration reads it.
        std::env::set_var("KHORA_TARGET", target);
        assert_eq!(khora_db::host_target(), target, "the override should take effect");

        let db = KhoraDatabase::new();
        let mut files = std_sources(&db, target);
        files.push(SourceFile::new(
            &db,
            PathBuf::from(format!("main_{target}.kh")),
            MAIN.to_string(),
        ));
        let root = SourceRoot::new(&db, files);

        if let Err(errors) = khora_codegen_llvm::verify_for_target(&db, root) {
            let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
            failures.push(format!("{target}:\n  {}", messages.join("\n  ")));
        }
    }

    std::env::remove_var("KHORA_TARGET");

    assert!(
        failures.is_empty(),
        "`std` does not generate a valid module for every target:\n\n{}",
        failures.join("\n\n")
    );
}
