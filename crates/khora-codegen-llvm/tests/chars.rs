#![cfg(feature = "llvm")]

//! Characters, end to end.
//!
//! A `Char` is one Unicode scalar value in thirty-two bits, written `'a'`. What
//! these pin is the part a program can see: the literal, the escapes, matching
//! on one, and the boundary API that makes `String::slice` safe to reach for.
//!
//! **The apostrophe is shared with row variables**, so `'a'` and `'a` compete
//! at every apostrophe in the file. The lexer breaks the tie by length and
//! `crates/khora-syntax/src/lexer.rs` has the tests for that; what is here is
//! the consequence — that both spellings still mean what they meant.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus one program.
///
/// The whole library rather than `core.kh` alone: `Char` is in `core` but
/// `Show` reaches through the rest, and a partial `std` fails in a way that
/// looks like the feature being tested.
fn std_and(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut out = Vec::new();
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
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out.push(SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

/// Compiles one program against `std` and returns what it printed.
fn built(name: &str, source: &str) -> (PathBuf, String) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, std_and(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    (exe, source.to_string())
}

fn run(name: &str, body: &str) -> String {
    let source = format!(
        "module demo::main;\nimport std::core::{{Eq, List, Option, Ord, Show, print}};\n\n{body}"
    );
    let (exe, _) = built(name, &source);
    let out = Command::new(&exe).output().expect("the program should run");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// **`chars_between` counts characters, and it advances.**
///
/// The first version was
///
/// ```khora
/// while at < stop { at = String::next_boundary(self, at); .. }
/// ```
///
/// which never ends, because `next_boundary` answers the boundary *at or
/// after* an offset and so answers where it already is. It was written by the
/// person who wrote `next_boundary`, it compiled, and it hung on the first
/// string with an accented letter in it — so the loop is what this pins, not
/// only the arithmetic.
#[test]
fn chars_between_counts_characters_and_terminates() {
    let out = run(
        "chars_between",
        r#"pub fn main() -> Int {
  let s = "café au lait";
  // "café" is five bytes and four characters.
  print(Int::to_string(String::chars_between(s, 0, 5)));
  print(Int::to_string(String::chars_between(s, 0, String::byte_length(s))));
  print(Int::to_string(String::char_length(s)));
  // An empty range, and one that starts past the end.
  print(Int::to_string(String::chars_between(s, 3, 3)));
  print(Int::to_string(String::chars_between(s, 0, 0)));
  // Ends that are not boundaries: `é` is bytes 3..5, so a range ending
  // inside it counts it rather than trapping or looping.
  print(Int::to_string(String::chars_between(s, 0, 4)));
  // Past the end is clamped.
  print(Int::to_string(String::chars_between(s, 0, 9999)));
  0
}
"#,
    );
    assert_eq!(out, "4\n12\n12\n0\n0\n4\n12\n");
}

/// **`Float::of_string` reads what a person writes and refuses the rest.**
///
/// It lived privately in `std::json` with a comment saying it belonged in
/// `core` as soon as something else needed one. `examples/khq` needed one.
///
/// Half of this is the refusals. A number parser that accepts more than the
/// caller expected is how a configuration value ends up meaning something
/// nobody wrote, and the shape here is JSON's exactly.
#[test]
fn a_float_can_be_read_from_text() {
    let out = run(
        "float_of_string",
        r#"fn show(text: String) -> String {
  match Float::of_string(text) {
    Option::Some(v) => Float::to_string(v),
    Option::None => "refused",
  }
}

pub fn main() -> Int {
  print(show("1"));
  print(show("-2.5"));
  print(show("1e3"));
  print(show("1.5e-2"));
  print(show("2E+2"));
  print(show("0.125"));
  print(show("1.2.3"));
  print(show(""));
  print(show("abc"));
  print(show("1."));
  print(show("1e"));
  print(show("+1"));
  print(show("1 2"));
  print(show(" 1"));
  print(show("1x"));
  print(show("inf"));
  0
}
"#,
    );
    assert_eq!(
        out,
        "1\n-2.5\n1000\n0.015\n200\n0.125\n\
         refused\nrefused\nrefused\nrefused\nrefused\n\
         refused\nrefused\nrefused\nrefused\nrefused\n"
    );
}

/// The literal, its escapes, and what `Show` makes of it.
#[test]
fn a_character_is_written_between_apostrophes() {
    let out = run(
        "char_literals",
        r#"pub fn main() -> Int {
  print(Show::show('a'));
  print(Show::show('é'));
  print(Show::show('\u{1F600}'));
  print(Show::show('\n') + "|");
  print(Int::to_string(Char::code('A')));
  0
}
"#,
    );
    assert_eq!(out, "a\né\n😀\n\n|\n65\n");
}

/// Matching on one is an integer comparison, and the arms are distinct.
#[test]
fn a_character_can_be_matched() {
    let out = run(
        "char_match",
        r#"fn name(c: Char) -> String {
  match c {
    'a' => "letter a",
    '0' => "zero",
    '\u{1F600}' => "a face",
    _ => "other",
  }
}

pub fn main() -> Int {
  print(name('a'));
  print(name('0'));
  print(name('\u{1F600}'));
  print(name('z'));
  0
}
"#,
    );
    assert_eq!(out, "letter a\nzero\na face\nother\n");
}

/// A row variable is still a row variable, in the same file as a literal.
///
/// The two spellings differ by a closing quote and nothing else, so a program
/// using both is the case worth compiling rather than reasoning about.
#[test]
fn a_row_variable_and_a_character_coexist() {
    let out = run(
        "char_and_row",
        r#"fn twice<'er>(body: () -> Int raises 'er) -> Int raises 'er {
  body()! + body()!
}

pub fn main() -> Int {
  print(Int::to_string(twice(fn () => Char::code('!'))));
  0
}
"#,
    );
    assert_eq!(out, "66\n");
}

/// **The crash the boundary API exists to prevent.**
///
/// `String::slice` counts bytes and stops the program when the cut lands
/// inside a character — right, but until `is_char_boundary` existed there was
/// no way to *ask*, so a program truncating text it did not write was one
/// non-ASCII input away from dying.
#[test]
fn a_cut_can_be_moved_to_a_boundary() {
    let out = run(
        "char_boundary",
        r#"pub fn main() -> Int {
  let text = "héllo";
  print(Int::to_string(String::byte_length(text)) + " bytes, "
        + Int::to_string(String::char_length(text)) + " chars");
  print(Show::show(String::is_char_boundary(text, 1)));
  print(Show::show(String::is_char_boundary(text, 2)));
  // Byte 2 is inside `é`; slicing there would stop the program.
  print("[" + String::slice(text, 0, String::next_boundary(text, 2)) + "]");
  print("[" + String::slice(text, 0, String::previous_boundary(text, 2)) + "]");
  0
}
"#,
    );
    assert_eq!(out, "6 bytes, 5 chars\ntrue\nfalse\n[hé]\n[h]\n");
}

/// Reading characters out, and the offsets that are not boundaries.
#[test]
fn a_string_hands_over_its_characters() {
    let out = run(
        "char_at",
        r#"pub fn main() -> Int {
  let text = "aé😀";
  print(Show::show(String::char_at(text, 0)));
  print(Show::show(String::char_at(text, 1)));
  print(Show::show(String::char_at(text, 2)));
  print(Show::show(String::char_at(text, 3)));
  print(Show::show(String::char_at(text, 99)));
  print(Show::show(String::chars(text)));
  0
}
"#,
    );
    assert_eq!(out, "Some(a)\nSome(é)\nNone\nSome(😀)\nNone\n[a, é, 😀]\n");
}

/// A number that is not a scalar value is not a character.
///
/// The surrogates are the interesting half: `0xD800` is a perfectly good
/// 32-bit number and exists only to encode a pair in UTF-16, so a `Char`
/// holding one would make a `String` that is not UTF-8 — a trap one layer
/// further from the mistake.
#[test]
fn a_surrogate_is_not_a_character() {
    let source = "module demo::main;\nimport std::core::{print};\n\n\
                  pub fn main() -> Int {\n  \
                    print(Char::to_string(Char::from_code(55296)));\n  0\n}\n";
    // It compiles: the range is a run-time fact, not one the checker knows.
    let (exe, _) = built("char_surrogate", source);
    let out = Command::new(&exe).output().expect("the program should run");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("not a Unicode scalar value"),
        "a surrogate is refused, and says why: {said}"
    );
    assert_ne!(out.status.code(), Some(0), "and the program does not carry on");
}
