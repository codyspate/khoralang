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
