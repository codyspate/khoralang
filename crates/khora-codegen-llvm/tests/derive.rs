#![cfg(feature = "llvm")]

//! `derive`, run.
//!
//! The checker tests in `khora-types/tests/derive.rs` pin what a derived impl
//! *is*; these pin what it does. Both are needed, and for one reason: a derive
//! is only worth having if the code it writes is the code you would have
//! written, and the only way to know that is to run it and read the answer.
//!
//! What is pinned here is a promise to whoever writes a test against a derived
//! `Show`, and a promise to `Map` about `Hash`:
//!
//! - a record shows as `Point { x: 1, y: 2 }` and a case as `Shape::Circle(2)`;
//! - a record orders by its fields in the order they are declared;
//! - a variant orders by the order its cases are declared, then by payload;
//! - **equal values hash equal**.
//!
//! The module is self-contained rather than compiled against the real `std`,
//! like `effects.rs`: these are questions about the expander, and answering
//! them should not depend on which impls `std/core.kh` happens to have today.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn run(name: &str, source: &str) -> String {
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
    assert_eq!(output.status.code(), Some(0), "`{name}` exited badly");
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n").to_string()
}

/// The four traits as `std/core.kh` declares them, and enough of their scalar
/// impls to have something to derive from. `Show for Int` covers one digit
/// only, which is all any of these need and keeps the module readable.
const CORE: &str = r#"module t;
fn print(value: String);

pub type Ordering = | Less | Equal | Greater;
pub trait Eq { fn eq(self, other: Self) -> Bool; }
pub trait Ord: Eq { fn cmp(self, other: Self) -> Ordering; }
pub trait Hash: Eq { fn hash(self) -> Int; }
pub trait Show { fn show(self) -> String; }

impl Eq for Int { fn eq(self, other: Int) -> Bool { self == other } }
impl Ord for Int {
  fn cmp(self, other: Int) -> Ordering {
    if self < other { Ordering::Less }
    else if self == other { Ordering::Equal }
    else { Ordering::Greater }
  }
}
impl Hash for Int { fn hash(self) -> Int { self } }
impl Show for Int {
  fn show(self) -> String {
    if self == 0 { "0" }
    else if self == 1 { "1" }
    else if self == 2 { "2" }
    else if self == 3 { "3" }
    else { "?" }
  }
}

impl Ordering {
  fn name(self) -> String {
    match self {
      Ordering::Less => "Less",
      Ordering::Equal => "Equal",
      Ordering::Greater => "Greater",
    }
  }
}

fn yes(answer: Bool) -> String { if answer { "yes" } else { "no" } }

derive(Eq, Ord, Show, Hash)
pub type Point = { x: Int, y: Int };

derive(Eq, Ord, Show, Hash)
pub type Shape = | Dot | Circle(r: Int) | Rect(w: Int, h: Int);
"#;

/// The format a test can assert on, and the reason to pin it: change it and
/// somebody's golden file breaks silently.
#[test]
fn show_writes_a_record_by_name_and_a_case_the_way_it_is_constructed() {
    let out = run(
        "derive_show",
        &format!(
            "{CORE}
fn main() -> Int {{
  let p: Point = {{ x: 1, y: 2 }};
  print(p.show());
  print(Shape::Dot.show());
  print(Shape::Circle(2).show());
  print(Shape::Rect(1, 2).show());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "Point { x: 1, y: 2 }\nShape::Dot\nShape::Circle(2)\nShape::Rect(1, 2)\n"
    );
}

#[test]
fn eq_compares_every_field() {
    let out = run(
        "derive_eq",
        &format!(
            "{CORE}
fn main() -> Int {{
  let a: Point = {{ x: 1, y: 2 }};
  let b: Point = {{ x: 1, y: 2 }};
  let c: Point = {{ x: 1, y: 3 }};
  print(yes(a.eq(b)));
  print(yes(a.eq(c)));
  print(yes(Shape::Circle(2).eq(Shape::Circle(2))));
  print(yes(Shape::Circle(2).eq(Shape::Circle(3))));
  print(yes(Shape::Circle(2).eq(Shape::Dot)));
  print(yes(Shape::Dot.eq(Shape::Dot)));
  0
}}
"
        ),
    );
    assert_eq!(out, "yes\nno\nyes\nno\nno\nyes\n");
}

/// The first field that differs decides, so `x` outranks `y` however large `y`
/// gets. Someone who reorders two fields has changed the sort order, which is
/// what every other language means by this and what a reader expects.
#[test]
fn a_record_orders_by_its_fields_in_declaration_order() {
    let out = run(
        "derive_ord_record",
        &format!(
            "{CORE}
fn main() -> Int {{
  let low: Point = {{ x: 1, y: 3 }};
  let high: Point = {{ x: 2, y: 0 }};
  print(low.cmp(high).name());
  print(high.cmp(low).name());
  let same_x: Point = {{ x: 1, y: 0 }};
  print(same_x.cmp(low).name());
  print(low.cmp(low).name());
  0
}}
"
        ),
    );
    assert_eq!(out, "Less\nGreater\nLess\nEqual\n");
}

/// Declaration order decides which case is `Less`; payloads only break a tie
/// between two of the same case.
#[test]
fn a_variant_orders_by_declaration_then_by_payload() {
    let out = run(
        "derive_ord_variant",
        &format!(
            "{CORE}
fn main() -> Int {{
  print(Shape::Dot.cmp(Shape::Circle(0)).name());
  print(Shape::Circle(3).cmp(Shape::Rect(0, 0)).name());
  print(Shape::Circle(1).cmp(Shape::Circle(2)).name());
  print(Shape::Circle(2).cmp(Shape::Circle(1)).name());
  print(Shape::Rect(1, 1).cmp(Shape::Rect(1, 2)).name());
  print(Shape::Rect(1, 2).cmp(Shape::Rect(1, 2)).name());
  0
}}
"
        ),
    );
    assert_eq!(out, "Less\nLess\nLess\nGreater\nLess\nEqual\n");
}

/// The invariant `std/core.kh` states on `Hash`: a map finds a key by hashing
/// it and then comparing, so two values that `eq` says are the same and `hash`
/// sends to different buckets are an entry that can be inserted and never
/// found. A derived pair cannot disagree, because both visit the same fields.
#[test]
fn equal_values_hash_equal() {
    let out = run(
        "derive_hash",
        &format!(
            "{CORE}
fn main() -> Int {{
  let a: Point = {{ x: 1, y: 2 }};
  let b: Point = {{ x: 0 + 1, y: 1 + 1 }};
  print(yes(a.eq(b)));
  print(yes(a.hash() == b.hash()));
  print(yes(Shape::Circle(2).eq(Shape::Circle(1 + 1))));
  print(yes(Shape::Circle(2).hash() == Shape::Circle(1 + 1).hash()));
  0
}}
"
        ),
    );
    assert_eq!(out, "yes\nyes\nyes\nyes\n");
}

/// Two payload-free cases would hash alike if the case's position were not
/// part of the answer, which is a map with every `Dot` and every `Nothing` in
/// one bucket.
#[test]
fn two_cases_are_told_apart_by_their_position() {
    let out = run(
        "derive_hash_cases",
        &format!(
            "{CORE}
fn main() -> Int {{
  print(yes(Shape::Dot.hash() == Shape::Circle(0).hash()));
  print(yes(Shape::Circle(0).hash() == Shape::Rect(0, 0).hash()));
  0
}}
"
        ),
    );
    assert_eq!(out, "no\nno\n");
}

/// A hash is a fold over the fields, and Khora's `*` and `+` trap on overflow.
/// A field big enough to overflow a naive `hash * 31 + next` is not exotic —
/// `Hash for Int` is the identity — so this is the test that the accumulator
/// stays in range rather than aborting the program.
#[test]
fn a_large_field_does_not_overflow_the_hash() {
    let out = run(
        "derive_hash_large",
        &format!(
            "{CORE}
fn main() -> Int {{
  let huge: Point = {{ x: 4000000000000000000, y: 4000000000000000000 }};
  print(yes(huge.hash() == huge.hash()));
  0
}}
"
        ),
    );
    assert_eq!(out, "yes\n");
}

/// A derived `cmp` on a variant matches its receiver three times — once for
/// each side's position and once for the payload — and a derived `eq` matches
/// it twice. That is the shape reference counting is most likely to get wrong,
/// so this counts what is still alive after the values are gone.
#[test]
fn a_derived_method_leaves_nothing_behind() {
    let out = run(
        "derive_liveness",
        &format!(
            "{CORE}
extern fn khora_live_count() -> Int;

fn work() -> Bool {{
  let a: Point = {{ x: 1, y: 2 }};
  let b: Point = {{ x: 1, y: 3 }};
  let wide = Shape::Rect(1, 2);
  a.eq(b) || wide.eq(Shape::Rect(1, 2)) || wide.cmp(Shape::Circle(1)).name() == \"Greater\"
}}

fn main() -> Int {{
  print(yes(work()));
  print(khora_live_count().show());
  0
}}
"
        ),
    );
    assert_eq!(out, "yes
0
");
}

/// A derived impl is an ordinary impl, so it satisfies a bound and the generic
/// function that asked for one is monomorphized against it — twice here, for
/// two different types, from one written function.
///
/// One bound per parameter, not `T: Ord + Show`. Two of them on one parameter
/// silently parse as a single bound named nothing, so the methods of both go
/// missing; that is a front-end bug older than this file and not one to fix
/// from inside a test.
#[test]
fn a_derived_impl_serves_a_generic_function() {
    let out = run(
        "derive_generic",
        &format!(
            "{CORE}
fn smaller<T: Ord>(a: T, b: T) -> T {{
  if a.cmp(b).name() == \"Greater\" {{ b }} else {{ a }}
}}

fn describe<T: Show>(value: T) -> String {{ value.show() }}

fn main() -> Int {{
  let low: Point = {{ x: 1, y: 0 }};
  let high: Point = {{ x: 2, y: 0 }};
  print(describe(smaller(high, low)));
  print(describe(smaller(Shape::Rect(1, 1), Shape::Circle(3))));
  0
}}
"
        ),
    );
    assert_eq!(out, "Point { x: 1, y: 0 }\nShape::Circle(3)\n");
}
