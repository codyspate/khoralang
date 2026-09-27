#![cfg(feature = "llvm")]

//! A closure chosen by a branching expression, called afterwards.
//!
//! **What this guards: a join of function type that joined nothing.** An
//! `if`, `match`, `loop`/`break` or `catch` whose value is a closure moved
//! the chosen closure out of its variable and then read the join back as a
//! null pointer. Calling it was a segmentation fault; a function returning
//! it as its tail was refused as a body with "no value"; not calling it
//! leaked the closure. Effect rows had nothing to do with it -- two closures
//! that cannot fail crashed the same way -- although that is how it was
//! found, so the reported programs are here as they were.
//!
//! Each program runs with both choices, so the closure that raises and the
//! one that does not are both called, and ends by printing the live count:
//! a closure moved out and never stored anywhere shows there even when
//! nothing calls it.
//!
//! A separate binary, because it builds every program with `KHORA_UNBOXED`
//! both ways and the environment belongs to the process. See
//! `tests/suite.rs`.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

impl String {
  fn byte_length(self) -> Int;
}

pub type Nf = { p: String };
pub type List<A> = | Cons(head: A, tail: List<A>) | Nil;

fn nf() -> Int raises Nf { raise { p: \"x\" + \"1\" } }
fn five(e: Nf) -> Int raises Nf { 5 }
fn bad(e: Nf) -> Int raises Nf { nf()! }
";

/// Calls `work(true)` and `work(false)`, then prints the live count.
const MAIN: &str = "
fn main() -> Int {
  print(work(true)! catch { Nf { p } => 100 + String::byte_length(p) });
  print(work(false)! catch { Nf { p } => 100 + String::byte_length(p) });
  print(khora_live_count());
  0
}
";

/// The tests here set process-wide state while they compile, so they take
/// turns.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Compiles `source` with `KHORA_UNBOXED` set to `unboxed`.
fn build(name: &str, unboxed: &str, source: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}_unboxed_{unboxed}"));
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the variable is process-wide, and every test in this binary
    // holds `ONE_AT_A_TIME` for as long as it is set, so no other test reads
    // it half-way. No other thread in this binary touches the environment.
    unsafe { std::env::set_var("KHORA_UNBOXED", unboxed) };
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    let outcome = khora_codegen_llvm::compile(&db, root, &exe);
    // SAFETY: as above; the lock is still held.
    unsafe { std::env::remove_var("KHORA_UNBOXED") };
    if let Err(errors) = outcome {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// Builds `work` with both layouts, runs each on both fiber backends, and
/// requires `expected` from all four.
fn all_four(name: &str, work: &str, expected: &str) {
    let source = format!("{PRELUDE}{work}\n{MAIN}");
    for unboxed in ["1", "0"] {
        let exe = build(name, unboxed, &source);
        for backend in ["threads", "scheduler"] {
            let out = Command::new(&exe)
                .env("KHORA_FIBERS", backend)
                .output()
                .expect("the program should run");
            let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
            assert_eq!(
                (stdout.as_str(), out.status.code()),
                (expected, Some(0)),
                "`{name}`, `KHORA_UNBOXED={unboxed}`, `{backend}`; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

// --- the reported programs -------------------------------------------------

/// `if_typed_ctrl_ret`: one closure cannot fail, the other raises.
#[test]
fn an_if_between_a_closure_that_raises_and_one_that_cannot() {
    all_four(
        "join_if_mixed",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => 5; \
         let k2 = fn (e: Nf) => nf()!; let k = if c { k1 } else { k2 }; k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// `if_typed_ctrl`: one closure re-raises its argument, the other raises
/// its own.
#[test]
fn an_if_between_two_closures_that_raise_different_values() {
    all_four(
        "join_if_ctrl",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => { raise e }; \
         let k2 = fn (e: Nf) => nf()!; let k = if c { k1 } else { k2 }; k({ p: \"abc\" })! }",
        "103\n102\n0\n",
    );
}

/// `if_typed_raise_both`: both closures raise the same way.
#[test]
fn an_if_between_two_closures_that_both_raise() {
    all_four(
        "join_if_both",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => { raise e }; \
         let k2 = fn (e: Nf) => { raise e }; let k = if c { k1 } else { k2 }; k({ p: \"abc\" })! }",
        "103\n103\n0\n",
    );
}

/// Neither closure can fail: the crash never needed a `raises` row.
#[test]
fn an_if_between_two_closures_that_cannot_fail() {
    all_four(
        "join_if_neither",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (x: Int) => x + 4; \
         let k2 = fn (x: Int) => x + 101; let k = if c { k1 } else { k2 }; k(1) }",
        "5\n102\n0\n",
    );
}

// --- every other way two closures meet -------------------------------------

#[test]
fn a_match_choosing_a_closure() {
    all_four(
        "join_match",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => 5; \
         let k2 = fn (e: Nf) => nf()!; let k = match c { true => k1, false => k2 }; \
         k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

#[test]
fn a_loop_breaking_with_a_closure() {
    all_four(
        "join_loop",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => 5; \
         let k2 = fn (e: Nf) => nf()!; \
         let k = loop { if c { break k1 } else { break k2 } }; k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// The value of a `catch`: the body's closure on one path, an arm's on the
/// other.
#[test]
fn a_catch_whose_value_is_a_closure() {
    all_four(
        "join_catch",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => 5; \
         let k2 = fn (e: Nf) => nf()!; \
         let k = (if c { k1 } else { nf()!; k1 }) catch { Nf { p } => k2 }; \
         k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// Two elements of one list, chosen between after the list is taken apart.
#[test]
fn a_closure_chosen_from_a_list() {
    all_four(
        "join_list",
        "fn work(c: Bool) -> Int raises Nf { let k1 = fn (e: Nf) => 5; \
         let k2 = fn (e: Nf) => nf()!; let l = List::Cons(k1, List::Cons(k2, List::Nil)); \
         let k = match l { List::Cons(a, List::Cons(b, _)) => if c { a } else { b }, _ => k1 }; \
         k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// Two fields of one record.
#[test]
fn a_closure_chosen_from_a_record() {
    all_four(
        "join_record",
        "type Two = { a: (Nf) -> Int raises Nf, b: (Nf) -> Int raises Nf };\n\
         fn work(c: Bool) -> Int raises Nf { \
         let r: Two = { a: fn (e: Nf) => 5, b: fn (e: Nf) => nf()! }; \
         let k = if c { r.a } else { r.b }; k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// A function whose tail is the join. This one was refused at build time
/// rather than crashing, because the joined value was not a pointer.
#[test]
fn a_function_returning_a_closure_chosen_by_if() {
    all_four(
        "join_return",
        "fn pick(c: Bool) -> (Nf) -> Int raises Nf { \
         if c { fn (e: Nf) => 5 } else { fn (e: Nf) => nf()! } }\n\
         fn work(c: Bool) -> Int raises Nf { let k = pick(c); k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// Named functions used as values go through the same join.
#[test]
fn an_if_between_two_named_functions() {
    all_four(
        "join_named",
        "fn work(c: Bool) -> Int raises Nf { let k = if c { five } else { bad }; \
         k({ p: \"abc\" })! }",
        "5\n102\n0\n",
    );
}

/// Never called: the chosen closure still has to be released.
#[test]
fn a_closure_chosen_and_never_called_is_released() {
    all_four(
        "join_uncalled",
        "fn work(c: Bool) -> Int raises Nf { let s = \"he\" + \"llo\"; \
         let k1 = fn (x: Int) => x + String::byte_length(s); let k2 = fn (x: Int) => x; \
         let k = if c { k1 } else { k2 }; if c { 5 } else { 102 } }",
        "5\n102\n0\n",
    );
}


/// A join of function type that is never bound: discarded, called on the
/// spot, or passed straight on. With no slot these crashed the compiler
/// ("expected PointerValue") or built invalid IR, rather than the program.
#[test]
fn a_closure_join_used_without_a_binding() {
    all_four(
        "join_unbound",
        "fn apply(k: (Int) -> Int, x: Int) -> Int { k(x) }\n\
         fn work(c: Bool) -> Int raises Nf { let s = \"he\" + \"llo\"; \
         let k1 = fn (x: Int) => x + String::byte_length(s); let k2 = fn (x: Int) => x + 97; \
         let k3 = fn (x: Int) => x + String::byte_length(s); let k4 = fn (x: Int) => x; \
         if c { k3 } else { k4 }; \
         (if c { k1 } else { k2 })(0) + apply(if c { fn (x: Int) => x } else { fn (x: Int) => x - 5 }, 0) }",
        "5\n92\n0\n",
    );
}
