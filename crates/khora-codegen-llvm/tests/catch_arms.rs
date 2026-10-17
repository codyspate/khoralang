#![cfg(feature = "llvm")]

//! What leaves a `catch` through its own arms, end to end.
//!
//! **A `raise` in an arm leaves the `catch` it is written in.** The checker
//! subtracted it with the operand's errors, so a function re-raising a type
//! it did not declare was accepted, and the error met a caller's `catch` the
//! checker had called total: a trap naming a compiler bug, exit 134. Inside
//! another `catch`'s operand, that outer `catch` is where it goes.
//!
//! **A rigid `'er` stays in the row it was joined into**, so a function
//! that declares it passes the caller's error on to the caller.

use crate::matching::{refused, run_both};

const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

impl String {
  fn byte_length(self) -> Int;
}

pub type Inv = | OutOfStock(sku: String) | Offline;
pub type Pay = | Declined(reason: String) | Timeout;
pub type Audit = | Tampered(who: String);

fn inv(n: Int) -> Int raises Inv {
  if n == 0 { raise Inv::Offline };
  if n == 1 { raise Inv::OutOfStock(\"sk\" + \"u1\") };
  n
}
fn pay(n: Int) -> Int raises Pay {
  if n == 0 { raise Pay::Timeout };
  n
}
";

/// The inner arm re-raises, the outer `catch` handles it, and every input
/// takes the arm that names its case.
#[test]
fn a_raise_in_an_inner_arm_reaches_the_outer_catch() {
    let ran = run_both(
        "catch_arms_nested",
        &format!(
            "{PRELUDE}
fn total(n: Int) -> Int {{
  (inv(n)! catch {{ e => raise e }}) catch {{
    Inv::OutOfStock(s) => String::byte_length(s),
    Inv::Offline => 0 - 1,
  }}
}}
fn main() -> Int {{
  print(total(0));
  print(total(1));
  print(total(2));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1\n4\n2\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// The shape that trapped, refused before it is built, naming the type.
#[test]
fn a_raise_in_an_arm_the_function_does_not_declare_is_refused() {
    let found = refused(
        "catch_arms_undeclared",
        &format!(
            "{PRELUDE}
fn narrow(n: Int) -> Int raises Pay {{ inv(n)! catch {{ e => raise e }} }}
fn main() -> Int {{
  print(narrow(0)! catch {{ Pay::Declined(_) => 1, Pay::Timeout => 2 }});
  0
}}
"
        ),
    );
    assert!(found.iter().any(|e| e.contains("`Inv`")), "{found:?}");
}

/// A closure joining `pay`'s row with `'er`, in a function that declares
/// both: an `Audit` raised through `'er` reaches `main`'s `catch`, and a
/// `Pay` raised through `pay` does too.
///
/// This pins that a declared `'er` still works after the join keeps it. It
/// stays green with that change undone, since the declaration closes the
/// closure's open tail to `'er` on its own; the refusal of the undeclared
/// form is guarded by `khora-types`'s
/// `a_rigid_tail_joined_with_an_open_one_is_required`.
#[test]
fn a_rigid_tail_joined_in_a_closure_reaches_the_caller() {
    let ran = run_both(
        "catch_arms_rigid",
        &format!(
            "{PRELUDE}
fn joined<'er>(n: Int, g: () -> Int raises 'er) -> Int raises Pay + 'er {{
  let f = fn () => pay(n)!;
  let k = fn () => if n == 0 {{ f()! }} else {{ g()! }};
  k()!
}}
fn run(n: Int) -> Int {{
  joined(n, fn () => raise Audit::Tampered(\"m\" + \"allory\"))! catch {{
    Pay::Timeout => 1,
    Pay::Declined(_) => 2,
    Audit::Tampered(who) => String::byte_length(who),
  }}
}}
fn main() -> Int {{
  print(run(0));
  print(run(1));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n7\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// The review's three: a closure `k` calls beside `g()!` keeps its own row.
/// `f` is passed where `raises Pay` is wanted, caught by its own type, and
/// one that cannot fail is called without `!`. All three ran this way on
/// 0.4.0, and were refused while the join unified `f`'s row with `'er`.
#[test]
fn closures_called_beside_a_rigid_tail_keep_their_own_rows() {
    let ran = run_both(
        "catch_arms_widen",
        &format!(
            "{PRELUDE}
fn takes_pay(cb: () -> Int raises Pay) -> Int {{
  cb()! catch {{ Pay::Declined(r) => String::byte_length(r), Pay::Timeout => 9 }}
}}
fn widened<'er>(g: () -> Int raises 'er) -> Int raises Pay + 'er {{
  let f = fn () => pay(0)!;
  let k = fn () => if false {{ f()! }} else {{ g()! }};
  takes_pay(f) * 100 + k()!
}}
fn caught<'er>(g: () -> Int raises 'er) -> Int {{
  let f = fn () => pay(0)!;
  let k = fn () => if false {{ f()! }} else {{ g()! }};
  let a = k()! catch {{ _ => 1 }};
  let b = f()! catch {{ Pay::Declined(_) => 2, Pay::Timeout => 3 }};
  a * 10 + b
}}
fn pure<'er>(g: () -> Int raises 'er) -> Int {{
  let f = fn () => 4;
  let k = fn () => f() + g()!;
  let a = k()! catch {{ _ => 0 - 1 }};
  a * 10 + f()
}}
fn main() -> Int {{
  print(widened(fn () => 5)! catch {{ _ => 0 - 1 }});
  print(caught(fn () => 6));
  print(pure(fn () => 7));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    // `pay(0)` raises `Timeout`: `takes_pay` answers 9 for it; `caught`'s
    // `k` returns `g`'s 6 and `f` takes the `Timeout` arm; `pure` adds `g`'s
    // 7 to `f`'s 4.
    assert_eq!(ran.stdout, "905\n63\n114\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// The binding arm over a closure whose raise is typed later, where that
/// raise is the binding's own type: the binding is a `Raised` over what can
/// reach it, raised into named arms, and each case reaches the arm for it.
///
/// It ran and stopped with `Illegal instruction` (exit 132) when the binding
/// was given the one type before the later raise was known.
#[test]
fn a_binding_arm_over_a_closure_raising_its_type_later_reads_it() {
    let ran = run_both(
        "catch_arms_late_bind",
        &format!(
            "{PRELUDE}
fn late(which: Int) -> Int {{
  let wrapper = fn e => {{
    let k = fn () => raise e;
    (if which == 0 {{ pay(0)! }} else {{ k()! }}) catch {{
      err => (raise err) catch {{ Pay::Declined(r) => String::byte_length(r), Pay::Timeout => 9 }},
    }}
  }};
  wrapper(Pay::Declined(\"ca\" + \"rd\"))
}}
fn main() -> Int {{
  print(late(0));
  print(late(1));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    // `pay(0)` raises `Timeout` (9); `k` raises the `Declined("card")` (4).
    assert_eq!(ran.stdout, "9\n4\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// The review's `three_deep_late`: `raise x` in the middle `catch`'s arm,
/// `x` typed by the call after all three. The outer `catch` was refused,
/// "nothing in this expression raises `E3`".
#[test]
fn a_raise_typed_later_in_an_inner_arm_reaches_the_outer_catch() {
    let ran = run_both(
        "catch_arms_three_deep_late",
        "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;
impl String {
  fn byte_length(self) -> Int;
}
pub type E1 = | One(s: String) | OneB;
pub type E2 = | Two(s: String) | TwoB;
pub type E3 = | Three(s: String) | ThreeB;
fn do_e1(n: Int) -> Int raises E1 {
  if n == 0 { raise E1::One(\"e1_\" + \"str\") };
  if n == 1 { raise E1::OneB };
  n
}
fn use_e3(x: E3) -> Int { match x { E3::Three(s) => String::byte_length(s), E3::ThreeB => 0 } }
fn three_deep_late(n: Int) -> Int {
  let k = fn x => {
    let r = ((do_e1(n)! catch {
      E1::One(_) => raise E2::Two(\"mid\"),
      E1::OneB => 10,
    }) catch {
      E2::Two(_) => raise x,
      E2::TwoB => 20,
    }) catch {
      E3::Three(s) => String::byte_length(s),
      E3::ThreeB => 30,
    };
    r + use_e3(x)
  };
  k(E3::Three(\"th\" + \"ree\"))
}
fn main() -> Int {
  print(three_deep_late(0));
  print(three_deep_late(1));
  print(three_deep_late(5));
  let live = khora_live_count();
  print(live);
  0
}
",
    );
    assert_eq!(ran.stdout, "10\n15\n10\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// `(raise e) catch { Gx::X(..) => .., Gx::Y(..) => .. }` with `e` typed
/// later. Only the first arm's instantiation was tied to the raise, so the
/// `Gx::Y` arm was compiled for a `Gx` of its own, the error missed it, and
/// the program stopped in the trap for a total `catch` (exit 132).
#[test]
fn an_untyped_raise_alone_in_a_catch_takes_its_arms() {
    let ran = run_both(
        "catch_arms_alone",
        "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;
impl String {
  fn byte_length(self) -> Int;
}
pub type Gx<A> = | X(s: String, v: A) | Y(n: Int);
fn alone(n: Int) -> Int {
  let k = fn (e) => (raise e) catch { Gx::X(s, v) => v + String::byte_length(s), Gx::Y(m) => m };
  if n == 0 { k(Gx::X(\"ab\" + \"c\", 40)) } else { k(Gx::Y(n)) }
}
fn main() -> Int {
  print(alone(0));
  print(alone(7));
  let live = khora_live_count();
  print(live);
  0
}
",
    );
    assert_eq!(ran.stdout, "43\n7\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}
