#![cfg(feature = "llvm")]

//! A string's bytes.
//!
//! Length and index are both in *bytes*, which is what a `String` is made of —
//! it is UTF-8, so a character is one to four of them, and a `length` that
//! quietly meant one of the two would be wrong for half its callers.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

const HEAD: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
  fn get(self, index: Int) -> A;
  fn set(self, index: Int, value: A) -> ();
}

impl String {
  fn byte_length(self) -> Int;
  fn byte(self, index: Int) -> U8;
  fn bytes(self) -> Array<U8>;
}

impl U8 {
  fn to_int(self) -> Int;
}

impl U32 {
  fn of(value: Int) -> U32;
  fn to_int(self) -> Int;
  fn wrapping_mul(self, other: U32) -> U32;
  fn xor(self, other: U32) -> U32;
}
";

/// Bytes, not characters — which is the whole reason the name says so. `é` is
/// two bytes and one character, and a test that only used ASCII would let the
/// wrong answer through.
#[test]
fn a_strings_length_is_in_bytes() {
    let ran = run(
        "str_len",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(String::byte_length(\"khora\"));
  print(String::byte_length(\"\"));
  print(String::byte_length(\"café\"));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n0\n5\n", "the last one is four characters and five bytes");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn a_byte_can_be_read_by_index() {
    let ran = run(
        "str_byte",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let text = \"khora\";
  print(U8::to_int(String::byte(text, 0)));
  print(U8::to_int(String::byte(text, 4)));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "107\n97\n", "`k` and `a`");
    assert_eq!(ran.code, Some(0));
}

/// The same rule as an array, for the same reason: reading past the end is a
/// wrong program and the useful thing to do is say where.
#[test]
fn a_byte_index_outside_the_string_stops_the_program() {
    let ran = run(
        "str_byte_out",
        &format!("{HEAD}fn main() -> Int {{ print(U8::to_int(String::byte(\"ab\", 2))); 0 }}\n"),
    );
    assert_eq!(ran.stdout, "");
    assert_ne!(ran.code, Some(0));
}

/// A copy, into a packed array, and both objects are released afterwards.
#[test]
fn the_bytes_come_back_as_an_array() {
    let ran = run(
        "str_bytes",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let cells = String::bytes(\"khora\");
  print(Array::length(cells));
  print(U8::to_int(Array::get(cells, 0)));
  print(U8::to_int(Array::get(cells, 4)));
  print(Array::length(String::bytes(\"\")));
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n107\n97\n0\n1\n",
        "the one still live is `cells` itself, which main has not finished with");
    assert_eq!(ran.code, Some(0));
}

/// What all of it was for. A string hash, written in Khora, with no `Int` key
/// standing in for one.
#[test]
fn a_string_can_be_hashed_in_khora() {
    let ran = run(
        "str_hash",
        &format!(
            "{HEAD}
fn step(text: String, index: Int, seed: U32) -> U32 {{
  if index >= String::byte_length(text) {{
    seed
  }} else {{
    let mixed = U32::xor(seed, U32::of(U8::to_int(String::byte(text, index))));
    step(text, index + 1, U32::wrapping_mul(mixed, 16777619))
  }}
}}

fn hash(text: String) -> U32 {{ step(text, 0, U32::of(2166136261)) }}

fn main() -> Int {{
  print(if U32::to_int(hash(\"khora\")) == U32::to_int(hash(\"khora\")) {{ 1 }} else {{ 0 }});
  print(if U32::to_int(hash(\"khora\")) == U32::to_int(hash(\"khorb\")) {{ 1 }} else {{ 0 }});
  print(U32::to_int(hash(\"\")));
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n0\n2166136261\n0\n",
        "equal strings hash equal, near-equal ones do not, and nothing leaked"
    );
    assert_eq!(ran.code, Some(0));
}

/// `+` on two strings. Generated rather than a runtime call, so this is the
/// test that the layout claim on both sides is the same claim.
#[test]
fn two_strings_can_be_joined() {
    let ran = run(
        "str_concat",
        &format!(
            "{HEAD}
fn shout(name: String) -> String {{ \"hello, \" + name + \"!\" }}

fn main() -> Int {{
  let greeting = shout(\"khora\");
  print(String::byte_length(greeting));
  print(U8::to_int(String::byte(greeting, 0)));
  print(U8::to_int(String::byte(greeting, 7)));
  print(U8::to_int(String::byte(greeting, 12)));
  print(String::byte_length(\"\" + \"\"));
  print(String::byte_length(\"\" + \"x\"));
  print(khora_live_count());
  0
}}
"
        ),
    );
    // The trailing 1 is the live-object count. `greeting`'s last read is a
    // `String::byte`, which only borrows — a borrow cannot take the binding's
    // reference, so the block still releases it, after this count.
    assert_eq!(
        ran.stdout, "13\n104\n107\n33\n0\n1\n1\n",
        "`h`, then `k` where the second half starts, then the `!` at the end"
    );
    assert_eq!(ran.code, Some(0));
}

/// Joined strings compare by their bytes, not by where they came from.
#[test]
fn a_joined_string_equals_the_literal_it_spells() {
    let ran = run(
        "str_concat_eq",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(if \"kh\" + \"ora\" == \"khora\" {{ 1 }} else {{ 0 }});
  print(if \"kh\" + \"ora\" == \"khoral\" {{ 1 }} else {{ 0 }});
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n0\n");
    assert_eq!(ran.code, Some(0));
}
