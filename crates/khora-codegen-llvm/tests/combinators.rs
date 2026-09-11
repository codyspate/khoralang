#![cfg(feature = "llvm")]

//! What the combinators cannot say from `tests/std-suite`.
//!
//! **Seven of these moved** to `tests/std-suite/src/combinators.kh`, where they
//! are `test` blocks sharing one compile. What is left needs a program of its
//! own for a reason: each declares an `effect` or a trait `impl` whose whole
//! point is that the *compiler* resolves it -- an iterator whose `next`
//! performs an effect, an adapter whose associated projections have to be
//! settled before code generation, a pipeline whose object count is the claim.
//! Those are statements about lowering rather than about `std`'s answers, and
//! lowering is what this crate tests.
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

/// The adapters are effect-polymorphic: a source whose `next` performs an
/// effect carries that row out through `map` and `fold`.
///
/// This is what `type Effects` on the trait buys. Without it `Iterator` would
/// be a pure-only interface and an effectful source could not implement it,
/// which is the trap Rust's `Iterator` is in.
#[test]
fn combinators_carry_the_sources_effect_row() {
    let out = run(
        "combinators_effects",
        r#"module main;
import std::core::{Iterator, Step, print};

pub effect Tick { now: () -> Int }

pub type Ticks = { left: Int };

impl Iterator for Ticks {
  type Item = Int;
  type Effects = { tick: Tick };
  fn next(self) -> Step<Ticks, Int> with { tick: Tick } {
    if self.left <= 0 { Step::Done } else { Step::Yield({ left: self.left - 1 }, tick.now()) }
  }
}

const clock = handler for Tick { now: fn () => 3 };

pub fn main() -> () {
  let src: Ticks = { left: 3 };
  with { tick: clock } {
    print(Int::to_string(Iterator::fold(Iterator::map(src, fn n => n * 2), 0, fn (acc, n) => acc + n)))
  }
}
"#,
    );
    // Three ticks of 3, doubled, summed.
    assert_eq!(out.trim(), "18");
}

/// A pipeline holds a bounded number of objects however long the source is.
///
/// Sampled *during* the fold, on the last element, so it sees what is live
/// mid-walk rather than after cleanup. A stage that materialised its output
/// -- the way a `map` that builds a list does -- would grow with `n`; these
/// hand each element straight to the next stage.
///
/// Note what this does *not* say: the count being flat rules out
/// accumulation, not per-element churn. Each `next` still allocates its
/// `Step` and its successor record, and `docs/design/reuse.md` has the
/// measurement and what removing them needs.
#[test]
fn a_pipeline_materialises_nothing() {
    let out = run(
        "combinators_live",
        r#"module main;
import std::core::{Iterator, Range, print};

extern fn khora_live_count() -> Int;

fn live_during(n: Int) -> Int {
  Iterator::fold(
    Iterator::map(Iterator::filter(Range::Of(0, n), fn i => i % 2 == 0), fn i => i * 2),
    0,
    fn (acc, i) => if i >= (n - 2) * 2 { khora_live_count() } else { acc },
  )
}

pub fn main() -> () {
  print(Int::to_string(live_during(100)));
  print(Int::to_string(live_during(1000)));
  print(Int::to_string(live_during(10000)));
}
"#,
    );
    let counts: Vec<&str> = out.trim().lines().collect();
    assert_eq!(counts.len(), 3, "three samples: {out}");
    assert_eq!(
        counts[0], counts[1],
        "live objects grew between n=100 and n=1000: {out}"
    );
    assert_eq!(
        counts[1], counts[2],
        "live objects grew between n=1000 and n=10000: {out}"
    );
}
