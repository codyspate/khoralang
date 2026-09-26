#![cfg(feature = "llvm")]

//! A `catch` over a raise that always happens, end to end.
//!
//! **The value is the arm's.** `(raise F::W) catch { F::W => 7 }` was typed
//! from its operand, a `Never`, so code generation kept no slot for what the
//! arm produced, and used as a value -- an interpolation, a function's tail,
//! an argument -- the `catch` yielded 0. Through an `if` it yielded 7.
//!
//! And the error an untyped `raise` sends. `raise e`, with `e` a lambda
//! parameter typed only later, was charged to no row: the `catch` beside it
//! did not see it and the error escaped a function called infallible, with
//! exit 130 and no message.

use crate::matching::{refused, run_both};

const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

impl String {
  fn byte_length(self) -> Int;
}

pub type E = | X | Y;
pub type F = | Z(n: Int) | W;
pub type Gx<A> = | X(s: String, v: A) | Y(n: Int);
pub type Nf = { p: String };
pub type Dn = { q: Int };

fn fs() -> Int raises Gx<String> { raise Gx::X(\"s\" + \"1\", \"str\" + \"2\") }
fn fi() -> Int raises Gx<Int> { raise Gx::X(\"i\" + \"1\", 3) }
fn nf() -> Int raises Nf { raise { p: \"x\" } }
fn id(n: Int) -> Int { n }
";

/// Each value position the `catch` can stand in: a tail, an argument, an
/// operand of `+`, a block's last expression, an arm of another `catch`, and
/// `raise` inside an `if` and a `match` under one.
#[test]
fn a_catch_over_a_raise_yields_its_arms_value_everywhere() {
    let ran = run_both(
        "raw_catch_values",
        &format!(
            "{PRELUDE}
fn tail() -> Int {{ (raise F::W) catch {{ F::W => 7, F::Z(k) => k }} }}
fn block() -> Int {{ ({{ let x = 8; raise F::Z(x) }}) catch {{ F::W => 7, F::Z(k) => k }} }}
fn nested() -> Int {{
  (raise E::X) catch {{ E::X => (raise F::W) catch {{ F::W => 9, F::Z(k) => k }}, E::Y => 1 }}
}}
fn in_if(n: Int) -> Int {{ (if n < 0 {{ raise F::W }} else {{ raise F::Z(n) }}) catch {{ F::W => 7, F::Z(k) => k }} }}
fn in_match(n: Int) -> Int {{
  (match n {{ 0 => raise F::W, _ => raise F::Z(n) }}) catch {{ F::W => 11, F::Z(k) => k + 1 }}
}}
fn main() -> Int {{
  print(tail());
  print(block());
  print(id((raise F::W) catch {{ F::W => 12, F::Z(k) => k }}));
  print(nested());
  print(in_if(0 - 1));
  print(in_if(14));
  print(in_match(0));
  print(in_match(15));
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "7\n8\n12\n9\n7\n14\n11\n16\n0\n",
        "stderr: {}",
        ran.stderr
    );
    assert_eq!(ran.code, Some(0));
}

/// A `String` in the same position: typed `Never`, it reached the code
/// generator as a missing value and panicked it ("expected PointerValue").
#[test]
fn a_catch_over_a_raise_yields_a_string_arm() {
    let ran = run_both(
        "raw_catch_string",
        &format!(
            "{PRELUDE}fn main() -> Int {{
  print(String::byte_length(\"<\" + ((raise F::W) catch {{ F::W => \"seven\", F::Z(_) => \"\" }}) + \">\"));
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "7\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// A `let` of one: refused while the `catch` was typed `Never` ("`v` has
/// type `Never`"), which is how the value positions above were the only way
/// to reach the 0.
#[test]
fn a_let_of_a_catch_over_a_raise_binds_its_arms_value() {
    let ran = run_both(
        "raw_catch_let",
        &format!(
            "{PRELUDE}fn main() -> Int {{
  let v = (raise F::Z(13)) catch {{ F::W => 7, F::Z(k) => k }};
  print(v);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "13\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// Arms that all leave too: the `catch` never finishes, and the raise from
/// its arm reaches the one outside.
#[test]
fn a_catch_whose_arms_all_raise_passes_the_raise_on() {
    let ran = run_both(
        "raw_catch_arms_raise",
        &format!(
            "{PRELUDE}
fn inner() -> Int raises E {{ (raise F::W) catch {{ F::W => raise E::Y, F::Z(_) => raise E::X }} }}
fn main() -> Int {{
  print(inner()! catch {{ E::X => 1, E::Y => 2 }});
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "2\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// Exit 130 with no message, then a trap naming a compiler bug: `e` turns out
/// a `Dn`, which the `catch` does not name. Now the closure's row carries
/// `Dn`, and a caller that does not say it raises `Dn` is refused.
#[test]
fn an_untyped_raise_the_catch_does_not_name_is_refused_at_the_caller() {
    let found = refused(
        "late_raise_escapes",
        &format!(
            "{PRELUDE}fn main() -> Int {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ nf()! }}) catch {{ Nf {{ p }} => 1 }}; \
             let d: Dn = {{ q: 4 }}; print(k(d)); 0 }}\n"
        ),
    );
    assert!(found.iter().any(|e| e.contains("needs `!`")), "{found:?}");
}

/// And declared, the error leaves through the function's row to a caller's
/// `catch`, which handles it.
///
/// Not a `catch` right around `k(d)!` in the same body: that one is checked
/// before `e`'s type is known, sees nothing it can name, and is refused with
/// "nothing in this expression raises `Dn`" -- the same rule that keeps
/// `(raise e) catch { .. }` refused on its own.
#[test]
fn an_untyped_raise_reaches_the_catch_outside_the_closure() {
    let ran = run_both(
        "late_raise_caught_outside",
        &format!(
            "{PRELUDE}fn work() -> Int raises Dn {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ nf()! }}) catch {{ Nf {{ p }} => 1 }}; \
             let d: Dn = {{ q: 4 }}; k(d)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Dn {{ q }} => q * 10 }}); print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "40\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// The generic form: a `Gx<Int>` raised where the arm was bound at the
/// `Gx<String>` `fs` raises. Exited 130 (the arm read an `Int` as a string's
/// pointer on 0.3.0, 139); refused as a typed raise would be.
#[test]
fn an_untyped_raise_at_another_instantiation_is_refused() {
    let found = refused(
        "late_raise_two_instantiations",
        &format!(
            "{PRELUDE}fn main() -> Int {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ fs()! }}) catch {{ \
             Gx::X(s, v) => String::byte_length(v), Gx::Y(n) => n }}; \
             print(k(Gx::X(\"a\" + \"1\", 3))); 0 }}\n"
        ),
    );
    assert!(found.iter().any(|e| e.contains("raises two")), "{found:?}");
}

/// The instantiation the `catch` names is handled by it, as before.
#[test]
fn an_untyped_raise_the_catch_names_is_handled() {
    let ran = run_both(
        "late_raise_handled",
        &format!(
            "{PRELUDE}fn main() -> Int {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ fi()! }}) catch {{ \
             Gx::X(s, v) => v * 10, Gx::Y(n) => n }}; \
             print(k(Gx::X(\"a\" + \"1\", 3))); print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "30\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}
