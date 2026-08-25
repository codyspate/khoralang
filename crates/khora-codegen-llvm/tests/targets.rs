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

// --- and a module that actually links -------------------------------------

/// The wasm runtime archive, if somebody has built one.
///
/// Not built here: `cargo build -p khora-rt --target wasm32-unknown-unknown`
/// downloads a target's standard library the first time and takes minutes, and
/// a test that does that on a cold machine is a test nobody runs. Skipped with
/// a message instead, which is the honest shape for a check whose input is a
/// build artefact.
fn wasm_runtime() -> Option<PathBuf> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path = workspace
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join("libkhora_rt.a");
    path.is_file().then_some(path)
}

/// **A `.wasm` module, linked, with the exports a host would call.**
///
/// `targets.md` step three for the one target that needs no sysroot: a wasm
/// module has no libc to find and no CRT to link, so the runtime archive and
/// the emitted object are the whole input and `wasm-ld` is the whole linker.
///
/// What this asserts is the export section, because that is where the two
/// mistakes were. `--export-dynamic` published 1,255 symbols from a
/// three-function library and defeated dead-code elimination — 2.5 MB where
/// naming them gives 1.9 MB and would give far less without the trap
/// machinery. And `--allow-undefined` silently turned `khora_overflow`,
/// `khora_contain_enabled` and `khora_export_call` into `env.` imports the
/// embedder would have had to supply, *while they were defined in the archive
/// on the same command line*: the module linked, validated, exported exactly
/// the right names, and could not have been instantiated.
#[test]
fn a_wasm_library_links_and_exports_what_it_should() {
    let Some(_archive) = wasm_runtime() else {
        eprintln!(
            "no wasm khora-rt; skipping. Build it with \
             `cargo build -p khora-rt --target wasm32-unknown-unknown`"
        );
        return;
    };
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm_library");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let out = dir.join("lib.wasm");

    let db = KhoraDatabase::new();
    let source = "module w;\n\
                  export extern fn price(units: Int, scale: Int) -> Int { units * scale }\n";
    let files = vec![SourceFile::new(&db, dir.join("lib.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: process-wide, and held by the lock above.
    unsafe { std::env::set_var("KHORA_TARGET", "wasm32-unknown-unknown") };
    let outcome = khora_codegen_llvm::compile_library(&db, root, &out);
    unsafe { std::env::remove_var("KHORA_TARGET") };

    if let Err(errors) = outcome {
        let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
        panic!("a wasm library should link:\n{said}");
    }

    let bytes = std::fs::read(&out).expect("the module was written");
    assert_eq!(&bytes[..4], b"\0asm", "it is a wasm module");

    let (exports, imports) = sections_of(&bytes);
    assert!(exports.contains(&"price".to_string()), "the export is there: {exports:?}");
    assert!(
        exports.contains(&"khora_set_trap_policy".to_string()),
        "and the control surface: {exports:?}"
    );
    // Nothing else. A Worker is measured in megabytes and this is a
    // three-function library.
    assert!(exports.len() < 20, "only what was asked for, got {}: {exports:?}", exports.len());
    assert!(
        !exports.iter().any(|e| e.starts_with("kh$")),
        "no mangled internals: {exports:?}"
    );

    // **No imports at all**, which is the whole of the `--allow-undefined`
    // lesson: everything this module needs, it carries.
    assert!(imports.is_empty(), "a self-contained module imports nothing, got {imports:?}");
}

/// The export and import names in a wasm module.
fn sections_of(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    fn uleb(b: &[u8], i: &mut usize) -> u64 {
        let (mut r, mut s) = (0u64, 0u32);
        loop {
            let x = b[*i];
            *i += 1;
            r |= u64::from(x & 0x7f) << s;
            s += 7;
            if x & 0x80 == 0 {
                return r;
            }
        }
    }
    fn name(b: &[u8], i: &mut usize) -> String {
        let len = uleb(b, i) as usize;
        let s = String::from_utf8_lossy(&b[*i..*i + len]).into_owned();
        *i += len;
        s
    }

    let (mut exports, mut imports) = (Vec::new(), Vec::new());
    let mut i = 8;
    while i < bytes.len() {
        let id = bytes[i];
        i += 1;
        let size = uleb(bytes, &mut i) as usize;
        let end = i + size;
        let mut j = i;
        if id == 7 {
            let n = uleb(bytes, &mut j);
            for _ in 0..n {
                exports.push(name(bytes, &mut j));
                j += 1;
                uleb(bytes, &mut j);
            }
        } else if id == 2 {
            let n = uleb(bytes, &mut j);
            for _ in 0..n {
                let module = name(bytes, &mut j);
                let what = name(bytes, &mut j);
                imports.push(format!("{module}.{what}"));
                let kind = bytes[j];
                j += 1;
                match kind {
                    0 => {
                        uleb(bytes, &mut j);
                    }
                    1 => {
                        j += 1;
                        let limits = uleb(bytes, &mut j);
                        uleb(bytes, &mut j);
                        if limits == 1 {
                            uleb(bytes, &mut j);
                        }
                    }
                    2 => {
                        let limits = uleb(bytes, &mut j);
                        uleb(bytes, &mut j);
                        if limits == 1 {
                            uleb(bytes, &mut j);
                        }
                    }
                    _ => j += 2,
                }
            }
        }
        i = end;
    }
    (exports, imports)
}
