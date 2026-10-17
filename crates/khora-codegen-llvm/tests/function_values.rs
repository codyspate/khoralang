#![cfg(feature = "llvm")]

//! A function with no Khora body, taken as a value.
//!
//! **What these guard: a valid program that checks clean and fails to build
//! as invalid LLVM IR.** `xs |> List::map(String::byte_length)` named an
//! adapter that forwarded to `String::byte_length`, which is lowered inline at
//! a call and has no function to forward to. The adapter was left a bare
//! declaration, and LLVM refused the module ("Global is external, but doesn't
//! have external or weak linkage"), reported as a compiler bug. Every `std`
//! intrinsic and every runtime-backed operation (`Fiber::wait`, `Shared::get`,
//! `Channel::send`, ...) had the same failure.
//!
//! Each program takes every such function with a function type as a value,
//! calls it through the value, and prints the answer, on both fiber backends.
//! Every one is listed, rather than one per kind, because each is lowered by
//! its own arm and an arm that is missed fails on its own.
//!
//! Not here: `assert` and `assert_that`, which exist only inside a `test`
//! block, and `std::schema::struct`, whose argument is a schema literal.

use crate::claims::{build, run, BACKENDS};

use std::time::Duration;

/// Builds `source` and runs it on both fiber backends, expecting `expected`
/// on stdout and exit 0 from each.
fn runs_on_both(name: &str, source: &str, expected: &str) {
    let exe = build(name, source);
    for backend in BACKENDS {
        let ran = run(&exe, backend, Duration::from_secs(60), None);
        assert!(!ran.hung, "{name} on {backend} hung");
        assert_eq!(ran.code, Some(0), "{name} on {backend}:\n{}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.stdout, expected, "{name} on {backend}:\n{}", ran.stderr);
    }
}

/// The case that was reported, as it was written.
#[test]
fn an_intrinsic_passed_to_a_combinator_runs() {
    runs_on_both(
        "fnval_byte_length",
        "module main;
import std::core::{List, String, print};

pub fn main() -> Int {
  let xs = [\"a\", \"bb\", \"ccc\"];
  let ns = xs |> List::map(String::byte_length);
  print(\"${List::length(ns)} ${List::fold(ns, 0, fn (a, n) => a + n)}\");
  0
}
",
        "3 6\n",
    );
}

/// `String`, `Char`, `Int`, `Float`, `Ptr`, `Array`, `Shared`, `Channel`,
/// `Region` and `print`.
#[test]
fn every_value_intrinsic_runs_through_a_function_value() {
    runs_on_both(
        "fnval_values",
        "module main;
import std::core::{Array, Channel, Option, Ptr, Region, Shared, String, print};

fn ap0<A>(f: () -> A) -> A { f() }
fn ap1<A, B>(f: (A) -> B, a: A) -> B { f(a) }
fn ap2<A, B, C>(f: (A, B) -> C, a: A, b: B) -> C { f(a, b) }
fn ap3<A, B, C, D>(f: (A, B, C) -> D, a: A, b: B, c: C) -> D { f(a, b, c) }

pub fn main() -> Int {
  ap1(print, \"print\");
  print(\"byte_length ${ap1(String::byte_length, \"abc\")}\");
  print(\"byte ${U8::to_int(ap2(String::byte, \"abc\", 1))}\");
  print(\"slice ${ap3(String::slice, \"hello\", 1, 3)}\");
  print(\"find ${ap3(String::find, \"hello\", \"l\", 0)}\");
  print(\"bytes ${ap1(String::from_bytes, ap1(String::bytes, \"xyz\"))}\");
  print(\"is_utf8 ${ap1(Array::is_utf8, String::bytes(\"xyz\"))}\");
  print(\"code ${ap1(Char::code, 'A')} ${Char::code(ap1(Char::from_code, 66))}\");
  print(\"int ${ap2(Int::wrapping_add, 1, 2)} ${ap2(Int::wrapping_sub, 5, 2)} ${ap2(Int::wrapping_mul, 3, 4)}\");
  print(\"bits ${ap2(Int::xor, 6, 3)} ${ap2(Int::and, 6, 3)} ${ap2(Int::or, 6, 3)} ${ap2(Int::shl, 1, 4)} ${ap2(Int::shr, 16, 2)}\");
  print(\"float ${ap1(Float::to_int, ap1(Int::to_float, 7))}\");
  print(\"ptr ${ap1(Ptr::is_null, ap0(Ptr::null))}\");
  let xs: Array<Int> = ap2(Array::new, 3, 4);
  ap3(Array::set, xs, 1, 9);
  let none: Array<Int> = ap0(Array::empty);
  print(\"array ${ap2(Array::get, xs, 1)} ${ap1(Array::length, xs)} ${Array::length(none)}\");
  let cell: Shared<Int> = ap1(Shared::of, 41);
  ap2(Shared::set, cell, 42);
  print(\"shared ${ap1(Shared::get, cell)}\");
  let ch: Channel<Int> = ap1(Channel::bounded, 2);
  print(\"send ${ap2(Channel::send, ch, 5)} depth ${ap1(Channel::depth, ch)}\");
  match ap1(Channel::poll, ch) { Option::Some(n) => print(\"poll ${n}\"), Option::None => print(\"poll none\") };
  let r = ap0(Region::open);
  ap2(Region::defer, r, fn () => print(\"deferred\"));
  let _ = ap0(Region::root);
  0
}
",
        "print\nbyte_length 3\nbyte 98\nslice el\nfind 2\nbytes xyz\nis_utf8 true\ncode 65 66\n\
         int 3 3 12\nbits 5 2 7 16 4\nfloat 7\nptr true\narray 9 3 0\nshared 42\n\
         send true depth 1\npoll 5\ndeferred\n",
    );
}

/// Every fixed-width integer's intrinsics. One program per family would be
/// eight builds; one program is one.
#[test]
fn every_fixed_width_intrinsic_runs_through_a_function_value() {
    let mut body = String::new();
    let mut expected = String::new();
    for ty in ["U8", "U16", "U32", "U64", "I8", "I16", "I32"] {
        body.push_str(&format!(
            "  let a = ap1({ty}::of, 6);\n  \
             let b = ap1({ty}::wrapping, 3);\n  \
             print(\"{ty} ${{ap1({ty}::to_int, a)}} \
             ${{{ty}::to_int(ap2({ty}::wrapping_add, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::wrapping_sub, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::wrapping_mul, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::xor, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::and, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::or, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::shl, a, b))}} \
             ${{{ty}::to_int(ap2({ty}::shr, a, b))}}\");\n"
        ));
        expected.push_str(&format!("{ty} 6 9 3 18 5 2 7 48 0\n"));
    }
    body.push_str("  print(\"U64 wrapping_to_int ${ap1(U64::wrapping_to_int, U64::of(5))}\");\n");
    expected.push_str("U64 wrapping_to_int 5\n");
    let source = format!(
        "module main;
import std::core::{{print}};

fn ap1<A, B>(f: (A) -> B, a: A) -> B {{ f(a) }}
fn ap2<A, B, C>(f: (A, B) -> C, a: A, b: B) -> C {{ f(a, b) }}

pub fn main() -> Int {{
{body}  0
}}
"
    );
    runs_on_both("fnval_fixed_width", &source, &expected);
}

/// The operations the runtime implements: fibers, crews, queues, `SharedFn`,
/// the borrowed-buffer functions, and `attempt`, whose value can raise.
#[test]
fn every_runtime_operation_runs_through_a_function_value() {
    runs_on_both(
        "fnval_runtime",
        "module main;
import std::core::{Array, Channel, Fiber, Fibers, Handoff, Option, Outcome, Ptr, Result, Shared, SharedFn, String, attempt, print};

type Oops = | Bad;

fn work() -> Int { 7 }
fn fails() -> Int raises Oops { raise Oops::Bad }
fn bump(n: Int) -> Int { n + 1 }

fn ap0<A>(f: () -> A) -> A { f() }
fn ap1<A, B>(f: (A) -> B, a: A) -> B { f(a) }
fn ap1r<A, B, 'er>(f: (A) -> B raises 'er, a: A) -> B raises 'er { f(a)! }
fn ap2<A, B, C>(f: (A, B) -> C, a: A, b: B) -> C { f(a, b) }

pub fn main() -> Int raises Oops {
  let f = ap1(Fiber::spawn, fn () => work());
  print(\"join ${ap1r(Fiber::join, f)!}\");
  let w = Fiber::spawn(fn () => 6);
  ap1r(Fiber::wait, w)!;
  let o = Fiber::spawn(fn () => 3);
  match ap1(Fiber::outcome, o) { Outcome::Answered(n) => print(\"outcome ${n}\"), Outcome::Stopped => print(\"stopped\") };
  let h = Fiber::spawn(fn () => work());
  let _ = ap1(Fiber::finished, h);
  ap1(Fiber::cancel, h);
  let _ = ap1(Fiber::canceled, h);
  ap2(Fiber::cancel_within, Fiber::spawn(fn () => 4), 10);
  ap1(Fiber::abort, Fiber::spawn(fn () => 5));
  ap1(Fiber::detach, Fiber::spawn(fn () => work()));
  let crew = ap0(Fibers::open);
  ap2(Fibers::spawn, crew, fn () => print(\"crew child\"));
  print(\"crew ${ap1(Fibers::wait, crew)}\");
  print(\"bounded ${Fibers::wait(ap1(Fibers::bounded, 2))}\");
  let ch: Channel<Int> = Channel::bounded(2);
  let _ = ap2(Channel::offer, ch, 3);
  ap1(Channel::close, ch);
  match ap1(Channel::receive, ch) { Option::Some(n) => print(\"receive ${n}\"), Option::None => print(\"none\") };
  let d: Channel<Int> = ap1(Channel::dropping, 1);
  let s: Channel<Int> = ap1(Channel::sliding, 1);
  print(\"dropping ${Channel::depth(d)} sliding ${Channel::depth(s)}\");
  let ho: Handoff<Int> = ap1(Handoff::bounded, 1);
  let _ = ap2(Handoff::send, ho, 8);
  ap1(Handoff::close, ho);
  match ap1(Handoff::receive, ho) { Option::Some(n) => print(\"handoff ${n}\"), Option::None => print(\"none\") };
  let cell = Shared::of(1);
  print(\"update ${ap2(Shared::update, cell, bump)}\");
  let got = ap2(Shared::modify, cell, fn n => { state: n + 1, result: n * 2 });
  print(\"modify ${got} ${Shared::get(cell)}\");
  print(\"sharedfn ${ap2(SharedFn::call, ap1(SharedFn::of, bump), 4)}\");
  print(\"with_data ${ap2(String::with_data, \"abcd\", fn (p, n) => n)}\");
  print(\"with_c_string ${ap2(String::with_c_string, \"ab\", fn p => Ptr::is_null(p))}\");
  print(\"array with_data ${ap2(Array::with_data, String::bytes(\"xyz\"), fn (p, n) => n)}\");
  match ap1(attempt, fn () => fails()!) { Result::Ok(n) => print(\"ok ${n}\"), Result::Err(_) => print(\"attempt err\") };
  0
}
",
        "join 7\noutcome 3\ncrew child\ncrew 0\nbounded 0\nreceive 3\ndropping 0 sliding 0\n\
         handoff 8\nupdate 2\nmodify 4 3\nsharedfn 5\nwith_data 4\nwith_c_string false\n\
         array with_data 3\nattempt err\n",
    );
}
