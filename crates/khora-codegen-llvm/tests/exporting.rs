#![cfg(feature = "llvm")]

//! `export extern fn` — a Khora library that C can call.
//!
//! `docs/design/c-export.md`. The marker is two words the language already
//! has, and a body is what tells the directions apart: `extern fn` without one
//! is a symbol to find at link time, with one is a symbol to publish.
//!
//! **The last test compiles a C program and runs it**, which is the only thing
//! that proves this works. Every cheaper check passed at a point where it did
//! not: the object had a `price` symbol, the DLL was written, the import
//! library was written, the header was correct — and `lld-link` told the first
//! C caller `undefined symbol: price`, because Windows publishes only what
//! carries `dllexport`. A library whose every artifact is present and none of
//! whose symbols are reachable is exactly the failure this file exists to catch.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    dir
}

/// Compiles `source` as a shared library, returning the library and header.
fn library(name: &str, source: &str) -> Result<(PathBuf, String), Vec<String>> {
    let dir = scratch(name);
    harness::ensure_runtime();
    let out = dir.join(format!("lib{name}.{}", std::env::consts::DLL_EXTENSION));

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::compile_library(&db, root, &out)
        .map_err(|errors| errors.into_iter().map(|e| e.message).collect::<Vec<_>>())?;

    let header = std::fs::read_to_string(out.with_extension("h")).expect("a header was written");
    Ok((out, header))
}

const PRICING: &str = "module t;

export extern fn price(units: Int, scale: Int) -> Int {
  units * scale
}

export extern fn over_budget(total: Int, budget: Int) -> Bool {
  total > budget
}

export extern fn tick() -> () {
  ()
}
";

#[test]
fn a_library_builds_and_declares_what_it_exports() {
    let (out, header) = library("export_basic", PRICING).expect("it should build");
    assert!(out.is_file(), "the library was written");
    assert!(header.contains("int64_t price(int64_t a0, int64_t a1);"), "{header}");
    assert!(header.contains("bool over_budget(int64_t a0, int64_t a1);"), "{header}");
    // `void`, not an empty list: `tick()` in C takes unspecified arguments.
    assert!(header.contains("void tick(void);"), "{header}");
}

/// **The test that matters.** A C program, compiled against the generated
/// header, linked against the library, run.
#[test]
fn a_c_program_can_call_into_khora() {
    let Some(clang) = khora_codegen_llvm::toolchain::tool("clang") else {
        eprintln!("no clang in the LLVM toolchain; skipping the C caller");
        return;
    };
    let (out, _) = library("export_from_c", PRICING).expect("it should build");
    let dir = out.parent().expect("a directory").to_path_buf();

    // The header is named after the *library*, not the source file: it is
    // written beside the artifact it describes.
    let header_name = out
        .with_extension("h")
        .file_name()
        .expect("the header has a name")
        .to_string_lossy()
        .into_owned();

    // Concatenated rather than `format!`ed: C is made of braces, and escaping
    // every one of them to get a single interpolation is the worse trade.
    const BODY: &str = "int main(void) {\n\
         \x20   printf(\"%lld\\n\", (long long) price(3, 4));\n\
         \x20   printf(\"%s\\n\", over_budget(10, 5) ? \"over\" : \"under\");\n\
         \x20   tick();\n\
         \x20   return 0;\n\
         }\n";
    let host_c = dir.join("host.c");
    std::fs::write(
        &host_c,
        String::from("#include <stdio.h>\n#include \"") + &header_name + "\"\n" + BODY,
    )
    .expect("writing the C host");

    // Windows links against the import library beside the DLL; the others link
    // the shared object itself.
    let link_against = {
        let implib = out.with_extension("lib");
        if implib.is_file() { implib } else { out.clone() }
    };
    let host = dir.join(format!("host{}", std::env::consts::EXE_SUFFIX));
    let compiled = Command::new(&clang)
        .arg(&host_c)
        .arg(&link_against)
        .arg("-o")
        .arg(&host)
        .current_dir(&dir)
        .output()
        .expect("could not run clang");
    assert!(
        compiled.status.success(),
        "the C host did not link:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    // Run from beside the library, so the loader finds it without an install.
    let ran = Command::new(&host).current_dir(&dir).output().expect("could not run the host");
    assert!(ran.status.success(), "the host exited with {:?}", ran.status.code());
    let stdout = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "12\nover\n", "C called Khora and got Khora's answers");
}

/// The header is generated from the checked signatures, so the C declaration
/// and the Khora definition cannot drift.
#[test]
fn the_header_guards_itself_and_is_usable_from_cpp() {
    let (_, header) = library("export_header", PRICING).expect("it should build");
    assert!(header.contains("#ifndef KHORA_"), "{header}");
    assert!(header.contains("extern \"C\" {"), "{header}");
    assert!(header.contains("#include <stdint.h>"), "{header}");
}

// --- what is refused, and where ------------------------------------------

fn refusal(name: &str, source: &str) -> Vec<String> {
    library(name, source).err().unwrap_or_default()
}

/// A `String` cannot cross, and the message says what to do instead.
#[test]
fn an_export_that_could_not_be_called_from_c_is_refused() {
    let found = refusal(
        "export_bad_string",
        "module t;\nexport extern fn greet(who: String) -> Int { 1 }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("cannot cross") && e.contains("khora_float_text")),
        "expected the buffer-and-capacity advice, got {found:?}"
    );
}

/// A C symbol is reachable by anything that links the library, so a private one
/// is a contradiction rather than a narrower promise.
#[test]
fn an_extern_body_without_export_is_refused() {
    let found = refusal("export_private", "module t;\nextern fn price(u: Int) -> Int { u }\n");
    assert!(
        found.iter().any(|e| e.contains("cannot be private")),
        "expected the visibility error, got {found:?}"
    );
}

/// A library with no exports has no way in, and building one silently would
/// hand somebody an artifact nothing can call.
#[test]
fn a_library_with_nothing_exported_is_refused() {
    let found = refusal("export_none", "module t;\nexport fn plain(n: Int) -> Int { n }\n");
    assert!(
        found.iter().any(|e| e.contains("no `export extern fn`")),
        "expected the empty-library error, got {found:?}"
    );
}

/// Two modules cannot publish one C symbol: the namespace is flat and the
/// linker would pick one, silently and not necessarily the same one twice.
#[test]
fn two_exports_of_one_name_are_refused() {
    let dir = scratch("export_collision");
    harness::ensure_runtime();
    let out = dir.join(format!("libclash.{}", std::env::consts::DLL_EXTENSION));

    let db = KhoraDatabase::new();
    let one = SourceFile::new(
        &db,
        dir.join("one.kh"),
        "module one;\nexport extern fn price(n: Int) -> Int { n }\n".to_string(),
    );
    let two = SourceFile::new(
        &db,
        dir.join("two.kh"),
        "module two;\nexport extern fn price(n: Int) -> Int { n + 1 }\n".to_string(),
    );
    let root = SourceRoot::new(&db, vec![one, two]);

    let found: Vec<String> = khora_codegen_llvm::compile_library(&db, root, &out)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert!(
        found.iter().any(|e| e.contains("exported to C as `price`")),
        "expected the collision to be named, got {found:?}"
    );
}

// --- containing a trap ----------------------------------------------------

/// Allocates, keeps it live, and then overflows. The allocation matters: an
/// empty registry proves nothing about whether discarding one works.
const TRAPS: &str = "module t;

import std::core::{Eq, Show};

export extern fn price(units: Int, scale: Int) -> Int {
  units * scale
}

export extern fn boom(n: Int) -> Int {
  let a = n.show() + \"-and-some-more-text-to-allocate\";
  let b = a + a;
  let big = 9223372036854775807;
  let bad = big + n;
  if b == \"never\" { bad } else { 0 }
}
";

/// Compiles a library against the real `std`, since `TRAPS` needs `Show`.
fn library_with_std(name: &str, source: &str) -> PathBuf {
    let dir = scratch(name);
    harness::ensure_runtime();
    let out = dir.join(format!("lib{name}.{}", std::env::consts::DLL_EXTENSION));

    let db = KhoraDatabase::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                files.push(SourceFile::new(&db, path, text));
            }
        }
    }
    files.push(SourceFile::new(&db, dir.join("main.kh"), source.to_string()));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile_library(&db, root, &out) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    out
}

/// Builds and runs a C host against `library`, returning its output.
fn host(library: &std::path::Path, name: &str, body: &str) -> std::process::Output {
    let clang = khora_codegen_llvm::toolchain::tool("clang").expect("clang, checked by the caller");
    let dir = library.parent().expect("a directory").to_path_buf();
    let header = library
        .with_extension("h")
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();

    let source = dir.join(format!("{name}.c"));
    std::fs::write(
        &source,
        String::from("#include <stdio.h>\n#include \"") + &header + "\"\n" + body,
    )
    .expect("writing the C host");

    let implib = library.with_extension("lib");
    let link_against = if implib.is_file() { implib } else { library.to_path_buf() };
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let built = std::process::Command::new(&clang)
        .arg(&source)
        .arg(&link_against)
        .arg("-o")
        .arg(&exe)
        .current_dir(&dir)
        .output()
        .expect("could not run clang");
    assert!(
        built.status.success(),
        "the C host did not link:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    std::process::Command::new(&exe).current_dir(&dir).output().expect("could not run the host")
}

/// **The whole point.** A host opts in, the library traps, the host is still
/// there afterwards and the memory came back.
#[test]
fn a_contained_trap_leaves_the_host_running() {
    if khora_codegen_llvm::toolchain::tool("clang").is_none() {
        eprintln!("no clang in the LLVM toolchain; skipping");
        return;
    }
    let library = library_with_std("contain_survives", TRAPS);
    let ran = host(
        &library,
        "survives",
        "int main(void) {\n\
        \x20   khora_set_trap_policy(1);\n\
        \x20   printf(\"%lld\\n\", (long long) price(3, 4));\n\
        \x20   long long before = khora_live_count();\n\
        \x20   boom(1);\n\
        \x20   printf(\"%s\\n\", khora_trapped() ? \"contained\" : \"NOT contained\");\n\
        \x20   khora_clear_trap();\n\
        \x20   printf(\"leaked %lld\\n\", khora_live_count() - before);\n\
        \x20   printf(\"%lld\\n\", (long long) price(5, 6));\n\
        \x20   return 0;\n\
        }\n",
    );

    assert!(ran.status.success(), "the host should survive, got {:?}", ran.status.code());
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(
        out, "12\ncontained\nleaked 0\n30\n",
        "a call before the trap, the trap contained, nothing leaked, a call after"
    );
    // The message is still printed. A contained trap is a bug, and a library
    // that swallowed one in silence would be worse than one that died.
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(said.contains("overflowed"), "it still says what happened: {said}");
    assert!(said.contains("object(s) released"), "and what it did about it: {said}");
}

/// **The default is unchanged.** A host that opted into nothing gets exactly
/// what it got before containment existed, which is the promise that made
/// opt-in the right shape.
#[test]
fn without_opting_in_the_process_still_dies() {
    if khora_codegen_llvm::toolchain::tool("clang").is_none() {
        eprintln!("no clang in the LLVM toolchain; skipping");
        return;
    }
    let library = library_with_std("contain_default", TRAPS);
    let ran = host(
        &library,
        "dies",
        "int main(void) {\n\
        \x20   printf(\"%lld\\n\", (long long) price(3, 4));\n\
        \x20   fflush(stdout);\n\
        \x20   boom(1);\n\
        \x20   printf(\"unreachable\\n\");\n\
        \x20   return 0;\n\
        }\n",
    );

    assert_eq!(ran.status.code(), Some(134), "a trap still ends the process");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert!(out.contains("12"), "the call before the trap ran: {out}");
    assert!(!out.contains("unreachable"), "and nothing after it did: {out}");
}
