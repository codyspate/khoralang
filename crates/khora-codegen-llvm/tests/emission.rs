#![cfg(feature = "llvm")]

//! What a build emits: what its entry points reach, and what is published.
//!
//! **What these prevent: a function removed from the binary that something
//! calls by name.** Monomorphization starts from the build's roots rather than
//! from every concrete function, so anything that is entered from outside the
//! program's own call graph has to be a root, or it is missing at link time --
//! or, for a symbol looked up when the program runs, missing when it is
//! called. A `pub extern fn` is the one such thing a Khora program can write:
//! C calls it, and no Khora function need. These link a C caller against one
//! that nothing in Khora calls and run it.
//!
//! The other half is the saving, measured as a count: a three-line program
//! emits its own two functions and none of `std`'s.

use crate::harness;

use std::path::{Path, PathBuf};
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    dir
}

/// Every `std` file this host selects, plus `source` as `main.kh`.
fn with_std(db: &KhoraDatabase, dir: &Path, source: &str) -> SourceRoot {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
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
                files.push(SourceFile::new(db, path, text));
            }
        }
    }
    files.push(SourceFile::new(db, dir.join("main.kh"), source.to_string()));
    SourceRoot::new(db, files)
}

/// The functions an object file defines, by name.
fn defined_functions(object: &Path) -> Vec<String> {
    let nm = khora_codegen_llvm::toolchain::tool("llvm-nm").expect("llvm-nm beside clang");
    let out = Command::new(nm)
        .args(["--defined-only", "--format=just-symbols"])
        .arg(object)
        .output()
        .expect("could not run llvm-nm");
    assert!(out.status.success(), "llvm-nm failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

/// Links `body` against `library` with clang and runs it from beside it.
fn run_c(library: &Path, extra: &[&Path], body: &str) -> String {
    let clang = khora_codegen_llvm::toolchain::tool("clang").expect("clang in the LLVM toolchain");
    let dir = library.parent().expect("a directory").to_path_buf();
    let source = dir.join("host.c");
    std::fs::write(&source, body).expect("writing the C host");
    let exe = dir.join(format!("host{}", std::env::consts::EXE_SUFFIX));
    let implib = library.with_extension("lib");
    let link_against = if implib.is_file() { implib } else { library.to_path_buf() };
    let built = Command::new(&clang)
        .arg(&source)
        .arg(&link_against)
        .args(extra)
        .arg("-o")
        .arg(&exe)
        .current_dir(&dir)
        .output()
        .expect("could not run clang");
    assert!(built.status.success(), "the C host did not link:\n{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&exe).current_dir(&dir).output().expect("could not run the host");
    assert!(ran.status.success(), "the host exited with {:?}", ran.status.code());
    String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n")
}

/// **A library's only roots are its exports**, and an export may reach `std`.
///
/// `List` comes from `std::core`, so the export's callees are found by the
/// walk from it and nowhere else: nothing in this library calls `total` but C.
#[test]
#[cfg(unix)]
fn a_library_export_and_what_it_reaches_are_emitted() {
    harness::ensure_runtime();
    let dir = scratch("emission_library");
    let out = dir.join(format!("libemitted.{}", std::env::consts::DLL_EXTENSION));
    let db = KhoraDatabase::new();
    let root = with_std(
        &db,
        &dir,
        "module emitted::main;\n\n\
         import std::core::{List};\n\n\
         /// Sums `[n, n + 1, n + 2]` through `std`'s list functions.\n\
         pub extern fn total(n: Int) -> Int {\n  List::fold([n, n + 1, n + 2], 0, fn acc, x => acc + x)\n}\n",
    );
    khora_codegen_llvm::compile_library(&db, root, &out)
        .unwrap_or_else(|e| panic!("it should build: {:?}", e.iter().map(|e| &e.message).collect::<Vec<_>>()));
    let printed = run_c(
        &out,
        &[],
        "#include <stdio.h>\n#include \"libemitted.h\"\nint main(void) { printf(\"%lld\\n\", (long long) total(5)); return 0; }\n",
    );
    assert_eq!(printed, "18\n", "C called the export and it ran through std");
}

/// **A `main` build publishes its exports too**, and nothing in Khora calls
/// them. This is the build where pruning from `main` would drop one: the
/// executable links, and a C object linked into it finds no `price`.
#[test]
#[cfg(unix)]
fn a_programs_export_survives_though_main_never_calls_it() {
    harness::ensure_runtime();
    let dir = scratch("emission_program_export");
    let exe = dir.join("program");
    let db = KhoraDatabase::new();
    let root = with_std(
        &db,
        &dir,
        "module emitted::main;\n\n\
         /// Doubles `n`, for whoever links against this program.\n\
         pub extern fn price(n: Int) -> Int {\n  n * 2\n}\n\n\
         pub fn main() -> Int {\n  0\n}\n",
    );
    khora_codegen_llvm::compile(&db, root, &exe)
        .unwrap_or_else(|e| panic!("it should build: {:?}", e.iter().map(|e| &e.message).collect::<Vec<_>>()));
    let object = PathBuf::from(format!("{}.o", exe.display()));
    let names = defined_functions(&object);
    assert!(
        names.iter().any(|n| n == "price"),
        "the published `price` is in the program's object: {names:?}"
    );
}

/// **A function reached only as a value is emitted.** Nothing calls
/// `tenfold`, `width` or `remember`: they are a handler's operation, an
/// argument to `List::map` and a region's finalizer, each named rather than
/// written as a lambda, so the walk meets them as a path and never as a call.
/// A walk that followed only calls would leave them undefined at the link.
#[test]
fn a_function_named_only_as_a_value_is_emitted() {
    harness::ensure_runtime();
    let dir = scratch("emission_values");
    let exe = dir.join(format!("values{}", std::env::consts::EXE_SUFFIX));
    let db = KhoraDatabase::new();
    let root = with_std(
        &db,
        &dir,
        "module emitted::main;\n\n\
         import std::core::{print, Region, List};\n\n\
         pub effect Ledger { balance: (Int) -> Int, }\n\n\
         fn tenfold(id: Int) -> Int { id * 10 }\n\n\
         fn remember() -> () { print(\"finalized\"); }\n\n\
         fn width(s: String) -> Int { s.byte_length() }\n\n\
         fn report(id: Int) -> Int with { ledger: Ledger } { ledger.balance(id) }\n\n\
         pub fn main() -> Int {\n\
         \x20 let live = handler for Ledger { balance: tenfold };\n\
         \x20 with { ledger: live } { print(report(4).show()); }\n\
         \x20 let words = [\"ab\", \"cde\"];\n\
         \x20 print(List::sum(List::map(words, width)).show());\n\
         \x20 let region = Region::open();\n\
         \x20 Region::defer(region, remember);\n\
         \x20 0\n\
         }\n",
    );
    khora_codegen_llvm::compile(&db, root, &exe)
        .unwrap_or_else(|e| panic!("it should build: {:?}", e.iter().map(|e| &e.message).collect::<Vec<_>>()));
    let ran = Command::new(&exe).output().expect("running the program");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "40\n5\nfinalized\n",
        "each function reached as a value ran: {}",
        String::from_utf8_lossy(&ran.stderr)
    );
}

/// **Every `std` function links against the runtime archive**, reached or not.
///
/// What this prevents: an `extern fn` in `std` naming a symbol the runtime
/// does not define, unnoticed. A build emits only what its entry points
/// reach, so such a function fails only the program that calls it. This
/// builds every concrete function `std` has into one executable and links
/// it, which fails with the missing symbol's name.
#[test]
fn every_std_function_links_against_the_runtime() {
    harness::ensure_runtime();
    let dir = scratch("emission_link_everything");
    let exe = dir.join(format!("everything{}", std::env::consts::EXE_SUFFIX));
    let db = KhoraDatabase::new();
    let root = with_std(&db, &dir, "module emitted::main;\n\npub fn main() -> Int {\n  0\n}\n");
    let began = std::time::Instant::now();
    if let Err(errors) = khora_codegen_llvm::link_everything(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("all of std must link against the runtime:\n  {}", messages.join("\n  "));
    }
    eprintln!("linking every std function took {:.2} s", began.elapsed().as_secs_f64());
    let names = defined_functions(&PathBuf::from(format!("{}.o", exe.display())));
    let from_std = names.iter().filter(|n| n.contains("kh$std$")).count();
    assert!(from_std > 500, "every concrete std function is in the object, not only what main reaches: {from_std}");
}

/// **An error type's `Show` that only a `Raised` calls is emitted.**
///
/// What this prevents: a failure printed by its type's name where its value
/// was asked for, with nothing reporting it. Code generation describes a
/// `Raised` -- `other.show()`, and a failure with context escaping `main` --
/// by calling each error type's `Show` by symbol, after monomorphization, and
/// by name if that symbol was not emitted. Nothing in either program calls
/// `Inv`'s `show` itself, so seeding from `main` alone printed `batch: Inv`
/// and `Inv` for these two.
#[test]
fn a_show_called_only_by_a_raised_is_emitted() {
    harness::ensure_runtime();
    let programs = [
        (
            "emission_raised_shown",
            "module emitted::main;\n\n\
             import std::core::{Raised, Result, Show, String, attempt, print};\n\n\
             derive(Show)\npub type Inv = | Out(sku: String) | Offline;\n\n\
             fn fetch(n: Int) -> Int raises Inv { raise Inv::Out(\"sku-${n}\") }\n\n\
             fn batch(n: Int) -> Int raises Inv {\n  \
               match attempt(fn () => fetch(n)!) {\n    \
                 Result::Ok(v) => v,\n    \
                 Result::Err(e) => raise Raised::context(e, \"batch\"),\n  \
               }\n}\n\n\
             pub fn main() -> () {\n  \
               match attempt(fn () => batch(7)!) {\n    \
                 Result::Ok(_) => (),\n    \
                 Result::Err(e) => print(e.show()),\n  \
               }\n}\n",
            None,
            "batch: Inv::Out(sku-7)\n",
        ),
        (
            "emission_raised_escapes",
            "module emitted::main;\n\n\
             import std::core::{Raised, Result, Show, String, attempt, print};\n\n\
             derive(Show)\npub type Inv = | Out(sku: String) | Offline;\n\n\
             fn fetch(n: Int) -> Int raises Inv { raise Inv::Out(\"sku-${n}\") }\n\n\
             fn batch(n: Int) -> Int raises Inv {\n  \
               match attempt(fn () => fetch(n)!) {\n    \
                 Result::Ok(v) => v,\n    \
                 Result::Err(e) => raise Raised::context(e, \"batch ${n}\"),\n  \
               }\n}\n\n\
             pub fn main() -> () raises Inv {\n  batch(3)!;\n}\n",
            Some(1),
            "batch 3: Inv::Out(sku-3)",
        ),
    ];
    for (name, source, code, wanted) in programs {
        let dir = scratch(name);
        let exe = dir.join(format!("program{}", std::env::consts::EXE_SUFFIX));
        let db = KhoraDatabase::new();
        let root = with_std(&db, &dir, source);
        khora_codegen_llvm::compile(&db, root, &exe)
            .unwrap_or_else(|e| panic!("{name} should build: {:?}", e.iter().map(|e| &e.message).collect::<Vec<_>>()));
        let ran = Command::new(&exe).output().expect("running the program");
        let out = format!("{}{}", String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
        match code {
            None => assert_eq!(String::from_utf8_lossy(&ran.stdout), wanted, "{name}"),
            Some(code) => {
                assert_eq!(ran.status.code(), Some(code), "{name}: {out}");
                assert!(out.contains(wanted), "{name}: the variant is missing: {out}");
            }
        }
    }
}

/// **The saving itself.** A program that calls nothing in `std` emits none of
/// it: its object defines `main` and the Khora `main`, and no `kh$std$...`.
#[test]
fn a_program_that_calls_nothing_in_std_emits_none_of_it() {
    harness::ensure_runtime();
    let dir = scratch("emission_hello");
    let exe = dir.join(format!("hello{}", std::env::consts::EXE_SUFFIX));
    let db = KhoraDatabase::new();
    let root = with_std(
        &db,
        &dir,
        "module emitted::main;\n\nimport std::core::{print};\n\npub fn main() { print(\"hello\"); }\n",
    );
    khora_codegen_llvm::compile(&db, root, &exe)
        .unwrap_or_else(|e| panic!("it should build: {:?}", e.iter().map(|e| &e.message).collect::<Vec<_>>()));
    let object = PathBuf::from(format!("{}.o", exe.display()));
    let from_std: Vec<String> =
        defined_functions(&object).into_iter().filter(|n| n.contains("kh$std$")).collect();
    assert!(from_std.is_empty(), "no std function is reached, so none is emitted: {from_std:?}");

    let ran = Command::new(&exe).output().expect("running the program");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "hello\n");
}
