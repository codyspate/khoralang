//! The advice a refused cross-fiber value gets, when its type has no body.
//!
//! **What these guard: advice nobody can take.** A type declared without a
//! body is refused across fibers until its author writes `impl Share` for it,
//! and the refusal said so whoever was reading. For an `Array<U8>` captured by
//! `Fiber::spawn` it told a program to write `impl Share for Array`, which the
//! orphan rule refuses anywhere but `std::core`: the one fix offered was one
//! the reader could not make. The advice for a type the reader declared stays.

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
/// `Array` is declared.
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

/// The agent-round case: a byte buffer captured by a spawned fiber.
#[test]
fn a_std_array_is_not_told_to_implement_share() {
    let found = errors_with_std(
        "module main;
import std::core::{Array, Fiber, U8};
pub fn main() -> () {
  let buffer: Array<U8> = Array::new(8, 0);
  let child = Fiber::spawn(fn () => Array::length(buffer));
  let _ = Fiber::join(child);
}",
    );
    let refusal = found
        .iter()
        .find(|m| m.contains("cannot be handed to another fiber"))
        .unwrap_or_else(|| panic!("the capture should be refused: {found:#?}"));
    assert!(!refusal.contains("impl Share for Array"), "advice nobody can take: {refusal}");
    assert!(refusal.contains("`std::core`"), "it says who may vouch for it: {refusal}");
    assert!(refusal.contains("`Handoff`"), "it names a route that works: {refusal}");
    assert!(refusal.contains("`List`"), "and a copy that can be shared: {refusal}");
    assert!(refusal.starts_with("`buffer` cannot"), "the binding is still named: {refusal}");
}

/// The route the advice names compiles: the array given away through a
/// `Handoff` rather than captured.
#[test]
fn the_route_the_advice_names_compiles() {
    let found = errors_with_std(
        "module main;
import std::core::{Array, Fiber, Handoff, Option, U8};
pub fn main() -> () {
  let pipe: Handoff<Array<U8>> = Handoff::bounded(1);
  let child = Fiber::spawn(fn () => match Handoff::receive(pipe) {
    Option::Some(bytes) => Array::length(bytes),
    Option::None => 0,
  });
  let buffer: Array<U8> = Array::new(8, 0);
  let _ = Handoff::send(pipe, buffer);
  let _ = Fiber::join(child);
}",
    );
    assert!(found.is_empty(), "{found:#?}");
}

/// **The half that has to keep working.** A type this file declares is one
/// its author can vouch for, and `impl Share` is the fix.
#[test]
fn a_type_declared_here_is_still_told_to_implement_share() {
    let found = errors_with_std(
        "module main;
import std::core::{Fiber};
pub type Buffer;
fn look(b: Buffer) -> () { }
pub fn f(b: Buffer) -> () {
  let child = Fiber::spawn(fn () => look(b));
  let _ = Fiber::join(child);
}",
    );
    assert!(
        found.iter().any(|m| m.contains("cannot be handed to another fiber")
            && m.contains("impl Share for Buffer")),
        "{found:#?}"
    );
}

/// **The route named for `Array` is one that exists.** The advice says a
/// `List` of the elements, and `Array::to_list` is how to get one: the copy
/// is what crosses, and the child reads it.
#[test]
fn the_array_copy_the_advice_names_compiles() {
    let found = errors_with_std(
        "module main;
import std::core::{Array, Fiber, List, U8};
pub fn main() -> () {
  let buffer: Array<U8> = Array::new(8, 0);
  let bytes: List<U8> = Array::to_list(buffer);
  let child = Fiber::spawn(fn () => List::length(bytes));
  let _ = Fiber::join(child);
}",
    );
    assert!(found.is_empty(), "{found:#?}");
    let refusal = errors_with_std(
        "module main;
import std::core::{Array, Fiber, U8};
pub fn main() -> () {
  let buffer: Array<U8> = Array::new(8, 0);
  let child = Fiber::spawn(fn () => Array::length(buffer));
  let _ = Fiber::join(child);
}",
    );
    assert!(
        refusal.iter().any(|m| m.contains("`Array::to_list(a)`")),
        "the advice names the call that makes the copy: {refusal:#?}"
    );
}

/// **`Opaque` is not told to use a `Handoff`**, because a `Handoff<Opaque>`
/// is itself refused: an `Opaque` is only ever held inside a `Raised`. Nor
/// is it told to keep the `Raised` and send that: a `Raised` stays on the
/// fiber that caught it, so that advice was refused when followed. What
/// crosses is the text, `failure.show()`.
#[test]
fn opaque_is_not_told_to_use_a_handoff() {
    let found = errors_with_std(
        "module main;
import std::core::{Fiber, Opaque};
pub fn o(x: Opaque) -> Int {
  let child = Fiber::spawn(fn () => { let _k = x; 1 });
  Fiber::join(child)
}",
    );
    let refusal = found
        .iter()
        .find(|m| m.contains("cannot be handed to another fiber"))
        .unwrap_or_else(|| panic!("the capture should be refused: {found:#?}"));
    assert!(!refusal.contains("Handoff"), "a route that does not compile: {refusal}");
    assert!(!refusal.contains("keep the `Raised`"), "a route that does not compile: {refusal}");
    assert!(refusal.contains("`failure.show()`"), "it names the route that does: {refusal}");
}
