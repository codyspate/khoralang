#![cfg(feature = "llvm")]

//! The fixed-width integers: `U8` through `I32`, and what makes them different
//! from `Int`.
//!
//! Three properties are worth a test each, because each one is silent when it
//! is wrong: arithmetic traps at the *type's* range rather than at 64 bits,
//! comparison follows the type's signedness, and a conversion either fits or
//! stops. `docs/design/numbers.md`.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn build(name: &str, source: &str) -> Result<PathBuf, Vec<String>> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => Ok(exe),
        Err(errors) => Err(errors.iter().map(|e| e.message.clone()).collect()),
    }
}

fn run(name: &str, source: &str) -> Ran {
    let exe = match build(name, source) {
        Ok(exe) => exe,
        Err(messages) => {
            panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "))
        }
    };
    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

/// Everything below needs the same preamble: the intrinsics are declared, not
/// defined, and `print` only knows how to print an `Int`.
const HEAD: &str = "module t;
fn print(value: Int);

pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
  fn get(self, index: Int) -> A;
  fn set(self, index: Int, value: A) -> ();
}

impl U8 {
  fn of(value: Int) -> U8;
  fn wrapping(value: Int) -> U8;
  fn to_int(self) -> Int;
  fn wrapping_add(self, other: U8) -> U8;
  fn wrapping_mul(self, other: U8) -> U8;
  fn xor(self, other: U8) -> U8;
  fn and(self, other: U8) -> U8;
  fn shl(self, other: U8) -> U8;
  fn shr(self, other: U8) -> U8;
}

impl U32 {
  fn of(value: Int) -> U32;
  fn wrapping(value: Int) -> U32;
  fn to_int(self) -> Int;
  fn wrapping_add(self, other: U32) -> U32;
  fn wrapping_mul(self, other: U32) -> U32;
  fn xor(self, other: U32) -> U32;
  fn shr(self, other: U32) -> U32;
}

impl U64 {
  fn of(value: Int) -> U64;
  fn wrapping(value: Int) -> U64;
  fn to_int(self) -> Int;
  fn wrapping_to_int(self) -> Int;
  fn shr(self, other: U64) -> U64;
}

impl U16 {
  fn of(value: Int) -> U16;
  fn to_int(self) -> Int;
}

impl I8 {
  fn of(value: Int) -> I8;
  fn wrapping(value: Int) -> I8;
  fn to_int(self) -> Int;
  fn shr(self, other: I8) -> I8;
}
";

/// A literal takes the type being asked of it. Without this every byte in a
/// table would be a conversion, which is the difference between a language
/// with bytes and a language that can describe them.
#[test]
fn a_literal_becomes_whatever_is_being_asked_for() {
    let ran = run(
        "fixed_literal",
        &format!(
            "{HEAD}
fn takes(byte: U8) -> Int {{ U8::to_int(byte) }}

fn main() -> Int {{
  let b: U8 = 65;
  print(U8::to_int(b));
  print(takes(200));
  let sum: U8 = 1 + 2;
  print(U8::to_int(sum));
  let branch: U8 = if b == 65 {{ 7 }} else {{ 8 }};
  print(U8::to_int(branch));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "65\n200\n3\n7\n");
    assert_eq!(ran.code, Some(0));
}

/// The hint reaches a literal and stops there. `array[0]` is indexed by an
/// `Int` however the element is used, and a literal in an unrelated position
/// must not pick up a type from a `let` three lines up.
#[test]
fn the_hint_does_not_leak_into_an_index() {
    let ran = run(
        "fixed_hint_scope",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let cells: Array<U8> = Array::new(4, 0);
  Array::set(cells, 2, 200);
  let read: U8 = Array::get(cells, 2);
  print(U8::to_int(read));
  print(Array::length(cells));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "200\n4\n");
    assert_eq!(ran.code, Some(0));
}

/// A literal that cannot be the type being asked of it is a compile error, not
/// a truncation. The same decision as trapping on overflow, made earlier.
#[test]
fn a_literal_that_does_not_fit_is_refused() {
    let errors = build(
        "fixed_literal_too_big",
        &format!("{HEAD}fn main() -> Int {{ let b: U8 = 300; U8::to_int(b) }}\n"),
    )
    .expect_err("300 is not a U8");
    assert!(
        errors.iter().any(|e| e.contains("does not fit in `U8`") && e.contains("0 to 255")),
        "{errors:?}"
    );
}

/// The point of the width. A `U8` addition traps at 255, not at 2^63 — a check
/// against the wrong range is worth nothing.
#[test]
fn arithmetic_traps_at_the_types_own_range() {
    let ran = run(
        "fixed_overflow",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let b: U8 = 200;
  print(U8::to_int(b + 55));
  print(U8::to_int(b + 56));
  print(0);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "255\n", "255 fits; 256 does not, and nothing after it ran");
    assert_ne!(ran.code, Some(0));
}

/// And the way out, by name.
#[test]
fn wrapping_arithmetic_wraps_at_the_types_own_range() {
    let ran = run(
        "fixed_wrapping",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let b: U8 = 200;
  print(U8::to_int(U8::wrapping_add(b, 56)));
  print(U8::to_int(U8::wrapping_mul(b, 3)));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n88\n");
    assert_eq!(ran.code, Some(0));
}

/// Unsigned means unsigned all the way down. `200 < 100` has to be false for a
/// `U8`, and `>>` has to bring in zeros — signing either one is silent and
/// wrong.
#[test]
fn an_unsigned_type_compares_and_shifts_unsigned() {
    let ran = run(
        "fixed_unsigned",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let big: U8 = 200;
  let small: U8 = 100;
  print(if big < small {{ 1 }} else {{ 0 }});
  print(if big > small {{ 1 }} else {{ 0 }});
  print(U8::to_int(U8::shr(big, 1)));
  let negative: I8 = -100;
  print(I8::to_int(I8::shr(negative, 1)));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "0\n1\n100\n-50\n",
        "the U8 shift brought in a zero; the I8 shift kept the sign"
    );
    assert_eq!(ran.code, Some(0));
}

/// A conversion that does not fit stops, and the one that does not check says
/// so in its name.
#[test]
fn a_narrowing_conversion_is_checked() {
    let fits = run(
        "fixed_convert_ok",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(U8::to_int(U8::of(255)));
  print(U8::to_int(U8::wrapping(300)));
  print(I8::to_int(I8::of(0 - 128)));
  print(U32::to_int(U32::of(4294967295)));
  0
}}
"
        ),
    );
    assert_eq!(fits.stdout, "255\n44\n-128\n4294967295\n");
    assert_eq!(fits.code, Some(0));

    let does_not = run(
        "fixed_convert_bad",
        &format!("{HEAD}fn main() -> Int {{ print(U8::to_int(U8::of(256))); 0 }}\n"),
    );
    assert_eq!(does_not.stdout, "");
    assert_ne!(does_not.code, Some(0), "256 is not a U8 and saying so at run time is the point");
}

/// `U64` is the only type here holding numbers `Int` cannot, so it is the only
/// one whose *widening* conversion can fail.
#[test]
fn a_u64_above_ints_maximum_is_the_one_widening_that_checks() {
    let ran = run(
        "fixed_u64",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let huge = U64::wrapping(0 - 1);
  print(U64::wrapping_to_int(huge));
  print(U64::to_int(U64::shr(huge, 1)));
  print(U64::to_int(huge));
  print(0);
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-1\n9223372036854775807\n",
        "the bits reinterpret fine and half of it fits; the whole of it does not"
    );
    assert_ne!(ran.code, Some(0));
}

/// The thing all of this was for: a hash over bytes that mixes properly,
/// written in Khora.
#[test]
fn a_byte_hash_can_be_written_in_khora() {
    let ran = run(
        "fixed_hash",
        &format!(
            "{HEAD}
/// FNV-1a, which is one multiply and one xor per byte and needs both
/// wrapping arithmetic and an unsigned shift to be itself.
fn hash(bytes: Array<U8>, index: Int, seed: U32) -> U32 {{
  if index >= Array::length(bytes) {{
    seed
  }} else {{
    let byte: U8 = Array::get(bytes, index);
    let mixed = U32::xor(seed, U32::of(U8::to_int(byte)));
    hash(bytes, index + 1, U32::wrapping_mul(mixed, 16777619))
  }}
}}

fn main() -> Int {{
  let bytes: Array<U8> = Array::new(3, 0);
  Array::set(bytes, 0, 107);
  Array::set(bytes, 1, 104);
  Array::set(bytes, 2, 111);
  let h = hash(bytes, 0, U32::of(2166136261));
  print(U32::to_int(h));
  print(U32::to_int(U32::shr(h, 24)));
  0
}}
"
        ),
    );
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{:?}", ran.stdout);
    let whole: i64 = lines[0].parse().expect("a number");
    let top: i64 = lines[1].parse().expect("a number");
    assert!(whole > 0 && whole <= u32::MAX.into(), "a U32 came back as {whole}");
    assert_eq!(top, whole >> 24, "the shift is logical, so the top byte is just the top byte");
    assert_eq!(ran.code, Some(0));
}

/// The point of a byte array: a byte per element.
///
/// Measured rather than asserted in a comment. `khora_alloc_count` cannot see
/// sizes, so this reads the array's own header — the length, and the stride the
/// allocator was told — which is the same pair the runtime uses to walk it. An
/// `Array<U8>` of a thousand elements is a thousand bytes plus a header, and a
/// word per byte would be the difference between a byte buffer and a joke.
#[test]
fn a_byte_array_is_packed() {
    let ran = run(
        "fixed_packed",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let bytes: Array<U8> = Array::new(1000, 0);
  Array::set(bytes, 0, 1);
  Array::set(bytes, 1, 2);
  Array::set(bytes, 999, 255);
  print(U8::to_int(Array::get(bytes, 0)));
  print(U8::to_int(Array::get(bytes, 1)));
  print(U8::to_int(Array::get(bytes, 999)));
  print(U8::to_int(Array::get(bytes, 500)));
  print(Array::length(bytes));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "1\n2\n255\n0\n1000\n",
        "neighbouring bytes do not overwrite each other, and the untouched ones are the fill"
    );
    assert_eq!(ran.code, Some(0));
}

/// Every width addresses its own elements, and none of them tread on the next.
/// A stride that is right for one width and wrong for another is a bug that
/// only the wrong width finds.
#[test]
fn every_width_addresses_its_own_elements() {
    let ran = run(
        "fixed_strides",
        &format!(
            "{HEAD}
fn main() -> Int {{
  let halves: Array<U16> = Array::new(3, 0);
  Array::set(halves, 0, 65535);
  Array::set(halves, 1, 1);
  print(U16::to_int(Array::get(halves, 0)));
  print(U16::to_int(Array::get(halves, 1)));

  let quarters: Array<U32> = Array::new(3, 0);
  Array::set(quarters, 1, 4294967295);
  Array::set(quarters, 2, 7);
  print(U32::to_int(Array::get(quarters, 1)));
  print(U32::to_int(Array::get(quarters, 2)));
  print(U32::to_int(Array::get(quarters, 0)));

  let signed: Array<I8> = Array::new(2, 0);
  Array::set(signed, 0, -128);
  Array::set(signed, 1, 127);
  print(I8::to_int(Array::get(signed, 0)));
  print(I8::to_int(Array::get(signed, 1)));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout,
        "65535\n1\n4294967295\n7\n0\n-128\n127\n"
    );
    assert_eq!(ran.code, Some(0));
}

// --- the traits, against the real `std` ------------------------------------

/// Every `.kh` file of `std`, plus the program under test.
///
/// The prelude above declares its own `U8` so the tests before this one can
/// pin the *backend's* behaviour without `std` in the way. This claim is about
/// what `std` ships, so it has to compile against `std`.
fn std_sources(
    db: &KhoraDatabase,
    dir: &std::path::Path,
    main: &str,
) -> Vec<khora_db::SourceFile> {
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
                out.push(khora_db::SourceFile::new(db, path, text));
            }
        }
    }
    out.push(khora_db::SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

fn with_std(name: &str, main: &str) -> String {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = khora_db::SourceRoot::new(&db, std_sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }
    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "{name} exited badly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// `Eq`, `Ord`, `Show` and `Hash` reach the fixed-width integers.
///
/// **The operators worked and the traits did not, which is the worse of the
/// two gaps.** `a == b` on two `U8`s has always compiled, so nothing looked
/// missing until something *generic* needed it: `List::contains`, a sort, a
/// `${..}` hole. An agent writing a tokenizer against this reached for
/// `byte.to_int() == 34` and a block of named ASCII constants, which is a
/// language telling you to work around it.
///
/// Asked through the traits by name rather than with operators, because it is
/// the traits that were absent and an operator would pass either way.
#[test]
fn the_fixed_widths_have_eq_ord_show_and_hash() {
    let out = with_std(
        "fixed_traits",
        "module main;
import std::core::{Eq, Hash, List, Ord, Ordering, Show, print};

pub fn main() -> Int {
  let quote = U8::of(34);
  let bytes = [U8::of(104), U8::of(105), U8::of(34)];
  print(\"show ${quote}\");
  print(\"eq ${Eq::eq(quote, U8::of(34))}\");
  print(\"contains ${List::contains(bytes, quote)}\");
  print(\"cmp ${Ord::cmp(U8::of(1), U8::of(2)) == Ordering::Less}\");
  print(\"hash ${Hash::hash(quote)}\");
  print(\"sorted ${List::sort_by(bytes, fn (a, b) => Ord::cmp(a, b))}\");
  print(\"i32 ${Eq::eq(I32::of(0 - 7), I32::of(0 - 7))} ${I32::of(0 - 7)}\");
  0
}
",
    );
    assert_eq!(
        out,
        "show 34\neq true\ncontains true\ncmp true\nhash 34\nsorted [34, 104, 105]\ni32 true -7\n"
    );
}
