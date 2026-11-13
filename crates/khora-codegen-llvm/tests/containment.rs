#![cfg(feature = "llvm")]

//! `std::core::contain`, end to end: what a contained trap leaves behind,
//! and what it does not contain.
//!
//! **What this guards: a contained trap that is not clean.** `contain` lets a
//! fiber carry on after a bug, which is only sound if the trap left the
//! process as a cancellation would have: every reference the abandoned
//! frames held released, every region's finalizer run once, every `Shared`
//! cell as it was. A leak here is one per bad request on a server, so the
//! live-object count is asserted to come back to where it started across
//! thousands of contained traps of every kind. The cases `contain` must
//! *not* contain -- a trap with no `contain` of its own, one in a finalizer,
//! running out of stack, the runtime's own fatal errors, a spent budget --
//! are asserted to end the process with their usual status.
//!
//! Every program runs on both fiber backends: the unwind runs finalizers,
//! which park and resume on the scheduler, and the fiber's `contain` depth
//! and trap sentence must follow it to whichever worker it lands on.

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

/// Compiles `main` against `std` once, into a directory of its own.
fn build(name: &str, main: &str) -> PathBuf {
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
    exe
}

/// What one run said: its status, its stdout and its stderr.
struct Ran {
    fibers: &'static str,
    code: Option<i32>,
    out: String,
    err: String,
}

/// Runs `exe` with `args` on both fiber backends, with `env` set.
fn run(exe: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Vec<Ran> {
    ["threads", "scheduler"]
        .into_iter()
        .map(|fibers| {
            let out = Command::new(exe)
                .args(args)
                .env("KHORA_FIBERS", fibers)
                .envs(env.iter().copied())
                .output()
                .expect("the program should run");
            Ran {
                fibers,
                code: out.status.code(),
                out: String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
                err: String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
            }
        })
        .collect()
}

/// The kinds of trap a program can hit, each in a function that holds heap
/// values when it traps. Shared by the tests below.
const KINDS: &str = r#"
extern fn khora_live_count() -> Int;

type Box = { name: String, items: List<String> };

fn big() -> Int { 9223372036854775807 }

fn overflow_with_live_values(n: Int) -> Int {
  let a = "held ${Int::to_string(n)}";
  let b: Box = { name: a, items: List::Cons("x", List::Cons(a, List::Nil)) };
  let c = big() + n;
  String::byte_length(b.name) + c
}

fn index_with_live_values(n: Int) -> Int {
  let xs: Array<String> = Array::new(3, "s ${Int::to_string(n)}");
  let label = "label ${Int::to_string(n)}";
  String::byte_length(label) + String::byte_length(Array::get(xs, n + 10))
}

fn set_out_of_range(n: Int) -> Int {
  let xs: Array<String> = Array::new(3, "s");
  Array::set(xs, n + 10, "v ${Int::to_string(n)}");
  0
}

fn decimal_overflow(n: Int) -> Int {
  let huge = Decimal::of_string("99999999999999999999999999999999999999");
  let keep = "keep ${Int::to_string(n)}";
  let sum = match huge {
    Option::Some(h) => Decimal::add(h, h),
    Option::None => Decimal::zero(),
  };
  String::byte_length(keep) + Decimal::truncated(sum)
}

fn unwritten(n: Int) -> Int {
  let s = "todo ${Int::to_string(n)}";
  let r: Int = todo();
  String::byte_length(s) + r
}

fn divide_by_zero(n: Int) -> Int {
  let s = "div ${Int::to_string(n)}";
  String::byte_length(s) / (n - n)
}

fn negated(n: Int) -> Int {
  let s = "neg ${Int::to_string(n)}";
  let m = 0 - big() - 1 + String::byte_length(s) - String::byte_length(s);
  -m
}

fn std_bounds(n: Int) -> Int {
  let source: Array<U8> = Array::new(4, 0);
  let target: Array<U8> = Array::new(4, 0);
  Array::copy_into(source, 0, target, 2, 3 + n);
  0
}

fn bad_utf8(n: Int) -> Int {
  let bytes: Array<U8> = Array::new(2, 255);
  String::byte_length(String::from_bytes(bytes)) + n
}

fn byte_out_of_range(n: Int) -> Int {
  let s = "byte ${Int::to_string(n)}";
  U8::to_int(String::byte(s, n + 100))
}

fn sliced_through_a_character(n: Int) -> Int {
  let s = "é and ${Int::to_string(n)}";
  String::byte_length(String::slice(s, 1, 4))
}

fn narrowing(n: Int) -> Int {
  let wide = 300 + n;
  let small: U8 = U8::of(wide);
  U8::to_int(small)
}

fn show(r: Result<Int, Trapped>) -> String {
  match r {
    Result::Ok(v) => "ok ${Int::to_string(v)}",
    Result::Err(t) => "trapped: ${t.message}",
  }
}

fn count(r: Result<Int, Trapped>) -> Int {
  match r {
    Result::Ok(_) => 0,
    Result::Err(_) => 1,
  }
}
"#;

const IMPORTS: &str = "module demo::main;
import std::core::{Array, Channel, Claim, Fiber, List, Option, Raised, Region, Result, Scope, Select, \
Selected, Shared, SharedFn, Trapped, acquire, attempt, contain, print, race, scoped, timeout, todo};
import std::decimal::{Decimal};
import std::env::{Env};
";

/// The program: [`KINDS`], and a `main` that runs the case named by its first
/// argument.
fn program(cases: &str) -> String {
    format!(
        "{IMPORTS}{KINDS}{cases}
fn which() -> String {{
  with {{ env: Env::real() }} {{
    match env.arguments() {{
      List::Cons(_, List::Cons(w, _)) => w,
      _ => \"\",
    }}
  }}
}}
"
    )
}

/// **Each kind of trap is contained, and says what it was.** One line per
/// kind, in a fixed order, on both backends, with the sentence the fatal path
/// would have printed.
#[test]
fn each_kind_of_trap_is_contained_with_its_sentence() {
    let main = program(
        r#"
pub fn main() -> Int {
  print(show(contain(fn () => overflow_with_live_values(1))));
  print(show(contain(fn () => index_with_live_values(1))));
  print(show(contain(fn () => set_out_of_range(1))));
  print(show(contain(fn () => decimal_overflow(1))));
  print(show(contain(fn () => unwritten(1))));
  print(show(contain(fn () => divide_by_zero(1))));
  print(show(contain(fn () => negated(1))));
  print(show(contain(fn () => std_bounds(1))));
  print(show(contain(fn () => bad_utf8(1))));
  print(show(contain(fn () => byte_out_of_range(1))));
  print(show(contain(fn () => sliced_through_a_character(1))));
  print(show(contain(fn () => narrowing(1))));
  print(show(contain(fn () => 21 * 2)));
  0
}
"#,
    );
    let exe = build("containment_kinds", &main);
    let expected = "trapped: Int addition overflowed
trapped: index 11 is outside an array of 3
trapped: index 11 is outside an array of 3
trapped: Decimal addition overflowed
trapped: this is not written yet
trapped: Int division by zero
trapped: Int negation overflowed
trapped: index 5 is outside an array of 4
trapped: these bytes are not UTF-8, so they are not a String
trapped: index 101 is outside an array of 6
trapped: this slice cuts a character in half, so it is not a String
trapped: this Int is outside the range of U8
ok 42
";
    for ran in run(&exe, &[], &[]) {
        assert_eq!(ran.code, Some(0), "{}: {}{}", ran.fibers, ran.out, ran.err);
        assert_eq!(ran.out, expected, "{}", ran.fibers);
        assert_eq!(
            ran.err.matches("(contained; the fiber continues)").count(),
            12,
            "{}: each contained trap is reported once on stderr: {}",
            ran.fibers,
            ran.err
        );
    }
}

/// **Nothing leaks.** Thousands of contained traps of every kind, each with
/// heap values live in the frames it abandons, and the live-object count is
/// where it started. The leak a trap exit that released nothing would make is
/// several objects per trap, so this needs no tolerance.
#[test]
fn contained_traps_leak_nothing() {
    let main = program(
        r#"
pub fn main() -> Int {
  let cell = Shared::of(5);
  let mut i = 0;
  while i < 20 {
    let _ = contain(fn () => overflow_with_live_values(i));
    let _ = contain(fn () => decimal_overflow(i));
    i = i + 1;
  };
  let before = khora_live_count();
  let mut trapped = 0;
  let mut j = 0;
  while j < 1000 {
    trapped = trapped + count(contain(fn () => overflow_with_live_values(j)))
      + count(contain(fn () => index_with_live_values(j)))
      + count(contain(fn () => set_out_of_range(j)))
      + count(contain(fn () => decimal_overflow(j)))
      + count(contain(fn () => unwritten(j)))
      + count(contain(fn () => Shared::update(cell, fn k => k + big())))
      + count(contain(fn () => divide_by_zero(j)))
      + count(contain(fn () => negated(j)))
      + count(contain(fn () => std_bounds(j)))
      + count(contain(fn () => bad_utf8(j)))
      + count(contain(fn () => byte_out_of_range(j)))
      + count(contain(fn () => sliced_through_a_character(j)))
      + count(contain(fn () => narrowing(j)));
    j = j + 1;
  };
  let after = khora_live_count();
  print("contained ${Int::to_string(trapped)}; live ${Int::to_string(after - before)}");
  0
}
"#,
    );
    let exe = build("containment_leaks", &main);
    for ran in run(&exe, &[], &[("KHORA_TRAP_BUDGET", "100000")]) {
        assert_eq!(ran.code, Some(0), "{}: {}{}", ran.fibers, ran.out, &ran.err[..ran.err.len().min(400)]);
        assert_eq!(ran.out, "contained 13000; live 0\n", "{}", ran.fibers);
    }
}

/// **A finalizer runs once, and a `Shared` cell is as it was.** A region
/// opened in a frame the trap abandons runs its finalizer on the way out,
/// exactly once; a `Shared::update` or `modify` whose change function traps
/// leaves the cell's old value, boxed or not, and its lock free.
#[test]
fn finalizers_run_once_and_cells_keep_their_value() {
    let main = program(
        r#"
type Account = { owner: String, cents: Int };

fn with_finalizer(n: Int, ran: Shared<Int>) -> Int {
  let region = Region::open();
  Region::defer(region, fn () => { Shared::update(ran, fn k => k + 1); () });
  big() + n
}

pub fn main() -> Int {
  let ran = Shared::of(0);
  print(show(contain(fn () => with_finalizer(1, ran))));
  print("finalizer ran ${Int::to_string(Shared::get(ran))} time(s)");

  let cell = Shared::of(5);
  print(show(contain(fn () => Shared::update(cell, fn k => k + big()))));
  print("int cell ${Int::to_string(Shared::update(cell, fn k => k + 1))}");

  let account = Shared::of({ owner: "ada", cents: 100 });
  let r = contain(fn () => Shared::update(account, fn a => { owner: "mallory", cents: a.cents + big() }));
  print("update: ${show(match r { Result::Ok(a) => Result::Ok(a.cents), Result::Err(t) => Result::Err(t) })}");
  let now = Shared::get(account);
  print("boxed cell after update ${now.owner} ${Int::to_string(now.cents)}");

  let m = contain(fn () => Shared::modify(account, fn a => { state: { owner: "eve", cents: a.cents }, result: a.cents + big() }));
  print("modify: ${show(m)}");
  let then = Shared::get(account);
  print("boxed cell after modify ${then.owner} ${Int::to_string(then.cents)}");
  print("and it can be updated ${Int::to_string(Shared::update(account, fn a => { owner: a.owner, cents: a.cents + 1 }).cents)}");
  0
}
"#,
    );
    let exe = build("containment_cleanup", &main);
    let expected = "trapped: Int addition overflowed
finalizer ran 1 time(s)
trapped: Int addition overflowed
int cell 6
update: trapped: Int addition overflowed
boxed cell after update ada 100
modify: trapped: Int addition overflowed
boxed cell after modify ada 100
and it can be updated 101
";
    for ran in run(&exe, &[], &[]) {
        assert_eq!(ran.code, Some(0), "{}: {}{}", ran.fibers, ran.out, ran.err);
        assert_eq!(ran.out, expected, "{}", ran.fibers);
    }
}

/// **The nearest `contain` takes the trap, and an error or a cancellation is
/// not one.** Nested, the inner one answers `Err` and the outer one sees a
/// value. `attempt` and a `catch _` inside a `contain` do not swallow a trap:
/// it passes through them. A raise comes out of `contain` as itself.
#[test]
fn the_nearest_contain_takes_a_trap_and_nothing_else_does() {
    let main = program(
        r#"
type Oops = | Oops;

fn maybe_fails(flag: Bool) -> Int raises Oops {
  if flag { raise Oops::Oops } else { big() + 1 }
}

pub fn main() -> Int raises {} {
  let nested = contain(fn () => count(contain(fn () => overflow_with_live_values(1))) + 100);
  print("nested: ${show(nested)}");

  let through = contain(fn () => {
    let inner = attempt(fn () => maybe_fails(false)!);
    match inner {
      Result::Ok(v) => v,
      Result::Err(_) => -1,
    }
  });
  print("attempt: ${show(through)}");

  let caught = contain(fn () => maybe_fails(false)! catch { _ => -2 });
  print("catch: ${show(caught)}");

  let raised = contain(fn () => maybe_fails(true)!)! catch { Oops::Oops => Result::Ok(-7) };
  print("raise: ${show(raised)}");
  0
}
"#,
    );
    let exe = build("containment_nesting", &main);
    let expected = "nested: ok 101
attempt: trapped: Int addition overflowed
catch: trapped: Int addition overflowed
raise: ok -7
";
    for ran in run(&exe, &[], &[]) {
        assert_eq!(ran.code, Some(0), "{}: {}{}", ran.fibers, ran.out, ran.err);
        assert_eq!(ran.out, expected, "{}", ran.fibers);
    }
}

/// The cases `contain` leaves fatal, one program, one case per argument.
const FATAL: &str = r#"
fn overflow() -> Int { big() + 1 }

fn in_finalizer() -> Int {
  print(show(contain(fn () => {
    let region = Region::open();
    Region::defer(region, fn () => { let _x = overflow(); () });
    1
  })));
  0
}

fn contain_in_finalizer() -> Int {
  print(show(contain(fn () => {
    let region = Region::open();
    Region::defer(region, fn () => {
      print("the finalizer's own contain: ${show(contain(fn () => overflow()))}");
      ()
    });
    1
  })));
  0
}

fn finalizer_traps_on_the_unwind() -> Int {
  print(show(contain(fn () => {
    let region = Region::open();
    Region::defer(region, fn () => { let _x = big() + 2; () });
    overflow()
  })));
  0
}

fn in_child() -> Int raises {} {
  print(show(contain(fn () => {
    let child = Fiber::spawn(fn () => overflow());
    Fiber::join(child)! catch { _ => 0 }
  })));
  0
}

fn deep(n: Int) -> Int { 1 + deep(n + 1) }

fn runtime_fatal() -> Int {
  let n = 0 - big();
  print(show(contain(fn () => { let xs: Array<Int> = Array::new(n, 0); Array::length(xs) })));
  0
}

fn budget() -> Int {
  let mut i = 0;
  while i < 10 {
    print("${Int::to_string(i)}: ${show(contain(fn () => overflow()))}");
    i = i + 1;
  };
  0
}

fn spin() -> Int {
  let mut i = 0;
  loop {
    i = i + 1;
    if i < 0 { break; }
  };
  i
}

fn canceled() -> Int raises {} {
  let child = Fiber::spawn(fn () => {
    let r = contain(fn () => spin());
    print("a cancellation was swallowed: ${show(r)}");
    7
  });
  Fiber::cancel(child);
  print("joined ${Int::to_string(Fiber::join(child)! catch { _ => -1 })}");
  0
}

pub fn main() -> Int {
  let w = which();
  if w == "bare" { overflow() }
  else if w == "finalizer" { in_finalizer() }
  else if w == "contain_in_finalizer" { contain_in_finalizer() }
  else if w == "finalizer_on_unwind" { finalizer_traps_on_the_unwind() }
  else if w == "child" { in_child() catch { _ => 0 } }
  else if w == "stack" { print(show(contain(fn () => deep(0)))); 0 }
  else if w == "fatal" { runtime_fatal() }
  else if w == "budget" { budget() }
  else if w == "cancel" { canceled() catch { _ => 0 } }
  else { 0 }
}
"#;

/// **What `contain` leaves fatal stays fatal**, with the status it always
/// had: no `contain` on the trapping fiber's own stack (the program's, or a
/// spawned child's, which does not inherit its parent's), a trap in a
/// finalizer, the runtime's own fatal errors, and running out of stack. A
/// cancellation passes through `contain` and is not turned into a value.
#[test]
fn what_contain_leaves_fatal_stays_fatal() {
    let exe = build("containment_fatal", &program(FATAL));
    for (case, status, said) in [
        ("bare", Some(134), "khora: Int addition overflowed\n"),
        ("finalizer", Some(134), "khora: Int addition overflowed\n"),
        // A `contain` written inside the finalizer does not help: nothing is
        // contained while a finalizer runs.
        ("contain_in_finalizer", Some(134), "khora: Int addition overflowed\n"),
        ("child", Some(134), "khora: Int addition overflowed on fiber"),
        ("fatal", Some(134), "khora runtime: an array cannot have a negative length"),
    ] {
        for ran in run(&exe, &[case], &[]) {
            assert_eq!(ran.code, status, "{case} on {}: {}{}", ran.fibers, ran.out, ran.err);
            assert!(ran.err.contains(said), "{case} on {}: {}", ran.fibers, ran.err);
            assert!(!ran.err.contains("contained;"), "{case} on {}: {}", ran.fibers, ran.err);
            assert_eq!(ran.out, "", "{case} on {}: nothing after the trap ran", ran.fibers);
        }
    }
    for ran in run(&exe, &["stack"], &[]) {
        // The platform's stack-overflow status, as with no `contain`.
        #[cfg(unix)]
        assert_eq!(ran.code, None, "stack on {}: killed by SIGSEGV: {}", ran.fibers, ran.err);
        assert!(ran.err.contains("the stack ran out"), "stack on {}: {}", ran.fibers, ran.err);
        assert_eq!(ran.out, "", "stack on {}", ran.fibers);
    }
    for ran in run(&exe, &["cancel"], &[]) {
        assert_eq!(ran.code, Some(130), "cancel on {}: {}{}", ran.fibers, ran.out, ran.err);
        assert!(!ran.out.contains("swallowed"), "cancel on {}: {}", ran.fibers, ran.out);
    }
    // A finalizer the unwind of a contained trap runs, which traps itself:
    // the first trap was contained when it happened and says so, and the
    // second ends the process. Both lines, in that order.
    for ran in run(&exe, &["finalizer_on_unwind"], &[]) {
        assert_eq!(ran.code, Some(134), "on {}: {}{}", ran.fibers, ran.out, ran.err);
        assert_eq!(ran.out, "", "on {}: nothing after the trap ran", ran.fibers);
        let contained = ran.err.find("(contained; the fiber continues)");
        let fatal = ran.err.find("khora: Int addition overflowed\n");
        assert!(
            contained.is_some() && fatal.is_some() && contained < fatal,
            "on {}: {}",
            ran.fibers,
            ran.err
        );
    }
}

/// **A spent budget makes the next trap fatal.** `KHORA_TRAP_BUDGET=3`
/// contains three traps and ends the process on the fourth, with 134 and a
/// sentence naming the budget.
#[test]
fn the_trap_budget_makes_the_next_trap_fatal() {
    let exe = build("containment_budget", &program(FATAL));
    for ran in run(&exe, &["budget"], &[("KHORA_TRAP_BUDGET", "3")]) {
        assert_eq!(ran.code, Some(134), "{}: {}{}", ran.fibers, ran.out, ran.err);
        assert_eq!(
            ran.out,
            "0: trapped: Int addition overflowed\n1: trapped: Int addition overflowed\n\
             2: trapped: Int addition overflowed\n",
            "{}",
            ran.fibers
        );
        assert!(ran.err.contains("which is the trap budget (KHORA_TRAP_BUDGET)"), "{}: {}", ran.fibers, ran.err);
    }
    // And with the default, ten are contained.
    for ran in run(&exe, &["budget"], &[]) {
        assert_eq!(ran.code, Some(0), "{}: {}{}", ran.fibers, ran.out, ran.err);
        assert_eq!(ran.out.lines().count(), 10, "{}: {}", ran.fibers, ran.out);
        assert!(!ran.err.contains("KHORA_TRAP_BUDGET"), "{}: nothing to warn about: {}", ran.fibers, ran.err);
    }
}

/// **A `KHORA_TRAP_BUDGET` that does not parse says so, once, and the
/// default stands.** A typo in a safety limit was silently 1,000 a minute.
/// Ten traps, so the default contains every one; the warning names the
/// value and the default, on one line, however many traps ask for the
/// budget.
#[test]
fn an_unreadable_trap_budget_is_named_once() {
    let exe = build("containment_budget_unreadable", &program(FATAL));
    for value in ["-1", "abc", "3/0", "4/-1", "2/1x", "18446744073709551616"] {
        for ran in run(&exe, &["budget"], &[("KHORA_TRAP_BUDGET", value)]) {
            assert_eq!(ran.code, Some(0), "{value} on {}: {}{}", ran.fibers, ran.out, ran.err);
            assert_eq!(ran.out.lines().count(), 10, "{value} on {}: the default contains all ten", ran.fibers);
            let warned: Vec<&str> = ran.err.lines().filter(|l| l.contains("KHORA_TRAP_BUDGET")).collect();
            assert_eq!(warned.len(), 1, "{value} on {}: once: {}", ran.fibers, ran.err);
            assert!(
                warned[0].contains(&format!("KHORA_TRAP_BUDGET={value:?}"))
                    && warned[0].contains("the default stands: 1000 traps in any 60 s"),
                "{value} on {}: {}",
                ran.fibers,
                warned[0]
            );
        }
    }
}

/// **What landed beside `contain` keeps its promises across a contained
/// trap.** A trap inside a `timeout` body is the surrounding `contain`'s, and
/// the deadline it armed does not fire later; a `race` branch is a fiber of
/// its own, so a trap there is fatal unless the branch has its own `contain`;
/// a `select` branch runs on the selecting fiber, and the value it was handed
/// is released; a value `acquire` registered is released once; a job
/// `Channel::claim` took and did not settle runs its abandon handler, which
/// the limitations page promises; a `Raised` held when the trap comes is
/// released.
#[test]
fn timeout_race_select_acquire_and_raised_across_a_trap() {
    let main = program(
        r#"
type Oops = | Oops;
type Conn = { name: String, n: Int };

fn fails() -> Int raises Oops { raise Oops::Oops }

fn in_timeout() -> Int raises {} {
  let r = contain(fn () => {
    match timeout(5000, fn () => big() + 1)! {
      Option::Some(v) => v,
      Option::None => -1,
    }
  });
  print("timeout: ${show(r)}");
  match timeout(5000, fn () => count(contain(fn () => big() + 1)))! {
    Option::Some(c) => print("contain inside timeout: ${Int::to_string(c)}"),
    Option::None => print("contain inside timeout: cut"),
  };
  let later = timeout(300, fn () => { let mut i = 0; while i < 3000000 { i = i + 1; }; i })!;
  match later {
    Option::Some(_) => print("a later timeout is not cut by the trapped one"),
    Option::None => print("a later timeout was cut"),
  };
  0
}

fn in_race() -> Int raises {} {
  print("race: ${Int::to_string(race([SharedFn::of(fn _ => count(contain(fn () => big() + 1)))])!)}");
  0
}

fn in_race_bare() -> Int raises {} {
  print(show(contain(fn () => race([SharedFn::of(fn _ => big() + 1)])!)));
  0
}

fn in_select() -> Int {
  let mut before = 0;
  let mut i = 0;
  while i < 200 {
    if i == 10 { before = khora_live_count() } else {};
    let ch: Channel<String> = Channel::bounded(1);
    let _ = Channel::offer(ch, "value ${Int::to_string(i)}");
    let r = contain(fn () => {
      let picked = Select::new() |> Select::on(ch, fn s => String::byte_length(s) + big());
      match Select::wait(picked) {
        Selected::Got(v) => v,
        _ => -1,
      }
    });
    if i == 0 { print("select: ${show(r)}") } else {};
    i = i + 1;
  };
  let after = khora_live_count();
  print("select leaked ${Int::to_string(after - before)}");
  0
}

fn note_release(released: Shared<Int>, _c: Conn) -> () {
  Shared::update(released, fn k => k + 1);
  ()
}

fn claimed(ch: Channel<Int>, abandoned: Shared<Int>) -> Int {
  match Channel::claim(ch, fn job => { Shared::update(abandoned, fn k => k + job); () }) {
    Option::Some(c) => c.value + big(),
    Option::None => -1,
  }
}

fn in_claim() -> Int {
  let ch: Channel<Int> = Channel::bounded(1);
  let abandoned = Shared::of(0);
  let _ = Channel::offer(ch, 5);
  print("claim: ${show(contain(fn () => claimed(ch, abandoned)))}; abandoned with ${Int::to_string(Shared::get(abandoned))}");
  0
}

fn in_acquire() -> Int raises {} {
  let released = Shared::of(0);
  let r = contain(fn () => scoped(fn () => {
    let c = acquire({ name: "conn", n: 1 }, fn v => note_release(released, v));
    c.n + big()
  })!);
  print("acquire: ${show(r)}; released ${Int::to_string(Shared::get(released))}");
  0
}

fn in_flight() -> Int {
  let mut before = 0;
  let mut i = 0;
  while i < 200 {
    if i == 10 { before = khora_live_count() } else {};
    let _ = contain(fn () => {
      match attempt(fn () => fails()!) {
        Result::Ok(v) => v,
        Result::Err(failure) => String::byte_length(failure.context("held ${Int::to_string(i)}").show()) + big(),
      }
    });
    i = i + 1;
  };
  let after = khora_live_count();
  print("raised leaked ${Int::to_string(after - before)}");
  0
}

pub fn main() -> Int {
  let w = which();
  if w == "timeout" { in_timeout() catch { _ => 0 } }
  else if w == "race" { in_race() catch { _ => 0 } }
  else if w == "race-bare" { in_race_bare() catch { _ => 0 } }
  else if w == "select" { in_select() }
  else if w == "acquire" { in_acquire() catch { _ => 0 } }
  else if w == "claim" { in_claim() }
  else if w == "raised" { in_flight() }
  else { 0 }
}
"#,
    );
    let exe = build("containment_interplay", &main);
    for (case, expected) in [
        (
            "timeout",
            "timeout: trapped: Int addition overflowed\ncontain inside timeout: 1\n\
             a later timeout is not cut by the trapped one\n",
        ),
        ("race", "race: 1\n"),
        ("select", "select: trapped: Int addition overflowed\nselect leaked 0\n"),
        ("acquire", "acquire: trapped: Int addition overflowed; released 1\n"),
        ("claim", "claim: trapped: Int addition overflowed; abandoned with 5\n"),
        ("raised", "raised leaked 0\n"),
    ] {
        for ran in run(&exe, &[case], &[]) {
            assert_eq!(ran.code, Some(0), "{case} on {}: {}{}", ran.fibers, ran.out, ran.err);
            assert_eq!(ran.out, expected, "{case} on {}", ran.fibers);
        }
    }
    for ran in run(&exe, &["race-bare"], &[]) {
        assert_eq!(ran.code, Some(134), "race-bare on {}: {}{}", ran.fibers, ran.out, ran.err);
        assert!(ran.err.contains("overflowed on fiber"), "race-bare on {}: {}", ran.fibers, ran.err);
    }
}

/// **`contain`'s body may not capture what a trap could leave half-written**:
/// a `mut` record, an `Array`, a `Region`. The same rule as a spawned fiber's
/// body, with a sentence that does not claim anything crosses a fiber.
#[test]
fn contain_refuses_what_a_trap_could_leave_half_written() {
    for (name, body, wanted) in [
        (
            "mut_record",
            "let r: Tally = { n: 0 }; let _ = contain(fn () => { r.n = r.n + 1; r.n }); 0",
            "`r` cannot be captured by `contain`'s body",
        ),
        (
            "array",
            "let xs: Array<Int> = Array::new(2, 0); let _ = contain(fn () => Array::get(xs, 0)); 0",
            "`xs` cannot be captured by `contain`'s body",
        ),
        (
            "region",
            "let region = Region::open(); let _ = contain(fn () => { Region::defer(region, fn () => ()); 1 }); 0",
            "`region` cannot be captured by `contain`'s body",
        ),
    ] {
        let main = format!(
            "module demo::main;
import std::core::{{Array, Region, Result, Shared, contain}};
type Tally = {{ mut n: Int }};
pub fn main() -> Int {{ {body} }}
"
        );
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("containment_refuses_{name}"));
        std::fs::create_dir_all(&dir).expect("a workspace");
        let db = KhoraDatabase::new();
        let root = SourceRoot::new(&db, sources(&db, &dir, &main));
        let exe = dir.join("program");
        let errors = match khora_codegen_llvm::compile(&db, root, &exe) {
            Ok(()) => panic!("{name}: a capture a trap could leave half-written compiled"),
            Err(errors) => errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n"),
        };
        assert!(errors.contains(wanted), "{name}: {errors}");
        assert!(!errors.contains("handed to another fiber"), "{name}: nothing crosses: {errors}");
        assert!(!errors.contains("two fibers"), "{name}: `contain` is one fiber: {errors}");
    }
    // The reason a writable value is refused names what a trap leaves.
    let main = "module demo::main;
import std::core::{contain};
type Tally = { mut n: Int };
pub fn main() -> Int { let r: Tally = { n: 0 }; let _ = contain(fn () => { r.n = r.n + 1; r.n }); 0 }
";
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("containment_refuses_reason");
    std::fs::create_dir_all(&dir).expect("a workspace");
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    let errors = match khora_codegen_llvm::compile(&db, root, &dir.join("program")) {
        Ok(()) => panic!("a captured `mut` record compiled"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n"),
    };
    assert!(
        errors.contains("half-written, and the code after `contain` would see that"),
        "the reason says what a trap leaves: {errors}"
    );
    // What it may capture: a `Shared` cell, and immutable values.
    let fine = "module demo::main;
import std::core::{Result, Shared, contain, print};
type Point = { x: Int, y: Int };
pub fn main() -> Int {
  let cell = Shared::of(1);
  let p: Point = { x: 2, y: 3 };
  let r = contain(fn () => Shared::update(cell, fn k => k + p.x + p.y));
  match r { Result::Ok(v) => print(\"${Int::to_string(v)}\"), Result::Err(_) => () };
  0
}
";
    let exe = build("containment_captures_fine", fine);
    for ran in run(&exe, &[], &[]) {
        assert_eq!((ran.code, ran.out.as_str()), (Some(0), "6\n"), "{}: {}", ran.fibers, ran.err);
    }
}

/// **`contain_with` is the standard library's own.** It moves a value into
/// the body without `contain`'s capture rule, which is sound only when
/// nothing reads the value afterwards, and the checker cannot see that. So a
/// program's call is refused with a sentence that sends it to `contain`; and
/// a program that declares its own bodiless `contain_run` -- the primitive
/// under both -- gets "no body", not containment with no rule at all.
#[test]
fn a_program_cannot_reach_contain_without_its_rule() {
    let mut failed = Vec::new();
    for (name, main, wanted) in [
        (
            "contain_with",
            "module demo::main;
import std::core::{Array, Result, contain_with};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = contain_with(xs, fn ys => { Array::set(ys, 0, 7); Array::get(ys, 5) });
  Array::get(xs, 0)
}
",
            "`contain_with` is the standard library's own",
        ),
        (
            "own_contain_run",
            "module demo::main;
import std::core::{Array, Option};
fn contain_run<A>(body: () -> A) -> Option<A>;
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = contain_run(fn () => { Array::set(xs, 0, 7); Array::get(xs, 5) });
  Array::get(xs, 0)
}
",
            "`contain_run` has no body",
        ),
        // M1: the rules key on the definition, not the spelling. An alias,
        // a function value, a higher-order pass and the qualified path, for
        // each of the two.
        (
            "contain_alias",
            "module demo::main;
import std::core::{Array, Result};
import std::core::{contain as guard};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = guard(fn () => { Array::set(xs, 0, 7); Array::get(xs, 5) });
  Array::get(xs, 0)
}
",
            "cannot be captured by `contain`'s body",
        ),
        (
            "contain_value",
            "module demo::main;
import std::core::{Array, Result, contain};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let c = contain;
  let _ = c(fn () => { Array::set(xs, 0, 7); Array::get(xs, 5) });
  Array::get(xs, 0)
}
",
            "`contain` is called directly",
        ),
        (
            "contain_passed",
            "module demo::main;
import std::core::{Array, Result, Trapped, contain};
fn run(f: (() -> Int) -> Result<Int, Trapped>, xs: Array<Int>) -> Result<Int, Trapped> {
  f(fn () => { Array::set(xs, 0, 7); Array::get(xs, 5) })
}
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = run(contain, xs);
  Array::get(xs, 0)
}
",
            "`contain` is called directly",
        ),
        (
            "contain_qualified",
            "module demo::main;
import std::core::{Array, Result};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = std::core::contain(fn () => { Array::set(xs, 0, 7); Array::get(xs, 5) });
  Array::get(xs, 0)
}
",
            // A body cannot name another module's function by its path at
            // all; kept so that the day it can, this case is not missing.
            "cannot resolve `std::core::contain`",
        ),
        (
            "contain_with_alias",
            "module demo::main;
import std::core::{Array, Result};
import std::core::{contain_with as guarded};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = guarded(xs, fn ys => { Array::set(ys, 0, 7); Array::get(ys, 5) });
  Array::get(xs, 0)
}
",
            "`contain_with` is the standard library's own",
        ),
        (
            "contain_with_value",
            "module demo::main;
import std::core::{Array, Result, contain_with};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let cw = contain_with;
  let _ = cw(xs, fn ys => { Array::set(ys, 0, 7); Array::get(ys, 5) });
  Array::get(xs, 0)
}
",
            "`contain_with` is called directly",
        ),
        (
            "contain_with_passed",
            "module demo::main;
import std::core::{Array, Result, Trapped, contain_with};
fn run(
  f: (Array<Int>, (Array<Int>) -> Int) -> Result<Int, Trapped>,
  xs: Array<Int>,
) -> Result<Int, Trapped> {
  f(xs, fn ys => { Array::set(ys, 0, 7); Array::get(ys, 5) })
}
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = run(contain_with, xs);
  Array::get(xs, 0)
}
",
            "`contain_with` is called directly",
        ),
        (
            "contain_with_qualified",
            "module demo::main;
import std::core::{Array, Result};
pub fn main() -> Int {
  let xs: Array<Int> = Array::new(2, 0);
  let _ = std::core::contain_with(xs, fn ys => { Array::set(ys, 0, 7); Array::get(ys, 5) });
  Array::get(xs, 0)
}
",
            "cannot resolve `std::core::contain_with`",
        ),
    ] {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("containment_escape_{name}"));
        std::fs::create_dir_all(&dir).expect("a workspace");
        let db = KhoraDatabase::new();
        let root = SourceRoot::new(&db, sources(&db, &dir, main));
        match khora_codegen_llvm::compile(&db, root, &dir.join("program")) {
            Ok(()) => failed.push(format!("{name}: a program reached containment without its rule")),
            Err(errors) => {
                let errors = errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n");
                if !errors.contains(wanted) {
                    failed.push(format!("{name}: wanted {wanted:?}, got:\n{errors}"));
                }
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// **What `contain_with` does for the standard library.** Written as a
/// `std::` module because that is the only place it may be called; `Router`
/// is the caller it exists for, and `traps_in_a_server` tests that end to
/// end. Here: a value it is handed comes back through the body's answer; a
/// fresh value and a moved binding are both fine; a trap in the body answers
/// `Err` with the sentence; and the value, a record holding a `Map` --
/// the shape `contain` refuses -- is released once on the trap, so the live
/// count over many traps does not move.
#[test]
fn contain_with_moves_its_value_and_releases_it_on_a_trap() {
    let main = r#"module std::contain_with_probe;
import std::core::{Array, Map, Result, Trapped, contain_with, print};

extern fn khora_live_count() -> Int;

type Held = { name: String, seen: Map<String, String> };

fn fresh(n: Int) -> Held {
  let seen: Map<String, String> = Map::new();
  Map::insert(seen, "k ${Int::to_string(n)}", "v ${Int::to_string(n)}");
  { name: "held ${Int::to_string(n)}", seen: seen }
}

fn big() -> Int { 9223372036854775807 }

fn show(r: Result<Int, Trapped>) -> String {
  match r {
    Result::Ok(v) => "ok ${Int::to_string(v)}",
    Result::Err(t) => "trapped: ${t.message}",
  }
}

pub fn main() -> Int {
  print(show(contain_with(fresh(1), fn h => String::byte_length(h.name))));
  let moved = fresh(2);
  print(show(contain_with(moved, fn h => Map::len(h.seen) + 40)));
  print(show(contain_with(fresh(3), fn h => String::byte_length(h.name) + big())));
  let mut i = 0;
  while i < 20 {
    let _ = contain_with(fresh(i), fn h => String::byte_length(h.name) + big());
    i = i + 1;
  };
  let before = khora_live_count();
  let mut j = 0;
  while j < 1000 {
    let _ = contain_with(fresh(j), fn h => Map::len(h.seen) + big());
    j = j + 1;
  };
  let after = khora_live_count();
  print("live ${Int::to_string(after - before)}");
  0
}
"#;
    let exe = build("containment_contain_with", main);
    for ran in run(&exe, &[], &[("KHORA_TRAP_BUDGET", "100000")]) {
        assert_eq!(ran.code, Some(0), "{}: {}{}", ran.fibers, ran.out, &ran.err[..ran.err.len().min(400)]);
        assert_eq!(
            ran.out,
            "ok 6\nok 41\ntrapped: Int addition overflowed\nlive 0\n",
            "{}",
            ran.fibers
        );
    }
}
