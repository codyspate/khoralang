#![cfg(feature = "llvm")]

//! Code generation for a machine that is not this one.
//!
//! `docs/design/targets.md` opens by saying Khora compiles for the machine it
//! is running on and nothing else, because `target_machine` initialized only
//! the native target and there was nowhere for a `--target` to point. These
//! check that there is now: an object emitted for a named triple, and the
//! bytes of that object saying which machine it is for.
//!
//! **Linking is not checked and cannot be**, which is the honest limit and the
//! same one `portability.rs` states. An object for `aarch64-unknown-linux-gnu`
//! needs a linker and a sysroot for that target, and this build fetches
//! neither yet — `targets.md` steps three and four. What is proven here is
//! step one, which is the part that had nowhere to point.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// **One lock for the whole file, and it has to be.** `KHORA_TARGET` is
/// process-wide and cargo runs a binary's tests on several threads. Two locks
/// were worse than none: each test took its own and none excluded the others,
/// so `--test-threads=1` passed and the baseline did not.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the first bytes of an object file say it is for.
fn machine_of(bytes: &[u8]) -> String {
    if bytes.starts_with(b"\x7fELF") {
        let machine = bytes[18];
        let name = match machine {
            0x3e => "x86-64".to_string(),
            0xb7 => "aarch64".to_string(),
            other => format!("machine {other:#x}"),
        };
        format!("ELF {name}")
    } else if bytes.starts_with(b"\0asm") {
        format!("WebAssembly v{}", bytes[4])
    } else if bytes.len() > 2 && bytes[0] == 0x64 && bytes[1] == 0x86 {
        "COFF x86-64".to_string()
    } else {
        format!("unrecognised {:02x?}", &bytes[..4.min(bytes.len())])
    }
}

/// Generates for `triple` and reports what the object turned out to be.
///
/// `KHORA_TARGET` is process-wide, so these run one at a time behind a lock —
/// the same reasoning `portability.rs` gives for the same variable.
fn emitted_for(triple: &str) -> String {
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("program.exe");
    let object = dir.join("program.exe.o");
    let _ = std::fs::remove_file(&object);

    let source = "module demo::main;\nfn main() -> () {}\n";
    let db = KhoraDatabase::new();
    let files = vec![SourceFile::new(&db, dir.join("main.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: the lock above is what makes this single-threaded.
    unsafe { std::env::set_var("KHORA_TARGET", triple) };
    // Linking is expected to fail for a foreign target and is not the claim;
    // the object beside it is.
    let _ = khora_codegen_llvm::compile(&db, root, &exe);
    unsafe { std::env::remove_var("KHORA_TARGET") };

    let bytes = std::fs::read(&object)
        .unwrap_or_else(|e| panic!("no object was emitted for {triple}: {e}"));
    machine_of(&bytes)
}

/// **The one this was all for.** A Worker takes a wasm module, and here is one,
/// emitted from a Windows host.
#[test]
fn webassembly_is_emitted() {
    assert_eq!(emitted_for("wasm32-unknown-unknown"), "WebAssembly v1");
}

/// arm64 containers and Graviton, from whatever the developer is sitting at.
#[test]
fn aarch64_linux_is_emitted() {
    assert_eq!(emitted_for("aarch64-unknown-linux-gnu"), "ELF aarch64");
}

/// The ordinary server target, cross-generated rather than native.
#[test]
fn x86_64_linux_is_emitted() {
    assert_eq!(emitted_for("x86_64-unknown-linux-gnu"), "ELF x86-64");
}

/// A triple no backend was built for is refused by name, rather than
/// producing something that cannot be run.
#[test]
fn an_unknown_triple_is_refused() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets_bad");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let db = KhoraDatabase::new();
    let source = "module demo::main;\nfn main() -> () {}\n";
    let files = vec![SourceFile::new(&db, dir.join("main.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: as above.
    unsafe { std::env::set_var("KHORA_TARGET", "sparc64-unknown-linux-gnu") };
    let outcome = khora_codegen_llvm::compile(&db, root, &dir.join("program.exe"));
    unsafe { std::env::remove_var("KHORA_TARGET") };

    let errors = outcome.expect_err("a target with no backend should be refused");
    let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
    assert!(
        said.contains("sparc64") && said.contains("inkwell"),
        "the refusal should name the triple and where the target list comes from: {said}"
    );
}
