#![cfg(feature = "llvm")]

//! `for` over a `Vector`, an `Array`, a `Map` and a `Dict`, compiled and run.
//!
//! **What these guard: a loop over a collection that gives a right-looking
//! answer and is wrong about cost, order or a body that changes what it
//! walks.** These four are walked through `Iterable::iter` and a cursor that
//! advances in place, so every sum can be right while a loop copies the
//! collection first, a `Dict` comes out of key order, a push in the body makes
//! the loop endless, a pop makes it read past the end, a map that grows mid-loop
//! visits an entry twice, or the loop loses the back-edge that makes it a
//! cancellation point. Each test pins one of those.
//!
//! Compiled against `std` itself, because the cursors and `Iterable` are
//! `std::core`'s.

use crate::harness;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test: `main.kh` and the
/// other modules `extra` names, as `(file name, text)`.
fn sources(
    db: &KhoraDatabase,
    dir: &std::path::Path,
    main: &str,
    extra: &[(&str, &str)],
) -> Vec<SourceFile> {
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
    for (name, text) in extra {
        out.push(SourceFile::new(db, dir.join(name), text.to_string()));
    }
    out
}

fn build(name: &str, source: &str) -> PathBuf {
    build_with(name, source, &[])
}

fn build_with(name: &str, source: &str, extra: &[(&str, &str)]) -> PathBuf {
    build_ordered(name, source, extra, false)
}

/// [`build_with`], with the program's files ahead of `std`'s when
/// `program_first`: the order the CLI gives a project whose path sorts
/// before the toolchain's, `/app` against `~/.khora`.
fn build_ordered(name: &str, source: &str, extra: &[(&str, &str)], program_first: bool) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let mut files = sources(&db, &dir, source, extra);
    if program_first {
        let mine = files.split_off(files.len() - 1 - extra.len());
        files.splice(0..0, mine);
    }
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on `backend` under a watchdog: a cursor that never reaches its
/// end, or a loop that ignores a cancel, is a hang, and a hang would take the
/// suite with it.
fn run(exe: &PathBuf, backend: &str) -> String {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    while child.try_wait().expect("waiting").is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("`{backend}`: {} did not finish in 30 s", exe.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    assert_eq!(status.code(), Some(0), "`{backend}` exited badly: {stderr}");
    stdout.replace("\r\n", "\n")
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Each of the four, summed or joined: full, empty, nested, with `break` and
/// `continue`, as a list element, inside a closure and inside a spawned fiber.
///
/// The `Map` is printed sorted because its order is not one; the `Dict` is
/// printed as walked because its order is the promise -- smallest key first,
/// from keys inserted out of order.
#[test]
fn each_collection_is_walked_in_full() {
    let exe = build(
        "for_collections_values",
        r#"module main;
import std::core::{Iterator, Step, List, Range, Vector, Array, Map, Dict, Pair, Fiber, print};

fn total(xs: Vector<Int>) -> Int {
  let mut t = 0;
  for x in xs { t = t + x; };
  t
}

pub fn main() -> Int {
  let v: Vector<Int> = Vector::from_list([3, 1, 4, 1, 5]);
  let a: Array<String> = Array::from_fn(3, fn i => "a${i}");
  let m: Map<String, Int> = Map::new();
  Map::insert(m, "x", 1);
  Map::insert(m, "y", 2);
  Map::insert(m, "z", 3);
  let d: Dict<Int, String> = Dict::new().insert(30, "c").insert(10, "a").insert(20, "b");

  let mut vs = 0;
  for x in v { vs = vs + x; };
  let mut joined = "";
  for s in a { joined = joined + s; };
  let mut ms = 0;
  let mut keys = [];
  for e in m { ms = ms + e.value; keys = List::Cons(e.key, keys); };
  let mut walked = "";
  for Pair { key, value } in d { walked = walked + "${key}${value} "; };
  print("full ${vs} ${joined} ${ms} ${keys.sort()} ${walked}");

  let ev: Vector<Int> = Vector::new();
  let ea: Array<Int> = Array::empty();
  let em: Map<Int, Int> = Map::new();
  let ed: Dict<Int, Int> = Dict::new();
  let mut n = 0;
  for _x in ev { n = n + 1; };
  for _x in ea { n = n + 1; };
  for _e in em { n = n + 1; };
  for _e in ed { n = n + 1; };
  print("empty ${n}");

  let mut pairs = 0;
  for x in v { for e in d { if x > 3 { pairs = pairs + e.key; }; }; };
  print("nested ${pairs}");

  let mut kept = [];
  for x in v {
    if x == 1 { continue; };
    if x == 5 { break; };
    kept = List::Cons(x, kept);
  };
  let mut first_key = 0;
  for e in d { first_key = e.key; break; };
  print("break ${kept.reverse()} ${first_key}");

  print("element ${[for x in v => x * 10]} ${[for e in d => e.value]}");

  let in_closure = fn (xs: Array<String>) => { let mut k = 0; for _s in xs { k = k + 1; }; k };
  print("closure ${in_closure(a)} ${total(v)}");

  let shared = d;
  let fiber = Fiber::spawn(fn () => {
    let local: Vector<Int> = Vector::from_list([1, 2, 3]);
    let mut t = 0;
    for x in local { t = t + x; };
    for e in shared { t = t + e.key; };
    t
  });
  print("fiber ${Fiber::join(fiber)}");
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, backend),
            "full 14 a0a1a2 6 [x, y, z] 10a 20b 30c \n\
             empty 0\n\
             nested 120\n\
             break [3, 4] 10\n\
             element [30, 10, 40, 10, 50] [a, b, c]\n\
             closure 3 14\n\
             fiber 66\n",
            "`{backend}`"
        );
    }
}

/// **A `Dict` deeper than the cursor's first stack is walked whole and in
/// order.** The balancing keeps a `Dict` built with `insert` shallow -- a
/// million entries never get past the 32 cells the stack starts with -- so
/// only a tree built by hand reaches the doubling. A left spine is as deep as
/// it is long: 33 is one past the first stack, 100 doubles twice, 5,000 many
/// times. A doubling that dropped what the stack held loses most of the keys
/// and every test with a balanced tree stays green.
#[test]
fn a_dict_deeper_than_the_first_stack_is_walked_in_order() {
    let exe = build(
        "for_collections_dict_spine",
        r#"module main;
import std::core::{Iterator, Step, List, Dict, Pair, print};

/// Keys `n - 1` down to 0 along the left edge: a valid search tree, with no
/// right children at all, as deep as it has keys.
fn spine(n: Int) -> Dict<Int, Int> {
  let mut d: Dict<Int, Int> = Dict::Empty;
  let mut k = 0;
  while k < n { d = Dict::Node(k + 1, k, k, d, Dict::Empty); k = k + 1; };
  d
}

pub fn main() -> Int {
  for n in [10, 32, 33, 100, 5000] {
    let mut count = 0;
    let mut sum = 0;
    let mut sorted = true;
    let mut previous = -1;
    for e in spine(n) {
      count = count + 1;
      sum = sum + e.key;
      if e.key != previous + 1 { sorted = false; };
      previous = e.key;
    };
    print("${n} ${count} ${sum == n * (n - 1) / 2} ${sorted}");
  };
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, backend),
            "10 10 true true\n\
             32 32 true true\n\
             33 33 true true\n\
             100 100 true true\n\
             5000 5000 true true\n",
            "`{backend}`"
        );
    }
}

/// **A loop over a `Map` yields exactly `Map::entries`' order**, entry for
/// entry, on maps built in a scrambled order with removals, from empty to ten
/// thousand. Keys that share a bucket are what tells a cursor walking each
/// chain from its head apart from `entries`, which reports a chain from its
/// end, and at these sizes many do. A key type with a constant hash puts
/// every entry on one chain, so the whole map is one bucket's order. Sizes 0
/// and 1 are the edges.
#[test]
fn a_loop_over_a_map_yields_the_order_of_entries() {
    let exe = build(
        "for_collections_map_order",
        r#"module main;
import std::core::{Iterator, Step, List, Range, Map, Pair, Hash, Eq, print};

/// A map of `n` keys, inserted in an order scrambled by `seed`, with every
/// fifth key then removed again.
fn built(n: Int, seed: Int) -> Map<Int, Int> {
  let m: Map<Int, Int> = Map::new();
  let mut x = seed;
  for i in Range::Of(0, n) {
    x = (x * 1103515245 + 12345) % 2147483648;
    Map::insert(m, x % (n * 4 + 1), i);
  };
  let mut y = seed;
  for i in Range::Of(0, n) {
    y = (y * 1103515245 + 12345) % 2147483648;
    if i % 5 == 0 { Map::remove(m, y % (n * 4 + 1)); };
  };
  m
}

/// Whether the loop and `entries` agree, pair for pair.
fn agree(m: Map<Int, Int>) -> Bool {
  let mut walked: List<Pair<Int, Int>> = List::Nil;
  for e in m { walked = List::Cons(e, walked); };
  let mut rest = m.entries();
  let mut same = true;
  for e in walked.reverse() {
    match rest {
      List::Cons(f, tail) => {
        if e.key != f.key || e.value != f.value { same = false; };
        rest = tail;
      },
      List::Nil => { same = false; },
    }
  };
  match rest { List::Cons(_f, _tail) => false, List::Nil => same }
}

/// A key whose hash is the same for every value, so every entry of a map of
/// them is in one bucket, on a chain as long as the map.
type Same = { n: Int };

impl Hash for Same {
  fn hash(self) -> Int { 7 }
}

impl Eq for Same {
  fn eq(self, other: Same) -> Bool { self.n == other.n }
}

/// Whether the loop over a map of `n` colliding keys agrees with `entries`.
fn collided(n: Int) -> Bool {
  let m: Map<Same, Int> = Map::new();
  for i in Range::Of(0, n) { Map::insert(m, { n: i }, i); };
  let mut walked = [];
  for e in m { walked = List::Cons(e.key.n, walked); };
  let listed = [for e in m.entries() => e.key.n];
  walked.reverse() == listed
}

pub fn main() -> Int {
  let mut failed = [];
  for n in [0, 1, 2, 7, 33, 100, 1000, 2000, 10000] {
    for seed in [1, 7, 42] {
      if !agree(built(n, seed)) { failed = List::Cons(n * 100 + seed, failed); };
    };
  };
  for n in [3, 9, 40] {
    if !collided(n) { failed = List::Cons(n, failed); };
  };
  print("disagree ${failed}");
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "disagree []\n", "`{backend}`");
    }
}

/// **A loop allocates the same however long the collection is.** Measured at
/// two sizes, so a cost per element shows as a difference and a fixed cost
/// does not: a loop that went through `to_list()` or `entries()` would cost
/// ten thousand more at twice the size.
///
/// The exact counts are pinned too, because they are small enough to mean
/// something: one cursor for a `Vector`, an `Array` or a `Map`; for a `Dict`,
/// the cursor, the empty array it starts with, and the one array its stack
/// grows into.
#[test]
fn a_loop_allocates_the_same_at_any_length() {
    let exe = build(
        "for_collections_allocations",
        r#"module main;
import std::core::{Iterator, Step, Range, Vector, Array, Map, Dict, Pair, print};

extern fn khora_alloc_count() -> Int;

fn vec_of(n: Int) -> Vector<Int> {
  let v: Vector<Int> = Vector::new();
  for i in Range::Of(0, n) { Vector::push(v, i); };
  v
}

fn map_of(n: Int) -> Map<Int, Int> {
  let m: Map<Int, Int> = Map::new();
  for i in Range::Of(0, n) { Map::insert(m, i, i); };
  m
}

fn dict_of(n: Int) -> Dict<Int, Int> {
  let mut d: Dict<Int, Int> = Dict::new();
  for i in Range::Of(0, n) { d = d.insert(i, i); };
  d
}

fn sum_vec(v: Vector<Int>) -> Int { let mut t = 0; for x in v { t = t + x; }; t }
fn sum_arr(a: Array<Int>) -> Int { let mut t = 0; for x in a { t = t + x; }; t }
fn sum_map(m: Map<Int, Int>) -> Int { let mut t = 0; for e in m { t = t + e.value; }; t }
fn sum_dict(d: Dict<Int, Int>) -> Int { let mut t = 0; for e in d { t = t + e.value; }; t }

pub fn main() -> Int {
  let v1 = vec_of(10000);
  let v2 = vec_of(20000);
  let a1: Array<Int> = Array::from_fn(10000, fn i => i);
  let a2: Array<Int> = Array::from_fn(20000, fn i => i);
  let m1 = map_of(10000);
  let m2 = map_of(20000);
  let d1 = dict_of(10000);
  let d2 = dict_of(20000);

  let x0 = khora_alloc_count();
  let r1 = sum_vec(v1);
  let x1 = khora_alloc_count();
  let r2 = sum_vec(v2);
  let x2 = khora_alloc_count();
  let y1 = khora_alloc_count();
  let q1 = sum_arr(a1);
  let y2 = khora_alloc_count();
  let q2 = sum_arr(a2);
  let y3 = khora_alloc_count();
  let z1 = khora_alloc_count();
  let p1 = sum_map(m1);
  let z2 = khora_alloc_count();
  let p2 = sum_map(m2);
  let z3 = khora_alloc_count();
  let w1 = khora_alloc_count();
  let o1 = sum_dict(d1);
  let w2 = khora_alloc_count();
  let o2 = sum_dict(d2);
  let w3 = khora_alloc_count();
  print("${x1 - x0} ${x2 - x1} ${y2 - y1} ${y3 - y2} ${z2 - z1} ${z3 - z2} ${w2 - w1} ${w3 - w2}");
  print("${r1 + q1 + p1 + o1} ${r2 + q2 + p2 + o2}");
  0
}
"#,
    );
    assert_eq!(run(&exe, "threads"), "1 1 1 1 1 1 3 3\n199980000 799960000\n");
}

/// **What the loop sees when its body changes the collection**, as each
/// cursor documents it, and in every case an end: no hang, no trap, no item
/// skipped that was there throughout.
///
/// - `Vector`: the loop visits index 0, 1, 2 and on while the index is below
///   both the length it began with and the length at that step, reading the
///   element there when it gets there. So a plain push is not visited and the
///   loop ends; a pop or a `clear` ends it at the new end; and a pop or a
///   `clear` *followed by* pushes is visited where the pushed elements land
///   below the starting length -- the shape the rule exists to state.
/// - `Array`: a `set` ahead is visited with its new value.
/// - `Map`: inserting enough to grow the table mid-loop still visits each
///   entry that was there all along exactly once; removing ahead neither
///   traps nor revisits.
/// - `Dict`: an insert makes a new dictionary and the loop walks the old one.
#[test]
fn a_body_that_changes_the_collection_gets_a_defined_answer() {
    let exe = build(
        "for_collections_mutation",
        r#"module main;
import std::core::{Iterator, Step, List, Range, Vector, Array, Map, Dict, Pair, print};

pub fn main() -> Int {
  let v: Vector<Int> = Vector::from_list([0, 1, 2, 3]);
  let mut seen = [];
  // Capped, so a loop that would never end gives a wrong answer here rather
  // than a vector that grows until the machine runs out of memory.
  let mut turns = 0;
  for x in v {
    turns = turns + 1;
    if turns > 100 { break; };
    seen = List::Cons(x, seen);
    Vector::push(v, x + 100);
  };
  print("push ${seen.reverse()} ${Vector::length(v)}");

  let w: Vector<Int> = Vector::from_list([1, 2, 3, 4, 5]);
  let mut popped = [];
  for x in w { popped = List::Cons(x, popped); let _ = Vector::pop(w); };
  print("pop ${popped.reverse()}");

  let c: Vector<Int> = Vector::from_list([1, 2, 3]);
  let mut cleared = 0;
  for _x in c { cleared = cleared + 1; Vector::clear(c); };
  print("clear ${cleared}");

  let s: Vector<Int> = Vector::from_list([1, 2, 3]);
  let mut set = [];
  for x in s { set = List::Cons(x, set); let _ = Vector::set(s, 2, 30); };
  let a: Array<Int> = Array::from_fn(3, fn i => i);
  let mut aset = [];
  for x in a { aset = List::Cons(x, aset); Array::set(a, 2, 99); };
  print("set ${set.reverse()} ${aset.reverse()}");

  let m: Map<Int, Int> = Map::new();
  for i in Range::Of(0, 6) { Map::insert(m, i, i); };
  let mut originals = [];
  for e in m {
    originals = List::Cons(e.key, originals);
    for j in Range::Of(0, 20) { Map::insert(m, 1000 + e.key * 100 + j, 0); };
  };
  print("grow ${originals.sort()} ${Map::len(m)}");

  let r: Map<Int, Int> = Map::new();
  for i in Range::Of(0, 50) { Map::insert(r, i, i); };
  let mut visited = [];
  for e in r { visited = List::Cons(e.key, visited); Map::remove(r, e.key + 1); };
  // Each visited at most once, and every entry either visited or removed.
  let mut repeats = 0;
  let mut last = -1;
  for k in visited.sort() { if k == last { repeats = repeats + 1; }; last = k; };
  print("remove ${repeats} ${List::length(visited) + Map::len(r) >= 50}");

  let mut d: Dict<Int, Int> = Dict::new().insert(1, 1).insert(2, 2);
  let mut dsum = 0;
  for e in d { dsum = dsum + e.key; d = d.insert(e.key + 10, 0); };
  print("dict ${dsum} ${Dict::size(d)}");

  // Pop then push each turn: index 4 holds a pushed element by the time the
  // loop reaches it. Once with no spare room and once with plenty, so an
  // answer that depended on whether the push reallocated would differ.
  for room in [0, 64] {
    let pp: Vector<Int> = Vector::with_capacity(room);
    for i in [1, 2, 3, 4, 5] { Vector::push(pp, i); };
    let mut seen = [];
    for x in pp { seen = List::Cons(x, seen); let _ = Vector::pop(pp); Vector::push(pp, x + 100); };
    print("pop-push ${room} ${seen.reverse()} ${Vector::to_list(pp)}");
  };

  // Clear, then push more than were there: the loop goes on through the new
  // elements, and stops at the length it began with.
  for room in [0, 64] {
    let cr: Vector<Int> = Vector::with_capacity(room);
    for i in [1, 2, 3, 4, 5] { Vector::push(cr, i); };
    let mut seen = [];
    let mut first = true;
    for x in cr {
      seen = List::Cons(x, seen);
      if first {
        first = false;
        Vector::clear(cr);
        for i in Range::Of(0, 10) { Vector::push(cr, 900 + i); };
      };
    };
    print("clear-refill ${room} ${seen.reverse()} ${Vector::length(cr)}");
  };
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, backend),
            "push [0, 1, 2, 3] 8\n\
             pop [1, 2, 3]\n\
             clear 1\n\
             set [1, 2, 30] [0, 1, 99]\n\
             grow [0, 1, 2, 3, 4, 5] 126\n\
             remove 0 true\n\
             dict 3 4\n\
             pop-push 0 [1, 2, 3, 4, 104] [1, 2, 3, 4, 204]\n\
             pop-push 64 [1, 2, 3, 4, 104] [1, 2, 3, 4, 204]\n\
             clear-refill 0 [1, 901, 902, 903, 904] 10\n\
             clear-refill 64 [1, 901, 902, 903, 904] 10\n",
            "`{backend}`"
        );
    }
}

/// **A `for` over an `Array` stops when its fiber is canceled**, on both
/// backends. The only loops in the fiber are two `for`s, so the back-edge that
/// stops it is a `for`'s and nobody else's; run to the end they are 10^11
/// steps, which the watchdog ends long before.
#[test]
fn a_for_over_an_array_is_a_cancellation_point() {
    let exe = build(
        "for_collections_cancel",
        r#"module main;
import std::core::{Iterator, Step, Array, Fiber, Channel, print};

fn walk(ready: Channel<Int>) -> Int {
  let outer: Array<Int> = Array::new(1000000, 1);
  let inner: Array<Int> = Array::new(100000, 1);
  Channel::send(ready, 1);
  let mut t = 0;
  for x in outer { for y in inner { t = t + x + y; }; };
  print("the loop finished");
  t
}

pub fn main() -> Int {
  let ready: Channel<Int> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => walk(ready));
  let _ = Channel::receive(ready);
  Fiber::cancel(f);
  Fiber::wait(f);
  print("canceled: ${Fiber::canceled(f)}");
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "canceled: true\n", "`{backend}`");
    }
}

/// **A loop that leaves early frees its cursor**, on both backends: a `break`
/// out of each of the four, a hundred times, leaves the live count where it
/// was.
#[test]
fn leaving_a_loop_early_frees_the_cursor() {
    let exe = build(
        "for_collections_leaks",
        r#"module main;
import std::core::{Iterator, Step, List, Range, Vector, Array, Map, Dict, Pair, print};

extern fn khora_live_count() -> Int;

pub fn main() -> Int {
  let v: Vector<Int> = Vector::from_list([1, 2, 3, 4, 5, 6, 7, 8]);
  let a: Array<Int> = Array::from_fn(8, fn i => i);
  let m: Map<Int, Int> = Map::new();
  let mut d: Dict<Int, Int> = Dict::new();
  for i in Range::Of(0, 8) { Map::insert(m, i, i); d = d.insert(i, i); };
  let before = khora_live_count();
  let mut k = 0;
  while k < 100 {
    for x in v { if x == 5 { break; }; };
    for x in a { if x == 5 { break; }; };
    for e in m { if e.key == 5 { break; }; };
    for e in d { if e.key == 5 { break; }; };
    k = k + 1;
  };
  let after = khora_live_count();
  print("delta ${after - before}");
  0
}
"#,
    );
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "delta 0\n", "`{backend}`");
    }
}

/// **A type of the program's own can hand out a cursor**, and a `for` over it
/// works with no import of `Iterable` beyond the one the `impl` needs.
#[test]
fn a_program_can_make_its_own_type_iterable() {
    let exe = build(
        "for_collections_user",
        r#"module main;
import std::core::{Iterator, Iterable, Step, List, Vector, VectorItems, print};

type Deck = { cards: Vector<Int> };

impl Iterable for Deck {
  type Cursor = VectorItems<Int>;
  fn iter(self) -> VectorItems<Int> { Vector::iter(self.cards) }
}

pub fn main() -> Int {
  let deck: Deck = { cards: Vector::from_list([7, 8, 9]) };
  let mut t = 0;
  for card in deck { t = t + card; };
  print("${t} ${[for card in deck => card + 1]}");
  0
}
"#,
    );
    assert_eq!(run(&exe, "threads"), "24 [8, 9, 10]\n");
}

const LIB: &str = "module lib;\nimport std::core::{Vector, List};\n\
                   pub fn make() -> Vector<Int> { Vector::from_list([1, 2, 3, 4]) }\n";

/// **A program's own `Vector` and `std`'s, each walked as itself**, in one
/// file and on both backends. The program's is its own `Iterator`; `std`'s
/// comes back from another module and is walked through `std`'s `Iterable`.
/// Asked by name, `std`'s `Vector` looked like the program's `Iterator` --
/// and the loop called a `next` with no body to call.
#[test]
fn an_own_vector_and_stds_are_walked_as_themselves() {
    let exe = build_with(
        "for_collections_two_vectors",
        r#"module main;
import std::core::{Iterator, Iterable, Step, List, print};
import lib::{make};

type Vector = { left: Int };

impl Iterator for Vector {
  type Item = Int;
  type Effects = {};
  fn next(self) -> Step<Vector, Int> {
    if self.left == 0 { Step::Done } else { Step::Yield({ left: self.left - 1 }, self.left) }
  }
}

type Bag<A> = { xs: List<A> };

impl<A> Iterable for Bag<A> {
  type Cursor = List<A>;
  fn iter(self) -> List<A> { self.xs }
}

pub fn main() -> Int {
  let mut theirs = 0;
  for x in make() { theirs = theirs + x; };
  let mine: Vector = { left: 3 };
  let mut counted = "";
  for y in mine { counted = counted + "${y}"; };
  let bag: Bag<Int> = { xs: [100, 200] };
  let mut bagged = 0;
  for z in bag { bagged = bagged + z; };
  print("${theirs} ${counted} ${bagged}");
  0
}
"#,
        &[("lib.kh", LIB)],
    );
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "10 321 300\n", "`{backend}`");
    }
}

const CARDS: &str = "module cards;\nimport std::core::{Iterable, Vector, VectorItems, List};\n\
                     pub type Deck = { pub cards: Vector<Int> };\n\
                     impl Iterable for Deck {\n  type Cursor = VectorItems<Int>;\n  \
                     fn iter(self) -> VectorItems<Int> { Vector::iter(self.cards) }\n}\n\
                     pub fn deck() -> Deck { { cards: Vector::from_list([7, 8, 9]) } }\n";

/// **A type with an `impl Iterable` is walkable wherever it is imported**,
/// as a library ships one: the `traits.md` `Deck` in a module of its own,
/// walked from `main` by `for`, through `d.iter()` and in a comprehension.
#[test]
fn an_iterable_type_from_another_module_is_walked() {
    let exe = build_with(
        "for_collections_imported_deck",
        r#"module main;
import std::core::{Iterator, Iterable, Step, List, print};
import cards::{Deck, deck};

pub fn main() -> Int {
  let d: Deck = deck();
  let mut t = 0;
  for x in d { t = t + x; };
  let mut u = 0;
  for y in d.iter() { u = u + y; };
  print("${t} ${u} ${[for z in d => z * 10]}");
  0
}
"#,
        &[("cards.kh", CARDS)],
    );
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "24 24 [70, 80, 90]\n", "`{backend}`");
    }
}

/// Another module's own `Vector`, with an `impl Iterable` and an `impl Show`
/// of its own, under the same method keys as `std`'s.
const OTHER: &str = "module other;\nimport std::core::{Iterator, Iterable, Show, Step, List};\n\n\
                     pub type Vector<A> = { pub xs: List<A> };\n\
                     impl<A> Iterable for Vector<A> { type Cursor = List<A>; fn iter(self) -> List<A> { self.xs } }\n\
                     impl<A> Show for Vector<A> { fn show(self) -> String { \"OTHER\" } }\n\
                     pub fn walk_mine() -> Int { let m: Vector<Int> = { xs: [100, 200] }; \
                     let mut d = 0; for w in m { d = d + w; }; d }\n";

/// **`Vector::iter(v)` and `Vector::show(v)` on `std`'s `Vector` call
/// `std`'s bodies, in either unit order.** `Iterable#Vector::iter` is the
/// key of `std`'s impl and of `other`'s too, and a call by that key went to
/// whichever unit came first. With the program's files ahead of `std`'s --
/// a project under `/app` -- `iter` called `other`'s body on `std`'s vector,
/// which was invalid IR, and `show` printed `OTHER` for `[1, 2, 3]`. The
/// loop and the `${..}` hole, which go through the trait, were right in
/// both orders and are here as the control.
#[test]
fn a_static_call_on_stds_vector_reaches_stds_body_in_either_order() {
    let main = r#"module main;
import std::core::{Iterator, Iterable, Show, Step, List, Vector, print};
import other::{walk_mine};

pub fn main() -> Int {
  let v: Vector<Int> = Vector::from_list([1, 2, 3]);
  let mut a = 0;
  for x in v { a = a + x; };
  let mut b = 0;
  for y in Vector::iter(v) { b = b + y; };
  print("${a} ${b} ${walk_mine()} ${Vector::show(v)} ${v}");
  0
}
"#;
    for program_first in [false, true] {
        let exe = build_ordered(
            &format!("for_collections_static_iter_{program_first}"),
            main,
            &[("other.kh", OTHER)],
            program_first,
        );
        for backend in BACKENDS {
            assert_eq!(
                run(&exe, backend),
                "6 6 300 [1, 2, 3] [1, 2, 3]\n",
                "`{backend}`, program first: {program_first}"
            );
        }
    }
}

/// One of two modules' `Entry`, each with an `impl Show`.
fn entry_module(name: &str) -> String {
    format!(
        "module {name};\nimport std::core::{{Show, print}};\n\n\
         pub type Entry = {{ pub n: Int }};\n\
         impl Show for Entry {{ fn show(self) -> String {{ \"{tag} ${{self.n}}\" }} }}\n\
         pub fn mk() -> Entry {{ {{ n: 1 }} }}\n\
         pub fn say() -> String {{ let e = mk(); e.show() }}\n",
        tag = name.to_uppercase()
    )
}

/// **`Entry::show(e)` calls the `Entry` the file imported**, whichever of two
/// modules with an `Entry` comes first. Not `for`'s, and older than it: the
/// static call took the first unit's body for `Show#Entry::show`, so it said
/// `AAA 1` on `bbb`'s entry while `e.show()` and `${e}` said `BBB 1`.
#[test]
fn a_static_call_reaches_the_named_types_impl() {
    let aaa = entry_module("aaa");
    let bbb = entry_module("bbb");
    for (used, other) in [("bbb", "aaa"), ("aaa", "bbb")] {
        let main = format!(
            "module main;\nimport std::core::{{Show, print}};\nimport {used}::{{Entry, mk}};\n\
             import {other}::{{say}};\n\n\
             pub fn main() -> Int {{ let e: Entry = mk(); \
             print(\"${{Entry::show(e)}} | ${{e.show()}} | ${{e}} | ${{say()}}\"); 0 }}\n"
        );
        let exe = build_with(
            &format!("for_collections_entry_{used}"),
            &main,
            &[("aaa.kh", &aaa), ("bbb.kh", &bbb)],
        );
        let (mine, theirs) = (used.to_uppercase(), other.to_uppercase());
        assert_eq!(
            run(&exe, "threads"),
            format!("{mine} 1 | {mine} 1 | {mine} 1 | {theirs} 1\n"),
            "`{used}`'s `Entry`"
        );
    }
}
