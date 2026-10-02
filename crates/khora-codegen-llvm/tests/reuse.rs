#![cfg(feature = "llvm")]

//! Phase 9's target: `map` over a uniquely-owned list allocates nothing.
//!
//! The cell being matched is dead the moment the arm has its fields, so the
//! cell the arm builds can be the same memory. The assertion below was written
//! before the work rather than after it — a criterion first evaluated once the
//! change is in is a criterion fitted to the result — and sat `#[ignore]`d
//! beside a `reuse_is_not_implemented_yet` recording the ten allocations a walk
//! cost instead. Deleting that one was its own instruction for the day it
//! failed.
//!
//! Allocation counts are the compiler's own instrument and not a promise to
//! anybody — `docs/design/compatibility.md` decides that when memory is
//! allocated is not observable, which is what makes phase 9 legal at all.

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

/// A list of ten built and then walked, incrementing each element.
///
/// The walk is the interesting half: it consumes a list nothing else holds and
/// produces one of the same shape, which is the exact case reuse exists for.
/// Counters are reset after the list is built so the number is the walk's
/// alone, and the sum is printed so nothing can be optimized away on the
/// grounds that the result is unused.
const WALK: &str = "module main;
import std::core::{List, print};

extern fn khora_alloc_count() -> Int;
extern fn khora_reset_counters();

fn build(n: Int) -> List<Int> {
  if n == 0 { List::Nil } else { List::Cons(n, build(n - 1)) }
}

fn increment(xs: List<Int>) -> List<Int> {
  match xs {
    List::Nil => List::Nil,
    List::Cons(head, tail) => List::Cons(head + 1, increment(tail)),
  }
}

fn total(xs: List<Int>) -> Int {
  match xs {
    List::Nil => 0,
    List::Cons(head, tail) => head + total(tail),
  }
}

pub fn main() -> () {
  let built = build(10);
  khora_reset_counters();
  let walked = increment(built);
  let allocations = khora_alloc_count();
  print(Int::to_string(total(walked)));
  print(Int::to_string(allocations))
}
";

/// **Phase 9's exit criterion.** `docs/design/reuse.md`.
///
/// Every cell the walk consumes is uniquely held by the time its fields are
/// out, so every cell it builds can be that memory. Ten in, ten out, nothing
/// allocated.
///
/// The number was eleven when this file was written and ten when reuse landed:
/// the `List::Nil` closing the walk stopped being an allocation once a
/// field-less constructor became one static object for the whole program, which
/// was phase 9.0.
#[test]
fn a_uniquely_owned_walk_allocates_nothing() {
    let out = run("reuse_walk_target", WALK);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "65", "the walk should sum 2..=11");
    assert_eq!(lines[1], "0", "a uniquely-owned walk should reuse every cell it consumes");
}

/// `Range::next` writes an `if` where reuse used to look for a constructor.
///
/// It matches the `Range` cell and then branches -- `Step::Done` one way, a
/// fresh `Range` inside a `Step::Yield` the other -- so the arm's body was not
/// itself a constructor and the cell it had just taken apart could never be
/// built in. Both leaves *are* constructors, which is all the token needs: one
/// path, one spend.
///
/// Walked recursively so the cell arrives uniquely owned; a loop copies its
/// cursor and there would be no token to spend either way, which is
/// `docs/design/reuse.md` §1 and not this.
const RANGE_WALK: &str = "module main;
import std::core::{Iterator, Range, Step, print};

extern fn khora_alloc_count() -> Int;
extern fn khora_reset_counters();

fn walk(r: Range) -> Int {
  match Iterator::next(r) {
    Step::Done => 0,
    Step::Yield(rest, item) => item + walk(rest),
  }
}

pub fn main() -> () {
  khora_reset_counters();
  let total = walk(Range::Of(0, 10));
  let made = khora_alloc_count();
  print(Int::to_string(total));
  print(Int::to_string(made))
}
";

/// **A branch is one path to a constructor, not none.**
///
/// Counted over five hundred elements this is 519 allocations where it was
/// 1,019 -- one per element rather than two, because the `Range` the next step
/// walks is now built in the cell the last one was matched out of.
#[test]
fn a_constructor_in_each_branch_still_reuses() {
    let out = run("reuse_range_walk", RANGE_WALK);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "45", "the walk should sum 0..=9");

    // The count is read before anything is printed, so it is the walk's
    // alone and doesn't move when `Int::to_string` or `print` allocate less.
    //
    // **Held inline there is no cell to reuse, and none to count.** `Range`
    // and `Step` are then registers rather than objects, so the walk makes no
    // heap cells at all. The branch shape this guards is still compiled, and
    // the walks over `List` in this file are recursive, so they are boxed
    // either way and keep testing it. Delete the branch when the flag goes.
    let inline = khora_codegen_llvm::unboxing_enabled();
    let expected = if inline { "0" } else { "11" };
    assert_eq!(lines[1], expected, "one allocation an element, not two");
}

/// **A branch that builds a constant has to give the cell back.**
///
/// `std::resilience` has this shape and it found the bug: a case with no
/// fields is one static object for the whole program, so it allocates nothing
/// and cannot build in the cell the arm was promised. Freeing the token where
/// the branches join instead frees memory the *other* branch has already
/// reused and returned -- which comes back as a tag that matches no arm, and
/// `llvm.trap`.
///
/// The live count is the assertion that matters. A double free shows up as a
/// crash only when the allocator reuses the memory quickly enough, and this
/// ran to completion with the wrong answer before it ever did.
const BRANCHED_TO_A_CONSTANT: &str = "module main;
import std::core::{Option, print};

extern fn khora_live_count() -> Int;

fn capped(o: Option<Int>, limit: Int) -> Option<Int> {
  match o {
    Option::None => Option::None,
    Option::Some(at) => if at < limit { Option::Some(at) } else { Option::None },
  }
}

pub fn main() -> () {
  let mut i = 0;
  let mut kept = 0;
  while i < 200 {
    kept = kept + (match capped(Option::Some(i), 100) {
      Option::Some(v) => v,
      Option::None => 0,
    });
    i = i + 1;
  };
  print(Int::to_string(kept));
  print(Int::to_string(khora_live_count()))
}
";

#[test]
fn a_branch_that_builds_a_constant_frees_the_token() {
    let out = run("reuse_constant_branch", BRANCHED_TO_A_CONSTANT);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "4950", "the kept values are 0..=99");
    assert_eq!(lines[1], "0", "nothing left over, and nothing freed twice");
}

/// **A static is never reused.** A reuse arm frees the matched cell and
/// builds its result there if the cell was unique. A field-less constructor
/// or a string literal is one object for the whole program. Building a
/// `Grown` in the one `Nil` would make every later `Nil` read as a `Grown`,
/// with a length read from beyond the static's end.
///
/// Each arm below builds its result, so each takes a reuse token, and `grow`
/// is called on `Nil` and on a list holding literals. The runtime refuses a
/// token for a static. The statics are in read-only memory, so a runtime
/// that forgot the test would fault here rather than corrupt `Nil`. The last
/// line reads the literals and `Nil` again, which catches a corruption that
/// did not fault.
const GROWN_FROM_A_LITERAL: &str = "module main;
import std::core::{print};

extern fn khora_live_count() -> Int;

type Words = | Nil | Word(String, Words) | Grown(String, Words);

fn grow(w: Words) -> Words {
  match w {
    Words::Nil => Words::Grown(\"new\", Words::Nil),
    Words::Word(s, rest) => Words::Grown(s + \"!\", grow(rest)),
    Words::Grown(s, rest) => Words::Word(s, grow(rest)),
  }
}

fn show(w: Words) -> String {
  match w {
    Words::Nil => \"nil\",
    Words::Word(s, rest) => \"word \" + s + \" \" + show(rest),
    Words::Grown(s, rest) => \"grown \" + s + \" \" + show(rest),
  }
}

pub fn main() -> () {
  let before = khora_live_count();
  let mut i = 0;
  while i < 100 {
    let _ = show(grow(Words::Nil));
    let _ = show(grow(Words::Word(\"lit\", Words::Nil)));
    i = i + 1;
  };
  let live = khora_live_count() - before;
  print(show(Words::Word(\"lit\", Words::Nil)));
  print(Int::to_string(live))
}
";

#[test]
fn a_reuse_arm_never_builds_in_a_static() {
    let out = run("reuse_static_scrutinee", GROWN_FROM_A_LITERAL);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "word lit nil", "the literal and `Nil` are what they were");
    assert_eq!(lines[1], "0", "nothing left over, and nothing freed twice");
}

/// `match receive(h) { Some(c) => send(other, c) }`, and the count of `c` at
/// the call.
///
/// **What this prevents: a hand-off that traps on the most natural spelling of
/// a pool.** A send that gives a value away checks that nothing else holds it,
/// by its count. The arm copies `c` out of the payload and releases the
/// scrutinee at its head, so the call should find `c` held once. It found it
/// held twice in the two places a pool's code lives: a body that can unwind (a
/// `!` anywhere in it), and a `match` inside a loop. In both the read at the
/// call copied instead of handing over the arm's reference, and the arm
/// released its copy after the call returned.
///
/// The count is read without an extern that takes an object, which the C ABI
/// refuses: `rebuilt` consumes its argument by rebuilding it, and that
/// allocates nothing exactly when the argument arrived held once, because the
/// rebuild is made in the cell it matched. `Conn` is recursive so it is a
/// counted cell with either representation.
///
/// `again` is the control: the scrutinee is read after the call, so it still
/// holds the payload there, the count is 2, and the rebuild allocates.
const HANDED_ON: &str = "module main;
import std::core::{Option, Result, print};

extern fn khora_alloc_count() -> Int;
extern fn khora_live_count() -> Int;

type Bad = | Bad(Int);
type Conn = | Open(Int, Conn) | End;

fn consume(c: Conn) -> Conn {
  match c {
    Conn::Open(n, rest) => Conn::Open(n + 1, rest),
    Conn::End => Conn::End,
  }
}

/// Allocations made consuming `c`: 0 when it arrived held once.
fn rebuilt(c: Conn) -> Int {
  let before = khora_alloc_count();
  let kept = consume(c);
  let made = khora_alloc_count() - before;
  let _ = kept;
  made
}

fn receive(n: Int) -> Option<Conn> {
  if n > 0 { Option::Some(Conn::Open(n, Conn::End)) } else { Option::None }
}

fn check(n: Int) -> Int raises Bad {
  if n < 0 { raise Bad::Bad(n) } else { n }
}

fn is_some(o: Option<Conn>) -> Int {
  match o { Option::Some(_) => 1, Option::None => 0 }
}

fn straight(o: Option<Conn>) -> Int {
  match o {
    Option::Some(c) => rebuilt(c),
    Option::None => 0,
  }
}

fn unwinds(o: Option<Conn>) -> Int raises Bad {
  let k = check(0)!;
  match o {
    Option::Some(c) => rebuilt(c) + k,
    Option::None => 0,
  }
}

fn nested(r: Result<Option<Conn>, String>) -> Int raises Bad {
  let k = check(0)!;
  match r {
    Result::Ok(Option::Some(c)) => rebuilt(c) + k,
    Result::Ok(Option::None) => 0,
    Result::Err(_) => 0,
  }
}

fn looped(n: Int) -> Int {
  let mut i = 0;
  let mut made = 0;
  while i < n {
    match receive(1) {
      Option::Some(c) => made = made + rebuilt(c),
      Option::None => (),
    };
    i = i + 1
  };
  made
}

fn nested_looped(n: Int) -> Int {
  let mut i = 0;
  let mut made = 0;
  while i < n {
    let r: Result<Option<Conn>, String> = Result::Ok(receive(1));
    match r {
      Result::Ok(Option::Some(c)) => made = made + rebuilt(c),
      Result::Ok(Option::None) => (),
      Result::Err(_) => (),
    };
    i = i + 1
  };
  made
}

fn again(o: Option<Conn>) -> Int raises Bad {
  let k = check(0)!;
  match o {
    Option::Some(c) => rebuilt(c) + k + 0 * is_some(o),
    Option::None => 0,
  }
}

fn again_looped(n: Int) -> Int {
  let mut i = 0;
  let mut made = 0;
  while i < n {
    let o = receive(1);
    match o {
      Option::Some(c) => made = made + rebuilt(c) + 0 * is_some(o),
      Option::None => (),
    };
    i = i + 1
  };
  made
}

/// Raises before the arm hands `c` on, and after.
fn raises_before(o: Option<Conn>) -> Int raises Bad {
  let k = check(0)!;
  match o {
    Option::Some(c) => {
      let x = check(-1)!;
      rebuilt(c) + x + k
    },
    Option::None => 0,
  }
}

fn raises_after(o: Option<Conn>) -> Int raises Bad {
  let k = check(0)!;
  match o {
    Option::Some(c) => {
      let m = rebuilt(c);
      check(m - 1)! + k
    },
    Option::None => 0,
  }
}

pub fn main() -> () raises Bad {
  let before = khora_live_count();
  print(\"straight ${straight(receive(1))}\");
  print(\"unwinds ${unwinds(receive(1))!}\");
  print(\"nested ${nested(Result::Ok(receive(1)))!}\");
  print(\"looped ${looped(3)}\");
  print(\"nested_looped ${nested_looped(3)}\");
  print(\"again ${again(receive(1))!}\");
  print(\"again_looped ${again_looped(3)}\");
  let b = raises_before(receive(1))! catch { Bad::Bad(_) => 7 };
  let a = raises_after(receive(1))! catch { Bad::Bad(_) => 8 };
  print(\"raised ${b} ${a}\");
  print(\"live ${khora_live_count() - before}\")
}
";

#[test]
fn an_arm_hands_its_binding_on_at_a_consuming_call() {
    let out = run("reuse_handed_on", HANDED_ON);
    assert_eq!(
        out.trim(),
        "straight 0\nunwinds 0\nnested 0\nlooped 0\nnested_looped 0\n\
         again 1\nagain_looped 3\nraised 7 8\nlive 0",
        "0 is a count of 1 at the call; `again` holds the payload past it"
    );
}
