#![cfg(feature = "llvm")]

//! Tuples, end to end.
//!
//! **A tuple is an anonymous record.** One heap object under the same header as
//! every other aggregate, with its elements as positional fields, counted and
//! released the same way. That is the whole of the representation decision, and
//! it is why nothing downstream had to learn what a tuple is: the reference
//! counting plan, the drop glue, the reuse analysis and pattern binding all ask
//! `instantiated_variants`, which answers for a tuple out of its type.
//!
//! Boxed rather than passed in registers, which is a real cost and the
//! consistent choice. `docs/design/compatibility.md` says when memory is
//! allocated is not observable, so unboxing small ones later stays legal.
//!
//! The front end already had all of this — `TupleExpr`, `TuplePat`, `TupleType`
//! parsed, checked and were exhaustiveness-checked. Only the backend had no
//! layout, so `(1, 2)` type checked and then failed with a message naming an
//! internal phase number.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
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
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

/// Compiles `source` expecting it to be refused, and hands back the messages.
fn refused(name: &str, source: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("rejected.exe");
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => panic!("`{name}` should have been refused:\n\n{source}"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
}

/// No `std`, so no `Int::to_string` — that one is written in Khora rather than
/// being an intrinsic. Numbers go through the runtime's own printer instead.
const PRELUDE: &str = "module t;
fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

pub type Option<A> = | Some(value: A) | None;
pub type List<A> = | Nil | Cons(head: A, tail: List<A>);
pub type Wrapper<A> = | Of(inner: A);
";

/// A tuple is built, returned, taken apart, and holds its elements.
#[test]
fn a_tuple_is_built_and_matched() {
    let ran = run(
        "tuple_basic",
        &format!(
            "{PRELUDE}
fn swap(p: (Int, String)) -> (String, Int) {{
  match p {{ (n, s) => (s, n) }}
}}

fn main() -> Int {{
  match swap((1, \"one\")) {{
    (s, n) => {{ print(s); khora_print_int(n) }},
  }};
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "one\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// Nesting, refutable elements, and a tuple inside a generic container — the
/// three ways the layout could have been looked up from the wrong place.
///
/// The generic one is the reason `at_this_instantiation` exists: `Cons(head: A,
/// ..)` declares an `A`, and a pattern descending into that field needs to know
/// that `A` is `(Int, String)` here. Reading the declaration handed the backend
/// a rigid parameter, which is not a shape anything can be loaded at.
#[test]
fn tuples_nest_and_survive_generics() {
    let ran = run(
        "tuple_nested",
        &format!(
            "{PRELUDE}
fn describe(p: (Int, (String, Bool))) -> String {{
  match p {{
    (0, (s, true)) => \"zero \" + s,
    (n, (s, false)) => s + \" off\",
    (n, (s, true)) => s + \" on\",
  }}
}}

fn head_of(xs: List<(Int, String)>) -> String {{
  match xs {{
    List::Nil => \"none\",
    List::Cons((n, s), _) => s,
  }}
}}

fn work() -> () {{
  print(describe((0, (\"a\", true))));
  print(describe((7, (\"b\", false))));
  print(describe((9, (\"c\", true))));
  print(head_of(List::Cons((1, \"x\"), List::Nil)));
  print(head_of(List::Nil));
  match Option::Some((3, 4)) {{
    Option::Some((a, b)) => khora_print_int(a * b),
    Option::None => print(\"-\"),
  }}
}}

fn main() -> Int {{
  work();
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout,
        "zero a\nb off\nc on\nx\nnone\n12\n0\n",
        "the trailing 0 is the live-object count"
    );
    assert_eq!(ran.code, Some(0));
}

/// **A tuple owns its elements.** Its `drop_fields` is generated from the
/// elements the same way a record's is from its fields.
///
/// This was the first thing that went wrong: `drop_glue` matched on
/// `Type::Adt` and handed everything else a null callback, so a tuple was
/// freed and every boxed element it held was not. The live count found it.
#[test]
fn a_tuple_releases_what_it_holds() {
    let ran = run(
        "tuple_leaks",
        &format!(
            "{PRELUDE}
fn build(tag: String) -> (String, (String, String)) {{
  (tag, (tag + \"a\", tag + \"b\"))
}}

fn work() -> () {{
  let mut i = 0;
  while i < 20 {{
    match build(\"n\") {{ (a, (b, c)) => print(a + b + c) }};
    i = i + 1;
  }}
}}

fn main() -> Int {{
  work();
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout.lines().last(), Some("0"), "nothing may be left alive");
    assert_eq!(ran.code, Some(0));
}

/// `let (a, b) = pair` — destructuring where the pattern cannot fail.
///
/// The bindings are projections into the object, exactly as a `match` arm's
/// are, so each takes a copy and the container is released. Without the copies
/// the block would release fields nobody had a reference for.
#[test]
fn a_let_takes_an_irrefutable_pattern_apart() {
    let ran = run(
        "tuple_let",
        &format!(
            "{PRELUDE}
fn divmod(a: Int, b: Int) -> (Int, Int) {{ (a / b, a % b) }}

fn work() -> () {{
  let (q, r) = divmod(17, 5);
  khora_print_int(q);
  khora_print_int(r);
  let (name, (x, y)) = (\"origin\", (2, 3));
  print(name);
  khora_print_int(x + y);
  // A single-case constructor is irrefutable too, which is what a record is.
  let Wrapper::Of(held) = Wrapper::Of(\"kept\");
  print(held)
}}

fn main() -> Int {{
  work();
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n2\norigin\n5\nkept\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// A `let` has nowhere to send a value that does not match, so a pattern that
/// can fail still needs a `match` — and the error says which of the two it is
/// rather than refusing all destructuring.
#[test]
fn a_let_refuses_a_pattern_that_can_fail() {
    let found = refused(
        "tuple_let_refutable",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let Option::Some(n) = Option::Some(1);
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("can fail") && e.contains("`match`")),
        "the error should name the reason: {found:?}"
    );
}
