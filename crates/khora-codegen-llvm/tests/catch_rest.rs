#![cfg(feature = "llvm")]

//! A `catch` whose named arms take part of an error type, beside an arm
//! that takes the rest.
//!
//! **A case the named arms do not take goes to the catch-all.** Code
//! generation sent every error of a named type into that type's arms and,
//! when none of them matched, reached the "no arm applied" trap: the checker
//! had accepted `catch { PaymentError::Declined(why) => .., _ => .. }`, and
//! a `GatewayTimeout` stopped the program with `Illegal instruction`.
//!
//! **And the error is still whole when it gets there.** A named type's
//! arms test the case before anything takes the error over, so the
//! catch-all releases it, or binds it, exactly as if no named arm had been
//! written. The live counts below are what show that.

use crate::matching::run_both;

const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

impl String {
  fn byte_length(self) -> Int;
}

pub type Two = | Named(s: String) | Plain;
pub type Three = | P(s: String) | Q(n: Int) | R(s: String);
pub type Other = | O(s: String);
pub type Wide = | W(a: String, b: String, c: String, d: String) | V;

fn two(n: Int) -> Int raises Two {
  if n == 0 { raise Two::Named(\"na\" + \"med\") };
  raise Two::Plain
}
fn three(n: Int) -> Int raises Three {
  if n == 0 { raise Three::P(\"p\" + \"p\") };
  if n == 1 { raise Three::Q(n + 40) };
  raise Three::R(\"rrr\" + \"r\")
}
fn mixed(n: Int) -> Int raises Three + Other {
  if n == 3 { raise Other::O(\"o\" + \"ooooo\") };
  three(n)!
}
fn wide(n: Int) -> Int raises Wide {
  if n == 0 { raise Wide::W(\"a\" + \"a\", \"b\" + \"b\", \"c\" + \"c\", \"d\" + \"d\") };
  raise Wide::V
}
";

/// Two cases, each named in turn, beside `_` and beside a binding. The
/// binding reads the case it was given, which is what says the error
/// reached it intact.
#[test]
fn a_two_case_type_takes_the_arm_for_each_case() {
    let ran = run_both(
        "catch_rest_two",
        &format!(
            "{PRELUDE}
fn wild_named(n: Int) -> Int {{ two(n)! catch {{ Two::Named(s) => String::byte_length(s), _ => 0 - 1 }} }}
fn wild_plain(n: Int) -> Int {{ two(n)! catch {{ Two::Plain => 7, _ => 0 - 1 }} }}
fn bound_named(n: Int) -> Int {{
  two(n)! catch {{
    Two::Named(s) => String::byte_length(s),
    e => match e {{ Two::Plain => 100, Two::Named(s) => 200 + String::byte_length(s) }},
  }}
}}
fn bound_plain(n: Int) -> Int {{
  two(n)! catch {{
    Two::Plain => 7,
    e => match e {{ Two::Plain => 100, Two::Named(s) => 200 + String::byte_length(s) }},
  }}
}}
fn main() -> Int {{
  print(wild_named(0));
  print(wild_named(1));
  print(wild_plain(0));
  print(wild_plain(1));
  print(bound_named(0));
  print(bound_named(1));
  print(bound_plain(0));
  print(bound_plain(1));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n-1\n-1\n7\n5\n100\n205\n7\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// Three cases: one named, two named, and a refutable field and a guard,
/// which send a case they do not accept on to the catch-all too.
#[test]
fn a_three_case_type_takes_the_arm_for_each_case() {
    let ran = run_both(
        "catch_rest_three",
        &format!(
            "{PRELUDE}
fn one(n: Int) -> Int {{ three(n)! catch {{ Three::Q(k) => k, _ => 0 - 1 }} }}
fn both(n: Int) -> Int {{
  three(n)! catch {{
    Three::P(s) => String::byte_length(s),
    Three::R(s) => 10 + String::byte_length(s),
    e => match e {{ Three::Q(k) => k * 2, Three::P(_) => 0 - 2, Three::R(_) => 0 - 3 }},
  }}
}}
fn refuted(n: Int) -> Int {{ three(n)! catch {{ Three::Q(41) => 1, Three::P(s) => 2, _ => 3 }} }}
fn guarded(n: Int) -> Int {{
  three(n)! catch {{ Three::R(s) if String::byte_length(s) > 9 => 1, _ => 2 }}
}}
fn main() -> Int {{
  print(one(0));
  print(one(1));
  print(one(2));
  print(both(0));
  print(both(1));
  print(both(2));
  print(refuted(0));
  print(refuted(1));
  print(refuted(2));
  print(guarded(2));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-1\n41\n-1\n2\n82\n14\n2\n1\n3\n2\n0\n",
        "stderr: {}",
        ran.stderr
    );
    assert_eq!(ran.code, Some(0));
}

/// A second error type beside the partly named one: the catch-all takes
/// the cases of both, and the named arm still takes its own.
#[test]
fn a_partly_named_type_beside_another_type_takes_the_arm_for_each() {
    let ran = run_both(
        "catch_rest_mixed",
        &format!(
            "{PRELUDE}
fn pick(n: Int) -> Int {{ mixed(n)! catch {{ Three::Q(k) => k, _ => 0 - 1 }} }}
fn main() -> Int {{
  print(pick(0));
  print(pick(1));
  print(pick(2));
  print(pick(3));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1\n41\n-1\n-1\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// A type held behind a pointer, not inline: four fields is more than an
/// inline value carries. The case with the payload falls through and is
/// released whole.
#[test]
fn a_boxed_type_takes_the_arm_for_each_case() {
    let ran = run_both(
        "catch_rest_wide",
        &format!(
            "{PRELUDE}
fn plain(n: Int) -> Int {{ wide(n)! catch {{ Wide::V => 7, _ => 0 - 1 }} }}
fn bound(n: Int) -> Int {{
  wide(n)! catch {{
    Wide::V => 7,
    e => match e {{ Wide::W(a, b, c, d) => String::byte_length(a + b + c + d), Wide::V => 0 }},
  }}
}}
fn main() -> Int {{
  print(plain(0));
  print(plain(1));
  print(bound(0));
  print(bound(1));
  let live = khora_live_count();
  print(live);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1\n7\n8\n7\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// Many errors through the fall-through, each carrying a counted payload,
/// inline and boxed, to `_` and to a binding: nothing is left alive. A
/// fall-through that took the error over before testing its case would
/// free it twice or not at all, and this count is what moves.
#[test]
fn the_fall_through_path_releases_what_it_was_given() {
    let ran = run_both(
        "catch_rest_live",
        &format!(
            "{PRELUDE}
fn wild(n: Int) -> Int {{ three(n)! catch {{ Three::Q(k) => k, _ => 1 }} }}
fn bound(n: Int) -> Int {{
  three(n)! catch {{ Three::Q(k) => k, e => match e {{ Three::R(s) => String::byte_length(s), Three::P(_) => 1, Three::Q(_) => 0 }} }}
}}
fn boxed(n: Int) -> Int {{ wide(n)! catch {{ Wide::V => 7, _ => 1 }} }}
fn boxed_bound(n: Int) -> Int {{
  wide(n)! catch {{ Wide::V => 7, e => match e {{ Wide::W(a, _, _, _) => String::byte_length(a), Wide::V => 0 }} }}
}}
fn small(n: Int) -> Int {{ two(n)! catch {{ Two::Plain => 7, _ => 1 }} }}
fn small_bound(n: Int) -> Int {{
  two(n)! catch {{ Two::Plain => 7, e => match e {{ Two::Named(s) => String::byte_length(s) - 4, Two::Plain => 0 }} }}
}}
fn main() -> Int {{
  let before = khora_live_count();
  let mut i = 0;
  let mut sum = 0;
  while i < 300 {{
    sum = sum + wild(2) + bound(2) + wild(0) + boxed(0) + boxed_bound(0) + small(0) + small_bound(0);
    i = i + 1;
  }}
  let after = khora_live_count();
  print(sum);
  print(after - before);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "3300\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// **A guard that fails tries the next arm's pattern, in a `catch` and in a
/// `match`.** It jumped to the next arm's bindings without testing them:
/// `A(s) if long(s)` failing ran `B(k) => k` on the `A`, and printed its
/// string's address as `k` (the review's `p1_guard`, `2199025680448` where
/// `99` is right). A failing guard on the last named arm goes to the
/// catch-all.
#[test]
fn a_failed_guard_tries_the_next_arm() {
    let ran = run_both(
        "catch_rest_guards",
        "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;
impl String {
  fn byte_length(self) -> Int;
}
pub type G = | A(s: String) | B(n: Int) | C;
fn boom(n: Int) -> Int raises G {
  if n == 0 { raise G::A(\"he\" + \"llo\") };
  if n == 1 { raise G::C };
  raise G::B(n)
}
fn long(s: String) -> Bool { String::byte_length(s) > 100 }
fn c1(n: Int) -> Int { boom(n)! catch { G::A(s) if long(s) => 1, G::B(k) => k, _ => 99 } }
fn c2(n: Int) -> Int {
  boom(n)! catch {
    G::B(k) if k > 10 => k,
    e => match e { G::A(s) => String::byte_length(s), G::B(k) => 0 - k, G::C => 7 },
  }
}
fn m1(v: G) -> Int { match v { G::A(s) if long(s) => 1, G::B(k) => k, _ => 99 } }
fn m2(v: G) -> Int {
  match v { G::B(k) if k > 10 => k, G::A(s) => String::byte_length(s), G::B(k) => 0 - k, G::C => 7 }
}
fn main() -> Int {
  let before = khora_live_count();
  print(c1(0)); print(c1(1)); print(c1(3));
  print(c2(0)); print(c2(1)); print(c2(3)); print(c2(30));
  print(m1(G::A(\"x\" + \"y\"))); print(m1(G::C)); print(m1(G::B(3)));
  print(m2(G::A(\"x\" + \"y\"))); print(m2(G::C)); print(m2(G::B(3))); print(m2(G::B(30)));
  let after = khora_live_count();
  print(after - before);
  0
}
",
    );
    assert_eq!(
        ran.stdout,
        "99\n99\n3\n5\n7\n-3\n30\n99\n99\n3\n2\n7\n-3\n30\n0\n",
        "stderr: {}",
        ran.stderr
    );
    assert_eq!(ran.code, Some(0));
}

/// **A guarded catch-all hands what its guard turns away to the arms after
/// it.** The director's probe, `handle`, with its three arms: a guarded
/// named arm, a guarded binding, `_`. It was refused ("a `catch` arm that
/// handles every failure cannot have a guard"). `wild` and `chain` do the
/// same with a guarded `_` and two guarded catch-alls in a row; every error
/// carries a counted `String`, so a failed guard that released the error
/// before the next arm read it, or never released it, moves the count.
#[test]
fn a_guarded_catch_all_falls_to_the_next_catch_all() {
    let ran = run_both(
        "catch_rest_guarded_all",
        "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;
impl String {
  fn byte_length(self) -> Int;
}
pub type E = | A(n: Int) | B(s: String);
fn boom(n: Int) -> Int raises E {
  if n == 0 { raise E::A(5) };
  if n == 1 { raise E::B(\"x\" + \"y\") };
  n
}
fn handle(n: Int) -> Int {
  boom(n)! catch {
    E::A(k) if k > 3 => 100 + k,
    e if n == 1 => 200,
    _ => 300,
  }
}
fn wild(n: Int) -> Int {
  boom(n)! catch {
    _ if n == 0 => 1,
    e => match e { E::A(k) => k, E::B(s) => String::byte_length(s) },
  }
}
fn chain(n: Int, m: Int) -> Int {
  boom(n)! catch {
    e if m == 0 => match e { E::A(k) => 10 + k, E::B(s) => 20 + String::byte_length(s) },
    _ if m == 1 => 30,
    e => match e { E::A(k) => 40 + k, E::B(s) => 50 + String::byte_length(s) },
  }
}
fn main() -> Int {
  let before = khora_live_count();
  print(handle(0)); print(handle(1)); print(handle(7));
  print(wild(0)); print(wild(1));
  print(chain(1, 0)); print(chain(1, 1)); print(chain(1, 2)); print(chain(0, 2));
  let after = khora_live_count();
  print(after - before);
  0
}
",
    );
    assert_eq!(
        ran.stdout,
        "105\n200\n7\n1\n2\n22\n30\n52\n45\n0\n",
        "stderr: {}",
        ran.stderr
    );
    assert_eq!(ran.code, Some(0));
}
