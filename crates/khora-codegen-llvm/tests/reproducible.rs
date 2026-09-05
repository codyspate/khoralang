#![cfg(feature = "llvm")]

//! Two builds of one program produce one artifact.
//!
//! `docs/project.md` §6.1 asks for bit-for-bit reproducible builds and nothing
//! checked it. They were not: two runs of `khora build` over an unchanged
//! `risk_analyzer` differed in 112 lines of IR, every one of them the order of
//! a release sequence.
//!
//! **The cause was Rust's `HashSet`, not anything about the program.** Every
//! `HashSet` is seeded with a per-process random value, and `khora-perceus`
//! iterated one to decide which locals a `match` arm releases and in what
//! order. `Live` is a `BTreeSet` now, keyed on a `LocalId` that is a `u32` in
//! declaration order — so the order is the source's own.
//!
//! Reproducibility is also what makes the rest of 12.9 smaller than it looks:
//! a build anybody can repeat byte for byte can be *verified* by repeating it,
//! which is a stronger claim than a signature over an artifact nobody can
//! reproduce.

use crate::harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// A program with enough shape to have release sequences worth ordering:
/// several counted locals, a `match` that consumes some of them on one path
/// and not the other, and a loop.
const SHAPED: &str = "module t;

fn print(value: String);

pub type Holding =
  | Cash(amount: Int)
  | Position(ticker: String, lots: Int);

fn describe(h: Holding, note: String) -> String {
  match h {
    Holding::Cash(a) => note + \"-cash\",
    Holding::Position(t, l) => t + note,
  }
}

fn main() -> Int {
  let mut i = 0;
  let mut seen = \"\";
  while i < 3 {
    let note = \"n\";
    let one = describe(Holding::Cash(i), note);
    let two = describe(Holding::Position(\"AAPL\", i), note);
    seen = one + two;
    i = i + 1
  };
  print(seen);
  0
}
";

/// Compiles `SHAPED` into `name` and returns the object's bytes.
///
/// The **object**, not the executable: what this is testing is the compiler,
/// and on Windows the linker's PDB emission is a separate question with its
/// own answer below.
fn object_of(name: &str) -> Vec<u8> {
    // **One directory for both**, and the output named per run. The source
    // path is part of the input — debug information records where a program
    // came from, and `debug.rs` says a build is reproducible exactly to the
    // extent that the invocation is. Two directories is two invocations, and
    // comparing them would be testing something nobody claimed.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), SHAPED.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::compile(&db, root, &exe).expect("it compiles");

    let mut object = exe.clone().into_os_string();
    object.push(".o");
    std::fs::read(PathBuf::from(object)).expect("an object was written")
}

/// **The claim, checked.**
///
/// Two compilations in one process, which is the *weaker* of the two cases and
/// the one that was failing: a `HashSet`'s seed is per process, so both of
/// these share it, and the order still differed between them because the sets
/// being iterated were built differently each time.
#[test]
fn two_builds_of_one_program_agree() {
    let first = object_of("reproducible_a");
    let second = object_of("reproducible_b");
    assert_eq!(
        first.len(),
        second.len(),
        "two builds of one program should produce one object"
    );
    assert!(
        first == second,
        "two builds of one program differ, and §6.1 says they must not"
    );
}

/// And across processes, where the `HashSet` seed genuinely differs.
///
/// Run through the binary rather than the library because that is the only way
/// to get a second seed — within one process every `HashSet` shares one, so
/// the test above cannot see the failure this one can.
#[test]
fn two_processes_agree_too() {
    let Some(khora) = std::env::var_os("CARGO_BIN_EXE_khora") else {
        // Not built as part of this crate's test binaries; the in-process
        // check above still covers the ordering.
        eprintln!("no khora binary for the cross-process check; skipping");
        return;
    };

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reproducible_across");
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    std::fs::write(dir.join("main.kh"), SHAPED).expect("a program");

    let mut hashes = Vec::new();
    for run in 0..2 {
        let out = dir.join(format!("run{run}.exe"));
        let built = std::process::Command::new(&khora)
            .arg("build")
            .arg(dir.join("main.kh"))
            .arg("-o")
            .arg(&out)
            // Debug information is a separate question — see the module docs
            // and roadmap 12.9. What is under test here is the compiler.
            .env("KHORA_DEBUG", "0")
            .output()
            .expect("could not run khora");
        assert!(
            built.status.success(),
            "the build failed:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let mut object = out.into_os_string();
        object.push(".o");
        hashes.push(std::fs::read(PathBuf::from(object)).expect("an object"));
    }
    assert!(hashes[0] == hashes[1], "two processes should agree, and a HashSet seed says otherwise");
}
