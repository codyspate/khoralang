#![cfg(feature = "llvm")]

//! What a trap says about where it happened.
//!
//! **Compiled at [`Profile::Debug`] explicitly**, which is the profile these
//! are *about*: a release build emits no line tables, so under
//! `KHORA_PROFILE=release` every assertion here would fail while nothing was
//! wrong. Naming the profile is the same reasoning as asserting on a program's
//! output rather than on its metadata — say what is being tested.
//!
//! `khora_bounds_fail`'s own doc comment said "the useful thing to do is say
//! where", and until the compiler emitted line tables there was no way for it
//! to. Both traps named what had happened — `Int addition overflowed`, `index 7
//! is outside an array of 3` — in a program of any size, with nothing
//! connecting the message to a line.
//!
//! These run the whole path: Khora source, DWARF or CodeView emitted by the
//! backend, kept through the link, symbolized at runtime by the executable's
//! own debug information. Asserting on the *output of a program that trapped*
//! rather than on the metadata, because every one of those steps has already
//! been a place where it silently stopped working — the object carried
//! `.debug$S` and `.debug$T` for a while before anybody noticed the linker was
//! discarding both.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_codegen_llvm::Profile;
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Compiles `source`, runs it with backtraces on, and returns what it printed
/// to stderr.
fn trap_of(name: &str, source: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    if let Err(errors) = khora_codegen_llvm::compile_with(&db, root, &exe, Profile::Debug) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let output = Command::new(&exe)
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("the program should run");
    assert_eq!(output.status.code(), Some(134), "a trap exits 134");
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

/// Overflow deep in a call chain names the function and the line it happened
/// on, and the callers underneath it.
#[test]
fn an_overflow_says_which_line_overflowed() {
    let out = trap_of(
        "trap_overflow_where",
        "module t;
fn print(value: Int);

fn deep(n: Int, big: Int) -> Int {
  big + n
}

fn middle(n: Int, big: Int) -> Int {
  deep(n, big) + 1
}

fn main() -> Int {
  let big = 9223372036854775807;
  print(middle(1, big));
  0
}
",
    );

    assert!(out.contains("overflowed"), "it still says what happened: {out}");
    assert!(out.contains("main.kh:5"), "the line that overflowed: {out}");
    // The callers, so a trap in a helper can be traced back to the request
    // that reached it — which is the whole reason a backtrace beats a line.
    assert!(out.contains("main.kh:9"), "the caller: {out}");
    assert!(out.contains("main.kh:14"), "and its caller: {out}");
    // Named, not mangled. `create_function` is given both, and a reader wants
    // the one they wrote.
    assert!(out.contains("deep"), "the function's own name: {out}");
}

/// **The runtime's own frames are not the answer.** Six frames of
/// `backtrace_rs` and `force_capture` sat above the line that trapped, and the
/// top of a backtrace is the part anybody reads first.
#[test]
fn the_runtimes_frames_are_not_at_the_top() {
    let out = trap_of(
        "trap_no_runtime_frames",
        "module t;
fn print(value: Int);

fn main() -> Int {
  let big = 9223372036854775807;
  print(big + 1);
  0
}
",
    );
    assert!(!out.contains("backtrace_rs"), "the capture machinery is trimmed: {out}");
    assert!(!out.contains("khora_rt::trap"), "and so is the handler: {out}");
    assert!(out.contains("main.kh:6"), "leaving the line that trapped: {out}");
}

/// Without the switch, a trap says how to get the rest. A backtrace on every
/// trap costs every well-behaved program a page of stack on the way out, and
/// the first thing anybody does with a bug is run it again.
#[test]
fn a_trap_without_the_switch_says_how_to_get_more() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("trap_quiet");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let source = "module t;
fn print(value: Int);

fn main() -> Int {
  let big = 9223372036854775807;
  print(big + 1);
  0
}
";
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    khora_codegen_llvm::compile_with(&db, root, &exe, Profile::Debug).expect("it compiles");

    let output = Command::new(&exe)
        .env_remove("RUST_BACKTRACE")
        .output()
        .expect("the program should run");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("overflowed"), "what happened is always said: {err}");
    assert!(err.contains("RUST_BACKTRACE=1"), "and how to learn where: {err}");
}

/// An index outside its array, which is the other trap and reaches the same
/// place by a different route.
#[test]
fn a_bounds_failure_says_which_line_indexed() {
    let out = trap_of(
        "trap_bounds_where",
        "module t;
fn print(value: Int);

pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn get(self, index: Int) -> A;
}

fn main() -> Int {
  let xs = Array::new(3, 0);
  print(Array::get(xs, 7));
  0
}
",
    );
    assert!(out.contains("outside an array"), "it still says what happened: {out}");
    assert!(out.contains("main.kh:12"), "the line that indexed: {out}");
}

/// A trap on a spawned fiber says which one.
///
/// **A trap takes the whole process down** — `docs/design/traps.md` argues why
/// that is the current answer and what containing it would cost. Until it is
/// contained, the least a server can be told is which of its concurrent pieces
/// of work was wrong: "some addition overflowed" and "fiber 2's addition
/// overflowed" are a different amount of help on a machine running a thousand
/// at once, because the second can be matched against a request log.
#[test]
fn a_trap_on_a_fiber_says_which_fiber() {
    let out = trap_of(
        "trap_on_a_fiber",
        "module t;
fn print(value: Int);

pub type Fiber;
impl Fiber {
  fn spawn<'e>(body: () -> () raises 'e) -> Fiber;
  fn join(self) -> ();
}

fn work() -> () {
  let big = 9223372036854775807;
  print(big + 1)
}

fn main() -> Int {
  let f = Fiber::spawn(fn () => work());
  Fiber::join(f);
  0
}
",
    );
    assert!(out.contains("overflowed on fiber "), "the fiber is named: {out}");
    assert!(out.contains("main.kh:12"), "and the line still is: {out}");
    assert!(out.contains("work"), "on a fiber the frames still resolve: {out}");
}

/// The root fiber is not numbered. There is nothing to disambiguate, and a
/// number on every single-threaded program's worst day is noise.
#[test]
fn a_trap_on_the_root_fiber_is_not_numbered() {
    let out = trap_of(
        "trap_on_the_root",
        "module t;
fn print(value: Int);

fn main() -> Int {
  let big = 9223372036854775807;
  print(big + 1);
  0
}
",
    );
    assert!(out.contains("overflowed\n"), "no fiber clause: {out}");
}

// --- and the locals in a frame --------------------------------------------

/// Every local is named, at its own line, with its own type.
///
/// **Asserted against the emitted IR, which is weaker than the rest of this
/// file and is said so deliberately.** Everything above runs a program because
/// the failure mode was metadata that was perfect and an artefact that could
/// not use it. The equivalent here would be driving `lldb`, which is not
/// something a `cargo test` can rely on finding — so this checks the
/// `DILocalVariable` records and their types, and the artefact-level check
/// stays the backtrace tests above, which share the same emission path.
///
/// What it does prove is the part that was wrong twice: names, the lines they
/// were declared on, and that a scalar is described *as* a scalar rather than
/// as an opaque word.
#[test]
fn every_local_is_named_with_its_type() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("debug_locals");
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });

    let db = KhoraDatabase::new();
    let source = "module t;
fn print(value: Int);

fn main() -> Int {
  let units = 7;
  let wide: U8 = 3;
  let flag = true;
  let ratio = 1.5;
  let label = \"widgets\";
  let total = units * 6;
  print(total);
  0
}
";
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    // SAFETY-of-a-sort: process-wide, and this is the only test that sets it.
    unsafe { std::env::set_var("KHORA_EMIT_LLVM", "1") };
    let outcome = khora_codegen_llvm::compile_with(&db, root, &exe, Profile::Debug);
    unsafe { std::env::remove_var("KHORA_EMIT_LLVM") };
    outcome.expect("it compiles");

    let ir = std::fs::read_to_string(dir.join(
        if cfg!(windows) { "program.exe.ll" } else { "program.ll" },
    ))
    .expect("the IR was dumped");

    // The file this program is in, so `std`'s thousand locals are not counted.
    let mine = ir
        .lines()
        .find(|l| l.contains("!DIFile(filename: \"main.kh\""))
        .and_then(|l| l.split(' ').next())
        .expect("main.kh has a DIFile")
        .to_string();

    let named: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("!DILocalVariable") && l.contains(&format!("file: {mine},")))
        .collect();

    for (name, line) in
        [("units", 5), ("wide", 6), ("flag", 7), ("ratio", 8), ("label", 9), ("total", 10)]
    {
        let found = named
            .iter()
            .find(|l| l.contains(&format!("name: \"{name}\"")))
            .unwrap_or_else(|| panic!("`{name}` should be a local, got:\n{}", named.join("\n")));
        assert!(
            found.contains(&format!("line: {line},")),
            "`{name}` should be declared on line {line}: {found}"
        );
    }

    // **Scalars are described, not merely counted.** A local whose type is a
    // word-shaped nothing is a name in a list; one with an encoding prints.
    assert!(
        ir.contains(r#"!DIBasicType(name: "Int", size: 64, encoding: DW_ATE_signed"#),
        "an `Int` is a signed 64-bit integer"
    );
    assert!(
        ir.contains(r#"!DIBasicType(name: "Bool", size: 8, encoding: DW_ATE_boolean"#),
        "a `Bool` is a boolean"
    );
    assert!(
        ir.contains(r#"!DIBasicType(name: "Float", size: 64, encoding: DW_ATE_float"#),
        "a `Float` is a float"
    );
    assert!(
        ir.contains(r#"!DIBasicType(name: "U8", size: 8, encoding: DW_ATE_unsigned"#),
        "a `U8` is unsigned and eight bits"
    );
    // And a boxed value is a *named pointer*: the address and the type's name,
    // which is what a frame can show without the heap layout being described.
    assert!(
        ir.contains(r#"DW_TAG_pointer_type, name: "String""#),
        "a `String` is a pointer that knows what it points at"
    );
}
