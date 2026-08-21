//! End to end: Khora source in, a running native executable out.
//!
//! Every test here compiles a program, runs it, and asserts on what it printed
//! and the code it exited with. Nothing inspects the generated IR — the IR is
//! an implementation detail, and a test that pins it would fail every time the
//! backend got better at its job. What must not change is the behavior of the
//! program.
//!
//! Requires `--features llvm` and a configured LLVM 22.1 prefix; without them
//! the whole file compiles to nothing so the default `cargo test` stays green.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

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

/// A private directory for one test's artifacts.
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
    let root = SourceRoot::new(&db, vec![file]);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    assert!(exe.is_file(), "`{name}` produced no executable");

    let output = Command::new(&exe).output().expect("running the compiled program");
    Ran {
        code: output.status.code(),
        // The runtime writes bare `\n`; normalize anyway so a failure message
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
    let root = SourceRoot::new(&db, vec![file]);
    match khora_codegen_llvm::compile(&db, root, &exe) {
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

export type Shape =
  | Circle(radius: Int)
  | Square(side: Int)
  | Point;

export fn area(s: Shape) -> Int {
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

export type Reading = | Sample(value: Int) | Missing;

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

export type List = | Nil | Cons(head: Int, tail: List);

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

export type Wrapper = | Wrap(value: Int);

/// The block releases what it declared *after* its tail is evaluated, so the
/// count is read while `held` is still alive.
fn while_alive() -> Int {
  let held = Wrapper::Wrap(1);
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

export type List = | Nil | Cons(head: Int, tail: List);

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
fn generic_functions_are_specialized_and_run() {
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

export type Option<A> = | Some(value: A) | None;

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

/// A call through a bound picks the impl at monomorphization, so the generic
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
/// what tells `impl<A> Holds for Wrapper<A>` what `A` is.
#[test]
fn a_parameterised_impl_is_selected_by_the_receiver() {
    let ran = run(
        "trait_param_impl",
        "module t;
fn print(value: Int);

export type Wrapper<A> = | Of(value: A);

trait Unwrap { fn get(self) -> Int; }

impl<A> Unwrap for Wrapper<A> {
  fn get(self) -> Int { 99 }
}

fn main() -> Int {
  print(Wrapper::Of(1).get());
  print(Wrapper::Of(true).get());
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

// ---------------------------------------------------------------------------
// Closures
// ---------------------------------------------------------------------------

#[test]
fn a_closure_is_called_directly_and_passed_along() {
    let ran = run(
        "closure_basic",
        "module t;
fn print(value: Int);

fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }

fn main() -> Int {
  let add_one = fn x => x + 1;
  print(add_one(41));
  print(apply(add_one, 10));
  print(apply(fn y => y * 3, 14));
  0
}
",
    );
    assert_eq!(ran.stdout, "42\n11\n42\n");
    assert_eq!(ran.code, Some(0));
}

/// The whole point of a closure: it reads a binding from where it was written,
/// not from where it is called.
#[test]
fn a_closure_captures_its_environment() {
    let ran = run(
        "closure_capture",
        "module t;
fn print(value: Int);

fn make(n: Int) -> (Int) -> Int { fn x => x + n }

fn main() -> Int {
  let add_ten = make(10);
  let add_hundred = make(100);
  print(add_ten(5));
  print(add_hundred(5));
  0
}
",
    );
    assert_eq!(ran.stdout, "15\n105\n", "each closure keeps its own captured `n`");
    assert_eq!(ran.code, Some(0));
}

#[test]
fn a_nested_closure_reaches_through_to_the_outer_scope() {
    let ran = run(
        "closure_nested",
        "module t;
fn print(value: Int);

fn main() -> Int {
  let base = 1000;
  let outer = fn x => fn y => x + y + base;
  let inner = outer(20);
  print(inner(3));
  0
}
",
    );
    assert_eq!(ran.stdout, "1023\n");
    assert_eq!(ran.code, Some(0));
}

/// A closure in a generic function is emitted once per specialization, because
/// what it captures has a different machine type in each.
#[test]
fn a_closure_inside_a_generic_function_is_specialized() {
    let ran = run(
        "closure_generic",
        "module t;
fn print(value: Int);

fn twice<A>(f: (A) -> A, x: A) -> A { f(f(x)) }

fn main() -> Int {
  print(twice(fn n => n + 3, 1));
  0
}
",
    );
    assert_eq!(ran.stdout, "7\n");
    assert_eq!(ran.code, Some(0));
}

/// Closures are heap objects under the same header as everything else, so the
/// same counting has to account for them — including the references they hold
/// to what they captured.
#[test]
fn closures_and_their_captures_are_freed() {
    let ran = run(
        "closure_leaks",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export type List = | Nil | Cons(head: Int, tail: List);

fn build(n: Int) -> List {
  if n == 0 { List::Nil } else { List::Cons(n, build(n - 1)) }
}

fn sum(l: List) -> Int {
  match l { List::Nil => 0, List::Cons(h, t) => h + sum(t) }
}

fn call_it(f: (Int) -> Int, x: Int) -> Int { f(x) }

/// The closure owns a reference to the captured list.
fn captures_a_list() -> Int {
  let l = build(4);
  let f = fn x => x + sum(l);
  call_it(f, 1)
}

/// A boxed argument is owned by the lambda, which releases it.
fn boxed_parameter() -> Int {
  let g = fn l => sum(l);
  g(build(3))
}

/// Built, never called: the captures still have to be let go.
fn discarded() {
  let l = build(2);
  fn x => x + sum(l);
}

fn main() -> Int {
  khora_print_int(captures_a_list());
  khora_print_int(boxed_parameter());
  discarded();
  khora_print_int(khora_live_count());
  0
}
",
    );
    assert_eq!(ran.stdout, "11\n6\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// The positive control for the test above: without it, a live count of zero
/// could mean the counter is broken rather than the program clean.
#[test]
fn a_leaked_closure_is_actually_observable() {
    let ran = run(
        "closure_leak_control",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;
fn khora_dup(object: String);

fn main() -> Int {
  let s = \"held\";
  khora_dup(s);
  let f = fn x => x + 0;
  khora_print_int(khora_live_count());
  0
}
",
    );
    assert_eq!(ran.stdout, "2\n", "an extra reference and a live closure are both counted");
    assert_eq!(ran.code, Some(0));
}

/// A named function is a value too: it becomes a closure that captures nothing
/// and forwards. Without this, every `map(xs, f)` would need a lambda wrapper.
#[test]
fn a_named_function_can_be_passed_as_a_value() {
    let ran = run(
        "fn_value",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

fn double(x: Int) -> Int { x * 2 }
fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }

/// Holds the adapter in a binding, so the block that declared it is what
/// releases it. Measured after this returns, or the count would include it.
fn through_a_binding() -> Int {
  let g = double;
  g(4)
}

fn main() -> Int {
  khora_print_int(apply(double, 21));
  khora_print_int(through_a_binding());
  khora_print_int(khora_live_count());
  0
}
",
    );
    assert_eq!(ran.stdout, "42\n8\n0\n", "the adapter object has to be freed too");
    assert_eq!(ran.code, Some(0));
}

/// A generic function used as a value resolves to the specialization the call
/// site asked for, exactly as a direct call would.
#[test]
fn a_generic_function_as_a_value_picks_its_specialization() {
    let ran = run(
        "fn_value_generic",
        "module t;
fn print(value: Int);

fn id<A>(x: A) -> A { x }
fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }

fn main() -> Int {
  print(apply(id, 7));
  0
}
",
    );
    assert_eq!(ran.stdout, "7\n");
    assert_eq!(ran.code, Some(0));
}

/// A type's own methods, with no trait declared anywhere. This is the first
/// thing a Go, TypeScript or Rust developer does, and until it worked every
/// private helper needed a public abstraction invented for it.
#[test]
fn a_type_can_have_methods_without_a_trait() {
    let ran = run(
        "inherent",
        "module t;
fn print(value: Int);

export type User = | Of(age: Int);

impl User {
  fn age(self) -> Int { match self { User::Of(a) => a } }
  fn birthday(self) -> User { User::Of(self.age() + 1) }
}

fn main() -> Int {
  let u = User::Of(41);
  print(u.birthday().age());
  0
}
",
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// A type's own method wins over a trait method of the same name, so adding a
/// trait to a program cannot silently change what an existing call does.
#[test]
fn an_inherent_method_wins_over_a_trait_method() {
    let ran = run(
        "inherent_shadow",
        "module t;
fn print(value: Int);

export type User = | Of(age: Int);

trait Describe { fn describe(self) -> Int; }

impl Describe for User { fn describe(self) -> Int { 1 } }
impl User { fn describe(self) -> Int { 2 } }

fn main() -> Int {
  print(User::Of(0).describe());
  0
}
",
    );
    assert_eq!(ran.stdout, "2\n", "the type's own method should be the one that runs");
    assert_eq!(ran.code, Some(0));
}

/// An inherent impl over a constructor learns its parameter from the receiver.
#[test]
fn a_parameterised_inherent_impl_runs() {
    let ran = run(
        "inherent_generic",
        "module t;
fn print(value: Int);

export type Wrapper<A> = | Of(value: A);

impl<A> Wrapper<A> {
  fn tag(self) -> Int { 7 }
}

fn main() -> Int {
  print(Wrapper::Of(1).tag());
  print(Wrapper::Of(true).tag());
  0
}
",
    );
    assert_eq!(ran.stdout, "7\n7\n");
    assert_eq!(ran.code, Some(0));
}

/// Methods on a type holding a reference count still release it.
#[test]
fn an_inherent_method_does_not_leak_its_receiver() {
    let ran = run(
        "inherent_leaks",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export type List = | Nil | Cons(head: Int, tail: List);

impl List {
  fn sum(self) -> Int {
    match self { List::Nil => 0, List::Cons(h, t) => h + t.sum() }
  }
}

fn build(n: Int) -> List {
  if n == 0 { List::Nil } else { List::Cons(n, build(n - 1)) }
}

fn total() -> Int { build(4).sum() }

fn main() -> Int {
  khora_print_int(total());
  khora_print_int(khora_live_count());
  0
}
",
    );
    assert_eq!(ran.stdout, "10\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// A higher-kinded trait, compiled and run. `Self<A>` against `Option<Int>`
/// has to decide `Self := Option` and `A := Int` separately, and the result
/// keeps the receiver's constructor with a new element type.
#[test]
fn a_higher_kinded_trait_runs() {
    let ran = run(
        "hkt",
        "module t;
fn print(value: Int);

export type Option<A> = | Some(value: A) | None;

trait Functor {
  fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;
}

impl Functor for Option {
  fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> {
    match self {
      Option::Some(v) => Option::Some(f(v)),
      Option::None => Option::None,
    }
  }
}

fn unwrap_or<A>(o: Option<A>, fallback: A) -> A {
  match o { Option::Some(v) => v, Option::None => fallback }
}

fn main() -> Int {
  print(unwrap_or(Option::Some(20).map(fn x => x + 22), 0));
  print(unwrap_or(Option::None.map(fn x => x + 1), 99));
  0
}
",
    );
    assert_eq!(ran.stdout, "42
99
");
    assert_eq!(ran.code, Some(0));
}

/// Phase 3's exit criterion: a `traverse` written against *any* `Applicative`,
/// working over `Option`, `List` and a user type, compiled to native code.
///
/// Everything the language has is load-bearing here at once — higher-kinded
/// traits, bounded type parameters, a trait function called with no receiver
/// (`F::pure`), closures passed through a recursive generic call, and static
/// dispatch selecting a different impl per instantiation. It is the single
/// best regression test in the repository for that reason.
#[test]
fn traverse_works_over_three_containers() {
    let ran = run(
        "traverse",
        "module t;
fn print(value: Int);

export type Option<A> = | Some(value: A) | None;
export type List<A> = | Nil | Cons(head: A, tail: List<A>);
export type Pair<A> = | Of(first: A, second: A);

export trait Applicative {
  fn pure<A>(value: A) -> Self<A>;
  fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;
  fn map2<A, B, C>(self: Self<A>, other: Self<B>, f: (A, B) -> C) -> Self<C>;
}

impl Applicative for Option {
  fn pure<A>(value: A) -> Option<A> { Option::Some(value) }
  fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> {
    match self { Option::Some(v) => Option::Some(f(v)), Option::None => Option::None }
  }
  fn map2<A, B, C>(self: Option<A>, other: Option<B>, f: (A, B) -> C) -> Option<C> {
    match self {
      Option::Some(a) => match other {
        Option::Some(b) => Option::Some(f(a, b)),
        Option::None => Option::None,
      },
      Option::None => Option::None,
    }
  }
}

// ONE traverse per container, written against any Applicative.
export trait Traversable {
  fn traverse<A, B, F: Applicative>(self: Self<A>, f: (A) -> F<B>) -> F<Self<B>>;
}

impl Traversable for Option {
  fn traverse<A, B, F: Applicative>(self: Option<A>, f: (A) -> F<B>) -> F<Option<B>> {
    match self {
      Option::Some(v) => f(v).map(fn b => Option::Some(b)),
      Option::None => F::pure(Option::None),
    }
  }
}

impl Traversable for List {
  fn traverse<A, B, F: Applicative>(self: List<A>, f: (A) -> F<B>) -> F<List<B>> {
    match self {
      List::Nil => F::pure(List::Nil),
      List::Cons(h, t) => f(h).map2(t.traverse(f), fn (b, rest) => List::Cons(b, rest)),
    }
  }
}

impl Traversable for Pair {
  fn traverse<A, B, F: Applicative>(self: Pair<A>, f: (A) -> F<B>) -> F<Pair<B>> {
    match self {
      Pair::Of(x, y) => f(x).map2(f(y), fn (a, b) => Pair::Of(a, b)),
    }
  }
}

fn halve(n: Int) -> Option<Int> {
  if n % 2 == 0 { Option::Some(n / 2) } else { Option::None }
}

fn sum(l: List<Int>) -> Int {
  match l { List::Nil => 0, List::Cons(h, t) => h + sum(t) }
}

fn or_else(o: Option<Option<Int>>, fallback: Int) -> Int {
  match o {
    Option::Some(inner) => match inner {
      Option::Some(v) => v,
      Option::None => fallback,
    },
    Option::None => fallback,
  }
}

fn list_or(o: Option<List<Int>>, fallback: Int) -> Int {
  match o { Option::Some(l) => sum(l), Option::None => fallback }
}

fn pair_or(o: Option<Pair<Int>>, fallback: Int) -> Int {
  match o { Option::Some(p) => match p { Pair::Of(a, b) => a + b }, Option::None => fallback }
}

export fn main() -> Int {
  // The same `halve` traversed through three different containers.
  print(or_else(Option::Some(8).traverse(halve), 0 - 1));
  print(or_else(Option::Some(7).traverse(halve), 0 - 1));

  let evens = List::Cons(4, List::Cons(6, List::Nil));
  let odd = List::Cons(4, List::Cons(7, List::Nil));
  print(list_or(evens.traverse(halve), 0 - 1));
  print(list_or(odd.traverse(halve), 0 - 1));

  print(pair_or(Pair::Of(10, 20).traverse(halve), 0 - 1));
  print(pair_or(Pair::Of(10, 21).traverse(halve), 0 - 1));
  0
}
",
    );
    assert_eq!(
        ran.stdout, "4\n-1\n5\n-1\n15\n-1\n",
        "even inputs halve and survive; one odd input collapses the whole traversal"
    );
    assert_eq!(ran.code, Some(0));
}

const ITERATOR: &str = "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export type Step<S, A> = | Yield(state: S, item: A) | Done;
export type Range = | Of(from: Int, to: Int);
export type List<A> = | Nil | Cons(head: A, tail: List<A>);

trait Iterator {
  type Item;
  fn next(self) -> Step<Self, Self::Item>;
}

impl Iterator for Range {
  type Item = Int;
  fn next(self) -> Step<Range, Int> {
    match self {
      Range::Of(from, to) =>
        if from >= to { Step::Done } else { Step::Yield(Range::Of(from + 1, to), from) },
    }
  }
}

impl<A> Iterator for List<A> {
  type Item = A;
  fn next(self) -> Step<List<A>, A> {
    match self { List::Nil => Step::Done, List::Cons(h, t) => Step::Yield(t, h) }
  }
}
";

/// Phase 3's other exit criterion: `for` iterates a user-defined type. Nothing
/// about `Range` is built in — it is an ordinary ADT with an `Iterator` impl.
#[test]
fn a_for_loop_iterates_a_user_defined_type() {
    let ran = run(
        "for_range",
        &format!(
            "{ITERATOR}
fn main() -> Int {{
  let mut total = 0;
  for n in Range::Of(1, 6) {{
    total = total + n;
  }}
  khora_print_int(total);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "15\n", "1 + 2 + 3 + 4 + 5");
    assert_eq!(ran.code, Some(0));
}

/// `break` and `continue` reach the desugared loop like any other.
#[test]
fn break_and_continue_work_inside_a_for_loop() {
    let ran = run(
        "for_jumps",
        &format!(
            "{ITERATOR}
fn main() -> Int {{
  let mut total = 0;
  for n in Range::Of(1, 100) {{
    if n == 4 {{ break; }}
    if n == 2 {{ continue; }}
    total = total + n;
  }}
  khora_print_int(total);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n", "1 + 3, skipping 2 and stopping at 4");
    assert_eq!(ran.code, Some(0));
}

/// Iterating a heap-allocated structure must not leak the cells it walks past.
#[test]
fn a_for_loop_over_a_boxed_structure_does_not_leak() {
    let ran = run(
        "for_leaks",
        &format!(
            "{ITERATOR}
fn total() -> Int {{
  let mut sum = 0;
  for n in List::Cons(10, List::Cons(20, List::Cons(12, List::Nil))) {{
    sum = sum + n;
  }}
  sum
}}

fn main() -> Int {{
  khora_print_int(total());
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// A generic container's declared field type is a parameter, and a parameter is
/// never boxed — so asking the *declaration* whether `Wrapper<A>` owns anything
/// always answered no, and every `Wrapper<String>` leaked its contents. Drop glue
/// is emitted per instantiation for this reason.
#[test]
fn a_generic_container_releases_what_it_holds() {
    let ran = run(
        "generic_glue",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export type Wrapper<A> = | Of(value: A);

fn holds_a_string() { let b = Wrapper::Of(\"held\"); }
fn holds_an_int() { let b = Wrapper::Of(1); }

fn main() -> Int {
  holds_an_int();
  khora_print_int(khora_live_count());
  holds_a_string();
  khora_print_int(khora_live_count());
  0
}
",
    );
    assert_eq!(ran.stdout, "0\n0\n", "both instantiations must come out clean");
    assert_eq!(ran.code, Some(0));
}

/// The counterpart: a generic *function* over a boxed type has to dup and drop
/// it. The plan is made per specialization because `A` is unboxed in the
/// generic body and a counted pointer at `A = String`.
#[test]
fn a_generic_function_counts_its_boxed_arguments() {
    let ran = run(
        "generic_rc",
        "module t;
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;
fn print(value: String);

export type Pair<A> = | Of(first: A, second: A);

fn duplicate<A>(x: A) -> Pair<A> { Pair::Of(x, x) }

fn strings() {
  let p = duplicate(\"held\");
  match p { Pair::Of(a, b) => print(a) }
}

fn ints() -> Int {
  match duplicate(21) { Pair::Of(a, b) => a + b }
}

fn main() -> Int {
  khora_print_int(ints());
  strings();
  khora_print_int(khora_live_count());
  0
}
",
    );
    assert_eq!(ran.stdout, "42\nheld\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// A tag is an index within *one* type's variant list, so resolving a
/// constructor by its bare name returns another type's tag and the wrong
/// `match` arm runs. The two types here declare the same cases in opposite
/// order precisely so that a bare-name lookup produces wrong output rather
/// than a type error.
#[test]
fn a_constructors_tag_comes_from_its_own_type() {
    let ran = run(
        "tags",
        "module t;
fn print(value: Int);

export type First = | A | B;
export type Second = | B | A;

fn which(s: Second) -> Int { match s { Second::B => 1, Second::A => 2 } }

fn main() -> Int {
  print(which(Second::A));
  print(which(Second::B));
  0
}
",
    );
    assert_eq!(ran.stdout, "2\n1\n", "a bare-name lookup would swap these");
    assert_eq!(ran.code, Some(0));
}
