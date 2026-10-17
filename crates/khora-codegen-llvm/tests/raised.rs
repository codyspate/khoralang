#![cfg(feature = "llvm")]

//! Failures as values: `Raised<'er>`, end to end.
//!
//! Each program here is compiled against the real `std` and run on both fiber
//! backends, because what goes wrong with a failure value is what happens to
//! its memory: a box read as the error it holds, a note dropped on the way
//! through `raise`, an error released twice by two joiners. The live-object
//! count is asserted wherever a path could leak, since a leak on a failure path
//! is one per request on a server.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std` for this target, plus the program under test.
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

/// Compiles `main` against `std`, runs it on both fiber backends, and gives
/// back the backend, the exit code and stdout of each.
fn run_program(name: &str, main: &str) -> Vec<(String, Option<i32>, String)> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    ["threads", "scheduler"]
        .iter()
        .map(|fibers| {
            let out = Command::new(&exe).env("KHORA_FIBERS", fibers).output().expect("the program should run");
            let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
            let stderr = String::from_utf8_lossy(&out.stderr);
            (fibers.to_string(), out.status.code(), format!("{stdout}{}", if stderr.is_empty() { String::new() } else { format!("[stderr] {stderr}") }))
        })
        .collect()
}

/// Asserts every backend exited 0 and printed exactly `expected`.
fn expect(name: &str, main: &str, expected: &str) {
    for (fibers, code, out) in run_program(name, main) {
        assert_eq!(code, Some(0), "{fibers}: {out}");
        assert_eq!(out, expected, "{fibers}");
    }
}

/// **A binding over two error types holds whichever arrived.** `attempt`
/// over a body raising `Inv + Pay`, and a `catch` binding over the same
/// operand, both name the type and show the error as its own type.
#[test]
fn a_binding_over_two_types_holds_either() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, attempt, print};

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(why: String) | Timeout;

fn both(n: Int) -> Int raises Inv + Pay {
  if n == 0 { raise Inv::Out("sku-0") };
  if n == 1 { raise Pay::Declined("card") };
  n
}

fn main() -> () {
  match attempt(fn () => both(0)!) { Result::Ok(_) => (), Result::Err(e) => print("${Raised::type_name(e)} ${e.show()}") };
  let s = both(1)! catch { err => { print("${Raised::type_name(err)} ${err.show()}"); 0 } };
  print("${s}");
}
"#;
    expect("raised_binding_two", main, "Inv Inv::Out(sku-0)\nPay Pay::Declined(card)\n0\n");
}

/// **A generic function over `'er`, instantiated at one type, still holds a
/// box** (review probe `p1c`). Instantiating at `{ Pay }` must not turn
/// `Raised<'er>` into `Pay`, or the body reads `Pay::Declined`'s first two
/// fields as an error id and a payload pointer.
#[test]
fn a_generic_failure_keeps_its_layout_at_one_type() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, attempt, print};

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(code: Int, amount: Int, why: String, n: Int) | Timeout;

fn two(n: Int) -> Int raises Inv + Pay { if n == 0 { raise Pay::Declined(2, 500, "card", 0) }; n }
fn one(n: Int) -> Int raises Pay { if n == 0 { raise Pay::Declined(2, 999, "card", 0) }; n }

fn format<'er>(r: Raised<'er>) -> String { "${Raised::type_name(r)}: ${Raised::describe(r)}" }

fn main() -> () {
  match attempt(fn () => two(0)!) { Result::Ok(_) => (), Result::Err(e) => print("two=${format(e)}") };
  match attempt(fn () => one(0)!) { Result::Ok(_) => (), Result::Err(e) => print("one=${format(e)}") };
}
"#;
    expect(
        "raised_generic_one",
        main,
        "two=Pay: Pay::Declined(2, 500, card, 0)\none=Pay: Pay::Declined(2, 999, card, 0)\n",
    );
}

/// **A cancellation inside an error's `Show` leaves `describe` as a
/// cancellation** (review probe `p6a`). Read without its tag, the text half
/// of the answer is null, and the fiber went on to read it (exit 139).
#[test]
fn a_cancel_inside_describe_is_a_cancel() {
    let main = r#"module demo::main;
import std::core::{Fiber, Raised, Result, Shared, Show, String, attempt, nursery, print};

pub type Inv = | Out(sku: String) | Offline;
impl Show for Inv {
  fn show(self) -> String {
    let mut i = 0;
    while i < 10000000 { i = i + 1; };
    match self { Inv::Out(s) => "Out(${s})", Inv::Offline => "Offline" }
  }
}
derive(Show)
pub type Pay = | Declined | Timeout;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n == 0 { raise Inv::Out("sku-1") };
  if n == 1 { raise Pay::Declined };
  "ok"
}

fn main() -> () {
  let started = Shared::of(0);
  nursery(fn () => {
    let f = Fiber::spawn(fn () => {
      match attempt(fn () => checkout(0)!) {
        Result::Ok(_) => 0,
        Result::Err(err) => {
          let _ = Shared::update(started, fn _ => 1);
          String::byte_length(err.show())
        }
      }
    });
    while Shared::get(started) == 0 {};
    Fiber::cancel(f);
  })! catch { _ => () };
  print("done");
}
"#;
    expect("raised_cancel_describe", main, "done\n");
}

/// **Context, forgetting the row, and `only`, end to end.** Context prints
/// outermost first and does not change what a `catch` sees; a forgotten
/// failure is still matched by its type; `only` gives back the error itself.
#[test]
fn context_forget_and_only_round_trip() {
    let main = r#"module demo::main;
import std::core::{Opaque, Raised, Result, Show, String, attempt, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(why: String) | Timeout;

fn both(n: Int) -> Int raises Inv + Pay {
  if n == 0 { raise Inv::Out("sku-${n}") };
  if n == 1 { raise Pay::Declined("card") };
  n
}
fn one(n: Int) -> Int raises Pay { if n == 0 { raise Pay::Declined("p") }; n }

fn stored(n: Int) -> Result<Int, Raised<Opaque>> {
  match attempt(fn () => both(n)!) {
    Result::Ok(v) => Result::Ok(v),
    Result::Err(f) => Result::Err(Raised::forget(Raised::context(Raised::context(f, "charging"), "order ${n}"))),
  }
}

fn main() -> () {
  let before = khora_live_count();
  match stored(1) {
    Result::Ok(_) => (),
    Result::Err(g) => {
      print(g.show());
      print(Raised::note(g));
      let tag = (raise g) catch { Pay::Declined(w) => "declined ${w}", other => Raised::type_name(other) };
      print(tag);
    }
  };
  match stored(0) {
    Result::Ok(_) => (),
    Result::Err(g) => {
      let tag = (raise g) catch { Pay::Declined(w) => "declined ${w}", other => Raised::type_name(other) };
      print(tag);
    }
  };
  match attempt(fn () => one(0)!) {
    Result::Ok(_) => (),
    Result::Err(f) => match Raised::only(f) { Pay::Declined(w) => print("only ${w}"), Pay::Timeout => () },
  };
  print("live delta ${khora_live_count() - before}");
}
"#;
    expect(
        "raised_context_forget_only",
        main,
        "order 1: charging: Pay::Declined(card)\norder 1: charging\ndeclined card\nInv\nonly p\nlive delta 0\n",
    );
}

/// **Context survives `raise`** (review 2, `p9d`). A note attached in a
/// `catch` arm and raised on reaches the caller's `attempt`, and a failure
/// with context raised into `(raise f) catch { other => .. }` keeps its note
/// in `other`.
#[test]
fn context_survives_raise() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, String, attempt, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(reason: String) | Timeout;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n == 0 { raise Inv::Out("sku-${n}") };
  raise Pay::Declined("card-${n}")
}

fn sync(n: Int) -> String raises Inv + Pay {
  checkout(n)! catch { err => raise Raised::context(err, "syncing ${n}") }
}

fn outer(n: Int) -> String raises Inv + Pay {
  sync(n)! catch { err => raise Raised::context(err, "batch") }
}

fn round(n: Int) -> String {
  match attempt(fn () => outer(n)!) {
    Result::Ok(v) => v,
    Result::Err(e) => {
      let note = Raised::note(e);
      let tag = (raise e) catch { other => "other ${Raised::note(other)} | ${other.show()}" };
      "${note} => ${tag}"
    }
  }
}

fn main() -> () {
  print(round(0));
  print(round(1));
  let before = khora_live_count();
  let mut i = 0;
  while i < 1000 {
    let _ = String::byte_length(round(i % 2));
    i = i + 1;
  };
  print("live delta ${khora_live_count() - before}");
}
"#;
    expect(
        "raised_context_raise",
        main,
        "batch: syncing 0 => other batch: syncing 0 | batch: syncing 0: Inv::Out(sku-0)\n\
         batch: syncing 1 => other batch: syncing 1 | batch: syncing 1: Pay::Declined(card-1)\n\
         live delta 0\n",
    );
}

/// **A named arm matches the error inside a failure that carries context.**
/// The failure travels in its box; the `catch` opens it, and the arm sees the
/// `Pay` it names.
#[test]
fn a_named_arm_sees_through_context() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, String, attempt, print};

derive(Show)
pub type Pay = | Declined(reason: String) | Timeout;
derive(Show)
pub type Inv = | Out | Offline;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n == 0 { raise Inv::Out };
  raise Pay::Declined("card-${n}")
}

fn sync(n: Int) -> String raises Inv + Pay {
  checkout(n)! catch { err => raise Raised::context(err, "syncing ${n}") }
}

fn main() -> () {
  let s = sync(1)! catch {
    Pay::Declined(r) => "declined ${r}",
    Pay::Timeout => "timeout",
    Inv::Out => "out",
    Inv::Offline => "offline",
  };
  print(s);
}
"#;
    expect("raised_named_through_context", main, "declined card-1\n");
}

/// **A case no named arm takes reaches the catch-all with its context**
/// (review 3, must-fix 2). The named arms read the error inside the box
/// without taking it, so `Pay::Timeout`, which only `other` takes, arrives
/// there whole: the note intact and nothing freed under it.
#[test]
fn a_noted_failure_falls_through_named_arms_whole() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, String, attempt, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Pay = | Declined(reason: String) | Timeout(after: String);
derive(Show)
pub type Inv = | Out(sku: String) | Offline;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n % 3 == 0 { raise Inv::Out("sku-${n}") };
  if n % 3 == 1 { raise Pay::Timeout("t-${n}") };
  raise Pay::Declined("card-${n}")
}

fn sync(n: Int) -> String raises Inv + Pay {
  checkout(n)! catch { err => raise Raised::context(err, "syncing ${n}") }
}

fn round(n: Int) -> String {
  sync(n)! catch {
    Pay::Declined(r) => "declined ${r}",
    Inv::Out(s) => "out ${s}",
    other => "other ${other.show()}",
  }
}

fn main() -> () {
  print(round(0));
  print(round(1));
  print(round(2));
  let before = khora_live_count();
  let mut i = 0;
  while i < 900 {
    let _ = String::byte_length(round(i));
    i = i + 1;
  };
  print("live delta ${khora_live_count() - before}");
}
"#;
    expect(
        "raised_noted_fallthrough",
        main,
        "out sku-0\nother syncing 1: Pay::Timeout(t-1)\ndeclined card-2\nlive delta 0\n",
    );
}

/// **A guarded binding over a row that turns a failure away leaves it for the
/// next arm** (review 3). The binding holds a copy of its own, released on
/// the turned-away edge, so the arm after it sees the failure as it came,
/// note and all, and nothing leaks either way.
#[test]
fn a_guarded_binding_hands_the_failure_on() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, String, attempt, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Pay = | Declined(reason: String) | Timeout;
derive(Show)
pub type Inv = | Out(sku: String) | Offline;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n % 2 == 0 { raise Inv::Out("sku-${n}") };
  raise Pay::Declined("card-${n}")
}

fn sync(n: Int) -> String raises Inv + Pay {
  if n % 4 < 2 { checkout(n)! } else { checkout(n)! catch { err => raise Raised::context(err, "at ${n}") } }
}

fn round(n: Int) -> String {
  sync(n)! catch {
    e if Raised::type_name(e) == "Inv" => "inv ${e.show()}",
    rest => "rest ${rest.show()}",
  }
}

fn main() -> () {
  print(round(0));
  print(round(1));
  print(round(2));
  print(round(3));
  let before = khora_live_count();
  let mut i = 0;
  while i < 1000 {
    let _ = String::byte_length(round(i));
    i = i + 1;
  };
  print("live delta ${khora_live_count() - before}");
}
"#;
    expect(
        "raised_guarded_binding",
        main,
        "inv Inv::Out(sku-0)\nrest Pay::Declined(card-1)\ninv at 2: Inv::Out(sku-2)\nrest at 3: Pay::Declined(card-3)\n\
         live delta 0\n",
    );
}

/// **`with_note`'s note is an operand like any other** (review 2, `p9c`,
/// `p9d`). A note that raises releases the failure on the way out.
#[test]
fn with_note_holds_the_failure_while_the_note_runs() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, String, attempt, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(reason: String) | Timeout;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n % 2 == 0 { raise Inv::Out("sku-${n}") };
  raise Pay::Declined("card-${n}")
}

fn failing_note(n: Int) -> String raises Pay { raise Pay::Declined("note-${n}") }

fn main() -> () {
  let before = khora_live_count();
  let mut i = 0;
  while i < 1000 {
    match attempt(fn () => checkout(i)!) {
      Result::Ok(_) => (),
      Result::Err(err) => {
        let _ = Raised::with_note(err, failing_note(i)!) catch { _ => err };
      }
    };
    i = i + 1;
  };
  print("live delta ${khora_live_count() - before}");
}
"#;
    expect("raised_with_note_operands", main, "live delta 0\n");
}

/// **A `with_note` whose note diverges builds** (review 2, `p9c`): a release
/// emitted after the block's terminator failed LLVM verification.
#[test]
fn with_note_with_a_diverging_note_builds() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, Show, String, attempt, print};

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(reason: String) | Timeout;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n == 0 { raise Inv::Out("sku-1") };
  raise Pay::Declined("card")
}

fn early(err: Raised<Inv + Pay>) -> String {
  let annotated = Raised::with_note(err, return "early");
  annotated.show()
}

fn main() -> () {
  match attempt(fn () => checkout(0)!) {
    Result::Ok(v) => print(v),
    Result::Err(err) => print(early(err)),
  };
}
"#;
    expect("raised_with_note_diverges", main, "early\n");
}

/// **Every path a failure with context can take releases it once** (review
/// 2, leak mutants): dropped by `_`, caught by named arms, kept through
/// `attempt`, and `only` on a unique box and on a shared one.
#[test]
fn every_path_of_a_noted_failure_releases_it() {
    let main = r#"module demo::main;
import std::core::{Opaque, Raised, Result, Show, String, attempt, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Inv = | Out(sku: String) | Offline;
derive(Show)
pub type Pay = | Declined(why: String) | Timeout;

fn both(n: Int) -> Int raises Inv + Pay {
  if n % 2 == 0 { raise Inv::Out("sku-${n}") };
  raise Pay::Declined("card-${n}")
}

fn one(n: Int) -> Int raises Pay { raise Pay::Declined("p-${n}") }

fn noted(n: Int) -> Int raises Inv + Pay {
  both(n)! catch { err => raise Raised::context(err, "at ${n}") }
}

fn round(n: Int) -> Int {
  let a = noted(n)! catch { _ => 1 };
  let b = noted(n)! catch {
    Inv::Out(s) => String::byte_length(s),
    Inv::Offline => 0,
    Pay::Declined(w) => String::byte_length(w),
    Pay::Timeout => 0,
  };
  let c = match attempt(fn () => noted(n)!) {
    Result::Ok(v) => v,
    Result::Err(f) => String::byte_length(Raised::note(Raised::forget(f))),
  };
  let d = match attempt(fn () => one(n)!) {
    Result::Ok(v) => v,
    Result::Err(f) => {
      let kept = f;
      let e1 = match Raised::only(f) { Pay::Declined(w) => String::byte_length(w), Pay::Timeout => 0 };
      e1 + String::byte_length(Raised::type_name(kept))
    }
  };
  a + b + c + d
}

fn main() -> () {
  let before = khora_live_count();
  let mut i = 0;
  let mut total = 0;
  while i < 2000 {
    total = total + round(i);
    i = i + 1;
  };
  print("live delta ${khora_live_count() - before}");
}
"#;
    expect("raised_noted_paths", main, "live delta 0\n");
}

/// **A failure two joiners hold is copied out of, not moved** (review 3,
/// must-fix 3). The fiber fails with context, so the `Raised` box itself
/// crosses the join and both joiners hold it. Each joiner's `only` copies the
/// error out while its own reference keeps the box alive, and lets go after.
///
/// What this pins is the shared path: moving the error out of a box someone
/// else holds releases it twice, and the count or the process says so. The
/// ordering inside it (copy, then let go) closes a window that needs the
/// fiber's own reference gone while two joiners are mid-copy, which a test
/// cannot schedule.
#[test]
fn only_on_a_failure_joined_twice_copies() {
    let main = r#"module demo::main;
import std::core::{Fiber, Raised, Result, Show, String, attempt, nursery, print};

extern fn khora_live_count() -> Int;

derive(Show)
pub type Pay = | Declined(why: String) | Timeout;

fn fail(n: Int) -> Int raises Pay { raise Pay::Declined("card-${n}") }

fn noted(n: Int) -> Int raises Pay {
  match attempt(fn () => fail(n)!) {
    Result::Ok(v) => v,
    Result::Err(e) => raise Raised::context(e, "at ${n}"),
  }
}

fn read(f: Fiber<Int, { Pay: Pay }>) -> Int {
  match attempt(fn () => Fiber::join(f)!) {
    Result::Ok(v) => v,
    Result::Err(e) => match Raised::only(e) { Pay::Declined(w) => String::byte_length(w), Pay::Timeout => 0 },
  }
}

fn main() -> () {
  let before = khora_live_count();
  let mut i = 0;
  let mut total = 0;
  while i < 300 {
    let got = nursery(fn () => {
      let f = Fiber::spawn(fn () => noted(i)!);
      let a = Fiber::spawn(fn () => read(f));
      let b = Fiber::spawn(fn () => read(f));
      Fiber::join(a)! + Fiber::join(b)!
    })! catch { _ => 0 };
    total = total + got;
    i = i + 1;
  };
  print("total ${total}");
  print("live delta ${khora_live_count() - before}");
}
"#;
    // "card-N" is 6, 7 or 8 bytes for N < 300, read twice per round.
    let expected_total: usize = (0..300).map(|n| 2 * format!("card-{n}").len()).sum();
    expect("raised_only_joined_twice", main, &format!("total {expected_total}\nlive delta 0\n"));
}

/// **A failure with context that escapes `main` is named by its type.** Its
/// tag is reserved, and an entry point that did not open the box printed
/// nothing for it.
#[test]
fn a_noted_failure_escaping_main_is_named() {
    let main = r#"module demo::main;
import std::core::{Raised, Show, String, print};

derive(Show)
pub type Pay = | Declined(reason: String) | Timeout;
derive(Show)
pub type Inv = | Out | Offline;

fn checkout(n: Int) -> String raises Inv + Pay {
  if n == 0 { raise Inv::Out };
  raise Pay::Declined("card")
}

fn main() -> () raises Inv + Pay {
  let s = checkout(1)! catch { err => raise Raised::context(err, "syncing") };
  print(s);
}
"#;
    for (fibers, code, out) in run_program("raised_noted_escape", main) {
        assert_ne!(code, Some(0), "{fibers}: {out}");
        assert!(out.contains("Pay"), "{fibers}: the escaped failure is named by its type: {out}");
    }
}

/// **A guard that leaves the `catch` releases what the dispatch holds**
/// (review probes `p_wildcard_guard_unwind_leak`, `p_wgl_rai`, `p_wgl_ret`).
/// A guard runs before its arm commits, so the failure is nobody's but the
/// dispatch's while it does. A guard on `_` that raises through `!` leaked
/// the error, 2 objects a round (pre-existing on main 48c5be3); one that
/// `return`s while a noted `Raised` was matched leaked the box, its note and
/// the error, 4 a round. Guarded binding and named arms are the controls.
#[test]
fn a_guard_that_leaves_the_catch_releases_the_failure() {
    let main = r#"module demo::main;
import std::core::{Raised, Result, attempt, print};

extern fn khora_live_count() -> Int;

type ErrA = | One(s: String);
type GuardErr = | Stop(s: String);

fn guard_fail(i: Int) -> Bool raises GuardErr { raise GuardErr::Stop("g-${i}") }

// A plain error, a `_` guard that raises out of the `catch`.
fn raise_wild(i: Int) -> Int {
  ((raise ErrA::One("payload-${i}")) catch {
    _ if guard_fail(i)! => 1,
    _ => 2,
  }) catch { GuardErr::Stop(_) => 9, _ => 0 }
}

// The same through a guarded binding and a guarded named arm.
fn raise_bound(i: Int) -> Int {
  ((raise ErrA::One("payload-${i}")) catch {
    e if guard_fail(i)! => 1,
    _ => 2,
  }) catch { GuardErr::Stop(_) => 9, _ => 0 }
}
fn raise_named(i: Int) -> Int {
  ((raise ErrA::One("payload-${i}")) catch {
    ErrA::One(s) if guard_fail(i)! => 1,
    _ => 2,
  }) catch { GuardErr::Stop(_) => 9, _ => 0 }
}

// A failure with context, a `_` guard that returns out of the function.
fn return_wild(i: Int) -> Int {
  let r: Result<Int, Raised<ErrA>> = attempt(fn () => raise ErrA::One("payload-${i}"));
  match r {
    Result::Ok(v) => v,
    Result::Err(f) => {
      let noted = Raised::context(f, "note-${i}");
      (raise noted) catch {
        _ if { if i >= 0 { return 7 }; false } => 1,
        _ => 2,
      }
    },
  }
}

fn rounds(which: Int) -> Int {
  let mut i = 0;
  let mut acc = 0;
  while i < 1000 {
    acc = acc + if which == 0 { raise_wild(i) } else if which == 1 { raise_bound(i) }
      else if which == 2 { raise_named(i) } else { return_wild(i) };
    i = i + 1;
  };
  acc
}

fn main() -> () {
  let mut w = 0;
  while w < 4 {
    let _warm = rounds(w);
    let before = khora_live_count();
    let total = rounds(w);
    let delta = khora_live_count() - before;
    print("shape ${w} total ${total} delta ${delta}");
    w = w + 1;
  };
}
"#;
    expect(
        "raised_guard_leaves",
        main,
        "shape 0 total 9000 delta 0\nshape 1 total 9000 delta 0\nshape 2 total 9000 delta 0\nshape 3 total 7000 delta 0\n",
    );
}

/// **A failure with context that escapes `main` says what it was and where
/// it came from** (second review, `top1`). Its type's name alone dropped the
/// variant and the context, the thing `context` is attached for, at the
/// place an operator reads. The `show()` line follows the name line, so a
/// failure without context, and a program that never attaches one, print
/// what they did.
#[test]
fn a_noted_failure_that_escapes_main_prints_its_context() {
    let program = "module main;
import std::core::{Raised, Result, Show, String, attempt, print};

derive(Show)
pub type PayError = | Declined(reason: String) | GatewayDown(attempt: Int);

derive(Show)
pub type ShipError = | NoAddress | Embargo(country: String);

fn step(n: Int) -> Int raises PayError + ShipError {
  if n == 0 { raise ShipError::Embargo(\"XX\") } else { raise PayError::Declined(\"funds\") }
}

fn order(id: Int) -> Int raises PayError + ShipError {
  match attempt(fn () => step(0)!) {
    Result::Ok(v) => v,
    Result::Err(e) => raise Raised::context(Raised::context(e, \"shipping order ${id}\"), \"processing batch 3\"),
  }
}

fn main() -> () raises PayError + ShipError {
  print(\"starting\");
  order(42)!;
  print(\"unreachable\")
}
";
    for (fibers, code, out) in run_program("escape_noted", program) {
        assert_eq!(code, Some(1), "{fibers}: {out}");
        assert!(out.contains("`ShipError` reached the entry point"), "{fibers}: {out}");
        assert!(
            out.contains("processing batch 3: shipping order 42: ShipError::Embargo(XX)"),
            "{fibers}: the context and the variant are missing: {out}"
        );
    }
}

/// **A `Raised` is refused at a `Handoff` when the program is built**, the
/// path `khora build` takes (second review, `e4`, `e9`). Before, a hand-off
/// walked the box, found no fields, and passed the error and note through
/// still counted by the sender: the debug owner check trapped (134), and a
/// release build let both fibers write one `mut` record.
#[test]
fn a_raised_through_a_handoff_is_refused_at_build() {
    let program = "module main;
import std::core::{Fiber, Raised, Result, Option, Handoff, String, attempt, print};

pub type Cell = { mut n: Int };
pub type Ctr = | C(cell: Cell);

fn raise_it(c: Cell) -> Int raises Ctr { raise Ctr::C(c) }

fn reader(h: Handoff<Raised<Ctr>>) -> Int {
  match Handoff::receive(h) {
    Option::None => 0,
    Option::Some(e) => match Raised::only(e) { Ctr::C(c) => { c.n = c.n + 1000; c.n } },
  }
}

fn main() -> () {
  let cell: Cell = { n: 1 };
  let h: Handoff<Raised<Ctr>> = Handoff::bounded(1);
  let f = Fiber::spawn(fn () => reader(h));
  match attempt(fn () => raise_it(cell)!) {
    Result::Ok(_) => (),
    Result::Err(e) => { Handoff::send(h, e); () },
  };
  cell.n = cell.n + 1;
  print(\"receiver saw ${Fiber::join(f)}, sender sees ${cell.n}\");
}
";
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("handoff_raised");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, program));
    let errors = match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => {
            let ran = Command::new(&exe).output().expect("the program should run");
            panic!(
                "a `Raised` crossed a `Handoff`: exit {:?}, {}{}",
                ran.status.code(),
                String::from_utf8_lossy(&ran.stdout),
                String::from_utf8_lossy(&ran.stderr)
            );
        }
        Err(errors) => errors,
    };
    assert!(
        errors.iter().any(|e| e.message.contains("a `Raised` stays on the fiber that caught it")),
        "{:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
