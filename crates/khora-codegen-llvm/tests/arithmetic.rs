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

// --- floats ----------------------------------------------------------------

const FLOAT: &str = "module t;
fn print(value: Float);
extern fn khora_print_int(value: Int);
";

/// IEEE, which is what every reader expects and is why `Float` implements
/// neither `Eq` nor `Ord` — see `docs/design/numbers.md`.
#[test]
fn float_arithmetic_is_ieee() {
    let ran = run(
        "float_ieee",
        &format!(
            "{FLOAT}
fn main() -> Int {{
  print(0.1 + 0.2);
  print(3.0 * 2.5);
  print(1.0 / 4.0);
  print(1.5 - 2.0);
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "0.30000000000000004\n7.5\n0.25\n-0.5\n",
        "the first one is the whole point: 0.1 + 0.2 is not 0.3"
    );
    assert_eq!(ran.code, Some(0));
}

#[test]
fn floats_compare_by_ieee_rules() {
    let ran = run(
        "float_compare",
        &format!(
            "{FLOAT}
fn main() -> Int {{
  khora_print_int(if 0.1 + 0.2 == 0.3 {{ 1 }} else {{ 0 }});
  khora_print_int(if 0.5 == 0.5 {{ 1 }} else {{ 0 }});
  khora_print_int(if 2.5 < 3.0 {{ 1 }} else {{ 0 }});
  khora_print_int(if 2.5 >= 2.5 {{ 1 }} else {{ 0 }});
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n1\n1\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// Floats do not overflow — they reach infinity — so there is nothing to trap
/// on and nothing to check. The opposite of the integer rule, and for a reason
/// rather than an oversight.
#[test]
fn floats_do_not_trap() {
    let ran = run(
        "float_no_trap",
        &format!(
            "{FLOAT}
fn main() -> Int {{
  let huge = 179769313486231570000000000000000000000.0;
  print(huge * huge * huge * huge * huge * huge * huge * huge * huge);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "inf\n", "reached infinity rather than stopping");
    assert_eq!(ran.code, Some(0));
}

/// No mixing and no promotion. `1 + 2.0` is an error rather than a silent
/// conversion, which is what stops a rounding surprise from being invisible.
#[test]
fn an_int_and_a_float_do_not_mix() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("float_mix");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let file = SourceFile::new(
        &db,
        dir.join("main.kh"),
        format!("{FLOAT}fn main() -> Int {{ print(1 + 2.0); 0 }}\n"),
    );
    let root = SourceRoot::new(&db, vec![file]);

    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("mixing an Int and a Float should be refused");
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    assert!(messages.iter().any(|m| m.contains("arithmetic")), "{messages:?}");
}

/// Negation is over whatever is being negated, and a negated literal is one
/// number rather than a negation applied to another — which is the only way
/// `I8`'s smallest value can be written at all.
#[test]
fn negation_follows_the_type_it_negates() {
    let ran = run(
        "neg_typed",
        &format!(
            "{FLOAT}
fn main() -> Int {{
  print(-1.5);
  print(-0.0 - 2.5);
  khora_print_int(-7);
  khora_print_int(- (3 + 4));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1.5\n-2.5\n-7\n-7\n");
    assert_eq!(ran.code, Some(0));
}

/// Both ways an integer division can go wrong are undefined in LLVM, and what
/// they do on hardware is a fault with no message — a bare 0xC0000094 or a
/// SIGFPE, which says nothing about which line or which value. Checked for the
/// same reason overflow is.
#[test]
fn division_that_cannot_work_stops_the_program() {
    let ordinary = run(
        "div_ordinary",
        &format!(
            "{INT}
fn main() -> Int {{
  print(7 / 2);
  print(0 - 7 / 2);
  print(7 % 2);
  print(0 - 7 % 2);
  0
}}
"
        ),
    );
    assert_eq!(ordinary.stdout, "3\n-3\n1\n-1\n", "truncating toward zero, as C and Rust do");
    assert_eq!(ordinary.code, Some(0));

    let by_zero = run(
        "div_zero",
        &format!("{INT}fn main() -> Int {{ let d = 0; print(1); print(7 / d); 0 }}\n"),
    );
    assert_eq!(by_zero.stdout, "1\n", "nothing after the division ran");
    assert_ne!(by_zero.code, Some(0));

    let remainder_by_zero = run(
        "rem_zero",
        &format!("{INT}fn main() -> Int {{ let d = 0; print(7 % d); 0 }}\n"),
    );
    assert_eq!(remainder_by_zero.stdout, "");
    assert_ne!(remainder_by_zero.code, Some(0));

    // The one other pair: the minimum over minus one, whose quotient is one
    // past the maximum.
    let overflowing = run(
        "div_overflow",
        &format!(
            "{INT}
fn main() -> Int {{
  let small = 0 - 9223372036854775807 - 1;
  let minus_one = 0 - 1;
  print(small / minus_one);
  0
}}
"
        ),
    );
    assert_eq!(overflowing.stdout, "");
    assert_ne!(overflowing.code, Some(0));
}


/// **`a.xor(3)` did not compile while `Int::xor(a, 3)` did.**
///
/// The method spelling looks up an intrinsic by the receiver's type, and the
/// lookup only knew `String` and declared types — so a receiver of `Int`,
/// `U8` or `Float` fell through to "resolve it to a body", and these eight
/// have none. What came back was
///
/// ```text
/// error: `#Int::xor` has no body, so there is nothing to call. Give it one,
///        or write `extern fn` if it is a C symbol to be found at link time
/// ```
///
/// which is advice nobody can take about a function in `std`. The reference
/// documents all eight as `self` methods, so the `self` was a lie for exactly
/// the operations a hash or a checksum is written out of — and it drove the
/// first of four people writing their first Khora program into the compiler's
/// own test files to find the working call form.
///
/// Both spellings, side by side, because the fix must not trade one for the
/// other.
#[test]
fn the_bit_operations_take_both_spellings() {
    let ran = run(
        "int_bits_method",
        &format!(
            "{INT}
fn main() -> Int {{
  let a = 12;
  print(a.xor(10));
  print(a.and(10));
  print(a.or(10));
  print(a.shl(2));
  print(a.shr(2));
  print(a.wrapping_add(1));
  print(a.wrapping_sub(1));
  print(a.wrapping_mul(3));
  // And the namespaced form still reaches the same operation.
  print(Int::xor(12, 10));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "6\n8\n14\n48\n3\n13\n11\n36\n6\n");
    assert_eq!(ran.code, Some(0));
}
