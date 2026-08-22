#![cfg(feature = "llvm")]

//! Phase 6's exit criterion: a hash map, written in Khora, that does not leak.
//!
//! The map itself lives in `std::core` and nothing in it is a compiler feature
//! — an array for the buckets, a `mut` field for the count, a recursive ADT for
//! each chain, and ordinary recursion over them. What these tests check is that
//! the pieces phase 6 added really do compose into a data structure, and that a
//! round trip of inserts and removals leaves nothing behind.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// `std::core`'s map and everything under it, copied here so the test is one
/// module. Kept in step with `std/core.kh` by
/// `the_standard_library_declares_what_it_promises`.
const MAP: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

export type Option<A> = | Some(value: A) | None;

export type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
  fn get(self, index: Int) -> A;
  fn set(self, index: Int, value: A) -> ();
}

impl Int {
  fn wrapping_mul(self, other: Int) -> Int;
  fn xor(self, other: Int) -> Int;
  fn and(self, other: Int) -> Int;
  fn shr(self, other: Int) -> Int;
}

export type Chain<V> = | Empty | Node(key: Int, value: V, rest: Chain<V>);

impl<V> Chain<V> {
  fn find(self, key: Int) -> Option<V> {
    match self {
      Chain::Empty => Option::None,
      Chain::Node(k, v, rest) => if k == key { Option::Some(v) } else { Chain::find(rest, key) },
    }
  }
  fn without(self, key: Int) -> Chain<V> {
    match self {
      Chain::Empty => Chain::Empty,
      Chain::Node(k, v, rest) =>
        if k == key { rest } else { Chain::Node(k, v, Chain::without(rest, key)) },
    }
  }
  fn holds(self, key: Int) -> Bool {
    match Chain::find(self, key) { Option::Some(v) => true, Option::None => false }
  }
}

export type Map<V> = { mut buckets: Array<Chain<V>>, mut count: Int };

impl<V> Map<V> {
  fn new() -> Map<V> { { buckets: Array::new(8, Chain::Empty), count: 0 } }
  fn len(self) -> Int { self.count }
  fn get(self, key: Int) -> Option<V> {
    Chain::find(Array::get(self.buckets, Map::slot(self, key)), key)
  }
  fn holds(self, key: Int) -> Bool {
    Chain::holds(Array::get(self.buckets, Map::slot(self, key)), key)
  }
  fn insert(self, key: Int, value: V) -> () {
    if self.count * 4 >= Array::length(self.buckets) * 3 { Map::grow(self); }
    let at = Map::slot(self, key);
    let bucket = Array::get(self.buckets, at);
    if Chain::holds(bucket, key) {
      Array::set(self.buckets, at, Chain::Node(key, value, Chain::without(bucket, key)));
    } else {
      Array::set(self.buckets, at, Chain::Node(key, value, bucket));
      self.count = self.count + 1;
    }
  }
  fn remove(self, key: Int) -> () {
    let at = Map::slot(self, key);
    let bucket = Array::get(self.buckets, at);
    if Chain::holds(bucket, key) {
      Array::set(self.buckets, at, Chain::without(bucket, key));
      self.count = self.count - 1;
    }
  }
  fn slot(self, key: Int) -> Int {
    let mixed = Int::wrapping_mul(key, 2654435761);
    let spread = Int::xor(mixed, Int::shr(mixed, 16));
    Int::and(spread, Array::length(self.buckets) - 1)
  }
  fn grow(self) -> () {
    let old = self.buckets;
    self.buckets = Array::new(Array::length(old) * 2, Chain::Empty);
    let mut i = 0;
    while i < Array::length(old) {
      Map::rehash(self, Array::get(old, i));
      i = i + 1;
    }
  }
  fn rehash(self, chain: Chain<V>) -> () {
    match chain {
      Chain::Empty => (),
      Chain::Node(k, v, rest) => {
        let at = Map::slot(self, k);
        Array::set(self.buckets, at, Chain::Node(k, v, Array::get(self.buckets, at)));
        Map::rehash(self, rest);
      },
    };
  }
}

fn found(o: Option<Int>) -> Int {
  match o { Option::Some(v) => v, Option::None => 0 - 1 }
}
";

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

/// Put things in, get them back out.
#[test]
fn a_map_returns_what_was_put_in_it() {
    let ran = run(
        "map_roundtrip",
        &format!(
            "{MAP}
fn work() -> Int {{
  let m = Map::new();
  Map::insert(m, 1, 10);
  Map::insert(m, 2, 20);
  Map::insert(m, 9, 90);
  print(Map::len(m));
  print(found(Map::get(m, 1)) + found(Map::get(m, 2)) + found(Map::get(m, 9)));
  found(Map::get(m, 7))
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(
        ran.stdout, "3\n120\n-1\n0\n",
        "three entries, summing to 120, and a miss for the key nobody put in"
    );
    assert_eq!(ran.code, Some(0));
}

/// Several keys, at least some of which will share a bucket — with eight
/// buckets and sixteen keys they must. Which ones is the hash's business, and
/// not something to assert.
#[test]
fn keys_that_collide_are_both_kept() {
    let ran = run(
        "map_collide",
        &format!(
            "{MAP}
fn work() -> Int {{
  let m = Map::new();
  Map::insert(m, 1, 100);
  Map::insert(m, 9, 900);
  Map::insert(m, 17, 1700);
  print(Map::len(m));
  found(Map::get(m, 1)) + found(Map::get(m, 9)) + found(Map::get(m, 17))
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n2700\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// Inserting a key that is already there replaces it, and the count does not
/// move — which is what makes it the number of entries.
#[test]
fn inserting_a_key_twice_replaces_it() {
    let ran = run(
        "map_replace",
        &format!(
            "{MAP}
fn work() -> Int {{
  let m = Map::new();
  Map::insert(m, 3, 30);
  Map::insert(m, 3, 33);
  Map::insert(m, 3, 333);
  print(Map::len(m));
  found(Map::get(m, 3))
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n333\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// Enough entries to force a rehash, and everything still findable afterwards.
#[test]
fn a_map_grows_and_keeps_everything() {
    let ran = run(
        "map_grow",
        &format!(
            "{MAP}
fn work() -> Int {{
  let m = Map::new();
  let mut i = 0;
  while i < 40 {{
    Map::insert(m, i, i * 3);
    i = i + 1;
  }}
  print(Map::len(m));

  let mut total = 0;
  let mut j = 0;
  while j < 40 {{
    total = total + found(Map::get(m, j));
    j = j + 1;
  }}
  total
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(
        ran.stdout, "40\n2340\n0\n",
        "forty entries survived the rehash, summing to 3 * (0 + .. + 39)"
    );
    assert_eq!(ran.code, Some(0));
}

/// **The exit criterion.** A round trip of inserts and removals leaves the
/// live-object count at zero: every chain node, every bucket array, every
/// boxed value released exactly once.
#[test]
fn a_round_trip_of_inserts_and_removals_leaves_nothing() {
    let ran = run(
        "map_exit",
        &format!(
            "{MAP}
fn work() -> Int {{
  let m = Map::new();
  let mut i = 0;
  while i < 60 {{
    Map::insert(m, i, i);
    i = i + 1;
  }}
  let mut j = 0;
  while j < 60 {{
    Map::remove(m, j);
    j = j + 1;
  }}
  print(Map::len(m));
  found(Map::get(m, 30))
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(
        ran.stdout, "0\n-1\n0\n",
        "emptied, nothing findable, and nothing left allocated"
    );
    assert_eq!(ran.code, Some(0));
}

/// Boxed values, so the map is releasing something a leak would show.
#[test]
fn a_map_of_boxed_values_leaves_nothing() {
    let ran = run(
        "map_boxed",
        &format!(
            "{MAP}
fn work() -> Int {{
  let m = Map::new();
  let mut i = 0;
  while i < 30 {{
    Map::insert(m, i, \"held\");
    i = i + 1;
  }}
  Map::remove(m, 7);
  Map::insert(m, 7, \"again\");
  Map::len(m)
}}

fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "30\n0\n");
    assert_eq!(ran.code, Some(0));
}
