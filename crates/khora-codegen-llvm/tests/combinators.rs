#![cfg(feature = "llvm")]

//! The collection combinators, and the failure system, composing.
//!
//! A function type in Khora carries its own capability and failure rows, and
//! `guide/collections-and-strings.md` has always said so. The combinators that
//! *take* a function declared theirs without, so a fallible step was refused:
//!
//! ```text
//! ids |> List::map(fn id => load_user(id)!)
//! error: this argument: `UserError` is not accounted for here
//! ```
//!
//! `fold` and `filter` said the same, which left a hand-written `while` over
//! `Nil`/`Cons` as the only way to walk a list with a step that can fail. For a
//! language whose headline is typed failure that is the collection library and
//! the failure system declining to compose, and it is the largest gap an agent
//! writing a real program hit. Roadmap #136.
//!
//! Compiled against `std` itself rather than a copy, because the claim is about
//! the signatures `std` actually ships.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test.
fn sources(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
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

fn run(name: &str, main: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    assert_eq!(output.status.code(), Some(0), "{name} exited badly");
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

const HEAD: &str = "module main;
import std::core::{Functor, List, Option, Result, attempt, print};

pub type UserError = | NotFound(id: Int);

fn load_user(id: Int) -> Int raises UserError {
  if id < 0 { raise UserError::NotFound(id) } else { id * 2 }
}

fn show(xs: List<Int>) -> String { List::fold(xs, \"\", fn (acc, n) => acc + \" ${n}\") }
";

/// The Guide's own example, which used to be refused.
///
/// Two marks, and they do different jobs: the inner one is `load_user`
/// failing, the outer one is `List::map` passing that on -- which it can only
/// do because its signature now says `raises 'er`.
#[test]
fn a_fallible_step_maps_over_a_list() {
    let out = run(
        "combinators_map",
        &format!(
            "{HEAD}
fn load_all(ids: List<Int>) -> List<Int> raises UserError {{
  ids |> List::map(fn id => load_user(id)!)!
}}

fn main() -> Int {{
  match attempt(fn () => load_all([1, 2, 3])!) {{
    Result::Ok(xs) => print(\"ok${{show(xs)}}\"),
    Result::Err(_) => print(\"refused\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(out, "ok 2 4 6\n");
}

/// And stops at the first failure, which is the thing `attempt` inside the
/// closure could never do.
///
/// That workaround answers a `List<Result<..>>`: every element is visited and
/// the failures come back as data. Useful, and a different operation -- the
/// Guide documents both, and only one of them existed.
#[test]
fn a_failing_step_stops_the_walk() {
    let out = run(
        "combinators_stop",
        &format!(
            "{HEAD}
fn load_all(ids: List<Int>) -> List<Int> raises UserError {{
  ids |> List::map(fn id => load_user(id)!)!
}}

fn main() -> Int {{
  match attempt(fn () => load_all([1, 0 - 5, 3])!) {{
    Result::Ok(_) => print(\"it should have refused\"),
    Result::Err(UserError::NotFound(id)) => print(\"stopped at ${{id}}\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(out, "stopped at -5\n");
}

/// The rest of the walks, which refused the same way and are fixed the same
/// way. `fold` and `filter` are the two the report named beside `map`.
#[test]
fn every_walk_takes_a_fallible_step() {
    let out = run(
        "combinators_all",
        &format!(
            "{HEAD}
fn everything() -> String raises UserError {{
  let kept = show(List::filter([1, 2, 3, 4], fn n => load_user(n)! > 4)!);
  let total = List::fold([1, 2, 3], 0, fn (a, n) => a + load_user(n)!)!;
  let any = List::any([1, 2, 3], fn n => load_user(n)! == 4)!;
  let all = List::all([1, 2, 3], fn n => load_user(n)! > 0)!;
  let flat = show(List::flat_map([1, 2], fn n => [load_user(n)!])!);
  \"kept${{kept}} total ${{total}} any ${{any}} all ${{all}} flat${{flat}}\"
}}

fn main() -> Int {{
  match attempt(fn () => everything()!) {{
    Result::Ok(text) => print(text),
    Result::Err(_) => print(\"refused\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(out, "kept 3 4 total 12 any true all true flat 2 4\n");
}

/// `Option` and `Result` too, because a surface where `List::map` takes a
/// fallible step and `Option::and_then` does not is worse than one where
/// neither did: the reader has to remember which is which.
#[test]
fn option_and_result_take_one_too() {
    let out = run(
        "combinators_option",
        &format!(
            "{HEAD}
fn chained() -> String raises UserError {{
  let mapped = Option::and_then(Option::Some(3), fn n => Option::Some(load_user(n)!))!;
  let kept = Option::filter(Option::Some(3), fn n => load_user(n)! > 4)!;
  let fell = Option::unwrap_or_else(Option::None, fn () => load_user(7)!)!;
  let ok: Result<Int, UserError> = Result::Ok(5);
  let doubled = Result::map(ok, fn n => load_user(n)!)!;
  let m = Option::unwrap_or(mapped, 0 - 1);
  let k = Option::unwrap_or(kept, 0 - 1);
  let d = Result::unwrap_or(doubled, 0 - 1);
  \"mapped ${{m}} kept ${{k}} fell ${{fell}} doubled ${{d}}\"
}}

fn main() -> Int {{
  match attempt(fn () => chained()!) {{
    Result::Ok(text) => print(text),
    Result::Err(_) => print(\"refused\"),
  }};
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "mapped 6 kept 3 fell 14 doubled 10\n",
        "each combinator carried the row and answered"
    );
}

/// **And a pure step still needs no mark at all.**
///
/// The rows are variables, so nothing that maps an ordinary function has to
/// change. This is the half that made the change safe to make: the whole
/// corpus -- `std`, the examples, the packages, the reference applications --
/// compiled unaltered.
#[test]
fn a_pure_step_is_unchanged() {
    let out = run(
        "combinators_pure",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(\"mapped${{show(List::map([1, 2], fn n => n + 10))}}\");
  print(\"kept${{show(List::filter([1, 2, 3], fn n => n > 1))}}\");
  print(\"total ${{List::fold([1, 2, 3], 0, fn (a, n) => a + n)}}\");
  0
}}
"
        ),
    );
    assert_eq!(out, "mapped 11 12\nkept 2 3\ntotal 6\n");
}

/// **One `for` loop, over an iterator and over a stream.**
///
/// `Iterator::next` requires `Self::Effects`, so an implementation decides
/// whether pulling costs anything. `Range` says `{}` and a `for` over it asks
/// its enclosing function for nothing; a source whose `next` performs an effect
/// says so, and the same `for` requires it of whoever wrote it.
///
/// Rust needs `Iterator`, `Stream` and `AsyncIterator` to say this, and Effect
/// needs `Iterable` and `Stream`, because in both an effect changes a
/// function's *type*. Here it is a row, and a row can be empty -- so one trait,
/// and one set of combinators written once.
#[test]
fn a_for_loop_walks_a_pure_iterator_and_an_effectful_one() {
    let out = run(
        "iterator_effects",
        "module main;
import std::core::{Iterator, Range, Step, print};

pub effect Tick { now: () -> Int }

pub type Ticks = { left: Int };

impl Iterator for Ticks {
  type Item = Int;
  type Effects = { tick: Tick };
  fn next(self) -> Step<Ticks, Int> with { tick: Tick } {
    if self.left <= 0 { Step::Done } else { Step::Yield({ left: self.left - 1 }, tick.now()) }
  }
}

const clock = handler for Tick { now: fn () => 7 };

fn pure_sum() -> Int {
  let mut total = 0;
  for n in Range::Of(0, 5) { total = total + n; }
  total
}

fn tick_sum() -> Int with { tick: Tick } {
  let mut total = 0;
  let src: Ticks = { left: 3 };
  for n in src { total = total + n; }
  total
}

pub fn main() -> () {
  print(Int::to_string(pure_sum()));
  with { tick: clock } { print(Int::to_string(tick_sum())) }
}
",
    );
    // 0+1+2+3+4, then 7 three times.
    assert_eq!(out, "10\n21\n");
}

/// **A generic impl's associated projections are resolved before code
/// generation**, which is what an adapter over an iterator is made of.
///
/// `impl<I: Walk, B> Walk for Mapped<I, B>` writes its `step` in terms of
/// `I::Item`. Substituting `I := Counted` turns that into `Counted::Item` and
/// stops, because `substitute` holds a mapping and resolving a projection needs
/// the impl that binds `Item`. The checker normalizes through
/// `Unifier::with_assoc`; monomorphization did not, so the backend met
/// `Counted::Item` and reported a type it "cannot represent yet".
///
/// `peek` covers the same gap in a *signature*: a default method whose return
/// type is the projection was emitted with it intact.
#[test]
fn a_generic_adapter_resolves_its_projections() {
    let out = run("adapter_projection", "module main;\nimport std::core::{Step, print};\n\npub trait Walk {\n  type Item;\n  type Effects;\n  fn step(self) -> Step<Self, Self::Item> with Self::Effects;\n  /// A default method whose *signature* mentions the projection.\n  fn peek(self) -> Step<Self, Self::Item> with Self::Effects { Walk::step(self) }\n}\n\npub type Mapped<I, B> = { inner: I, f: (I::Item) -> B };\n\nimpl<I: Walk, B> Walk for Mapped<I, B> {\n  type Item = B;\n  type Effects = I::Effects;\n  fn step(self) -> Step<Mapped<I, B>, B> with Self::Effects {\n    match Walk::step(self.inner) {\n      Step::Done => Step::Done,\n      Step::Yield(rest, item) => Step::Yield({ inner: rest, f: self.f }, (self.f)(item)),\n    }\n  }\n}\n\npub type Counted = { at: Int, to: Int };\nimpl Walk for Counted {\n  type Item = Int;\n  type Effects = {};\n  fn step(self) -> Step<Counted, Int> {\n    if self.at >= self.to { Step::Done } else { Step::Yield({ at: self.at + 1, to: self.to }, self.at) }\n  }\n}\n\npub fn main() -> () {\n  let src: Counted = { at: 0, to: 5 };\n  let doubled: Mapped<Counted, Int> = { inner: src, f: fn n => n * 2 };\n  let mut total = 0;\n  let mut cur = doubled;\n  loop {\n    match Walk::step(cur) {\n      Step::Done => break,\n      Step::Yield(next, item) => { total = total + item; cur = next; },\n    }\n  }\n  print(Int::to_string(total));\n  match Walk::peek(src) {\n    Step::Yield(_r, item) => print(Int::to_string(item)),\n    Step::Done => print(Int::to_string(0)),\n  }\n}");
    // 2*(0+1+2+3+4), then the first item of the source.
    assert_eq!(out, "20\n0\n");
}
