#![cfg(feature = "llvm")]

//! Arrays, end to end.
//!
//! Fixed length, contiguous, bounds-checked. Fixed because growing is a library
//! question and this is the primitive it is built out of — the last test here
//! writes the vector, in Khora, out of an array and the mutable fields from
//! 6.1, which is the pair phase 6 exists to make possible.

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
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

const ARRAY: &str = "module t;
fn print(value: Int);
fn khora_live_count() -> Int;

export type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
  fn get(self, index: Int) -> A;
  fn set(self, index: Int, value: A) -> ();
}
";

#[test]
fn an_array_holds_what_was_written_to_it() {
    let ran = run(
        "array_write",
        &format!(
            "{ARRAY}
fn work() -> Int {{
  let a = Array::new(4, 0);
  Array::set(a, 0, 10);
  Array::set(a, 3, 40);
  print(Array::length(a));
  Array::get(a, 0) + Array::get(a, 3) + Array::get(a, 1)
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n50\n0\n", "the untouched slot still holds the fill");
    assert_eq!(ran.code, Some(0));
}

/// Every slot owns its element, so an array of boxed values releases all of
/// them. Getting this wrong leaks once per element and no other test notices.
#[test]
fn an_array_of_boxed_elements_releases_all_of_them() {
    let ran = run(
        "array_boxed",
        &format!(
            "{ARRAY}
fn work() -> Int {{
  let a = Array::new(3, \"empty\");
  Array::set(a, 1, \"filled\");
  Array::set(a, 1, \"filled again\");
  Array::length(a)
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// `a.set(i, a.get(i))` reads before it writes, and the read took a reference.
/// Storing before releasing is what stops it freeing what it just wrote.
#[test]
fn writing_an_element_back_to_itself_is_not_a_free() {
    let ran = run(
        "array_self",
        &format!(
            "{ARRAY}
fn work() -> Int {{
  let a = Array::new(2, \"kept\");
  Array::set(a, 0, Array::get(a, 0));
  Array::set(a, 1, Array::get(a, 0));
  Array::length(a)
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "2\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// An index outside the array stops the program and says which index and what
/// length, rather than reading whatever is next in memory.
#[test]
fn an_index_outside_the_array_stops_the_program() {
    let ran = run(
        "array_bounds",
        &format!(
            "{ARRAY}
fn main() -> Int {{
  let a = Array::new(3, 0);
  print(Array::get(a, 5));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "", "nothing was read");
    assert_ne!(ran.code, Some(0), "and the program did not carry on");
}

/// A negative index is out of range too, and by the same comparison: the check
/// is unsigned, so below zero is enormously above the length.
#[test]
fn a_negative_index_is_out_of_range() {
    let ran = run(
        "array_negative",
        &format!(
            "{ARRAY}
fn main() -> Int {{
  let a = Array::new(3, 0);
  print(Array::get(a, 0 - 1));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "");
    assert_ne!(ran.code, Some(0));
}

/// The pair phase 6 exists for: an array for the storage, a mutable field for
/// the length, and a growable vector written in Khora out of the two. Nothing
/// here is a compiler feature.
#[test]
fn a_vector_can_be_written_in_khora() {
    let ran = run(
        "array_vector",
        &format!(
            "{ARRAY}
export type Vec = {{ mut items: Array<Int>, mut len: Int }};

fn empty() -> Vec {{ {{ items: Array::new(2, 0), len: 0 }} }}

/// Doubles the storage and copies what was in it.
fn grow(v: Vec) -> () {{
  let bigger = Array::new(Array::length(v.items) * 2, 0);
  let mut i = 0;
  while i < v.len {{
    Array::set(bigger, i, Array::get(v.items, i));
    i = i + 1;
  }}
  v.items = bigger;
}}

fn push(v: Vec, value: Int) -> () {{
  if v.len == Array::length(v.items) {{ grow(v); }}
  Array::set(v.items, v.len, value);
  v.len = v.len + 1;
}}

fn sum(v: Vec) -> Int {{
  let mut total = 0;
  let mut i = 0;
  while i < v.len {{
    total = total + Array::get(v.items, i);
    i = i + 1;
  }}
  total
}}

fn work() -> Int {{
  let v = empty();
  let mut i = 1;
  while i <= 10 {{
    push(v, i);
    i = i + 1;
  }}
  print(v.len);
  print(Array::length(v.items));
  sum(v)
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(
        ran.stdout, "10\n16\n55\n0\n",
        "ten pushed, grown from two to sixteen, summing to 55, nothing left over"
    );
    assert_eq!(ran.code, Some(0));
}
