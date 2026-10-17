//! A `Raised` stays on the fiber that caught it.
//!
//! **What these guard: an error and its note counted by two fibers.** A
//! `Raised` is a box whose error and note its own fiber counts, unmarked.
//! `Handoff<A>` has no `Share` bound -- its send walks the value and marks
//! what it finds -- and a `Raised` has no body to walk, so a hand-off of one
//! marked the box and passed the rest through: the debug owner check trapped
//! with status 134, and a release build let both fibers write one `mut`
//! record that the same hand-off refuses when it is sent without the
//! `Raised`. So every route is refused at compile time, with a message that
//! names the `Raised`, the field holding it, and what to send instead.
//!
//! The programs are the second review's probes `e2`, `e4`, `e6` and `e9`,
//! plus a generic send.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the source directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic from one program compiled together with `std`, where
/// `Raised`, `Handoff` and `Channel` are declared.
fn errors_with_std(program: &str) -> Vec<String> {
    let std_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut paths = Vec::new();
    sources(&std_dir, &mut paths);
    paths.sort();

    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("the sources should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    let mine = SourceFile::new(&db, PathBuf::from("program.kh"), program.to_string());
    files.push(mine);
    SourceRoot::new(&db, files);

    khora_types::diagnostics(&db, mine).iter().map(|e| e.message.clone()).collect()
}

/// Requires a refusal containing every one of `about`, which says the
/// `Raised` is why and what to send instead, and never suggests declaring
/// `Share` for it.
fn refused(program: &str, about: &[&str]) {
    let found = errors_with_std(program);
    assert!(
        found.iter().any(|m| about.iter().all(|a| m.contains(a))
            && m.contains("stays on the fiber that caught it")
            && m.contains("Raised::only")),
        "expected a refusal containing {about:?}, got {found:#?}\n{program}"
    );
    assert!(
        !found.iter().any(|m| m.contains("impl Share for Raised")),
        "a refusal suggested `impl Share for Raised`: {found:#?}"
    );
}

const HEAD: &str = "module main;
import std::core::{Channel, Fiber, Handoff, List, Option, Raised, Result, Show, String, attempt};

derive(Show)
pub type Pay = | Declined(why: String) | Timeout;

pub type Cell = { mut n: Int };
pub type Ctr = | C(cell: Cell);

fn fails() -> Int raises Pay { raise Pay::Declined(\"card\") }
fn raise_it(c: Cell) -> Int raises Ctr { raise Ctr::C(c) }

fn caught() -> Raised<Pay> {
  match attempt(fn () => fails()!) { Result::Ok(_) => caught(), Result::Err(e) => e }
}
";

fn program(rest: &str) -> String {
    format!("{HEAD}\n{rest}")
}

/// `e4`: a `Raised` sent through a `Handoff` of its own type.
#[test]
fn a_handoff_refuses_a_raised() {
    refused(
        &program(
            "pub fn main() -> () {
  let h: Handoff<Raised<Pay>> = Handoff::bounded(1);
  let _ = Handoff::send(h, caught());
}",
        ),
        &["`Raised<Pay>` cannot be handed to another fiber: a `Raised` stays"],
    );
}

/// `e9`: the error holds a `mut` record that the sender keeps writing.
///
/// Sent bare, the hand-off's own walk refuses it at run time; inside a
/// `Raised` nothing did, and both fibers wrote it.
#[test]
fn a_handoff_refuses_a_raised_over_a_writable_error() {
    refused(
        &program(
            "pub fn main() -> () {
  let cell: Cell = { n: 1 };
  let h: Handoff<Raised<Ctr>> = Handoff::bounded(1);
  match attempt(fn () => raise_it(cell)!) {
    Result::Ok(_) => (),
    Result::Err(e) => { let _ = Handoff::send(h, e); () },
  };
}",
        ),
        &["cannot be handed to another fiber: a `Raised` stays"],
    );
}

/// `e6`: a user type holding a `Raised`, and the `Raised` inside a list
/// and a `Result`. The refusal names the field the `Raised` sits in.
#[test]
fn a_handoff_refuses_a_raised_inside_its_value() {
    refused(
        &program(
            "pub type Wrapped = | Because(stage: String, cause: Raised<Pay>);
pub fn main() -> () {
  let h: Handoff<Wrapped> = Handoff::bounded(1);
  let _ = Handoff::send(h, Wrapped::Because(\"stage\", caught()));
}",
        ),
        &["a `Raised` (in `Wrapped`'s field `cause`) stays"],
    );
    for (ty, value) in [
        ("List<Raised<Pay>>", "List::Cons(caught(), List::Nil)"),
        ("Result<Int, Raised<Pay>>", "Result::Err(caught())"),
    ] {
        refused(
            &program(&format!(
                "pub fn main() -> () {{
  let h: Handoff<{ty}> = Handoff::bounded(1);
  let _ = Handoff::send(h, {value});
}}"
            )),
            &["cannot be handed to another fiber: a `Raised`"],
        );
    }
}

/// A generic `send<A>` instantiated at a `Raised`: the send sees only `A`,
/// so the call that chose it is refused.
#[test]
fn a_generic_send_refuses_a_raised_at_the_call() {
    refused(
        &program(
            "fn send<A>(h: Handoff<A>, a: A) -> Bool { Handoff::send(h, a) }
pub fn main() -> () {
  let h: Handoff<Option<Raised<Pay>>> = Handoff::bounded(1);
  let _ = send(h, Option::Some(caught()));
}",
        ),
        &["`send` gives its argument to `Handoff::send`", "a `Raised` stays"],
    );
}

/// A closure sent through a `Handoff` is checked by what it captured.
#[test]
fn a_handoff_refuses_a_closure_capturing_a_raised() {
    refused(
        &program(
            "pub fn main() -> () {
  let e = caught();
  let h: Handoff<() -> Int> = Handoff::bounded(1);
  let _ = Handoff::send(h, fn () => String::byte_length(e.show()));
}",
        ),
        &["this closure captures `e`", "a `Raised` stays"],
    );
}

/// `e2`: the routes that require `Share` were refused already; the message
/// says the `Raised` is why, which field holds it, and what to send.
#[test]
fn share_refusals_name_the_raised_and_the_field() {
    refused(
        &program(
            "pub fn main() -> () {
  let e = caught();
  let f = Fiber::spawn(fn () => String::byte_length(e.show()));
  let _ = Fiber::join(f);
}",
        ),
        &["`e` cannot be handed to another fiber: a `Raised` stays"],
    );
    refused(
        &program(
            "pub type Outcome = { order: Int, result: Result<Int, Raised<Pay>> };
pub fn main() -> () {
  let c: Channel<Outcome> = Channel::bounded(1);
  let _ = c;
}",
        ),
        &["`Outcome` does not implement `Share`", "a `Raised` (in `Outcome`'s field `result`) stays"],
    );
    refused(
        &program(
            "pub fn main() -> () {
  let c: Channel<Raised<Pay>> = Channel::bounded(1);
  let _ = Channel::claim(c, fn _ => ());
}",
        ),
        &["does not implement `Share`", "a `Raised` stays"],
    );
    // A child raising an error that holds a `Raised` (`e6`'s first part):
    // its joiner would hold the box. Refused for the `Raised`, not for a
    // `mut` field that `Wrapped` does not have.
    refused(
        &program(
            "pub type Wrapped = | Because(stage: String, cause: Raised<Pay>);
fn wrapped() -> Int raises Wrapped { raise Wrapped::Because(\"stage\", caught()) }
pub fn main() -> () {
  let f = Fiber::spawn(fn () => wrapped()!);
  let _ = { Fiber::join(f)! } catch { Wrapped::Because(_, _) => 0 };
}",
        ),
        &["`Wrapped`, which this fiber can raise", "a `Raised` (in `Wrapped`'s field `cause`) stays"],
    );
}

/// What the refusals advise compiles: the text, or the error by
/// `Raised::only` when its type is `Share`, sent in its place.
#[test]
fn sending_the_text_or_the_error_compiles() {
    let found = errors_with_std(&program(
        "pub fn main() -> () {
  let text: Handoff<String> = Handoff::bounded(1);
  let _ = Handoff::send(text, caught().show());
  let error: Handoff<Pay> = Handoff::bounded(1);
  let _ = Handoff::send(error, Raised::only(caught()));
  let e = caught();
  let shown = e.show();
  let f = Fiber::spawn(fn () => String::byte_length(shown));
  let _ = Fiber::join(f);
}",
    ));
    assert!(found.is_empty(), "{found:#?}");
}
