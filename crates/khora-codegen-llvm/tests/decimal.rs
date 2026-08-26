#![cfg(feature = "llvm")]

//! Exact decimal arithmetic, compiled and run.
//!
//! `docs/positioning.md` opens by claiming Khora suits financial
//! reconciliation, and until `std/decimal.kh` there was no way to write ten
//! pence. These are the tests that make the claim checkable rather than
//! stated: the first is the sum every language's floating point gets wrong,
//! and the rest are what a ledger actually does.

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

/// Compiles `body` as the whole of `main`, runs it, and gives back its output.
fn run(name: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, Option, Show, print}};
import std::decimal::{{Decimal, Rounding}};

/// `Option<Decimal>` has no `Show`, and adding one to `std` for a test would
/// be the test deciding a library question.
fn shown(value: Option<Decimal>) -> String {{
  match value {{
    Option::Some(number) => number.show(),
    Option::None => "None",
  }}
}}

fn main() -> () {{
{body}
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("decimal.kh"), std_source("decimal.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// [`run`], for a body that is *supposed* to stop the program.
///
/// Separate rather than a flag, because "this program ends badly" is a
/// different claim from "this program prints x" and a test should say which
/// one it is making. Answers what was printed before it stopped, and whether
/// it stopped.
fn run_until_it_stops(name: &str, body: &str) -> (String, bool) {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, Option, Show, print}};
import std::decimal::{{Decimal, Rounding}};

fn main() -> () {{
{body}
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("decimal.kh"), std_source("decimal.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    let printed = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    (printed, out.status.code() != Some(0))
}

/// **The sum that motivates the whole module.**
///
/// `0.1 + 0.2` is not `0.3` in IEEE 754, in any language, which is why
/// `docs/design/numbers.md` refuses `Eq` for `Float`. Here it is, and the
/// equality is one a ledger can be built on.
#[test]
fn a_tenth_and_a_fifth_are_exactly_three_tenths() {
    let out = run(
        "decimal_tenths",
        r#"  let sum = Decimal::add(Decimal::scaled(1, 1), Decimal::scaled(2, 1));
  print(sum.show());
  print(if sum == Decimal::scaled(3, 1) { "exact" } else { "not exact" });"#,
    );
    assert_eq!(out, "0.3\nexact\n");
}

/// Money keeps the places it was written with: `1.50` shows as `1.50`, because
/// a price to two places stays a price to two places and that is how a column
/// of them lines up.
#[test]
fn trailing_zeros_are_part_of_the_number() {
    let out = run(
        "decimal_places",
        r#"  print(Decimal::scaled(150, 2).show());
  print(Decimal::scaled(0 - 150, 2).show());
  print(Decimal::scaled(5, 3).show());
  print(Decimal::of_int(42).show());"#,
    );
    assert_eq!(out, "1.50\n-1.50\n0.005\n42\n");
}

/// **Equal by value, not by representation.** `1.0` and `1.00` are the same
/// number written twice, and a reconciliation that thought otherwise would
/// report a break where there is none.
#[test]
fn the_same_number_at_two_scales_is_equal() {
    let out = run(
        "decimal_scales",
        r#"  let a = Decimal::scaled(1, 0);
  let b = Decimal::scaled(100, 2);
  print(if a == b { "equal" } else { "different" });
  print(Decimal::sub(b, a).show());"#,
    );
    assert_eq!(out, "equal\n0.00\n");
}

/// A hundred pounds split three ways, to the penny, with the remainder
/// visible — which is what an allocation actually is.
#[test]
fn a_hundred_pounds_split_three_ways() {
    let out = run(
        "decimal_split",
        r#"  let total = Decimal::scaled(10000, 2);
  let three = Decimal::of_int(3);
  match Decimal::divide(total, three, 2, Rounding::HalfEven) {
    Option::Some(share) => {
      print(share.show());
      let paid = Decimal::rounded(Decimal::mul(share, three), 2, Rounding::HalfEven);
      print(paid.show());
      print(Decimal::sub(total, paid).show());
    },
    Option::None => print("no answer"),
  }"#,
    );
    assert_eq!(out, "33.33\n99.99\n0.01\n");
}

/// Half-to-even is the default because half-up biases a column upward, and
/// over a ledger that bias is money.
#[test]
fn rounding_modes_differ_where_it_matters() {
    let out = run(
        "decimal_rounding",
        r#"  let one = Decimal::of_int(1);
  print(shown(Decimal::divide(Decimal::scaled(125, 3), one, 2, Rounding::HalfEven)));
  print(shown(Decimal::divide(Decimal::scaled(135, 3), one, 2, Rounding::HalfEven)));
  print(shown(Decimal::divide(Decimal::scaled(125, 3), one, 2, Rounding::HalfUp)));
  print(shown(Decimal::divide(Decimal::scaled(125, 3), one, 2, Rounding::Towards)));"#,
    );
    assert_eq!(
        out,
        "0.12\n0.14\n0.13\n0.12\n",
        "half-to-even should send 0.125 down and 0.135 up"
    );
}

/// Dividing by zero is an answer, not a trap: data does that, and
/// `numbers.md` reserves stopping the program for a bug.
#[test]
fn dividing_by_zero_is_none() {
    let out = run(
        "decimal_zero",
        r#"  print(shown(Decimal::divide(Decimal::of_int(1), Decimal::zero(), 2, Rounding::HalfEven)));"#,
    );
    assert_eq!(out, "None\n");
}

/// Text in, exactly the number out — and text that is not a decimal refused
/// rather than guessed at. `1e-3` is refused on purpose: a number that arrived
/// in exponent notation has been through a float somewhere, and accepting it
/// here would launder that history.
#[test]
fn reading_a_decimal_from_text() {
    let out = run(
        "decimal_parse",
        r#"  print(shown(Decimal::of_string("12.34")));
  print(shown(Decimal::of_string("-0.05")));
  print(shown(Decimal::of_string("7")));
  print(shown(Decimal::of_string("1.2.3")));
  print(shown(Decimal::of_string("1e-3")));
  print(shown(Decimal::of_string("")));"#,
    );
    assert_eq!(out, "12.34\n-0.05\n7\nNone\nNone\nNone\n");
}

// --- the literal ------------------------------------------------------------
//
// 13.5. `docs/design/numbers.md` decides the spelling and `0.01` stays a
// `Float`: making bare decimals exact would be the most visible thing about
// the language and would make it a finance language whatever the
// documentation said.

/// The whole point of having a suffix: an exact decimal *constant*.
/// `Decimal::of("0.01")` parses at run time, costs something at every
/// evaluation, and returns a `Result` because a string might not be a number.
#[test]
fn a_decimal_literal_is_exact() {
    let out = run(
        "decimal_literal",
        r#"  print(Decimal::show(0.01d));
  print(Decimal::show(Decimal::add(0.01d, 0.02d)));"#,
    );
    assert_eq!(out, "0.01\n0.03\n", "the sum every float gets wrong");
}

/// **The scale is the digits written, not the value's magnitude.** `1.50d`
/// keeps its trailing zero, because a price to two places stays a price to two
/// places — the same reasoning `Show for Decimal` gives for printing it.
#[test]
fn the_written_scale_is_kept() {
    let out = run(
        "decimal_literal_scale",
        r#"  print(Decimal::show(1.50d));
  print(Decimal::show(1.5d));
  print(Decimal::show(1d));
  print(Decimal::show(0.000000d));"#,
    );
    assert_eq!(out, "1.50\n1.5\n1\n0.000000\n");
}

/// A whole amount of money is the common case, so `1.00d` is not mandatory.
#[test]
fn an_integer_literal_may_carry_the_suffix() {
    let out = run(
        "decimal_literal_int",
        r#"  print(Decimal::show(Decimal::add(1d, 0.5d)));
  print(Decimal::show(1_000_000d));"#,
    );
    assert_eq!(out, "1.5\n1000000\n", "and underscores separate here too");
}

/// **The exponent moves the point rather than becoming a negative scale.**
/// `Decimal::scaled` has no negative scale — it is a large number spelled
/// confusingly — so `1.5e3d` is fifteen hundred at scale zero.
#[test]
fn an_exponent_becomes_a_whole_number() {
    let out = run(
        "decimal_literal_exponent",
        r#"  print(Decimal::show(1.5e3d));
  print(Decimal::show(1.5e1d));
  print(Decimal::show(1.25e1d));"#,
    );
    assert_eq!(out, "1500\n15\n12.5\n");
}

// --- what a ledger does to a decimal ----------------------------------------

/// **Adding two scales aligns to the larger.** A penny added to a price in
/// mills is a price in mills; going the other way would throw a digit away
/// silently, which is the failure this type exists to prevent.
#[test]
fn addition_aligns_to_the_larger_scale() {
    let out = run(
        "decimal_align",
        r#"  print(Decimal::show(Decimal::add(0.01d, 0.001d)));
  print(Decimal::show(Decimal::add(0.001d, 0.01d)));
  print(Decimal::show(Decimal::sub(1.00d, 0.001d)));
  print(Decimal::show(Decimal::add(1d, 0.01d)));"#,
    );
    assert_eq!(out, "0.011\n0.011\n0.999\n1.01\n", "and it commutes");
}

/// Multiplication adds the scales, which is what the arithmetic says and not
/// a choice: two prices to two places multiply to four.
#[test]
fn multiplication_adds_the_scales() {
    let out = run(
        "decimal_multiply_scale",
        r#"  print(Decimal::show(Decimal::mul(1.10d, 1.10d)));
  print(Decimal::show(Decimal::mul(2d, 0.005d)));"#,
    );
    assert_eq!(out, "1.2100\n0.010\n");
}

/// Comparison is by value and not by representation. `1.5d` and `1.50d` are
/// the same number written twice, and a ledger that said otherwise would be
/// unusable.
#[test]
fn the_same_number_at_two_scales_compares_equal() {
    let out = run(
        "decimal_compare_scales",
        r#"  if Decimal::eq(1.5d, 1.50d) { print("equal") } else { print("NOT equal") };
  if Decimal::eq(1d, 1.000d) { print("equal") } else { print("NOT equal") };
  if Decimal::eq(0.1d, 0.2d) { print("equal") } else { print("NOT equal") };"#,
    );
    assert_eq!(out, "equal\nequal\nNOT equal\n");
}

/// **The significand is sixty-four bits and the limit is real.** Eighteen
/// digits is every currency amount anybody transacts, and `numbers.md` says
/// so; what matters is that going past it stops rather than wrapping. A wrong
/// number in a ledger is worse than no number.
#[test]
fn a_scale_that_cannot_be_reached_stops_the_program() {
    let (printed, stopped) = run_until_it_stops(
        "decimal_overflow_scale",
        r#"  print(Decimal::show(Decimal::at_scale(1d, 3)));
  print(Decimal::show(Decimal::at_scale(9300000000000000d, 3)));
  print("reached the end");"#,
    );
    // Nine point three quintillion does not fit sixty-four bits. The first
    // line succeeds; the second cannot, and the program ends there rather than
    // printing something plausible.
    assert_eq!(printed, "1.000\n", "nothing after the overflow should print");
    assert!(stopped, "an unrepresentable scale must stop rather than wrap");
}
