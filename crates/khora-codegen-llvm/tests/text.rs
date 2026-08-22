#![cfg(feature = "llvm")]

//! Text handling, written in Khora.
//!
//! Slicing, searching, splitting and writing a number out — all of it in
//! `std::core`, over `String::byte` and `Array<U8>`, with no intrinsic behind
//! any of it. That is the point: if slicing a string needed the compiler's
//! help, so would everything above it.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn run(name: &str, main: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main.to_string()),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

const HEAD: &str = "module demo::main;
import std::core::{Array, Option, Split};

fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;
";

#[test]
fn a_number_can_be_written_out() {
    let out = run(
        "text_to_string",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(Int::to_string(0));
  print(Int::to_string(7));
  print(Int::to_string(1536));
  print(Int::to_string(0 - 42));
  print(Int::to_string(9223372036854775807));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "0\n7\n1536\n-42\n9223372036854775807\n0\n");
}

#[test]
fn a_string_can_be_sliced() {
    let out = run(
        "text_slice",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(String::slice(\"hello, khora\", 0, 5));
  print(String::slice(\"hello, khora\", 7, 12));
  print(String::slice(\"hello\", 2, 2));
  print(String::slice(\"hello\", 3, 999));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "hello\nkhora\n\nlo\n0\n", "the third is empty, the fourth clamped");
}

#[test]
fn a_string_can_be_searched() {
    let out = run(
        "text_search",
        &format!(
            "{HEAD}
fn main() -> Int {{
  khora_print_int(String::index_of(\"GET /analyze HTTP/1.1\", \" \").unwrap_or(0 - 1));
  khora_print_int(String::index_of(\"hello\", \"llo\").unwrap_or(0 - 1));
  khora_print_int(String::index_of(\"hello\", \"nope\").unwrap_or(0 - 1));
  khora_print_int(if String::starts_with(\"GET /x\", \"GET \") {{ 1 }} else {{ 0 }});
  khora_print_int(if String::starts_with(\"GET /x\", \"POST\") {{ 1 }} else {{ 0 }});
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "3\n2\n-1\n1\n0\n0\n");
}

/// The shape an HTTP request line wants: head, rest, repeat.
#[test]
fn a_string_can_be_split_once() {
    let out = run(
        "text_split",
        &format!(
            "{HEAD}
fn main() -> Int {{
  match String::split_once(\"GET /analyze/acc_1 HTTP/1.1\", \" \") {{
    Option::None => print(\"no\"),
    Option::Some(parts) => {{
      print(parts.head);
      print(parts.rest);
    }}
  }};
  match String::split_once(\"nospaces\", \" \") {{
    Option::None => print(\"absent\"),
    Option::Some(parts) => print(parts.head),
  }};
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "GET\n/analyze/acc_1 HTTP/1.1\nabsent\n0\n");
}
