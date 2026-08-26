#![cfg(feature = "llvm")]

//! `Vector<A>` and `Map::entries`, against the real `std`.
//!
//! The two arrived together because they answer the same complaint: `std` had
//! containers you could not accumulate into and a container you could not walk.
//! `Array<A>` is fixed at the length it was allocated with, `List<A>` grows
//! only at the front, and a `Map` could only be iterated by reaching into its
//! `buckets` — which is what `std/json.kh` had to do, with a comment saying so.
//!
//! The vector is built on `Array::empty`, which landed with it: `Array::new`
//! needs a value to fill with, so before it there was no way to write down an
//! empty `Array<A>` at all, and the first version of this type worked around
//! that by holding `Array<Option<A>>` — one heap object per element, which is
//! exactly the property a vector exists to not have. `std/core.kh` carries the
//! reasoning. What these tests check is that the consequences are the right
//! ones: that growth copies everything, that the allocation count stays flat
//! as the length grows, that a `pop` releases what it removed rather than
//! parking it in the array, and that a vector filled and emptied leaves
//! nothing behind.
//!
//! Compiled against `std` itself rather than a copy, because the point of most
//! of these is that the library composes.

mod harness;

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

// --- the array's missing constructor ---------------------------------------

/// `Array::empty`, on its own: a zero-length array of a number and of a boxed
/// element, both released without incident.
///
/// The boxed one is the case worth having a test for. Nothing is stored in it,
/// but its header still carries the element's stride and drop glue, and a
/// header that lied about either would be a wild free the first time an
/// element was written — or, for a length of zero, a release that walked past
/// the end of the allocation.
#[test]
fn an_array_can_be_empty_without_a_fill() {
    let out = run(
        "array_empty",
        "module main;
import std::core::{Array, print};

extern fn khora_live_count() -> Int;

fn sizes() -> Int {
  let numbers: Array<Int> = Array::empty();
  let texts: Array<String> = Array::empty();
  Array::length(numbers) + Array::length(texts)
}

pub fn main() -> () {
  let total = sizes();
  let live = khora_live_count();
  print(Int::to_string(total));
  print(Int::to_string(live))
}
",
    );
    assert_eq!(out, "0\n0\n");
}

// --- the vector ------------------------------------------------------------

/// The whole surface of a small one: push, index, replace, remove.
///
/// `get` and `set` disagree on purpose — one answers with an `Option` and the
/// other with a `Bool` — and both of those answers are checked here at an index
/// that is off the end, because that is the pair of decisions a reader is most
/// likely to trip over.
#[test]
fn a_vector_pushes_pops_and_indexes() {
    let out = run(
        "vector_basics",
        "module main;
import std::core::{Option, Vector, print};

fn at(v: Vector<String>, index: Int) -> String {
  match Vector::get(v, index) { Option::None => \"-\", Option::Some(s) => s }
}

pub fn main() -> () {
  let v: Vector<String> = Vector::new();
  print(if Vector::is_empty(v) { \"empty\" } else { \"not empty\" });
  Vector::push(v, \"a\");
  Vector::push(v, \"b\");
  Vector::push(v, \"c\");
  print(Int::to_string(Vector::length(v)));
  // The fourth is off the end, and reads as absent rather than as whatever the
  // cell beyond the length happens to still hold.
  print(at(v, 0) + at(v, 1) + at(v, 2) + at(v, 3));
  print(if Vector::set(v, 1, \"B\") { \"wrote\" } else { \"refused\" });
  print(if Vector::set(v, 3, \"d\") { \"wrote\" } else { \"refused\" });
  print(at(v, 1));
  match Vector::pop(v) { Option::None => print(\"-\"), Option::Some(s) => print(s) };
  match Vector::pop(v) { Option::None => print(\"-\"), Option::Some(s) => print(s) };
  match Vector::pop(v) { Option::None => print(\"-\"), Option::Some(s) => print(s) };
  // Popping an empty one is the loop condition, not a trap.
  match Vector::pop(v) { Option::None => print(\"-\"), Option::Some(s) => print(s) };
  print(Int::to_string(Vector::length(v)));
  // Draining it gave the array back, so this push has to allocate again — and
  // it comes back to the size the vector had reached, not to nothing.
  Vector::push(v, \"z\");
  print(at(v, 0) + Int::to_string(Vector::capacity(v)))
}
",
    );
    assert_eq!(out, "empty\n3\nabc-\nwrote\nrefused\nB\nc\nB\na\n-\n0\nz8\n");
}

/// Growth, which is the only thing about a vector that is not obvious from the
/// outside: three hundred pushes into a vector that started with no storage at
/// all, and every element still there afterwards.
///
/// The capacities are asserted rather than just the contents. Doubling is what
/// makes `push` amortised constant, and a growth policy that quietly became a
/// fixed increment would still pass a test that only looked at the elements —
/// it would just be quadratic.
#[test]
fn a_vector_grows_past_its_first_allocation() {
    let out = run(
        "vector_growth",
        "module main;
import std::core::{Option, Vector, print};

pub fn main() -> () {
  let v: Vector<Int> = Vector::new();
  print(Int::to_string(Vector::capacity(v)));
  let mut i = 0;
  while i < 300 {
    Vector::push(v, i * 2);
    i = i + 1;
  };
  print(Int::to_string(Vector::length(v)));
  print(Int::to_string(Vector::capacity(v)));
  // Every element survived seven reallocations, in the order it was pushed.
  let mut sum = 0;
  let mut wrong = 0;
  let mut j = 0;
  while j < 300 {
    match Vector::get(v, j) {
      Option::None => { wrong = wrong + 1; },
      Option::Some(value) => {
        sum = sum + value;
        if value != j * 2 { wrong = wrong + 1; } else { }
      },
    };
    j = j + 1;
  };
  print(Int::to_string(sum));
  print(Int::to_string(wrong));

  // `with_capacity` is a promise, not storage: nothing is allocated until
  // there is an element to fill an array with, so the capacity reads as zero
  // until the first push and as the whole hundred from then on.
  let w: Vector<Int> = Vector::with_capacity(100);
  print(Int::to_string(Vector::capacity(w)));
  let mut k = 0;
  while k < 100 {
    Vector::push(w, k);
    k = k + 1;
  };
  print(Int::to_string(Vector::capacity(w)));
  Vector::push(w, 100);
  print(Int::to_string(Vector::capacity(w)))
}
",
    );
    assert_eq!(
        out,
        "0\n300\n512\n89700\n0\n0\n100\n200\n",
        "grown 0 -> 512 by doubling, summing to 2*(0+..+299), nothing misplaced"
    );
}

/// The property the `Option<A>` cells cost and `Array::empty` bought back: a
/// vector of `Int` is *two* heap objects — itself and its array — whatever its
/// length, and two before anything has been pushed at all, because
/// `Array::empty` allocates a header with no cells under it.
///
/// This is pinned rather than described because it regressed silently once
/// already. Every element being a `Some` type checked, passed every other test
/// in this file, and made a hundred integers into a hundred and three live
/// objects.
///
/// The count is read while the vector is still alive, which is the only moment
/// it says anything, and read into a local before any string is built for
/// printing — a literal being concatenated is itself a live object.
#[test]
fn a_vector_of_numbers_allocates_a_constant_number_of_objects() {
    let out = run(
        "vector_allocations",
        "module main;
import std::core::{Option, Vector, print};

extern fn khora_live_count() -> Int;

fn live_with(n: Int) -> Int {
  let v: Vector<Int> = Vector::new();
  let mut i = 0;
  while i < n {
    Vector::push(v, i);
    i = i + 1;
  };
  khora_live_count()
}

/// Boxed elements, where the elements themselves are objects and the vector
/// must add no more than the same two.
fn live_with_strings(n: Int) -> Int {
  let v: Vector<String> = Vector::new();
  let mut i = 0;
  while i < n {
    Vector::push(v, Int::to_string(i));
    i = i + 1;
  };
  khora_live_count()
}

/// A popped element dies at the `pop`, not whenever the cell it vacated is
/// next written. Without the backfill the array would still be holding every
/// one of them.
///
/// Fifty left of a hundred is fifty-one strings, not fifty. The cells past the
/// length still hold the fill the last `grow` used — the element that was
/// being pushed at the time, here the sixty-fifth — and that one string stays
/// alive until the array grows again or goes. It is one object however long
/// the vector is, because every one of those cells holds the same one.
fn live_after_popping(n: Int, dropped: Int) -> Int {
  let v: Vector<String> = Vector::new();
  let mut i = 0;
  while i < n {
    Vector::push(v, Int::to_string(i));
    i = i + 1;
  };
  let mut j = 0;
  while j < dropped {
    match Vector::pop(v) { Option::None => (), Option::Some(item) => () };
    j = j + 1;
  };
  khora_live_count()
}

pub fn main() -> () {
  let none = live_with(0);
  let one = live_with(1);
  let hundred = live_with(100);
  let thousand = live_with(1000);
  let strings = live_with_strings(100);
  let half = live_after_popping(100, 50);
  let drained = live_after_popping(100, 100);
  print(Int::to_string(none));
  print(Int::to_string(one));
  print(Int::to_string(hundred));
  print(Int::to_string(thousand));
  print(Int::to_string(strings));
  print(Int::to_string(half));
  print(Int::to_string(drained));
  let live = khora_live_count();
  print(Int::to_string(live))
}
",
    );
    assert_eq!(
        out,
        "2\n2\n2\n2\n101\n52\n2\n0\n",
        "two objects — the vector and its array — at every length, including none; \
         a hundred strings are those two plus ninety-nine, because `Int::to_string(0)` \
         gives back the literal `\"0\"` and a literal is a static rather than an \
         allocation; popping fifty leaves those, the last grow's fill, and the two"
    );
}

/// Both conversions, and the two orders they have to agree on.
///
/// `to_list` walks the array backwards to build the list forwards, which is the
/// sort of thing that is off by one until a test says otherwise.
#[test]
fn a_vector_and_a_list_convert_both_ways() {
    let out = run(
        "vector_lists",
        "module main;
import std::core::{List, Vector, print};

fn show(items: List<Int>) -> String {
  match items {
    List::Nil => \"\",
    List::Cons(head, rest) => Int::to_string(head) + \" \" + show(rest),
  }
}

pub fn main() -> () {
  let v = Vector::from_list(List::Cons(1, List::Cons(2, List::Cons(3, List::Nil))));
  print(Int::to_string(Vector::length(v)));
  print(show(Vector::to_list(v)));
  Vector::push(v, 4);
  print(show(Vector::to_list(v)));
  // Clearing drops the storage — there is nothing to blank a cell with, so
  // keeping it would keep the elements alive — but remembers how big it was,
  // and the next push comes back to that size in one allocation.
  Vector::clear(v);
  print(Int::to_string(Vector::length(v)));
  print(Int::to_string(Vector::capacity(v)));
  print(show(Vector::to_list(v)) + \"|\");
  Vector::push(v, 9);
  print(Int::to_string(Vector::capacity(v)));
  let empty: Vector<Int> = Vector::from_list(List::Nil);
  print(Int::to_string(Vector::length(empty)))
}
",
    );
    assert_eq!(out, "3\n1 2 3 \n1 2 3 4 \n0\n0\n|\n8\n0\n");
}

// --- the map's entries -----------------------------------------------------

/// Every entry comes back, exactly once, with the value that was put under it.
///
/// The order is the table's and is not promised, so what is checked is what the
/// map actually claims: the count, and that each pair holds a key and the value
/// that key was inserted with. A duplicated or dropped entry moves the sums.
#[test]
fn a_map_reports_every_entry() {
    let out = run(
        "map_entries",
        "module main;
import std::core::{List, Map, Pair, print};

fn count(entries: List<Pair<Int, Int>>) -> Int {
  match entries { List::Nil => 0, List::Cons(first, rest) => 1 + count(rest) }
}

fn keys(entries: List<Pair<Int, Int>>) -> Int {
  match entries { List::Nil => 0, List::Cons(first, rest) => first.key + keys(rest) }
}

/// How many pairs hold something other than the square of their key.
fn wrong(entries: List<Pair<Int, Int>>) -> Int {
  match entries {
    List::Nil => 0,
    List::Cons(first, rest) =>
      (if first.value == first.key * first.key { 0 } else { 1 }) + wrong(rest),
  }
}

fn total(values: List<Int>) -> Int {
  match values { List::Nil => 0, List::Cons(head, rest) => head + total(rest) }
}

pub fn main() -> () {
  let m: Map<Int, Int> = Map::new();
  let mut i = 0;
  // Fifty entries is past two growths, so the entries are spread over buckets
  // that were rehashed rather than the eight `new` started with.
  while i < 50 {
    Map::insert(m, i, i * i);
    i = i + 1;
  };
  // A key inserted twice is one entry, not two.
  Map::insert(m, 7, 49);
  let entries = Map::entries(m);
  print(Int::to_string(Map::len(m)));
  print(Int::to_string(count(entries)));
  print(Int::to_string(keys(entries)));
  print(Int::to_string(wrong(entries)));
  print(Int::to_string(total(Map::keys(m))));
  print(Int::to_string(total(Map::values(m))));
  // A removed key is gone from the walk too.
  Map::remove(m, 0);
  Map::remove(m, 49);
  print(Int::to_string(count(Map::entries(m))));
  print(Int::to_string(keys(Map::entries(m))))
}
",
    );
    assert_eq!(
        out,
        "50\n50\n1225\n0\n1225\n40425\n48\n1176\n",
        "0..49 once each, every value the square of its key, and the sums move when two go"
    );
}

/// String keys, and an order made deterministic by sorting rather than assumed.
///
/// This is the shape `std/json.kh` wanted: an object's fields, walked without
/// reaching into `Map`'s buckets.
#[test]
fn a_maps_keys_and_values_line_up() {
    let out = run(
        "map_keys",
        "module main;
import std::core::{List, Map, print};

fn join(items: List<String>) -> String {
  match items {
    List::Nil => \"\",
    List::Cons(head, rest) => head + \" \" + join(rest),
  }
}

pub fn main() -> () {
  let m: Map<String, String> = Map::new();
  Map::insert(m, \"gamma\", \"three\");
  Map::insert(m, \"alpha\", \"one\");
  Map::insert(m, \"beta\", \"two\");
  print(join(List::sort(Map::keys(m))));
  print(join(List::sort(Map::values(m))));
  print(Int::to_string(Map::len(m)))
}
",
    );
    assert_eq!(out, "alpha beta gamma \none three two \n3\n");
}

// --- what the reference counting has to survive ----------------------------

/// Filled, grown, popped down, cleared, converted both ways and walked, with
/// nothing left alive at the end.
///
/// Boxed elements throughout, because an `Int` in a cell would leak nothing
/// whatever the code did. The two places a vector can hold on too long are the
/// copy in `grow` — which must not retain the old array's cells twice — and the
/// slot a `pop` vacates, which is written back to `Option::None` precisely so
/// that the element dies then rather than whenever something is pushed over it.
///
/// `live` is read into a local *before* any string is built for printing: a
/// literal being concatenated is itself a live object, and reading the count
/// inside the argument to `print` reports it as a leak.
#[test]
fn a_vector_and_a_walked_map_leave_nothing_behind() {
    let out = run(
        "vector_leaks",
        "module main;
import std::core::{List, Map, Option, Pair, Vector, print};

extern fn khora_live_count() -> Int;

fn work() -> Int {
  let v: Vector<String> = Vector::new();
  let mut i = 0;
  while i < 200 {
    Vector::push(v, \"item \" + Int::to_string(i));
    i = i + 1;
  };
  let mut taken = 0;
  let mut j = 0;
  while j < 100 {
    match Vector::pop(v) {
      Option::None => (),
      Option::Some(item) => { taken = taken + String::byte_length(item); },
    };
    j = j + 1;
  };
  // Replacing an element releases the one it replaced.
  Vector::set(v, 0, \"replaced\");
  let round = Vector::from_list(Vector::to_list(v));
  Vector::clear(v);
  let m: Map<String, String> = Map::new();
  let mut rest = Vector::to_list(round);
  let mut walking = true;
  while walking {
    match rest {
      List::Nil => { walking = false; },
      List::Cons(head, more) => {
        Map::insert(m, head, head + \"!\");
        rest = more;
      },
    }
  };
  let entries = Map::entries(m);
  let names = Map::keys(m);
  let mut seen = 0;
  let mut left = entries;
  let mut going = true;
  while going {
    match left {
      List::Nil => { going = false; },
      List::Cons(first, more) => {
        seen = seen + String::byte_length(first.value);
        left = more;
      },
    }
  };
  Vector::length(round) + Map::len(m) + List::length(names) + (if seen > 0 { 0 } else { 1 })
    + (if taken > 0 { 0 } else { 1 })
}

pub fn main() -> () {
  let total = work();
  let live = khora_live_count();
  print(Int::to_string(total));
  print(Int::to_string(live))
}
",
    );
    assert_eq!(out, "300\n0\n", "100 left in the vector, 100 in the map, 100 keys, nothing leaked");
}
