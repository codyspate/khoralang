#![cfg(feature = "llvm")]

//! `std::fs`, against the real file system.
//!
//! The first test of whether Khora is pleasant to write a library in — a
//! question no amount of compiler work answers. Everything under test is
//! written in Khora: the C conventions, the region that closes the file, the
//! effect that gates access to it.

use crate::harness;

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
        SourceFile::new(&db, dir.join("permissions.kh"), std_source("permissions.kh")),
        SourceFile::new(&db, dir.join("grants.kh"), std_source("grants.kh")),
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
import std::fs::{FsRead, FsWrite, IoError, append_text, chunk_size, copy, fold_chunks, fold_lines, join, read_text, write_text};

fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;
";

/// **A cancelled fiber closes the file it was holding.**
///
/// Not "does not crash": the assertion is that Windows lets the file be
/// *deleted* afterwards, which it refuses while a handle is open.
///
/// `fold_lines` is the shape that can be caught holding one. It opens the
/// file, registers the close with a region, and calls the step once per line —
/// so a cancellation set inside the step is taken at the fold's own loop
/// back-edge, with the file still open. Nothing in the source says to close
/// it; the region does, on a path `fold_lines` does not mention.
///
/// **The cancellation travels out on the row**, as it does in every test in
/// `db.rs`, rather than being caught. A `catch` does not absorb one — it is a
/// tagged return and not a raise — and a cancellation that reaches a fiber's
/// *root* is a case the runtime declines with
/// `a cancellation reached a fiber's root, which cannot absorb one yet`.
/// Reaching the root is a different missing feature from the one under test.
#[test]
fn a_cancelled_fiber_closes_the_file_it_was_reading() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs_cancel_closes");
    let ran = run(
        "fs_cancel_closes",
        &format!(
            "{HEAD}
import std::core::{{Fiber}};

extern fn khora_cancel();

pub type Oops = | Bad;

/// A fallible call, so that `!` is a cancellation point. It never fails.
fn mark() -> Int raises Oops {{ 1 }}

fn hold() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError + Oops {{
  write_text(\"@DIR@/held.txt\", \"one\\ntwo\\nthree\\nfour\\n\")!;
  let _ = fold_lines(\"@DIR@/held.txt\", 0, fn (n, _line) => {{
    // Set on the first line. The step has no `!`, so the fold's own loop
    // takes it -- with the file open.
    khora_cancel();
    n + 1
  }})!;
  // Not reached: the fold does not return once the cancellation is taken.
  let _ = mark()!;
  print(\"the fold returned, which is wrong\");
}}

pub fn main() -> Int {{
  let f = Fiber::spawn(fn () => {{
    with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{ hold()! }}
  }});
  Fiber::wait(f);
  print(\"the fiber settled\");
  0
}}
"
        ),
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stdout);
    assert!(ran.stdout.contains("the fiber settled"), "{}", ran.stdout);
    assert!(
        !ran.stdout.contains("which is wrong"),
        "the fold should not have finished: {}",
        ran.stdout
    );

    // **The proof.** A file with an open handle cannot be removed on Windows,
    // so a removal that succeeds is a handle that was closed.
    let held = dir.join("held.txt");
    assert!(held.is_file(), "the program should have written it: {}", held.display());
    std::fs::remove_file(&held).unwrap_or_else(|e| {
        panic!("the file was still open after the fiber was cancelled: {e}")
    });
}

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
      IoError::Denied(p) => 0 - 2,
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
    IoError::Denied(p) => \"22\",
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
    IoError::Denied(p) => \"not text\",
  }});
  0
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    khora_print_int(work()! catch {{
      IoError::NotFound(p) => 0 - 1,
      IoError::Failed(p) => 0 - 2,
      IoError::Denied(p) => 0 - 2,
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
      read_at: fn (path, offset, want) => Array::empty(),
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
      IoError::Denied(p) => 0 - 2,
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
        SourceFile::new(&db, dir.join("permissions.kh"), std_source("permissions.kh")),
        SourceFile::new(&db, dir.join("grants.kh"), std_source("grants.kh")),
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
      IoError::Denied(p) => print(\"failed\"),
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
  print(yes_no(reads.exists(path)!));
  print(yes_no(reads.exists(\"@DIR@/never-written.txt\")!));
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
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
  print(yes_no(reads.exists(from)!));
  print(yes_no(reads.exists(to)!));
  print(read_text(to)!);
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
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
  print(yes_no(reads.exists(path)!));
}}

fn again() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  writes.remove(\"@DIR@/doomed.txt\")!;
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
    }};
    again()! catch {{
      IoError::NotFound(p) => print(\"refused\"),
      IoError::Failed(p) => print(\"refused\"),
      IoError::Denied(p) => print(\"refused\"),
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
      read_at: fn (path, offset, want) => Array::empty(),
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
      IoError::Denied(p) => print(\"failed\"),
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
  print(if reads.is_dir(dir)! {{ \"still there\" }} else {{ \"gone\" }});
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
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
  print(yes_no(reads.is_dir(dir)!));
  print(yes_no(reads.is_dir(file)!));
  print(yes_no(reads.is_dir(\"@DIR@/never\")!));
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
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
        SourceFile::new(&db, dir.join("permissions.kh"), std_source("permissions.kh")),
        SourceFile::new(&db, dir.join("grants.kh"), std_source("grants.kh")),
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

// --- streaming -------------------------------------------------------------
//
// The file under test is built by doubling a string until it is larger than
// one chunk, so the interesting case -- a line straddling a chunk boundary --
// is the ordinary case rather than one the test has to contrive.

/// Every byte, once. `fold_chunks` against `size` is the whole claim: chunks
/// that overlapped would count too many and chunks that skipped would count
/// too few, and only the right answer is the right answer.
#[test]
fn fold_chunks_sees_every_byte_exactly_once() {
    let ran = run(
        "fs_fold_chunks",
        &format!(
            "{HEAD}
fn doubled(text: String, times: Int) -> String {{
  if times == 0 {{ text }} else {{ doubled(\"${{text}}${{text}}\", times - 1) }}
}}

fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let path = \"@DIR@/big.txt\";
  write_text(path, doubled(\"a line of text that is long enough\\n\", 12))!;

  let counted = fold_chunks(path, 0, fn (n, chunk) => n + Array::length(chunk))!;
  print(Int::to_string(counted));
  print(Int::to_string(reads.size(path)!));
  print(if counted > chunk_size() {{ \"spans chunks\" }} else {{ \"one chunk only\" }});
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    let lines: Vec<&str> = ran.stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3, "{}", ran.stdout);
    assert_eq!(lines[0], lines[1], "the folded byte count must equal `size`");
    assert_eq!(lines[2], "spans chunks", "the fixture has to be bigger than one chunk");
    assert_eq!(ran.code, Some(0));
}

/// Lines are counted across chunk boundaries, which is the case a naive
/// implementation gets wrong by exactly the number of chunks.
#[test]
fn fold_lines_counts_across_chunk_boundaries() {
    let ran = run(
        "fs_fold_lines",
        &format!(
            "{HEAD}
fn doubled(text: String, times: Int) -> String {{
  if times == 0 {{ text }} else {{ doubled(\"${{text}}${{text}}\", times - 1) }}
}}

fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  let path = \"@DIR@/lines.txt\";
  // 2^12 copies of one line, so the count is known without counting.
  write_text(path, doubled(\"a line of text that is long enough\\n\", 12))!;
  print(Int::to_string(fold_lines(path, 0, fn (n, line) => n + 1)!));
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4096\n", "2^12 lines, none lost at a boundary");
    assert_eq!(ran.code, Some(0));
}

/// A file that ends mid-line still has a last line. Dropping it is the bug
/// this asserts against, and it is the one every buffered reader has had.
#[test]
fn a_file_without_a_trailing_newline_keeps_its_last_line() {
    let ran = run(
        "fs_last_line",
        &format!(
            "{HEAD}
fn work() -> () with {{ reads: FsRead, writes: FsWrite }} raises IoError {{
  write_text(\"@DIR@/tail.txt\", \"a\\nb\\nc\")!;
  print(Int::to_string(fold_lines(\"@DIR@/tail.txt\", 0, fn (n, line) => n + 1)!));
  print(fold_lines(\"@DIR@/tail.txt\", \"\", fn (acc, line) => line)!);

  // And `\\r\\n` reads the same as `\\n`.
  write_text(\"@DIR@/crlf.txt\", \"a\\r\\nb\\r\\n\")!;
  print(fold_lines(\"@DIR@/crlf.txt\", \"\", fn (acc, line) => \"${{acc}}[${{line}}]\")!);
}}

fn main() -> Int {{
  with {{ reads: FsRead::real(), writes: FsWrite::real() }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "3\nc\n[a][b]\n", "three lines, the last is `c`, and no stray `\\r`");
    assert_eq!(ran.code, Some(0));
}

/// **The reason the position is an argument.** A double for `read_at` needs
/// arithmetic and nothing else -- no handle, no opaque type, nothing only the
/// real implementation could make. That is the property the whole shape was
/// chosen for, so it gets a test.
#[test]
fn a_double_can_stream_without_a_handle() {
    let ran = run(
        "fs_stream_double",
        &format!(
            "{HEAD}
fn work() -> () with {{ reads: FsRead }} raises IoError {{
  print(Int::to_string(fold_chunks(\"anywhere\", 0, fn (n, chunk) => n + Array::length(chunk))!));
}}

fn main() -> Int {{
  with {{ reads: handler for FsRead {{
    read: fn path => String::bytes(\"\"),
    exists: fn path => true,
    size: fn path => 10,
    read_dir: fn path => List::Nil,
    is_dir: fn path => false,
    // Ten bytes in total, handed over four at a time. All it needs is `if`.
    read_at: fn (path, offset, want) => {{
      let left = 10 - offset;
      let n = if left < 4 {{ left }} else {{ 4 }};
      let bytes: Array<U8> = Array::new(if n < 0 {{ 0 }} else {{ n }}, 65);
      bytes
    }},
  }} }} {{
    work()! catch {{
      IoError::NotFound(p) => print(\"missing\"),
      IoError::Failed(p) => print(\"failed\"),
      IoError::Denied(p) => print(\"failed\"),
    }};
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "10\n", "4 + 4 + 2, from a double that knows only arithmetic");
    assert_eq!(ran.code, Some(0));
}


/// **`read = ["./data/**"]` did not grant `data/foo.txt`.**
///
/// The `./` was significant on both sides: a grant and a request were compared
/// as strings after only `\` → `/`, so the example in the capabilities guide
/// wrote a manifest line that looks like it says yes and refuses. Two readers
/// hit it independently, which is what made it a trap rather than a surprise.
///
/// A `.` segment is dropped now, on both sides, so all four spellings agree.
///
/// **`..` is still not resolved**, and that is deliberate: dropping `a/../b`
/// to `b` is only correct if `a` exists and is a directory rather than a link,
/// which is a question about the filesystem — and a normalizer that guessed
/// would *widen* a grant, which is the one direction a permission check must
/// never be wrong in.
#[test]
fn a_dot_segment_does_not_change_what_a_grant_covers() {
    let ran = run(
        "fs_grant_dots",
        &format!(
            "{HEAD}
import std::permissions::{{granted, normalized}};

fn say(grant: String, path: String) -> () {{
  let ok = granted(List::Cons(grant, List::Nil), path);
  print(if ok {{ \"granted\" }} else {{ \"refused\" }})
}}

pub fn main() -> Int {{
  // The four spellings of the same grant and the same path.
  say(\"./data/**\", \"data/foo.txt\");
  say(\"data/**\", \"./data/foo.txt\");
  say(\"data/**\", \"data/foo.txt\");
  say(\"./data/**\", \"./data/foo.txt\");
  // Separators still level, which is the half that already worked.
  say(\"data/**\", \"data\\\\foo.txt\");
  // And the two that must still be refused.
  say(\"data/**\", \"a/../data/foo.txt\");
  say(\"data/**\", \"other/foo.txt\");
  // The normalizer's own edges: a repeated `.`, a bare `.` that names the
  // current directory and must stay something, and an absolute path.
  print(\"[\" + normalized(\"a/././b\") + \"]\");
  print(\"[\" + normalized(\".\") + \"]\");
  print(\"[\" + normalized(\"/abs/./path\") + \"]\");
  0
}}
"
        ),
    );

    assert_eq!(
        ran.stdout,
        "granted\ngranted\ngranted\ngranted\ngranted\nrefused\nrefused\n\
         [a/b]\n[.]\n[/abs/path]\n"
    );
}
