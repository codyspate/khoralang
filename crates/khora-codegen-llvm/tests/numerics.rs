#![cfg(feature = "llvm")]

//! `checked_*` arithmetic and based integer literals, compiled with `std` and
//! run on both fiber backends.
//!
//! **What these guard: one hostile number stopping a whole server.** `+`,
//! `-`, `*` and `/` stop the program when the answer does not fit, and so do
//! `Decimal::add` and its siblings: right for a bug, and wrong for an amount
//! that arrived in a request, where the stop takes every other request with
//! it. The `checked_*` forms answer `None` at exactly the inputs the plain
//! ones stop on, and the same number everywhere else. Each row here is an
//! edge: one past the type's range and the value at it, so a check that is
//! off by one either way goes red.
//!
//! A based literal is the other half: `0xFF` and `-0x80` are the numbers
//! they name, in every width, so the constants of a hash can be written the
//! way the standard writes them.

use crate::harness;

use std::path::{Path, PathBuf};
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `std` module and `main`, as a package build sees them.
fn sources(db: &KhoraDatabase, dir: &Path, main: &str) -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
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

const HEAD: &str = "module main;
import std::core::{print, Array, Hash, List, Map, Option, Show, U8, U16, U32, U64, I8, I16, I32, I64};
import std::decimal::{Decimal, Rounding};

fn shown<A: Show>(value: Option<A>) -> String {
  match value {
    Option::Some(x) => \"Some(${x.show()})\",
    Option::None => \"None\",
  }
}

fn same_int(x: Int) -> Int { x }
fn same_i8(x: I8) -> I8 { x }
fn same_i16(x: I16) -> I16 { x }
fn same_i32(x: I32) -> I32 { x }
fn same_u8(x: U8) -> U8 { x }
fn same_u16(x: U16) -> U16 { x }
fn same_u32(x: U32) -> U32 { x }
fn same_u64(x: U64) -> U64 { x }
";

/// Compiles `body` as the whole of `main` after [`HEAD`].
fn build(name: &str, body: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("numerics_{name}"));
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let source = format!("{HEAD}\npub fn main() -> () {{\n{body}\n}}\n");
    let root = SourceRoot::new(&db, sources(&db, &dir, &source));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// Runs `body` on both fiber backends and requires `expected`, one line per
/// `print`, and a clean exit.
fn prints(name: &str, body: &str, expected: &[&str]) {
    let exe = build(name, body);
    for backend in ["threads", "scheduler"] {
        let out = Command::new(&exe).env("KHORA_FIBERS", backend).output().expect("it runs");
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "`{backend}`: {stderr}\n{stdout}");
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines, expected, "`{backend}`");
    }
}

/// `Int` at both ends of its range, for every operation.
#[test]
fn int_checked_arithmetic_refuses_at_the_edges() {
    prints(
        "int",
        "  let max = 0x7FFF_FFFF_FFFF_FFFF;
  let min = -max - 1;
  print(shown(Int::checked_add(max, 1)));
  print(shown(Int::checked_add(max, 0)));
  print(shown(Int::checked_sub(min, 1)));
  print(shown(Int::checked_sub(min, 0)));
  print(shown(Int::checked_mul(min, -1)));
  print(shown(Int::checked_mul(max, -1)));
  print(shown(Int::checked_div(min, -1)));
  print(shown(Int::checked_div(min, 1)));
  print(shown(Int::checked_div(7, 0)));
  print(shown(Int::checked_div(-7, 2)));
  print(shown(Int::checked_neg(min)));
  print(shown(Int::checked_neg(max)));
  print(shown(Int::checked_abs(min)));
  print(shown(Int::checked_abs(min + 1)));
  print(shown(Int::checked_abs(5)));
  print(shown(max.checked_add(-1)));",
        &[
            "None",
            "Some(9223372036854775807)",
            "None",
            "Some(-9223372036854775808)",
            "None",
            "Some(-9223372036854775807)",
            "None",
            "Some(-9223372036854775808)",
            "None",
            "Some(-3)",
            "None",
            "Some(-9223372036854775807)",
            "None",
            "Some(9223372036854775807)",
            "Some(5)",
            "Some(9223372036854775806)",
        ],
    );
}

/// The unsigned widths: the top and zero are the edges, and a negation is
/// `None` for everything but zero.
#[test]
fn unsigned_checked_arithmetic_refuses_at_the_type_s_own_edges() {
    prints(
        "unsigned",
        "  let a: U8 = 0xFF;
  let zero: U8 = 0;
  print(shown(U8::checked_add(a, 1)));
  print(shown(U8::checked_add(a, 0)));
  print(shown(U8::checked_sub(zero, 1)));
  print(shown(U8::checked_mul(a, 2)));
  print(shown(U8::checked_mul(0x7F, 2)));
  print(shown(U8::checked_div(a, 0)));
  print(shown(U8::checked_div(a, 2)));
  print(shown(U8::checked_neg(zero)));
  print(shown(U8::checked_neg(1)));
  let b: U16 = 0xFFFF;
  print(shown(U16::checked_add(b, 1)));
  print(shown(U16::checked_sub(b, 0xFFFF)));
  let c: U32 = 0xFFFF_FFFF;
  print(shown(U32::checked_add(c, 1)));
  print(shown(U32::checked_mul(0x8000_0000, 1)));
  let d: U64 = 0xFFFF_FFFF_FFFF_FFFF;
  print(shown(U64::checked_add(d, 1)));
  let less = match U64::checked_sub(d, 1) { Option::Some(x) => U64::wrapping_to_int(x), Option::None => 0 };
  print(\"Some(${less})\");",
        &[
            "None",
            "Some(255)",
            "None",
            "None",
            "Some(254)",
            "None",
            "Some(127)",
            "Some(0)",
            "None",
            "None",
            "Some(0)",
            "None",
            "Some(2147483648)",
            "None",
            "Some(-2)",
        ],
    );
}

/// The signed widths below `Int`: each one's minimum and maximum, with the
/// minimum written as a negated based literal.
#[test]
fn signed_checked_arithmetic_refuses_at_the_type_s_own_edges() {
    prints(
        "signed",
        "  let lo8: I8 = -0x80;
  let hi8: I8 = 0x7F;
  print(shown(I8::checked_add(hi8, 1)));
  print(shown(I8::checked_sub(lo8, 1)));
  print(shown(I8::checked_mul(lo8, -1)));
  print(shown(I8::checked_div(lo8, -1)));
  print(shown(I8::checked_div(lo8, 0)));
  print(shown(I8::checked_neg(lo8)));
  print(shown(I8::checked_abs(lo8)));
  print(shown(I8::checked_abs(-0x7F)));
  let lo16: I16 = -0x8000;
  print(shown(I16::checked_sub(lo16, 1)));
  print(shown(I16::checked_add(lo16, 0x7FFF)));
  let lo32: I32 = -0x8000_0000;
  print(shown(I32::checked_neg(lo32)));
  print(shown(I32::checked_div(lo32, -1)));
  print(shown(I32::checked_mul(0x4000_0000, 2)));
  print(shown(I32::checked_mul(0x4000_0000, -2)));",
        &[
            "None",
            "None",
            "None",
            "None",
            "None",
            "None",
            "None",
            "Some(127)",
            "None",
            "Some(-1)",
            "None",
            "None",
            "None",
            "Some(-2147483648)",
        ],
    );
}

/// `Decimal` at the 128-bit edge: `of_parts(i64::MAX, -1, 0)` is the largest
/// significand, `i128::MAX`, and its negation less one is the smallest.
#[test]
fn decimal_checked_arithmetic_refuses_at_the_128_bit_edge() {
    prints(
        "decimal",
        "  let top = Decimal::of_parts(9223372036854775807, -1, 0);
  let one = Decimal::of_int(1);
  let bottom = Decimal::sub(Decimal::negate(top), one);
  print(shown(Decimal::checked_add(top, one)));
  print(shown(Decimal::checked_add(Decimal::sub(top, one), one)));
  print(shown(Decimal::checked_sub(bottom, one)));
  print(shown(Decimal::checked_sub(Decimal::add(bottom, one), one)));
  print(shown(Decimal::checked_mul(top, Decimal::of_int(2))));
  print(shown(Decimal::checked_mul(top, one)));
  print(shown(Decimal::checked_mul(Decimal::scaled(150, 2), Decimal::scaled(3, 1))));
  print(shown(Decimal::checked_add(Decimal::of_int(2), Decimal::scaled(1, 38))));
  print(shown(Decimal::checked_add(Decimal::scaled(10000000000, 2), Decimal::scaled(1, 12))));
  print(shown(Decimal::checked_divide(one, Decimal::zero(), 2, Rounding::HalfEven)));
  print(shown(Decimal::checked_divide(top, one, 1, Rounding::HalfEven)));
  print(shown(Decimal::checked_divide(top, one, 0, Rounding::HalfEven)));
  print(shown(Decimal::checked_divide(Decimal::scaled(125, 3), one, 2, Rounding::HalfUp)));
  print(shown(Decimal::checked_divide(Decimal::scaled(-125, 3), one, 2, Rounding::HalfUp)));
  print(shown(Decimal::checked_sub(Decimal::scaled(150, 2), Decimal::scaled(3, 3))));
  print(shown(Decimal::checked_sub(Decimal::scaled(3, 3), Decimal::scaled(150, 2))));
  print(shown(Decimal::checked_add(Decimal::scaled(3, 3), Decimal::scaled(150, 2))));
  print(shown(Decimal::checked_add(Decimal::scaled(150, 2), Decimal::scaled(3, 3))));
  print(shown(Decimal::checked_divide(Decimal::scaled(1, 0), Decimal::scaled(3, 0), -2, Rounding::HalfEven)));
  let at_minus_two = match Decimal::checked_divide(Decimal::scaled(150, 0), one, -2, Rounding::HalfEven) {
    Option::Some(d) => Decimal::scale(d),
    Option::None => 99,
  };
  print(\"${at_minus_two}\");",
        &[
            "None",
            "Some(170141183460469231731687303715884105727)",
            "None",
            "Some(-170141183460469231731687303715884105728)",
            "None",
            "Some(170141183460469231731687303715884105727)",
            "Some(0.450)",
            "None",
            "Some(100000000.000000000001)",
            "None",
            "None",
            "Some(170141183460469231731687303715884105727)",
            "Some(0.13)",
            "Some(-0.13)",
            // Mixed scales, in both orders: the answer is at the larger.
            "Some(1.497)",
            "Some(-1.497)",
            "Some(1.503)",
            "Some(1.503)",
            // A negative scale is clamped to zero, as in `divide`: the
            // significand is computed at zero, and the answer's scale says so.
            "Some(0)",
            "0",
        ],
    );
}

/// A checked operation taken as a function value is the same operation.
#[test]
fn a_checked_operation_works_as_a_function_value() {
    prints(
        "values",
        "  let doubled = List::map([1, 0x4000_0000_0000_0000], fn x => Int::checked_mul(x, 2));
  print(List::show(List::map(doubled, fn o => shown(o))));
  let add: (Int, Int) -> Option<Int> = Int::checked_add;
  print(shown(add(0x7FFF_FFFF_FFFF_FFFF, 1)));",
        &["[Some(2), None]", "None"],
    );
}

/// **And the plain operators still stop the program on the same inputs.**
/// The checked forms are a second way to ask, not a change to the first.
#[test]
fn the_plain_operators_still_stop_on_the_same_inputs() {
    for (name, body, said) in [
        ("plain_add", "  let max = 0x7FFF_FFFF_FFFF_FFFF;\n  print(\"${max + 1}\");", "Int addition overflowed"),
        ("plain_u8", "  let a: U8 = 0xFF;\n  print(\"${U8::to_int(a + 1)}\");", "U8 addition overflowed"),
        (
            "plain_div",
            "  let min = -0x7FFF_FFFF_FFFF_FFFF - 1;\n  print(\"${min / -1}\");",
            "Int division overflowed",
        ),
        (
            "plain_decimal",
            "  let top = Decimal::of_parts(9223372036854775807, -1, 0);\n  print(Decimal::add(top, Decimal::of_int(1)).show());",
            "Decimal addition overflowed",
        ),
    ] {
        stops(name, body, said);
    }
}

/// Builds `body`, runs it on both fiber backends, and requires the overflow
/// stop: exit 134, `said` on stderr, nothing printed.
fn stops(name: &str, body: &str, said: &str) {
    let exe = build(name, body);
    for backend in ["threads", "scheduler"] {
        let out = Command::new(&exe).env("KHORA_FIBERS", backend).output().expect("it runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(134), "`{name}` on `{backend}`: {stderr}");
        assert!(stderr.contains(said), "`{name}` on `{backend}`: {stderr}");
        assert!(out.stdout.is_empty(), "`{name}`: nothing after the overflow ran");
    }
}

/// **`-x` on a signed type's minimum stops the program, as `x * -1` does.**
/// The minimum has no positive twin, and the negation answered the minimum
/// itself: `-x` printed `-9223372036854775808` and exited 0, where `x * -1`
/// on the same `x` stopped. Each type's negation and multiplication are
/// built from the same `x`, read through a function so no constant is
/// folded before the operator runs.
#[test]
fn negating_the_minimum_stops_like_multiplying_by_minus_one() {
    for (ty, min) in [
        ("Int", "-0x7FFF_FFFF_FFFF_FFFF - 1"),
        ("I8", "-128"),
        ("I16", "-32768"),
        ("I32", "-2147483648"),
    ] {
        let lower = ty.to_lowercase();
        let shown = if ty == "Int" { "y".to_string() } else { format!("{ty}::to_int(y)") };
        for (form, op, what) in
            [("neg", "-x", "negation"), ("mul", "x * -1", "multiplication")]
        {
            let body = format!(
                "  let at: {ty} = {min};\n  let x = same_{lower}(at);\n  let y: {ty} = {op};\n  print(\"${{{shown}}}\");"
            );
            stops(&format!("min_{lower}_{form}"), &body, &format!("{ty} {what} overflowed"));
        }
    }
}

/// **`-x` on any unsigned value but zero stops the program**, the way `0 - x`
/// does: there is no negative `U8` to answer. The negation answered `256 - x`
/// before.
#[test]
fn negating_an_unsigned_value_stops() {
    for ty in ["U8", "U16", "U32", "U64"] {
        let lower = ty.to_lowercase();
        let body = format!(
            "  let one: {ty} = 1;\n  let x = same_{lower}(one);\n  let y: {ty} = -x;\n  print(\"${{y}}\");"
        );
        stops(&format!("neg_{lower}"), &body, &format!("{ty} negation overflowed"));
    }
}

/// The value next to each minimum negates exactly to the maximum, zero to
/// zero, and an unsigned zero to zero: the check is at the one value with
/// no answer and nowhere else.
#[test]
fn negating_next_to_the_minimum_is_exact() {
    prints(
        "near_min",
        "  let a = same_int(-0x7FFF_FFFF_FFFF_FFFF);
  let b: I8 = -127;
  let c: I16 = -32767;
  let d: I32 = -2147483647;
  let z = same_int(0);
  let u: U8 = 0;
  let nb: I8 = -same_i8(b);
  let nc: I16 = -same_i16(c);
  let nd: I32 = -same_i32(d);
  let nu: U8 = -same_u8(u);
  print(\"${-a} ${I8::to_int(nb)} ${I16::to_int(nc)} ${I32::to_int(nd)} ${-z} ${U8::to_int(nu)}\");
  print(\"${-same_int(5)} ${-(-same_int(5))}\");",
        &["9223372036854775807 127 32767 2147483647 0 0", "-5 5"],
    );
}

/// **Each signed type's minimum can be written as a negated literal, in
/// either base.** The minus belongs to the number, so `-128` is an `I8`
/// although `128` is not -- and the same for `Int`, whose
/// `-9223372036854775808` was refused as "`9223372036854775808` does not
/// fit in an `Int`", decimal and hexadecimal alike.
#[test]
fn every_signed_minimum_is_a_negated_literal() {
    prints(
        "minimums",
        "  let a = -9223372036854775808;
  let ah = -0x8000_0000_0000_0000;
  let b: I64 = -9223372036854775808;
  let bh: I64 = -0x8000_0000_0000_0000;
  print(\"${a} ${ah} ${b} ${bh} ${a == -0x7FFF_FFFF_FFFF_FFFF - 1}\");
  let c: I8 = -128;
  let ch: I8 = -0x80;
  let d: I16 = -32768;
  let dh: I16 = -0x8000;
  let e: I32 = -2147483648;
  let eh: I32 = -0x8000_0000;
  print(\"${I8::to_int(c)} ${I8::to_int(ch)} ${I16::to_int(d)} ${I16::to_int(dh)} ${I32::to_int(e)} ${I32::to_int(eh)}\");",
        &[
            "-9223372036854775808 -9223372036854775808 -9223372036854775808 -9223372036854775808 true",
            "-128 -128 -32768 -32768 -2147483648 -2147483648",
        ],
    );
}

/// **A `U64` prints as the number it is, all the way to its maximum, and is
/// a key at every value.** Its `Show` and its `Hash` went through
/// `U64::to_int`, which stops the program above `Int::MAX`, so `"${x}"` on
/// half of the type's values stopped the program, and so did using one as a
/// `Map` key.
#[test]
fn every_u64_prints_as_its_number() {
    prints(
        "u64_show",
        "  let top: U64 = 0xFFFF_FFFF_FFFF_FFFF;
  let past: U64 = 9223372036854775808;
  let below: U64 = 9223372036854775807;
  let zero: U64 = 0;
  let nine: U64 = 9;
  let ten: U64 = 0x8000_0000_0000_000A;
  print(\"${same_u64(top)}\");
  print(\"${same_u64(past)} ${same_u64(below)} ${same_u64(zero)} ${same_u64(nine)}\");
  print(same_u64(ten).show());
  let u32: U32 = 0xFFFF_FFFF;
  print(\"${same_u32(u32)}\");
  let seen: Map<U64, String> = Map::new();
  Map::insert(seen, top, \"top\");
  Map::insert(seen, zero, \"zero\");
  let found = match Map::get(seen, same_u64(top)) { Option::Some(name) => name, Option::None => \"none\" };
  print(\"${found} ${Hash::hash(top) == Hash::hash(same_u64(top))}\");",
        &[
            "18446744073709551615",
            "9223372036854775808 9223372036854775807 0 9",
            "9223372036854775818",
            "4294967295",
            "top true",
        ],
    );
}

/// Based literals print as the numbers they name, in `Int` and narrower,
/// and `-0x80` is the same `I8` as `-128`.
///
/// **And a `U64` above `Int`'s maximum builds**, in either base. The checker
/// accepted `let top: U64 = 18446744073709551615;` and the code generator
/// read every literal as an `i64`, so `khora check` passed it and
/// `khora build` refused it as not fitting an `Int`.
#[test]
fn based_literals_are_the_numbers_they_name() {
    prints(
        "literals",
        "  print(\"${0xFF} ${0b1010} ${0o17} ${0xFFFF_FFFF} ${-0x80}\");
  let a: I8 = -0x80;
  let b: I8 = -128;
  print(\"${I8::to_int(a)} ${a == b}\");
  let c: U8 = 0b1111_1111;
  print(\"${U8::to_int(c)}\");
  let d: U64 = 0xFFFF_FFFF_FFFF_FFFF;
  print(\"${U64::wrapping_to_int(d)}\");
  let top: U64 = 18446744073709551615;
  print(\"${U64::wrapping_to_int(top)}\");
  let e = match 255 { 0xFE => \"no\", 0xFF => \"yes\", _ => \"no\" };
  print(e);
  print(\"${0x428A_2F98 + 0XAB_cd + 0O7 + 0B1}\");",
        &["255 10 15 4294967295 -128", "-128 true", "255", "-1", "-1", "yes", "1116396397"],
    );
}

/// **A literal pattern on a fixed-width scrutinee is compared at the
/// scrutinee's width.** `match (b: U8) { 0xFF => .. }` compared an `i8` with
/// an `i64` constant, which LLVM refuses, so a byte could not be matched
/// against a literal at all -- the thing a hexadecimal literal is most often
/// for. Decimal and hexadecimal patterns, on a `U8`, an `I16` and a `U64`.
#[test]
fn a_literal_pattern_matches_at_the_scrutinee_width() {
    prints(
        "pattern_width",
        r#"  let a: U8 = 255;
  let b: U8 = 10;
  let c: I16 = 32767;
  let d: I16 = 300;
  let e: U64 = 0xFFFF_FFFF_FFFF_FFFF;
  let f: U64 = 0xFFFF_FFFF_FFFF_FFFE;
  let g: U8 = 7;
  let byte = fn (x: U8) => match x { 0xFF => "ff", 10 => "ten", _ => "other" };
  let word = fn (x: I16) => match x { 0x7FFF => "max", 300 => "three hundred", _ => "other" };
  let wide = fn (x: U64) => match x { 0xFFFF_FFFF_FFFF_FFFF => "top", 18446744073709551614 => "below", _ => "other" };
  let lo: I8 = -128;
  let hi: I8 = 127;
  let z: U8 = 0;
  let small = fn (x: I8) => match x { 0x7F => "top", 0 => "zero", _ => "other" };
  let pair = fn (p: (U8, I8)) => match p { (0xFF, 0x7F) => "both", (0, 0) => "nought", _ => "other" };
  print("${byte(a)} ${byte(b)} ${byte(g)} ${byte(z)}");
  print("${word(c)} ${word(d)}");
  print("${wide(e)} ${wide(f)}");
  print("${small(hi)} ${small(lo)} ${pair((a, hi))} ${pair((z, lo))}");"#,
        &["ff ten other other", "max three hundred", "top below", "top other both other"],
    );
}

/// **`+`, `-`, `*` and unary `-` on `Decimal` are `add`, `sub`, `mul` and
/// `negate`**: the same number at the same scale, printed side by side with
/// the method's answer. Mixed scales in both orders, a product's added
/// scales, a chain, and a comparison of operator results.
#[test]
fn decimal_operators_are_the_methods() {
    prints(
        "decimal_ops",
        r#"  let a = Decimal::scaled(150, 2);
  let b = Decimal::scaled(3, 3);
  print("${a + b} ${Decimal::add(a, b)}");
  print("${b + a} ${Decimal::add(b, a)}");
  print("${a - b} ${Decimal::sub(a, b)}");
  print("${b - a} ${Decimal::sub(b, a)}");
  print("${a * b} ${Decimal::mul(a, b)}");
  print("${-a} ${Decimal::negate(a)} ${-(-a)}");
  print("${19.99d * 3d + 4.50d - 0.47d}");
  print("${a + b == Decimal::add(a, b)} ${a * b < a} ${-a < a}");"#,
        &[
            "1.503 1.503",
            "1.503 1.503",
            "1.497 1.497",
            "-1.497 -1.497",
            "0.00450 0.00450",
            "-1.50 -1.50 1.50",
            "64.00",
            "true true true",
        ],
    );
}

/// **A `Decimal` operator stops the program where its method does, with the
/// method's message**: the operator is the method, not a second arithmetic
/// with its own edge. `-` on the smallest significand is `negate`, which is
/// a subtraction from zero, and says so.
#[test]
fn decimal_operators_stop_where_the_methods_do() {
    let edges = "  let top = Decimal::of_parts(9223372036854775807, -1, 0);\n  let one = Decimal::of_int(1);\n  let bottom = Decimal::negate(top) - one;\n";
    for (name, line, said) in [
        ("dec_op_add", "  print(\"${top + one}\");", "Decimal addition overflowed"),
        ("dec_op_sub", "  print(\"${bottom - one}\");", "Decimal subtraction overflowed"),
        ("dec_op_mul", "  print(\"${top * Decimal::of_int(2)}\");", "Decimal multiplication overflowed"),
        ("dec_op_neg", "  print(\"${-bottom}\");", "Decimal subtraction overflowed"),
        (
            "dec_op_scales",
            "  print(\"${Decimal::of_int(2) + Decimal::scaled(1, 40)}\");",
            "Decimal addition overflowed",
        ),
    ] {
        stops(name, &format!("{edges}{line}"), said);
    }
}

/// **A negative literal pattern matches the number it names**, at the
/// scrutinee's width: `-128` and `-0x80` on an `I8` are its minimum, `-1` on
/// an `I16` is all ones, and `Int`'s minimum is a pattern too. Without the
/// grammar for it the smallest value of a signed type could be matched only
/// by a guard.
#[test]
fn a_negative_literal_pattern_matches_its_number() {
    prints(
        "negative_patterns",
        r#"  let lo: I8 = -128;
  let hi: I8 = 127;
  let m1: I8 = -1;
  let w: I16 = -1;
  let w2: I16 = 1;
  let small = fn (x: I8) => match x { -128 => "min", -1 => "minus one", 127 => "max", _ => "other" };
  let based = fn (x: I8) => match x { -0x80 => "min", _ => "other" };
  let word = fn (x: I16) => match x { -1 => "all ones", _ => "other" };
  let int = fn (x: Int) => match x { -9223372036854775808 => "min", -1 => "minus one", 0 => "zero", _ => "other" };
  let pair = fn (p: (I8, Int)) => match p { (-128, -2) => "both", (_, -2) => "right", _ => "other" };
  let floor = -9223372036854775807 - 1;
  print("${small(lo)} ${small(m1)} ${small(hi)} ${based(lo)} ${based(m1)}");
  print("${word(w)} ${word(w2)}");
  print("${int(floor)} ${int(-1)} ${int(0)} ${int(1)}");
  print("${pair((lo, -2))} ${pair((hi, -2))} ${pair((lo, 2))}");"#,
        &[
            "min minus one max min other",
            "all ones other",
            "min minus one zero other",
            "both right other",
        ],
    );
}

/// **A `match` on a fixed-width integer with no catch-all is refused**, not
/// built: with literal arms alone a value none of them names reached the end
/// of the arms and stopped the program with an illegal instruction.
#[test]
fn a_byte_match_without_a_catch_all_is_refused() {
    let db = KhoraDatabase::new();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("numerics_byte_no_catch_all");
    std::fs::create_dir_all(&dir).expect("a workspace");
    let source = format!(
        "{HEAD}\nfn name(b: U8) -> String {{ match b {{ 0 => \"zero\", 1 => \"one\" }} }}\n\
         pub fn main() -> () {{\n  let b: U8 = 2;\n  print(name(b));\n}}\n"
    );
    let root = SourceRoot::new(&db, sources(&db, &dir, &source));
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let errors = match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => panic!("a byte match with no catch-all should be refused"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect::<Vec<_>>(),
    };
    assert!(
        errors.iter().any(|e| e.contains("this `match` is not exhaustive: pattern `_` not covered")),
        "{errors:?}"
    );
}

/// `Array::to_list` is the elements first to last, and an empty array is an
/// empty list.
#[test]
fn an_array_copies_into_a_list_in_order() {
    prints(
        "array_to_list",
        r#"  let a: Array<Int> = Array::from_fn(4, fn i => i * 10);
  print(List::show(Array::to_list(a)));
  let none: Array<Int> = Array::empty();
  print("${List::length(Array::to_list(none))}");"#,
        &["[0, 10, 20, 30]", "0"],
    );
}

/// **An unannotated operand takes its type from a `Decimal` or `Float` on
/// the right**, and the program it settles runs: the operator is still the
/// method, with the method's answer. `fn x => x + 1` stays `Int`.
#[test]
fn a_right_hand_decimal_or_float_settles_the_left_and_runs() {
    prints(
        "pin_left",
        r#"  let addv = fn x => x + 1.5d;
  let subv = fn x => x - 0.25d;
  let mulv = fn x => x * 0.5d;
  let addf = fn x => x + 1.5;
  let inc = fn x => x + 1;
  print("${addv(1d)} ${subv(1d)} ${mulv(3d)} ${Decimal::add(1d, 1.5d)}");
  print("${addf(1.0)} ${inc(41)}");"#,
        &["2.5 0.75 1.5 2.5", "2.5 42"],
    );
}
