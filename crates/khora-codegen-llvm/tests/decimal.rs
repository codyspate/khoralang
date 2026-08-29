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
import std::core::{{Eq, Option, Ord, Ordering, Show, print}};
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
import std::core::{{Eq, Option, Ord, Ordering, Show, print}};
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

/// **The significand is a hundred and twenty-eight bits and the limit is
/// still real**, just much further out.
///
/// It was sixty-four, on the argument that eighteen digits is every currency
/// amount anybody transacts — true about amounts and false about arithmetic on
/// them, since `at_scale` is a multiplication and `add` goes through it. What
/// has not changed is what happens at the edge: going past stops rather than
/// wrapping, because a wrong number in a ledger is worse than no number.
#[test]
fn a_scale_that_cannot_be_reached_stops_the_program() {
    let (printed, stopped) = run_until_it_stops(
        "decimal_overflow_scale",
        r#"  print(Decimal::show(Decimal::at_scale(1d, 3)));
  print(Decimal::show(Decimal::at_scale(100000000000000000000000000000000000000d, 3)));
  print("reached the end");"#,
    );
    // Ten to the thirty-eighth, times a thousand, is past a hundred and
    // twenty-eight bits. The first line succeeds; the second cannot, and the
    // program ends there rather than printing something plausible.
    assert_eq!(printed, "1.000\n", "nothing after the overflow should print");
    assert!(stopped, "an unrepresentable scale must stop rather than wrap");
}

/// **And the addition that made the width change now works.**
///
/// A hundred million against a rate to twelve places. `add` brings both to the
/// larger scale first, so this needed `1e10 * 10^10` against a sixty-four bit
/// ceiling of `9.22e18` and stopped the program — on two numbers a rates desk
/// writes down every day.
#[test]
fn a_notional_plus_a_twelve_place_rate_is_a_number() {
    let out = run(
        "decimal_wide_add",
        r#"  let notional = 100000000.00d;
  let rate = 0.000000000001d;
  print(Decimal::show(Decimal::add(notional, rate)));
  print(Decimal::show(Decimal::sub(notional, rate)));
  // And the multiplication, whose scales add to fourteen places.
  print(Decimal::show(Decimal::mul(notional, rate)));"#,
    );

    assert_eq!(
        out,
        "100000000.000000000001\n99999999.999999999999\n0.00010000000000\n"
    );
}

/// **An eighteen-decimal token balance, which sixty-four bits could not hold
/// past 9.2 whole units.**
#[test]
fn an_eighteen_decimal_balance_is_a_number() {
    let out = run(
        "decimal_token_balance",
        r#"  let balance = 1000000.000000000000000000d;
  let fee = 0.000000000000000001d;
  print(Decimal::show(Decimal::sub(balance, fee)));"#,
    );

    assert_eq!(out, "999999.999999999999999999\n");
}

/// **Ten divided by one is ten.**
///
/// It was not. `divide` computed its intermediate in a hundred and twenty-eight
/// bits and then brought it back with a saturating cast, so any quotient that
/// did not fit the significand answered `i64::MAX` -- which at eighteen places
/// prints as `9.223372036854775807`. In range, right number of decimal places,
/// balances against itself, and wrong.
///
/// The whole module exists so that money is exact. Found by somebody writing a
/// reconciler, eight minutes in. Ten at eighteen places is an ordinary number
/// now, so the edge has moved to thirty-nine digits — and it is still an edge.
#[test]
fn a_quotient_that_does_not_fit_stops_rather_than_saturating() {
    let (printed, stopped) = run_until_it_stops(
        "decimal_divide_overflow",
        r#"  // Thirty-eight places of a two digit number is forty digits, and the
  // significand holds thirty-nine.
  match Decimal::divide(10d, 1d, 38, Rounding::HalfEven) {
    Option::Some(answer) => print(Decimal::show(answer)),
    Option::None => print("None"),
  };
  print("reached the end");"#,
    );

    assert_eq!(printed, "", "a quotient that does not fit must print nothing");
    assert!(stopped, "it must stop rather than answer the largest significand");
}

/// **A scale no significand reaches is refused, not quietly met.**
///
/// Asking for a hundred places has no representable answer. The runtime
/// clamped the scale to thirty-eight for a few minutes during the widening,
/// which computed to thirty-eight places and labelled the result as having a
/// hundred: a wrong number wearing the right hat, which is precisely the thing
/// this module exists to make impossible.
#[test]
fn a_scale_past_every_power_is_refused() {
    let (printed, stopped) = run_until_it_stops(
        "decimal_divide_absurd_scale",
        r#"  match Decimal::divide(1d, 3d, 100, Rounding::HalfEven) {
    Option::Some(answer) => print(Decimal::show(answer)),
    Option::None => print("None"),
  };
  print("reached the end");"#,
    );

    assert_eq!(printed, "");
    assert!(stopped, "a hundred places must stop rather than answer thirty-eight");
}

/// The same, through `rounded`, which is `divide` by one and inherited the bug.
#[test]
fn rounding_to_a_scale_that_does_not_fit_stops_too() {
    let (printed, stopped) = run_until_it_stops(
        "decimal_rounded_overflow",
        r#"  print(Decimal::show(Decimal::rounded(10d, 38, Rounding::HalfEven)));
  print("reached the end");"#,
    );

    assert_eq!(printed, "");
    assert!(stopped, "`rounded` goes through `divide` and stops where it does");
}

/// **The quotients that do fit are still exact**, which is the half of the
/// overflow question that has an answer.
#[test]
fn a_quotient_at_the_edge_of_the_significand_is_exact() {
    let out = run(
        "decimal_divide_edge",
        r#"  // A hundred divided four ways, at the scales a ledger uses.
  print(shown(Decimal::divide(100d, 4d, 2, Rounding::HalfEven)));
  print(shown(Decimal::divide(100d, 4d, 8, Rounding::HalfEven)));
  // Eighteen places of something small enough to hold them.
  print(shown(Decimal::divide(1d, 3d, 18, Rounding::HalfEven)));"#,
    );

    assert_eq!(out, "25.00\n25.00000000\n0.333333333333333333\n");
}

/// **Two legal numbers can always be compared.**
///
/// `Eq` and `Ord` used to bring both operands to a common scale, the way `add`
/// does. A hundred million against a rate to twelve places wants a scale of
/// twelve, which is a multiplication by `10^12` that no `Int` survives -- so
/// asking whether two numbers were equal stopped the program.
///
/// An equality that traps is worse than one that surprises, and this one is
/// sold as the reason to prefer `Decimal` over `Float`.
#[test]
fn comparing_two_far_apart_scales_does_not_stop_the_program() {
    let out = run(
        "decimal_compare_wide",
        r#"  let notional = 100000000.00d;
  let rate = 0.000000000001d;
  print(if notional == rate { "equal" } else { "different" });
  print(if notional == notional { "equal" } else { "different" });
  print(if rate == rate { "equal" } else { "different" });
  // And the order, both ways round, on both sides of zero.
  print(if notional > rate { "bigger" } else { "not bigger" });
  print(if rate < notional { "smaller" } else { "not smaller" });
  print(if Decimal::negate(notional) < Decimal::negate(rate) {
    "smaller"
  } else {
    "not smaller"
  });"#,
    );

    assert_eq!(out, "different\nequal\nequal\nbigger\nsmaller\nsmaller\n");
}

/// A numeral too long for the significand is text that is not a number, and
/// `of_string` already has somewhere to say so.
///
/// It used to stop the program instead -- in the one function whose whole job
/// is reading numbers out of files somebody else wrote. One long cell in one
/// CSV killed the process.
#[test]
fn a_numeral_too_long_to_hold_is_none_and_not_a_trap() {
    let out = run(
        "decimal_of_string_long",
        r#"  // Twenty-seven digits, which sixty-four bits refused and this reads.
  print(shown(Decimal::of_string("1234567890123456789012345.00")));
  print(shown(Decimal::of_string("-1234567890123456789012345.00")));
  // The largest one that does fit still reads.
  print(shown(Decimal::of_string("170141183460469231731687303715884105727")));
  // And one past it does not.
  print(shown(Decimal::of_string("170141183460469231731687303715884105728")));
  // Nor does a cell that is simply long.
  print(shown(Decimal::of_string("999999999999999999999999999999999999999999")));
  // An ordinary amount is unaffected.
  print(shown(Decimal::of_string("1250.00")));"#,
    );

    assert_eq!(
        out,
        "1234567890123456789012345.00\n\
         -1234567890123456789012345.00\n\
         170141183460469231731687303715884105727\n\
         None\n\
         None\n\
         1250.00\n"
    );
}

/// **Every scale a `Decimal` can carry can be printed.**
///
/// `show` split the significand with `units / 10^scale`, which needs
/// `10^scale` to be a number -- and past eighteen places it is not. So `mul`
/// on two numbers with ten decimal places each produced a value that was
/// perfectly legal, compared correctly, and stopped the program when printed.
///
/// It is string work now, which needs nothing to fit.
#[test]
fn a_scale_past_eighteen_prints() {
    let out = run(
        "decimal_show_wide_scale",
        r#"  // Ten places times ten places is twenty.
  let small = Decimal::mul(0.0000000001d, 0.0000000001d);
  print(Decimal::show(small));
  print(Int::to_string(Decimal::truncated(small)));
  // Nineteen, which is exactly where `ten_to` used to give out.
  print(shown(Decimal::divide(1d, 3d, 19, Rounding::Towards)));
  // And the ordinary scales still print the way they always did.
  print(Decimal::show(Decimal::scaled(150, 2)));
  print(Decimal::show(Decimal::scaled(-150, 2)));
  print(Decimal::show(Decimal::scaled(1, 2)));"#,
    );

    assert_eq!(
        out,
        "0.00000000000000000001\n0\n0.3333333333333333333\n1.50\n-1.50\n0.01\n"
    );
}

/// The most negative `Int` is a number, so it prints.
///
/// Taking a magnitude by negating it would overflow on the way. The sign is a
/// character, and treating it as one costs nothing.
#[test]
fn the_most_negative_significand_prints() {
    let out = run(
        "decimal_show_min",
        r#"  // There is no literal for it: `9223372036854775808` does not fit an
  // `Int`, and the minus arrives after the number is read.
  let smallest = 0 - 9223372036854775807 - 1;
  print(Decimal::show(Decimal::scaled(smallest, 2)));
  print(Decimal::show(Decimal::scaled(smallest, 0)));"#,
    );

    assert_eq!(out, "-92233720368547758.08\n-9223372036854775808\n");
}

/// **A negative decimal literal is a decimal.**
///
/// `-99.95d` was `negation: expected `Int`, found `Decimal``, with a second
/// error calling a `Decimal` literal an `Int` — while `-99.95` and `-5`
/// compiled in the same program. The one type where a negative number could
/// not be written was the one for money, so every refund and every credit in
/// somebody's test data was `neg(99.95d)`.
///
/// A decimal literal is not a value of its own: it desugars to
/// `Decimal::scaled(units, scale)`, so a minus in front of one was an ordinary
/// negation applied to a *call*. The minus belongs to the number, and is
/// folded into it — which is what the checker already did for a fixed-width
/// integer, where `-128` is an `I8`.
#[test]
fn a_negative_decimal_literal_is_a_decimal() {
    let out = run(
        "decimal_negative_literal",
        r#"  print(Decimal::show(-99.95d));
  print(Decimal::show(-0.5d));
  print(Decimal::show(-1250.00d));
  // Negative zero is not a thing a scaled integer has.
  print(Decimal::show(-0.0d));
  // The positive ones are unchanged.
  print(Decimal::show(99.95d));
  // And it agrees with the function that was the workaround.
  print(if -99.95d == Decimal::negate(99.95d) { "same" } else { "different" });"#,
    );

    assert_eq!(out, "-99.95\n-0.5\n-1250.00\n0.0\n99.95\nsame\n");
}

/// The exponent form too, since it takes a different path through the parts.
///
/// A *positive* exponent only. `25e-2d` is not lexed as a decimal literal at
/// all, although `decimal_parts` handles a negative exponent perfectly well —
/// a gap older than this test and unrelated to the sign in front of it.
#[test]
fn a_negative_decimal_literal_may_carry_an_exponent() {
    let out = run(
        "decimal_negative_exponent",
        r#"  print(Decimal::show(-1.5e3d));"#,
    );

    assert_eq!(out, "-1500\n");
}
