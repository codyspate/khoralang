//! End to end: Khora source in, a running native executable out.
//!
//! Every test here compiles a program, runs it, and asserts on what it printed
//! and the code it exited with. Nothing inspects the generated IR — the IR is
//! an implementation detail, and a test that pins it would fail every time the
//! backend got better at its job. What must not change is the behaviour of the
//! program.
//!
//! Requires `--features llvm` and a configured LLVM 22.1 prefix; without them
//! the whole file compiles to nothing so the default `cargo test` stays green.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use khora_db::{KhoraDatabase, SourceFile};

/// What a compiled program did.
struct Ran {
    code: Option<i32>,
    stdout: String,
}

/// Makes sure the runtime archive exists and is current.
///
/// `cargo test -p khora-codegen-llvm` builds `khora-rt`'s *rlib*, because that
/// is what a dependency needs; it does not build the `staticlib` crate type,
/// which is what generated executables link against. So the archive is built
/// here, once per test binary. A nested cargo invocation is safe: the build
/// lock is released before test binaries run.
fn ensure_runtime() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let built = Command::new("cargo")
            .args(["build", "-p", "khora-rt"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output();
        match built {
            Ok(output) if output.status.success() => {}
            other => {
                // Only fatal if there is no archive to fall back on: a
                // developer may have built one by hand, or be running from a
                // packaged toolchain with no cargo at all.
                assert!(
                    khora_codegen_llvm::toolchain::runtime_archive().is_some(),
                    "could not build khora-rt and no runtime archive was found: {other:?}"
                );
            }
        }
    });
}

/// A private directory for one test's artefacts.
///
/// Per test, because tests run in parallel and `compile` writes an object file
/// and an executable next to each other under names it derives from `out`.
fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("khora-codegen-tests").join(name);
    std::fs::create_dir_all(&dir).expect("creating a test directory");
    dir
}

/// Compiles `source`, runs it, and reports what happened.
fn run(name: &str, source: &str) -> Ran {
    ensure_runtime();

    let dir = workspace(name);
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, format!("{name}.kh").into(), source.to_string());
    if let Err(errors) = khora_codegen_llvm::compile(&db, file, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    assert!(exe.is_file(), "`{name}` produced no executable");

    let output = Command::new(&exe).output().expect("running the compiled program");
    Ran {
        code: output.status.code(),
        // The runtime writes bare `\n`; normalise anyway so a failure message
        // is readable rather than full of escapes.
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
    }
}

/// Compiles `source` expecting it to be rejected, and reports the messages.
fn errors(name: &str, source: &str) -> Vec<String> {
    let dir = workspace(name);
    let exe = dir.join("rejected.exe");
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, format!("{name}.kh").into(), source.to_string());
    match khora_codegen_llvm::compile(&db, file, &exe) {
        Ok(()) => panic!("`{name}` was expected to be rejected, but it compiled"),
        Err(errors) => {
            assert!(!exe.is_file(), "a rejected program must not leave an executable behind");
            errors.into_iter().map(|e| e.message).collect()
        }
    }
}

// ---------------------------------------------------------------------------
// The language
// ---------------------------------------------------------------------------

#[test]
fn integer_arithmetic() {
    let ran = run(
        "arithmetic",
        "module t;
fn print(value: Int);

fn main() -> Int {
  print(2 + 3 * 4);
  print(20 - 6 / 2);
  print(17 % 5);
  print(-7 + 10);
  0
}
",
    );
    assert_eq!(ran.stdout, "14\n17\n2\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// The exit code comes from Khora's `main`, truncated to the `i32` a process
/// status carries.
#[test]
fn a_function_call_produces_the_exit_code() {
    let ran = run(
        "call",
        "module t;
fn add(a: Int, b: Int) -> Int { a + b }
fn double(n: Int) -> Int { add(n, n) }

fn main() -> Int { double(add(1, 20)) }
",
    );
    assert_eq!(ran.code, Some(42));
}

#[test]
fn if_else_chooses_a_branch() {
    let ran = run(
        "if_else",
        "module t;
fn print(value: Int);

fn sign(n: Int) -> Int {
  if n < 0 {
    -1
  } else if n == 0 {
    0
  } else {
    1
  }
}

fn main() -> Int {
  print(sign(-9));
  print(sign(0));
  print(sign(9));
  0
}
",
    );
    assert_eq!(ran.stdout, "-1\n0\n1\n");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn boolean_operators_short_circuit() {
    let ran = run(
        "booleans",
        "module t;
fn print(value: Bool);

fn boom() -> Bool { 1 / 0 == 0 }

fn main() -> Int {
  print(true && false);
  print(true || boom());
  print(!(false && boom()));
  0
}
",
    );
    // `boom` divides by zero, so reaching it would kill the process. That it
    // does not is the assertion: `||` and `&&` must not evaluate their right
    // operand once the answer is known.
    assert_eq!(ran.stdout, "false\ntrue\ntrue\n");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn a_while_loop_accumulates() {
    let ran = run(
        "while_loop",
        "module t;
fn print(value: Int);

fn sum_to(n: Int) -> Int {
  if n < 0 {
    return 0;
  }

  let mut total = 0;
  let mut i = 1;
  while i <= n {
    total = total + i;
    i = i + 1;
  }
  total
}

fn main() -> Int {
  print(sum_to(10));
  print(sum_to(-3));
  sum_to(10) - 55
}
",
    );
    assert_eq!(ran.stdout, "55\n0\n");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn loop_break_and_continue() {
    let ran = run(
        "loop_break",
        "module t;
fn print(value: Int);

/// Sums the odd numbers below `limit`, using every loop exit there is.
fn odd_sum(limit: Int) -> Int {
  let mut total = 0;
  let mut i = 0;
  loop {
    i = i + 1;
    if i >= limit {
      break;
    }
    if i % 2 == 0 {
      continue;
    }
    total = total + i;
  }
  total
}

fn main() -> Int {
  print(odd_sum(10));
  0
}
",
    );
    assert_eq!(ran.stdout, "25\n");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn a_recursive_function_terminates() {
    let ran = run(
        "recursion",
        "module t;
fn print(value: Int);

fn fib(n: Int) -> Int {
  if n < 2 {
    n
  } else {
    fib(n - 1) + fib(n - 2)
  }
}

fn main() -> Int {
  print(fib(10));
  print(fib(20));
  0
}
",
    );
    assert_eq!(ran.stdout, "55\n6765\n");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn an_adt_is_built_and_matched() {
    let ran = run(
        "adt",
        "module t;
fn print(value: Int);

pub type Shape =
  | Circle(radius: Int)
  | Square(side: Int)
  | Point;

pub fn area(s: Shape) -> Int {
  match s {
    Shape::Circle(r) => 3 * r * r,
    Shape::Square(side) => side * side,
    Shape::Point => 0,
  }
}

fn main() -> Int {
  print(area(Shape::Circle(4)));
  print(area(Shape::Square(5)));
  print(area(Shape::Point));
  0
}
",
    );
    assert_eq!(ran.stdout, "48\n25\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// A guard makes the `match` fall through to the next arm, which a `switch` on
/// the tag cannot express — the backend takes its sequential path instead.
#[test]
fn match_guards_fall_through() {
    let ran = run(
        "match_guards",
        "module t;
fn print(value: Int);

pub type Reading = | Sample(value: Int) | Missing;

fn describe(r: Reading) -> Int {
  match r {
    Reading::Sample(v) if v > 100 => 2,
    Reading::Sample(v) if v > 0 => 1,
    Reading::Sample(v) => 0,
    Reading::Missing => -1,
  }
}

fn main() -> Int {
  print(describe(Reading::Sample(500)));
  print(describe(Reading::Sample(7)));
  print(describe(Reading::Sample(-7)));
  print(describe(Reading::Missing));
  0
}
",
    );
    assert_eq!(ran.stdout, "2\n1\n0\n-1\n");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn a_string_literal_is_printed() {
    let ran = run(
        "strings",
        "module t;
fn print(value: String);

fn greeting() -> String { \"hello, khora\" }

fn main() -> Int {
  print(greeting());
  print(\"\");
  let held = greeting();
  print(held);
  0
}
",
    );
    assert_eq!(ran.stdout, "hello, khora\n\nhello, khora\n");
    assert_eq!(ran.code, Some(0));
}

// ---------------------------------------------------------------------------
// The phase 2 exit criterion
// ---------------------------------------------------------------------------

/// Every allocation is freed.
///
/// `docs/roadmap.md` phase 2 exits on this: a compiled program runs to
/// completion and the runtime's live count is zero. It is asserted from inside
/// the program, through `khora_live_count` declared as an extern, because that
/// is the only place the counter can be read after the last `drop` and before
/// the process is gone.
///
/// The work it does is chosen to hit every case where a reference can go
/// missing: a recursive ADT built and consumed, a `match` that borrows a
/// payload out of its scrutinee, a boxed local overwritten by assignment, a
/// boxed value discarded as a statement, an early `return` past a live binding,
/// and a `break` out of a loop whose body holds one.
#[test]
fn every_allocation_is_freed() {
    let ran = run(
        "leaks",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;
fn print(value: String);

pub type List = | Nil | Cons(head: Int, tail: List);

fn build(n: Int) -> List {
  if n == 0 {
    List::Nil
  } else {
    List::Cons(n, build(n - 1))
  }
}

fn sum(l: List) -> Int {
  match l {
    List::Nil => 0,
    List::Cons(h, t) => h + sum(t),
  }
}

/// Returns early with a boxed local still live, so the `return` has to release
/// it on the way out.
fn early(l: List) -> Int {
  let held = l;
  if sum(held) > 3 {
    return 1;
  }
  0
}

fn strings() {
  let mut s = \"first\";
  s = \"second\";
  print(s);
  \"discarded\";
}

/// Breaks out of a loop while a boxed local declared inside it is live, so the
/// same slot is released on two different paths across two iterations.
fn looping() {
  let mut i = 0;
  while i < 5 {
    let s = \"tick\";
    print(s);
    i = i + 1;
    if i == 2 {
      break;
    }
  }
}

fn work() {
  let xs = build(4);
  khora_print_int(sum(xs));
  khora_print_int(early(build(4)));
  strings();
  looping();
}

fn main() -> Int {
  work();
  khora_live_count()
}
",
    );
    assert_eq!(ran.stdout, "10\n1\nsecond\ntick\ntick\n");
    assert_eq!(ran.code, Some(0), "the runtime's live count was not zero at exit");
}

/// A positive control for the test above.
///
/// A leak check that can only ever report zero proves nothing, and there are
/// several ways for it to end up that way — an extern that resolved to the
/// wrong symbol, a `main` that never ran the work. Reading the counter while an
/// object is deliberately still live has to give a number greater than zero.
#[test]
fn the_live_count_is_actually_observable() {
    let ran = run(
        "live_count",
        "module t;
fn khora_live_count() -> Int;

pub type Box = | Wrap(value: Int);

/// The block releases what it declared *after* its tail is evaluated, so the
/// count is read while `held` is still alive.
fn while_alive() -> Int {
  let held = Box::Wrap(1);
  khora_live_count()
}

fn main() -> Int { while_alive() }
",
    );
    assert!(
        ran.code.unwrap_or(0) > 0,
        "the live count read while an object was alive was {:?}",
        ran.code
    );
}

/// A constructor pattern inside a constructor pattern.
///
/// Reading the inner tag is only safe once the outer one has been checked, so
/// the tests have to be a chain rather than a conjunction — a `Nil` has no
/// field to look inside.
///
/// Every case is spelled out, with no trailing wildcard. That only became
/// possible once the usefulness check learned to thread payload types into
/// nested columns; before, it reported this as inexhaustive.
#[test]
fn nested_constructor_patterns_match() {
    let ran = run(
        "nested_patterns",
        "module t;
fn print(value: Int);

pub type List = | Nil | Cons(head: Int, tail: List);

fn second(l: List) -> Int {
  match l {
    List::Cons(_, List::Cons(v, _)) => v,
    List::Cons(_, List::Nil) => -1,
    List::Nil => -2,
  }
}

fn main() -> Int {
  print(second(List::Cons(1, List::Cons(2, List::Nil))));
  print(second(List::Cons(1, List::Nil)));
  print(second(List::Nil));
  0
}
",
    );
    assert_eq!(ran.stdout, "2\n-1\n-2\n");
    assert_eq!(ran.code, Some(0));
}

/// A generic function has no machine representation until its type arguments
/// are known, so each one is emitted once per set of arguments it is used at.
#[test]
fn generic_functions_are_specialised_and_run() {
    let ran = run(
        "generics",
        "module t;
fn print(value: Int);

fn id<A>(x: A) -> A { x }
fn twice<B>(x: B) -> B { id(id(x)) }

fn main() -> Int {
  print(id(7));
  print(twice(9));
  0
}
",
    );
    assert_eq!(ran.stdout, "7
9
");
    assert_eq!(ran.code, Some(0));
}

/// A generic function over a generic type, matching on it. This is the shape
/// most of `std` will be written in.
#[test]
fn a_generic_function_over_a_generic_type_runs() {
    let ran = run(
        "generic_adt",
        "module t;
fn print(value: Int);

pub type Option<A> = | Some(value: A) | None;

fn unwrap_or<A>(o: Option<A>, fallback: A) -> A {
  match o {
    Option::Some(v) => v,
    Option::None => fallback,
  }
}

fn main() -> Int {
  print(unwrap_or(Option::Some(42), 0));
  print(unwrap_or(Option::None, 5));
  0
}
",
    );
    assert_eq!(ran.stdout, "42
5
");
    assert_eq!(ran.code, Some(0));
}

/// Two instantiations of one function must not share code: `Bool` is an `i1`
/// and `Int` an `i64`, so one body cannot serve both.
///
/// Observed through the exit code rather than printing, because `print` is an
/// intrinsic a program declares once and so cannot cover two types here.
#[test]
fn two_instantiations_do_not_interfere() {
    let ran = run(
        "two_instances",
        "module t;

fn id<A>(x: A) -> A { x }

fn main() -> Int {
  let flag = id(true);
  let n = id(3);
  if flag { n } else { 0 }
}
",
    );
    assert_eq!(ran.code, Some(3), "both instantiations should carry their own value");
}

/// The program `docs/roadmap.md` names in the phase 2 exit criterion.
///
/// Compiled from the file rather than from a copy, so that the example and the
/// backend cannot drift apart while both look fine on their own.
#[test]
fn the_core_demo_example_runs() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/core_demo/src/main.kh")
        .canonicalize()
        .expect("examples/core_demo/src/main.kh");
    let text = std::fs::read_to_string(&source).expect("reading the example");

    let ran = run("core_demo", &text);
    assert_eq!(ran.stdout, "48\n55\n25\n");
    assert_eq!(ran.code, Some(0));
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Type checking is a precondition, not a stage that runs alongside emission.
#[test]
fn a_type_error_emits_nothing() {
    let found = errors(
        "type_error",
        "module t;
fn main() -> Int { true }
",
    );
    assert!(
        found.iter().any(|m| m.contains("returns `Int`")),
        "expected the checker's message, got {found:?}"
    );
}

/// What the backend cannot lower is a diagnostic with a source range, not a
/// panic and not a wrong program.
#[test]
fn an_unsupported_construct_is_reported() {
    let found = errors(
        "unsupported",
        "module t;
fn join(a: String, b: String) -> String { a + b }
fn main() -> Int { 0 }
",
    );
    assert!(
        found.iter().any(|m| m.contains("concatenation")),
        "expected a message about string concatenation, got {found:?}"
    );
}

#[test]
fn a_program_without_main_is_reported() {
    let found = errors(
        "no_main",
        "module t;
fn helper() -> Int { 1 }
",
    );
    assert!(
        found.iter().any(|m| m.contains("no `main`")),
        "expected a message about the missing entry point, got {found:?}"
    );
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// Dispatch is static: the call becomes a direct call to the impl's function,
/// with no dictionary and no vtable. See `docs/design/typeclasses.md`.
#[test]
fn a_method_calls_the_impl_directly() {
    let ran = run(
        "trait_direct",
        "module t;
fn print(value: Int);

trait Double {
  fn double(self) -> Int;
}

impl Double for Int {
  fn double(self) -> Int { self * 2 }
}

fn main() -> Int {
  print(21.double());
  0
}
",
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// A call through a bound picks the impl at monomorphisation, so the generic
/// function costs nothing the concrete one would not have.
#[test]
fn a_bounded_generic_resolves_per_instantiation() {
    let ran = run(
        "trait_bound",
        "module t;
fn print(value: Int);

trait Size {
  fn size(self) -> Int;
}

impl Size for Int { fn size(self) -> Int { 8 } }
impl Size for Bool { fn size(self) -> Int { 1 } }

fn total<T: Size>(a: T, b: T) -> Int { a.size() + b.size() }

fn main() -> Int {
  print(total(1, 2));
  print(total(true, false));
  0
}
",
    );
    assert_eq!(ran.stdout, "16\n2\n", "each instantiation should reach its own impl");
    assert_eq!(ran.code, Some(0));
}

/// A supertrait's functions are available through the subtrait's bound.
#[test]
fn a_supertrait_method_is_callable_through_the_subtrait() {
    let ran = run(
        "trait_super",
        "module t;
fn print(value: Int);

trait Base { fn base(self) -> Int; }
trait Derived: Base { fn derived(self) -> Int; }

impl Base for Int { fn base(self) -> Int { self } }
impl Derived for Int { fn derived(self) -> Int { self * 10 } }

fn both<T: Derived>(x: T) -> Int { x.base() + x.derived() }

fn main() -> Int {
  print(both(4));
  0
}
",
    );
    assert_eq!(ran.stdout, "44\n");
    assert_eq!(ran.code, Some(0));
}

/// An impl over a constructor is selected by matching the receiver, which is
/// what tells `impl<A> Holds for Box<A>` what `A` is.
#[test]
fn a_parameterised_impl_is_selected_by_the_receiver() {
    let ran = run(
        "trait_param_impl",
        "module t;
fn print(value: Int);

pub type Box<A> = | Of(value: A);

trait Unwrap { fn get(self) -> Int; }

impl<A> Unwrap for Box<A> {
  fn get(self) -> Int { 99 }
}

fn main() -> Int {
  print(Box::Of(1).get());
  print(Box::Of(true).get());
  0
}
",
    );
    assert_eq!(ran.stdout, "99\n99\n");
    assert_eq!(ran.code, Some(0));
}

/// A default body runs when the impl does not supply one.
#[test]
fn a_default_body_is_emitted_for_an_impl_that_omits_it() {
    let ran = run(
        "trait_default",
        "module t;
fn print(value: Int);

trait Describe {
  fn size(self) -> Int;
  fn doubled(self) -> Int { self.size() + self.size() }
}

impl Describe for Int {
  fn size(self) -> Int { self * 3 }
}

fn main() -> Int {
  print(7.doubled());
  0
}
",
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}
