#![cfg(feature = "llvm")]

//! `Shared<A>` and `Dict<K, V>`, against the real `std`.
//!
//! The two go together. The sharing rules refuse a mutable value to a second
//! fiber on purpose, and `Shared` is what they refuse it *until* — but a cell
//! may only hold something shareable, and `Map` mutates its buckets in place,
//! so a shared table needs a map that is never written. `Dict` is that map.
//!
//! `docs/design/shared.md` decides the shape: a cell, not a lock over a
//! mutable record. Nothing unshareable goes in or comes out, which is what
//! makes the escape question — the one Rust answers with lifetimes — not arise.
//!
//! Compiled against `std` itself rather than a copy, because the point of
//! most of these is that the library composes.

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
    assert!(output.status.success(), "`{name}` exited with {:?}", output.status.code());
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

// --- the cell --------------------------------------------------------------

/// Read, replace, and read-modify-write, which is the whole surface.
#[test]
fn a_cell_holds_a_value_and_swaps_it() {
    let out = run(
        "shared_basics",
        "module main;
import std::core::{Shared, print};

pub fn main() -> () {
  let count = Shared::of(0);
  print(Int::to_string(Shared::get(count)));
  Shared::set(count, 7);
  print(Int::to_string(Shared::get(count)));
  // `update` gives back what it left in there, so a caller can see what it did
  // without a second read another fiber could get between.
  print(Int::to_string(Shared::update(count, fn n => n + 1)));
  print(Int::to_string(Shared::get(count)))
}
",
    );
    assert_eq!(out, "0\n7\n8\n8\n");
}

/// The motivating case, and the one the sharing rules refuse outright for a
/// `mut` record: a test double that counts its calls.
///
/// The handler captures a `Shared<Int>`, which is shareable, so it passes the
/// certification at the `handler for` literal that every handler goes through.
#[test]
fn a_handler_can_count_its_calls() {
    let out = run(
        "shared_double",
        "module main;
import std::core::{Shared, print};

effect Ledger { balance: (Int) -> Int, }

fn report(id: Int) -> Int with { ledger: Ledger } { ledger.balance(id) }

pub fn main() -> () {
  let calls = Shared::of(0);
  with { ledger: handler for Ledger {
    balance: fn id => { Shared::update(calls, fn n => n + 1); id * 10 },
  } } {
    print(Int::to_string(report(4)));
    print(Int::to_string(report(5)));
  };
  print(Int::to_string(Shared::get(calls)))
}
",
    );
    assert_eq!(out, "40\n50\n2\n", "the double recorded both calls");
}

/// No lost update. Each fiber reads, adds and writes as one step, so the
/// increments cannot interleave and lose one.
#[test]
fn several_fibers_update_one_cell() {
    let out = run(
        "shared_fibers",
        "module main;
import std::core::{Fiber, Fibers, Shared, print};

fn bump(cell: Shared<Int>) -> () { Shared::update(cell, fn n => n + 1); }

pub fn main() -> () {
  let count = Shared::of(0);
  let crew = Fibers::open();
  let mut i = 0;
  while i < 8 {
    Fibers::adopt(crew, Fiber::spawn(fn () => bump(count)));
    i = i + 1;
  };
  let _stopped = Fibers::wait(crew);
  print(Int::to_string(Shared::get(count)))
}
",
    );
    assert_eq!(out, "8\n", "every one of the eight increments landed");
}

/// A cell holding something boxed releases it, and releases the one it
/// replaced.
#[test]
fn a_cell_releases_what_it_held() {
    let out = run(
        "shared_leaks",
        "module main;
import std::core::{List, Shared, print};

extern fn khora_live_count() -> Int;

fn work() -> () {
  let names = Shared::of(List::Cons(\"a\", List::Nil));
  Shared::update(names, fn rest => List::Cons(\"b\", rest));
  Shared::set(names, List::Nil);
  print(Int::to_string(List::length(Shared::get(names))))
}

pub fn main() -> () {
  work();
  let live = khora_live_count();
  print(Int::to_string(live))
}
",
    );
    assert_eq!(out, "0\n0\n", "the trailing 0 is the live-object count");
}

// --- the map ---------------------------------------------------------------

/// Ordered, and a later insert for the same key replaces rather than adds.
#[test]
fn a_dict_keeps_its_keys_in_order() {
    let out = run(
        "dict_order",
        "module main;
import std::core::{Dict, List, Option, Pair, print};

fn show(entries: List<Pair<Int, String>>) -> String {
  match entries {
    List::Nil => \"\",
    List::Cons(first, rest) =>
      Int::to_string(first.key) + \":\" + first.value + \" \" + show(rest),
  }
}

pub fn main() -> () {
  let d = Dict::new()
    |> Dict::insert(3, \"c\")
    |> Dict::insert(1, \"a\")
    |> Dict::insert(2, \"b\")
    |> Dict::insert(1, \"A\");
  print(show(Dict::entries(d)));
  print(Int::to_string(Dict::size(d)));
  match Dict::get(d, 2) { Option::None => print(\"missing\"), Option::Some(v) => print(v) };
  match Dict::get(d, 9) { Option::None => print(\"missing\"), Option::Some(v) => print(v) }
}
",
    );
    assert_eq!(out, "1:A 2:b 3:c \n3\nb\nmissing\n");
}

/// Persistent: the map an insert or a removal was taken from is still there,
/// unchanged. That is the property a `Shared<Dict>` rests on — a reader holds
/// a whole map, not a view of one being edited underneath it.
#[test]
fn a_dict_is_not_changed_by_what_is_derived_from_it() {
    let out = run(
        "dict_persistent",
        "module main;
import std::core::{Dict, List, Pair, print};

fn keys(entries: List<Pair<Int, String>>) -> String {
  match entries {
    List::Nil => \"\",
    List::Cons(first, rest) => Int::to_string(first.key) + \" \" + keys(rest),
  }
}

pub fn main() -> () {
  let d = Dict::new() |> Dict::insert(1, \"a\") |> Dict::insert(2, \"b\");
  let more = Dict::insert(d, 3, \"c\");
  let fewer = Dict::remove(d, 1);
  print(keys(Dict::entries(d)));
  print(keys(Dict::entries(more)));
  print(keys(Dict::entries(fewer)))
}
",
    );
    assert_eq!(out, "1 2 \n1 2 3 \n2 \n", "the original outlived both");
}

/// Balanced, which is the whole reason it is a tree and not an association
/// list. Sorted insertion is the shape that degenerates an unbalanced one into
/// a chain: 500 of them must not be 500 deep.
#[test]
fn a_dict_stays_balanced_under_sorted_insertion() {
    let out = run(
        "dict_balance",
        "module main;
import std::core::{Dict, print};

fn ladder(into: Dict<Int, Int>, n: Int) -> Dict<Int, Int> {
  if n == 0 { into } else { ladder(Dict::insert(into, n, n), n - 1) }
}

fn depth(d: Dict<Int, Int>) -> Int {
  match d {
    Dict::Empty => 0,
    Dict::Node(n, k, v, l, r) => {
      let a = depth(l);
      let b = depth(r);
      1 + (if a > b { a } else { b })
    },
  }
}

pub fn main() -> () {
  let big = ladder(Dict::new(), 500);
  print(Int::to_string(Dict::size(big)));
  // Perfectly balanced would be 9. Weight-balanced with a slack of three
  // gives a little more; anything near 500 would mean no rotation happened.
  print(if depth(big) < 20 { \"balanced\" } else { \"a chain\" })
}
",
    );
    assert_eq!(out, "500\nbalanced\n");
}

/// Emptied one key at a time, with the rotations that go with it, and nothing
/// left behind.
#[test]
fn a_dict_can_be_emptied_without_leaking() {
    let out = run(
        "dict_leaks",
        "module main;
import std::core::{Dict, print};

extern fn khora_live_count() -> Int;

fn work() -> Int {
  let mut d = Dict::new();
  let mut i = 0;
  while i < 200 { d = Dict::insert(d, (i * 37) % 200, i); i = i + 1; };
  let kept = Dict::size(d);
  let mut j = 0;
  while j < 200 { d = Dict::remove(d, j); j = j + 1; };
  kept + Dict::size(d)
}

pub fn main() -> () {
  let total = work();
  let live = khora_live_count();
  print(Int::to_string(total));
  print(Int::to_string(live))
}
",
    );
    assert_eq!(out, "200\n0\n", "200 distinct keys in, none left, nothing leaked");
}

// --- the two together ------------------------------------------------------

/// What the pair is for: a table several fibers read and write.
///
/// A `Map` cannot go in a cell — it mutates its buckets in place, so it is not
/// `Share` — and that is the whole reason `Dict` exists. Each fiber does a
/// read-modify-write of the entire map, which is cheap because a persistent
/// tree shares everything the insert did not touch.
#[test]
fn fibers_share_a_dict_as_a_cache() {
    let out = run(
        "shared_dict",
        "module main;
import std::core::{Dict, Fiber, Fibers, Option, Shared, print};

fn record(cache: Shared<Dict<Int, Int>>, key: Int) -> () {
  Shared::update(cache, fn table => Dict::insert(table, key, key * key));
}

pub fn main() -> () {
  let cache = Shared::of(Dict::new());
  let crew = Fibers::open();
  let mut i = 0;
  while i < 16 {
    Fibers::adopt(crew, Fiber::spawn(fn () => record(cache, i)));
    i = i + 1;
  };
  let _stopped = Fibers::wait(crew);
  let table = Shared::get(cache);
  print(Int::to_string(Dict::size(table)));
  match Dict::get(table, 9) {
    Option::None => print(\"missing\"),
    Option::Some(v) => print(Int::to_string(v)),
  }
}
",
    );
    assert_eq!(out, "16\n81\n", "all sixteen writers landed and the table reads back");
}

/// Accumulating a string through `update`, which is the shape a log is.
///
/// This is where the closure-concatenation bug was found: `soFar + "begin;"`
/// inside the closure `update` takes reported `arithmetic: expected Int, found
/// String`, because the parameter reaches `+` as a solved inference variable
/// rather than as a literal `String` and the check ran before zonking. The
/// annotation did not help — it was dropped in lowering. Compiled and run here
/// rather than only type-checked, so the fix is proved against a program.
#[test]
fn a_cell_accumulates_a_string() {
    let out = run(
        "shared_string_log",
        "module main;
import std::core::{Shared, print};

fn note(log: Shared<String>, what: String) -> () {
  Shared::update(log, fn (soFar: String) => soFar + what + \";\");
}

pub fn main() -> () {
  let log = Shared::of(\"\");
  note(log, \"begin\");
  note(log, \"execute\");
  note(log, \"commit\");
  // And the same with no annotation at all, solved from the other operand.
  Shared::update(log, fn soFar => soFar + \"done\");
  print(Shared::get(log))
}
",
    );
    assert_eq!(out, "begin;execute;commit;done\n");
}

/// **A method call reaches an intrinsic, the same as the namespaced call.**
///
/// `Shared::get(cell)` worked and `cell.get()` did not: only the namespaced
/// spelling looked for a backend implementation, so the method spelling
/// resolved to a declaration with no body and failed at code generation --
/// telling the caller to give a `std` function a body they do not own.
///
/// The part that made it bad is that `khora check` passed. The checker
/// resolves both spellings to the same method, so the fast loop -- and the LSP
/// that shares it -- was green while the build was red, which is the one thing
/// a toolchain must not do.
///
/// Found by somebody meeting the language for the first time, on their fourth
/// program.
#[test]
fn a_method_call_reaches_the_same_intrinsic_the_path_call_does() {
    let out = run(
        "shared_method_syntax",
        "module main;
import std::core::{Array, Channel, Region, Shared, print};

// A row on `main` only so the channel stanza below can carry the `!` that
// `Channel::send` now wants. Nothing here raises.
pub type Stop = | Never;

pub fn main() -> () raises Stop {
  let cell = Shared::of(1);
  Shared::set(cell, 7);
  print(Int::to_string(Shared::get(cell)) + \" \" + Int::to_string(cell.get()));

  // The same for every other type whose methods the backend fills in.
  let text = \"khora\";
  print(Int::to_string(String::byte_length(text)) + \" \" + Int::to_string(text.byte_length()));
  print(String::slice(text, 0, 2) + \" \" + text.slice(0, 2));

  let room: Array<Int> = Array::new(3, 0);
  Array::set(room, 1, 9);
  print(Int::to_string(Array::length(room)) + \" \" + Int::to_string(room.length()));
  print(Int::to_string(Array::get(room, 1)) + \" \" + Int::to_string(room.get(1)));

  let line: Channel<Int> = Channel::bounded(2);
  Channel::send(line, 4)!;
  print(Int::to_string(Channel::depth(line)) + \" \" + Int::to_string(line.depth()));
  Channel::close(line);
}
",
    );

    assert_eq!(out, "7 7\n5 5\nkh kh\n3 3\n9 9\n1 1\n");
}
