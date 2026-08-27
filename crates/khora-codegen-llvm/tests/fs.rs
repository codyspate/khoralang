#![cfg(feature = "llvm")]

//! `std::fs`, against the real file system.
//!
//! The first test of whether Khora is pleasant to write a library in — a
//! question no amount of compiler work answers. Everything under test is
//! written in Khora: the C conventions, the region that closes the file, the
//! effect that gates access to it.

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
        SourceFile::new(&db, dir.join("fs_native.kh"), std_source("fs_native.kh")),
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
import std::core::{Array, List, Option, Result, attempt};
import std::fs::{FsRead, FsWrite, IoError, append_text, copy, join, read_text, write_text};

fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;
";

/// Write a file and read it back, through the effect, with nothing left alive.
#[test]
fn a_file_written_by_khora_can_be_read_by_khora() {
    let ran = run(
        "fs_round_trip",
        &format!(
            "{HEAD}
pub fn work() -> Int with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let bytes: Array<U8> = Array::new(3, 65);
  writes.write(\"@DIR@/greeting.txt\", bytes)!;
  print(read_text(\"@DIR@/greeting.txt\")!);
  Array::length(reads.read(\"@DIR@/greeting.txt\")!)
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
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
pub fn work() -> Int with {{ reads: FsRead, writes: FsWrite }} {{
  String::byte_length(read_text(\"@DIR@/nothing-here.txt\")! catch {{
    IoError::NotFound(p) => \"1\",
    IoError::Failed(p) => \"22\",
  }})
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{ khora_print_int(work()); }}
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
pub fn work() -> Int with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  // 0xFF is not a byte any UTF-8 sequence starts with.
  let bytes: Array<U8> = Array::new(2, 255);
  writes.write(\"@DIR@/binary.dat\", bytes)!;
  khora_print_int(Array::length(reads.read(\"@DIR@/binary.dat\")!));
  print(read_text(\"@DIR@/binary.dat\")! catch {{
    IoError::NotFound(p) => \"missing\",
    IoError::Failed(p) => \"not text\",
  }});
  0
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
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
pub fn work() -> Int with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  String::byte_length(read_text(\"/etc/passwd\")!)
}}

fn main() -> Int {{
  with {{
    reads: handler for FsRead {{
      read: fn path => String::bytes(\"pretend\"),
      exists: fn path => true,
      size: fn path => 7,
      read_dir: fn path => List::Nil,
      is_dir: fn path => false,
    }},
    writes: handler for FsWrite {{
      write: fn (path, bytes) => (),
      append: fn (path, bytes) => (),
      remove: fn path => (),
      rename: fn (from, to) => (),
      create_dir: fn path => (),
      remove_dir: fn path => (),
    }},
  }} {{
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
        SourceFile::new(&db, dir.join("fs_native.kh"), std_source("fs_native.kh")),
        SourceFile::new(
            &db,
            dir.join("main.kh"),
            "module demo::main;
import std::fs::{read_text};

extern fn khora_print_int(value: Int);

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
        messages.iter().any(|m| m.contains("FsRead")),
        "expected the missing capability to be named, got {messages:?}"
    );
}

// --- the rest of the surface -----------------------------------------------
//
// Each of these puts the work in a function with a `raises` row and catches
// the call, rather than wrapping the `with` block in a `catch`: a `with`
// block is block-like, so a trailing `catch` is a separate statement and does
// not attach to it -- the same rule that lets `if c { .. }` stand without a
// semicolon.

/// `append` adds rather than replacing, which is the whole reason it is an
/// operation instead of a read followed by a write.
#[test]
fn append_adds_to_the_end() {
    let ran = run(
        "fs_append",
        &format!(
            "{HEAD}
fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let path = \"@DIR@/notes.txt\";
  write_text(path, \"one\\n\")!;
  append_text(path, \"two\\n\")!;
  append_text(path, \"three\\n\")!;
  print(read_text(path)!);
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "one\ntwo\nthree\n\n", "three appends, in order");
    assert_eq!(ran.code, Some(0));
}

/// `size` is the byte count, and `exists` distinguishes a file from nothing.
#[test]
fn size_and_exists_answer_about_a_real_file() {
    let ran = run(
        "fs_size_exists",
        &format!(
            "{HEAD}
fn yes_no(b: Bool) -> String {{ if b {{ \"yes\" }} else {{ \"no\" }} }}

fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let path = \"@DIR@/sized.txt\";
  write_text(path, \"12345\")!;
  print(Int::to_string(reads.size(path)!));
  print(yes_no(reads.exists(path)));
  print(yes_no(reads.exists(\"@DIR@/never-written.txt\")));
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\nyes\nno\n", "five bytes, present, absent");
    assert_eq!(ran.code, Some(0));
}

/// `rename` moves: the old name stops answering and the new one starts, and
/// the bytes are the ones that were written.
#[test]
fn rename_moves_the_bytes() {
    let ran = run(
        "fs_rename",
        &format!(
            "{HEAD}
fn yes_no(b: Bool) -> String {{ if b {{ \"yes\" }} else {{ \"no\" }} }}

fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let from = \"@DIR@/before.txt\";
  let to = \"@DIR@/after.txt\";
  write_text(from, \"carried\")!;
  writes.rename(from, to)!;
  print(yes_no(reads.exists(from)));
  print(yes_no(reads.exists(to)));
  print(read_text(to)!);
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "no\nyes\ncarried\n", "gone from one name, present under the other");
    assert_eq!(ran.code, Some(0));
}

/// `remove` deletes, and removing what is not there fails rather than passing
/// quietly -- a caller wanting "delete if present" asks `exists` first, and
/// one that cannot tell the difference has a bug.
#[test]
fn remove_deletes_and_refuses_what_is_absent() {
    let ran = run(
        "fs_remove",
        &format!(
            "{HEAD}
fn yes_no(b: Bool) -> String {{ if b {{ \"yes\" }} else {{ \"no\" }} }}

fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let path = \"@DIR@/doomed.txt\";
  write_text(path, \"x\")!;
  writes.remove(path)!;
  print(yes_no(reads.exists(path)));
}}

fn again() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  writes.remove(\"@DIR@/doomed.txt\")!;
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
    again()! catch {{
      IoError::NotFound(p) => print(\"refused\"),
      IoError::Failed(p) => print(\"refused\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "no\nrefused\n", "deleted, and deleting it again is refused");
    assert_eq!(ran.code, Some(0));
}

/// `copy` is `read` then `write` and is deliberately not an operation of the
/// effect, so a double that answers those two gets a working `copy` for
/// nothing. That is the argument for it being a function, tested rather than
/// asserted.
#[test]
fn copy_goes_through_read_and_write() {
    let ran = run(
        "fs_copy",
        &format!(
            "{HEAD}
fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  copy(\"a\", \"b\")!;
}}

fn main() -> Int {{
  with {{
    reads: handler for FsRead {{
      read: fn path => String::bytes(\"from the double\"),
      exists: fn path => true,
      size: fn path => 0,
      read_dir: fn path => List::Nil,
      is_dir: fn path => false,
    }},
    writes: handler for FsWrite {{
      write: fn (path, bytes) => print(String::from_bytes(bytes)),
      append: fn (path, bytes) => (),
      remove: fn path => (),
      rename: fn (from, to) => (),
      create_dir: fn path => (),
      remove_dir: fn path => (),
    }},
  }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "from the double\n", "what `read` gave is what `write` got");
    assert_eq!(ran.code, Some(0));
}

// --- directories -----------------------------------------------------------

/// Create, list, and remove -- and the listing is names rather than paths.
#[test]
fn a_directory_can_be_made_listed_and_removed() {
    let ran = run(
        "fs_dir",
        &format!(
            "{HEAD}
fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let dir = \"@DIR@/nest\";
  writes.create_dir(dir)!;
  write_text(join(dir, \"a.txt\"), \"a\")!;
  write_text(join(dir, \"b.txt\"), \"b\")!;

  let names = reads.read_dir(dir)!;
  print(Int::to_string(List::length(names)));
  print(List::fold(names, \"\", fn (acc, n) => \"${{acc}}${{n}} \"));

  writes.remove(join(dir, \"a.txt\"))!;
  writes.remove(join(dir, \"b.txt\"))!;
  writes.remove_dir(dir)!;
  print(if reads.is_dir(dir) {{ \"still there\" }} else {{ \"gone\" }});
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "2\na.txt b.txt \ngone\n", "two names, then the directory is gone");
    assert_eq!(ran.code, Some(0));
}

/// `is_dir` tells a directory from a file, which is the question a caller asks
/// before deciding which of the two to use.
#[test]
fn is_dir_distinguishes_a_directory_from_a_file() {
    let ran = run(
        "fs_is_dir",
        &format!(
            "{HEAD}
fn yes_no(b: Bool) -> String {{ if b {{ \"yes\" }} else {{ \"no\" }} }}

fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let dir = \"@DIR@/somewhere\";
  let file = \"@DIR@/plain.txt\";
  writes.create_dir(dir)!;
  write_text(file, \"x\")!;
  print(yes_no(reads.is_dir(dir)));
  print(yes_no(reads.is_dir(file)));
  print(yes_no(reads.is_dir(\"@DIR@/never\")));
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "yes\nno\nno\n", "a directory, a file, and nothing");
    assert_eq!(ran.code, Some(0));
}

/// **The split, doing its job.** A function given only `FsRead` cannot delete,
/// and the compiler says so rather than the file system finding out.
#[test]
fn reading_does_not_grant_writing() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs_read_only");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("fs_native.kh"), std_source("fs_native.kh")),
        SourceFile::new(
            &db,
            dir.join("main.kh"),
            "module demo::main;
import std::fs::{FsRead};

fn tidy(path: String) -> () with { reads: FsRead } {
  reads.remove(path);
}

fn main() -> Int { 0 }
"
            .to_string(),
        ),
    ];
    let root = SourceRoot::new(&db, files);
    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("`remove` is not an operation of `FsRead`");
    let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("remove")),
        "expected the missing operation to be named, got {messages:?}"
    );
}
