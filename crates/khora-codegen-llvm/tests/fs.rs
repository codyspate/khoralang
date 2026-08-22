#![cfg(feature = "llvm")]

//! `std::fs`, against the real file system.
//!
//! Phase 8's first module, and the first test of whether Khora is pleasant to
//! write a library in — which is a question no amount of compiler work
//! answers. Everything under test is written in Khora: the C conventions, the
//! region that closes the file, the effect that gates access to it.

mod harness;

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_source(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

struct Ran {
    stdout: String,
    code: Option<i32>,
}

/// Compiles `main` against the real `std::core` and `std::fs`, and runs it.
///
/// `@DIR@` in the source becomes the scratch directory, as a Khora string
/// literal.
fn run(name: &str, main: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let here = dir.to_string_lossy().replace('\\', "/");

    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("fs.kh"), std_source("fs.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main.replace("@DIR@", &here)),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let output = std::process::Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

const HEAD: &str = "module demo::main;
import std::core::{Array, Option, Result, attempt};
import std::fs::{Fs, IoError, read_text};

fn print(value: String);
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;
";

/// Write a file and read it back, through the effect, with nothing left alive.
#[test]
fn a_file_written_by_khora_can_be_read_by_khora() {
    let ran = run(
        "fs_round_trip",
        &format!(
            "{HEAD}
export fn work() -> Int with {{ fs: Fs }} raises IoError {{
  let bytes: Array<U8> = Array::new(3, 65);
  fs.write(\"@DIR@/greeting.txt\", bytes)!;
  print(read_text(\"@DIR@/greeting.txt\")!);
  Array::length(fs.read(\"@DIR@/greeting.txt\")!)
}}

fn main() -> Int {{
  with {{ fs: Fs::real() }} {{
    khora_print_int(work()! catch {{
      IoError::NotFound(p) => 0 - 1,
      IoError::Failed(p) => 0 - 2,
    }});
  }}
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "AAA\n3\n0\n", "three `A`s written, read back as text, then as bytes");
    assert_eq!(ran.code, Some(0));
}

/// A file that is not there is `NotFound`, not a crash and not an empty string.
#[test]
fn a_missing_file_raises_not_found() {
    let ran = run(
        "fs_missing",
        &format!(
            "{HEAD}
export fn work() -> Int with {{ fs: Fs }} {{
  String::byte_length(read_text(\"@DIR@/nothing-here.txt\")! catch {{
    IoError::NotFound(p) => \"1\",
    IoError::Failed(p) => \"22\",
  }})
}}

fn main() -> Int {{
  with {{ fs: Fs::real() }} {{ khora_print_int(work()); }}
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n", "NotFound, and the failed open leaked nothing");
    assert_eq!(ran.code, Some(0));
}

/// Bytes that are not text are not a `String`, and `read_text` says so through
/// the error channel rather than trapping — which is the whole reason
/// `is_utf8` and `from_bytes` are two things.
#[test]
fn a_file_that_is_not_text_fails_rather_than_trapping() {
    let ran = run(
        "fs_not_text",
        &format!(
            "{HEAD}
export fn work() -> Int with {{ fs: Fs }} raises IoError {{
  // 0xFF is not a byte any UTF-8 sequence starts with.
  let bytes: Array<U8> = Array::new(2, 255);
  fs.write(\"@DIR@/binary.dat\", bytes)!;
  khora_print_int(Array::length(fs.read(\"@DIR@/binary.dat\")!));
  print(read_text(\"@DIR@/binary.dat\")! catch {{
    IoError::NotFound(p) => \"missing\",
    IoError::Failed(p) => \"not text\",
  }});
  0
}}

fn main() -> Int {{
  with {{ fs: Fs::real() }} {{
    khora_print_int(work()! catch {{
      IoError::NotFound(p) => 0 - 1,
      IoError::Failed(p) => 0 - 2,
    }});
  }}
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "2\nnot text\n0\n0\n",
        "the bytes read back fine; only reading them *as text* failed"
    );
    assert_eq!(ran.code, Some(0));
}

/// **The seam.** The code under test asks for `Fs` and gets whatever the caller
/// installed — here a handler that never touches a disk. This is the thing a
/// file system mock is usually a poor imitation of, and it needed nothing from
/// `std::fs` to work: the effect *is* the interface.
///
/// Neither operation can fail, though `Fs` says both may. That is the point of
/// the change these two lines are really testing: what a lambda raises is a
/// *lower bound*, so a stub that never fails satisfies an interface that allows
/// failure. Before it, this mock had to raise on some branch it never took.
#[test]
fn the_file_system_can_be_replaced_wholesale() {
    let ran = run(
        "fs_mocked",
        &format!(
            "{HEAD}
/// Ordinary code. It has no idea whether a disk exists.
export fn work() -> Int with {{ fs: Fs }} raises IoError {{
  String::byte_length(read_text(\"/etc/passwd\")!)
}}

fn main() -> Int {{
  with {{ fs: handler for Fs {{
    read: fn path => String::bytes(\"pretend\"),
    write: fn (path, bytes) => (),
  }} }} {{
    khora_print_int(work()! catch {{
      IoError::NotFound(p) => 0 - 1,
      IoError::Failed(p) => 0 - 2,
    }});
  }}
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "7\n0\n", "`pretend` is seven bytes, and no file was opened");
    assert_eq!(ran.code, Some(0));
}

/// And the permission half: a function that has no `Fs` cannot get one.
#[test]
fn nothing_reaches_a_file_without_the_capability() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs_denied");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("fs.kh"), std_source("fs.kh")),
        SourceFile::new(
            &db,
            dir.join("main.kh"),
            "module demo::main;
import std::fs::{read_text};

fn khora_print_int(value: Int);

fn main() -> Int {
  khora_print_int(String::byte_length(read_text(\"any\")!));
  0
}
"
            .to_string(),
        ),
    ];
    let root = SourceRoot::new(&db, files);
    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("reading a file without `fs` should be refused");
    let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("fs")),
        "expected the missing capability to be named, got {messages:?}"
    );
}
