//! Where a codegen test's time actually goes.
//!
//! Not an assertion about behaviour — a measurement, printed, so that the next
//! person deciding what to speed up is reading numbers rather than the
//! roadmap's guess. Roadmap 14.28 and 14.29 both name a suspect; this says
//! which one is right on this machine.
//!
//! Ignored by default because it is a report, not a test. Run it with:
//!
//! ```text
//! cargo nextest run -p khora-codegen-llvm --features llvm --run-ignored all \
//!     -E 'test(where_the_time_goes)' --no-capture
//! ```
//!
//! # One database per binary is not a free change
//!
//! 14.28 proposes keeping one `KhoraDatabase` per test binary so that shared
//! work is done once. `SourceRoot` is a Salsa **singleton input** — a second
//! one in the same database panics with "singleton struct may not be
//! duplicated" — so a shared database means *replacing* the root between
//! tests, not adding to it. Replacing it invalidates everything downstream of
//! it, which for single-file programs that share no source is nearly
//! everything. The saving is whatever survives a root change, and that is a
//! number worth having before doing the work rather than after.

#![cfg(feature = "llvm")]


use std::time::Instant;

use crate::harness::ensure_runtime;
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The shape of the programs the codegen suite actually compiles: small.
const SOURCE: &str = "module t;\n\npub fn main() -> Int {\n  let a = 1;\n  let b = 2;\n  a + b\n}\n";

fn seconds(at: Instant) -> f64 {
    at.elapsed().as_secs_f64()
}

/// Verifies `SOURCE` in a database of its own, and says how long it took.
fn verify_once(module: &str) -> f64 {
    let at = Instant::now();
    let db = KhoraDatabase::new();
    let file =
        SourceFile::new(&db, format!("{module}.kh").into(), SOURCE.replace("module t", &format!("module {module}")));
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::verify_for_target(&db, root).expect("it verifies");
    seconds(at)
}

#[test]
#[ignore = "a measurement, not an assertion"]
fn where_the_time_goes() {
    let started = Instant::now();
    ensure_runtime();
    let runtime = seconds(started);

    let directory = std::env::temp_dir().join("khora-phases");
    std::fs::create_dir_all(&directory).expect("a directory");
    let exe = directory.join(if cfg!(windows) { "p.exe" } else { "p" });

    // First one in a process pays for whatever LLVM initialises lazily.
    let first = verify_once("a");
    let second = verify_once("b");
    let third = verify_once("c");

    let at = Instant::now();
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "v.kh".into(), SOURCE.replace("module t", "module v"));
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::compile(&db, root, &exe).expect("it compiles");
    let full = seconds(at);

    // Again, so a second link in the same process is separated from the
    // first: `clang` is a fresh process either way, but the object write is
    // not, and the operating system's file cache is warm the second time.
    let at = Instant::now();
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "w.kh".into(), SOURCE.replace("module t", "module w"));
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::compile(&db, root, &exe).expect("it compiles");
    let full_again = seconds(at);

    let verify = second.min(third);
    println!("\n  ensure_runtime         {runtime:7.3}s");
    println!("  verify, first          {first:7.3}s");
    println!("  verify, then           {second:7.3}s and {third:7.3}s");
    println!("  compile and link       {full:7.3}s, then {full_again:7.3}s");
    println!("  -> object and link is  {:7.3}s of it", full_again - verify);
    println!(
        "  -> everything before the object is {:.0}% of a whole test\n",
        100.0 * verify / full_again
    );
}

/// The same split, for a build that includes the standard library.
///
/// **This is the one a person feels.** `khora build` compiles the program
/// *and* every `std` module the host selects, which is what the codegen tests
/// above deliberately do not do — so their 1-2 ms front end says nothing about
/// what a real build costs. Roadmap 14.35.
#[test]
#[ignore = "a measurement, not an assertion"]
fn where_a_real_build_spends_it() {
    ensure_runtime();

    let Some(std_dir) = khora_db::standard_library() else {
        println!("\n  no standard library beside the compiler; nothing to measure\n");
        return;
    };

    // Every `.kh` under `std` the host would select, which is what
    // `collect_sources` hands a real build.
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    gather(&std_dir, &mut paths);
    paths.sort();

    let directory = std::env::temp_dir().join("khora-phases");
    std::fs::create_dir_all(&directory).expect("a directory");
    let exe = directory.join(if cfg!(windows) { "big.exe" } else { "big" });

    let load = |db: &KhoraDatabase| {
        let mut files = vec![SourceFile::new(
            db,
            "main.kh".into(),
            "module app::main;\n\npub fn main() -> Int {\n  0\n}\n".to_string(),
        )];
        for path in &paths {
            let text = std::fs::read_to_string(path).expect("a std file");
            files.push(SourceFile::new(db, path.clone(), text));
        }
        SourceRoot::new(db, files)
    };

    let at = Instant::now();
    let db = KhoraDatabase::new();
    let root = load(&db);
    khora_codegen_llvm::verify_for_target(&db, root).expect("std verifies");
    let front = seconds(at);

    let at = Instant::now();
    let db = KhoraDatabase::new();
    let root = load(&db);
    khora_codegen_llvm::compile(&db, root, &exe).expect("it compiles");
    let whole = seconds(at);

    println!("\n  {} std module(s) plus a four-line program", paths.len());
    println!("  up to and including verify   {front:7.3}s");
    println!("  the whole build              {whole:7.3}s");
    println!("  -> object and link           {:7.3}s", whole - front);
    println!(
        "  -> the back end is {:.0}% of it\n",
        100.0 * (whole - front) / whole
    );
}

/// Every `.kh` under `directory` this host would compile.
fn gather(directory: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            gather(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh")
            && khora_db::selected_for_target(&path, khora_db::host_target())
        {
            out.push(path);
        }
    }
}
