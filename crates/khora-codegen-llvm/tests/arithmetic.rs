#![cfg(feature = "llvm")]

//! Integer arithmetic, and what it does when it does not fit.
//!
//! **Overflow traps, in every build.** Swift's answer rather than Rust's: a
//! program that passes its tests and then wraps in production is the failure
//! worth spending a branch to prevent, and two behaviours — one for testing,
//! one for shipping — put the difference exactly where it is most expensive to
//! find. `docs/roadmap.md` 6.2.
//!
//! Wrapping is still reachable, by name, for the places that genuinely want it.

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

const INT: &str = "module t;
fn print(value: Int);

impl Int {
  fn wrapping_add(self, other: Int) -> Int;
  fn wrapping_sub(self, other: Int) -> Int;
  fn wrapping_mul(self, other: Int) -> Int;
  fn xor(self, other: Int) -> Int;
  fn and(self, other: Int) -> Int;
  fn or(self, other: Int) -> Int;
  fn shl(self, other: Int) -> Int;
  fn shr(self, other: Int) -> Int;
}
";

/// Arithmetic that fits is arithmetic. The check is a branch, not a change of
/// answer.
#[test]
fn arithmetic_that_fits_is_unchanged() {
    let ran = run(
        "int_ordinary",
        &format!(
            "{INT}
fn main() -> Int {{
  print(2 + 3);
  print(10 - 4);
  print(6 * 7);
  print(0 - 5 + 2);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n6\n42\n-3\n");
    assert_eq!(ran.code, Some(0));
}

/// The decision. Everything before the overflow ran; nothing after it did.
#[test]
fn addition_that_overflows_stops_the_program() {
    let ran = run(
        "int_add_overflow",
        &format!(
            "{INT}
fn main() -> Int {{
  let big = 9223372036854775807;
  print(big - 1);
  print(big + 1);
  print(0);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "9223372036854775806\n", "nothing after the overflow ran");
    assert_ne!(ran.code, Some(0));
}

#[test]
fn multiplication_that_overflows_stops_the_program() {
    let ran = run(
        "int_mul_overflow",
        &format!(
            "{INT}
fn main() -> Int {{
  print(4611686018427387904 * 4);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "");
    assert_ne!(ran.code, Some(0));
}

#[test]
fn subtraction_that_overflows_stops_the_program() {
    let ran = run(
        "int_sub_overflow",
        &format!(
            "{INT}
fn main() -> Int {{
  let small = 0 - 9223372036854775807;
  print(small - 2);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "");
    assert_ne!(ran.code, Some(0));
}

/// And the way out, for the places that want it. Same expression, asked for by
/// name, does not stop.
#[test]
fn wrapping_arithmetic_wraps_instead() {
    let ran = run(
        "int_wrapping",
        &format!(
            "{INT}
fn main() -> Int {{
  let big = 9223372036854775807;
  print(Int::wrapping_add(big, 1));
  print(Int::wrapping_mul(big, 3));
  print(Int::wrapping_sub(0 - big, 2));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-9223372036854775808\n9223372036854775805\n9223372036854775807\n",
        "each one wrapped rather than stopping"
    );
    assert_eq!(ran.code, Some(0));
}

/// The bits underneath, which is what a hash is made of.
#[test]
fn the_bit_operations_do_what_they_say() {
    let ran = run(
        "int_bits",
        &format!(
            "{INT}
fn main() -> Int {{
  print(Int::and(12, 10));
  print(Int::or(12, 10));
  print(Int::xor(12, 10));
  print(Int::shl(1, 10));
  print(Int::shr(1024, 3));
  print(Int::shr(0 - 16, 2));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "8\n14\n6\n1024\n128\n-4\n",
        "the last one is arithmetic: a negative number stays negative"
    );
    assert_eq!(ran.code, Some(0));
}
