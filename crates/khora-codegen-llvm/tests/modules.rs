#![cfg(feature = "llvm")]

//! Compiling and linking a program made of several modules.
//!
//! Whole-program, not separate compilation: a generic function is compiled by
//! substituting its type arguments into its *body*, so every module's source
//! has to be present at once. The same constraint C++ templates and Rust
//! generics have, and the reason a symbol carries the module that defines it.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

/// Compiles every `(name, source)` as one program and runs it.
fn run(test: &str, modules: &[(&str, &str)]) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = modules
        .iter()
        .map(|(name, source)| {
            SourceFile::new(&db, dir.join(format!("{name}.kh")), source.to_string())
        })
        .collect();
    let root = SourceRoot::new(&db, files);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{test}` failed:\n  {}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

const LIB: (&str, &str) = (
    "lib",
    "module demo::lib;
pub type Option<A> = | Some(value: A) | None;
pub type Step<S, A> = | Yield(state: S, item: A) | Done;
pub type Range = | Of(from: Int, to: Int);

pub fn double(x: Int) -> Int { x * 2 }

pub trait Iterator {
  type Item;
  fn next(self) -> Step<Self, Self::Item>;
}

impl Iterator for Range {
  type Item = Int;
  fn next(self) -> Step<Range, Int> {
    match self {
      Range::Of(from, to) => if from >= to {
        Step::Done
      } else {
        Step::Yield(Range::Of(from + 1, to), from)
      },
    }
  }
}

impl<A> Option<A> {
  pub fn unwrap_or(self, fallback: A) -> A {
    match self { Option::Some(v) => v, Option::None => fallback }
  }
}
",
);

#[test]
fn a_concrete_function_is_callable_across_modules() {
    let ran = run(
        "cross_concrete",
        &[
            LIB,
            (
                "main",
                "module demo::main;
import demo::lib::{double};
fn print(value: Int);
fn main() -> Int { print(double(21)); 0 }
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// The case separate compilation cannot serve: the body has to be specialized
/// at the *use* site, in a module that never declared it.
#[test]
fn a_generic_method_is_instantiated_across_modules() {
    let ran = run(
        "cross_generic",
        &[
            LIB,
            (
                "main",
                "module demo::main;
import demo::lib::{Option};
fn print(value: Int);
fn main() -> Int {
  print(Option::Some(41).unwrap_or(0) + 1);
  print(Option::None.unwrap_or(99));
  // A third instantiation, at `Bool`, observed through an `if`.
  print(if Option::Some(true).unwrap_or(false) { 7 } else { 0 });
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "42\n99\n7\n", "three instantiations of one imported method");
    assert_eq!(ran.code, Some(0));
}

/// `for` desugars to `Iterator::next` and `Step`, which arrive by import.
#[test]
fn a_for_loop_iterates_an_imported_type() {
    let ran = run(
        "cross_for",
        &[
            LIB,
            (
                "main",
                "module demo::main;
import demo::lib::{Step, Range, Iterator};
fn print(value: Int);
fn main() -> Int {
  let mut total = 0;
  for n in Range::Of(1, 6) {
    total = total + n;
  }
  print(total);
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "15\n");
    assert_eq!(ran.code, Some(0));
}

/// Two modules may each declare a `helper`. Symbols carry the module that
/// defines them, so both are emitted and neither is silently renamed.
#[test]
fn two_modules_may_declare_the_same_name() {
    let ran = run(
        "cross_collide",
        &[
            (
                "a",
                "module demo::a;
pub fn helper() -> Int { 1 }
",
            ),
            (
                "b",
                "module demo::b;
pub fn helper() -> Int { 2 }
",
            ),
            (
                "main",
                "module demo::main;
import demo::a::{helper as first};
import demo::b::{helper as second};
fn print(value: Int);
fn main() -> Int { print(first()); print(second()); 0 }
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "1\n2\n", "each module's `helper` is its own function");
    assert_eq!(ran.code, Some(0));
}

/// One instantiation asked for by two modules is emitted once, not twice under
/// names LLVM would silently disambiguate.
#[test]
fn one_instantiation_shared_by_two_modules_is_emitted_once() {
    let ran = run(
        "cross_shared",
        &[
            LIB,
            (
                "one",
                "module demo::one;
import demo::lib::{Option};
pub fn take(o: Option<Int>) -> Int { o.unwrap_or(1) }
",
            ),
            (
                "two",
                "module demo::two;
import demo::lib::{Option};
pub fn take(o: Option<Int>) -> Int { o.unwrap_or(2) }
",
            ),
            (
                "main",
                "module demo::main;
import demo::lib::{Option};
import demo::one::{take as first};
import demo::two::{take as second};
fn print(value: Int);
fn main() -> Int {
  print(first(Option::None));
  print(second(Option::Some(7)));
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "1\n7\n");
    assert_eq!(ran.code, Some(0));
}

/// A program built against the real `std/core.kh`, not a fixture.
///
/// This is what "the standard library works" means: `for` over std's `Range`
/// through std's `Iterator` impl, std's generic methods instantiated here, a
/// closure handed to std's `fold`, and trait dispatch on a std impl — all
/// linked into one binary.
#[test]
fn a_program_runs_against_the_real_standard_library() {
    let core = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std")
            .join("core.kh"),
    )
    .expect("std/core.kh");

    let ran = run(
        "against_std",
        &[
            ("core", core.as_str()),
            (
                "main",
                "module demo::main;
import std::core::{Option, List, Range, Ordering, Iterator, Ord, Step};

fn print(value: Int);

fn main() -> Int {
  let mut total = 0;
  for n in Range::Of(1, 6) {
    total = total + n;
  }
  print(total);

  print(Option::Some(41).unwrap_or(0) + 1);
  print(Option::None.unwrap_or(99));

  let xs = List::Cons(10, List::Cons(20, List::Cons(12, List::Nil)));
  print(xs.fold(0, fn (acc, x) => acc + x));
  print(xs.length());

  print(if 3.cmp(5).is_less() { 1 } else { 0 });
  0
}
",
            ),
        ],
    );
    assert_eq!(ran.stdout, "15\n42\n99\n42\n3\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// The payoff for bytes: a real `Map` from `std::core`, keyed by `String`.
///
/// Everything here is written in Khora. `Hash` is a trait in the standard
/// library, `impl Hash for String` folds FNV-1a over the bytes, and
/// `Map<K, V>` is generic over any key that has one — which is only possible
/// because a bound on an *impl block* is now read rather than parsed and
/// discarded.
#[test]
fn the_standard_maps_keys_can_be_strings() {
    let core = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std")
            .join("core.kh"),
    )
    .expect("std/core.kh");

    let ran = run(
        "std_map_strings",
        &[
            ("core", core.as_str()),
            (
                "main",
                "module demo::main;
import std::core::{Option, Map, Hash};

fn print(value: Int);
extern fn khora_live_count() -> Int;

fn main() -> Int {
  let ages = Map::new();
  Map::insert(ages, \"ada\", 36);
  Map::insert(ages, \"grace\", 45);
  Map::insert(ages, \"alan\", 41);
  print(Map::len(ages));
  print(Map::get(ages, \"grace\").unwrap_or(0));
  print(Map::get(ages, \"nobody\").unwrap_or(0));

  // A key built at run time rather than written as a literal, so the lookup
  // cannot be matching on the pointer.
  let built = \"gr\" + \"ace\";
  print(Map::get(ages, built).unwrap_or(0));

  Map::insert(ages, \"ada\", 37);
  print(Map::len(ages));
  print(Map::get(ages, \"ada\").unwrap_or(0));

  Map::remove(ages, \"alan\");
  print(Map::len(ages));
  print(if Map::holds(ages, \"alan\") { 1 } else { 0 });
  0
}
",
            ),
        ],
    );

    assert_eq!(
        ran.stdout, "3\n45\n0\n45\n3\n37\n2\n0\n",
        "the fourth line is the built key finding the same entry as the literal"
    );
    assert_eq!(ran.code, Some(0));
}

/// And the old keys still work, because `Int` is a `Hash` too.
#[test]
fn the_standard_map_still_takes_int_keys() {
    let core = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std")
            .join("core.kh"),
    )
    .expect("std/core.kh");

    let ran = run(
        "std_map_ints",
        &[
            ("core", core.as_str()),
            (
                "main",
                "module demo::main;
import std::core::{Option, Map, Hash};

fn print(value: Int);

fn main() -> Int {
  let squares = Map::new();
  let mut i = 0;
  while i < 40 {
    Map::insert(squares, i, i * i);
    i = i + 1;
  }
  print(Map::len(squares));
  print(Map::get(squares, 7).unwrap_or(0));
  print(Map::get(squares, 39).unwrap_or(0));
  print(Map::get(squares, 40).unwrap_or(0 - 1));
  0
}
",
            ),
        ],
    );

    assert_eq!(ran.stdout, "40\n49\n1521\n-1\n");
    assert_eq!(ran.code, Some(0));
}

/// **Two modules exporting a type of the same name must not share a layout.**
///
/// Drop glue was cached by the type's *printed* name, and `Display` leaves the
/// module out on purpose — `Request` reads better than `std.net.http.Request`
/// in an error message. So `alpha::Holder` and `beta::Holder` got one drop
/// routine: whichever was emitted first. One record's fields were then
/// released through the other's field list.
///
/// The layouts here differ in the way that makes that fatal: one holds a
/// boxed value where the other holds a number, so releasing the wrong one
/// treats an integer as a pointer. `khora_live_count` is the assertion because
/// the *quiet* version of this bug is a leak; the loud version is the crash
/// that found it, several frames from anything that looks related.
#[test]
fn two_modules_may_export_a_type_of_the_same_name() {
    let alpha = "module alpha;
impl String { fn byte_length(self) -> Int; }
pub type Holder = { text: String };
pub fn make_alpha(t: String) -> Holder { { text: t } }
pub fn width(h: Holder) -> Int { String::byte_length(h.text) }
";
    let beta = "module beta;
pub type Holder = { count: Int };
pub fn make_beta(n: Int) -> Holder { { count: n } }
pub fn value(h: Holder) -> Int { h.count }
";
    let main = "module main;
import alpha::{Holder as Boxed, make_alpha, width};
import beta::{Holder as Counted, make_beta, value};
fn print(value: Int);
extern fn khora_live_count() -> Int;

fn use_both() -> Int {
  let a = make_alpha(\"twelve chars\");
  let b = make_beta(7);
  width(a) + value(b)
}

fn main() -> Int {
  print(use_both());
  print(khora_live_count());
  0
}
";
    let ran = run(
        "same_name_types",
        &[("alpha.kh", alpha), ("beta.kh", beta), ("main.kh", main)],
    );
    assert_eq!(ran.stdout, "19\n0\n", "12 + 7, and nothing left alive: {}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}
