#![cfg(feature = "llvm")]

//! The foreign boundary: what may cross it, and what a `with` clause means on
//! the far side of it.
//!
//! A function declared without a body is a foreign function. `docs/design/ffi.md`
//! is the contract; this is the test that the compiler holds to it. Every
//! rejection here used to compile, and would have handed C something it could
//! not read — the same shape of mistake as errata 35, which cost a day and
//! reported every failing test as passing.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn build(name: &str, source: &str) -> Result<PathBuf, Vec<String>> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => Ok(exe),
        Err(errors) => Err(errors.into_iter().map(|e| e.message).collect()),
    }
}

fn refused(name: &str, source: &str) -> Vec<String> {
    match build(name, source) {
        Ok(_) => panic!("`{name}` should have been refused at the boundary:\n\n{source}"),
        Err(messages) => messages,
    }
}

fn rejects(name: &str, declaration: &str, call: &str, needle: &str) {
    let source = format!(
        "module t;
fn khora_print_int(value: Int);
export type Pair = {{ a: Int, b: Int }};
export type Oops = | Bad;

{declaration}

fn main() -> Int {{ {call} 0 }}
"
    );
    let found = refused(name, &source);
    assert!(
        found.iter().any(|m| m.contains(needle) && m.contains("foreign function")),
        "expected a boundary error mentioning {needle:?}, got {found:?}"
    );
}

/// Scalars are the whole of what crosses today. Each of these is a C type of
/// the same width, and the declaration is accepted — the *symbol* is missing,
/// which is a link error rather than a boundary one, so this checks the
/// message rather than the outcome.
#[test]
fn scalars_cross() {
    let found = refused(
        "ffi_scalars",
        "module t;
fn khora_print_int(value: Int);
fn takes_numbers(a: Int, b: U8, c: I32, d: Float, e: Bool) -> I64;

fn main() -> Int {
  khora_print_int(takes_numbers(1, 2, 3, 4.0, true));
  0
}
",
    );
    assert!(
        found.iter().all(|m| !m.contains("foreign function")),
        "a signature of scalars is allowed to cross; only the symbol is missing: {found:?}"
    );
    assert!(
        found.iter().any(|m| m.contains("undefined symbol") || m.contains("link")),
        "expected a link error, got {found:?}"
    );
}

/// A Khora object is a reference-counted heap allocation with a header the C
/// side knows nothing about. Handing one over gives the callee a pointer it
/// cannot read and a reference it cannot release.
#[test]
fn a_record_may_not_cross() {
    rejects(
        "ffi_record",
        "fn takes_a_record(p: Pair) -> Int;",
        "khora_print_int(takes_a_record({ a: 1, b: 2 }));",
        "`Pair` cannot cross",
    );
}

#[test]
fn a_string_may_not_cross() {
    rejects(
        "ffi_string",
        "fn takes_a_string(s: String) -> Int;",
        "khora_print_int(takes_a_string(\"hi\"));",
        "`String` cannot cross",
    );
}

/// A closure is a heap object holding its captures, called through an adapter.
/// C expects a bare function pointer.
#[test]
fn a_closure_may_not_cross() {
    rejects(
        "ffi_closure",
        "fn takes_a_closure(f: (Int) -> Int) -> Int;",
        "khora_print_int(takes_a_closure(fn (x) => x + 1));",
        "cannot cross",
    );
}

/// The one errata 35 is about. A fallible function returns `{ i32, i64 }`, and
/// how a 16-byte aggregate comes back is a target decision each side makes
/// separately — silently, and in the direction that reads a failure as a pass.
#[test]
fn a_foreign_function_may_not_raise() {
    let found = refused(
        "ffi_raises",
        "module t;
fn khora_print_int(value: Int);
export type Oops = | Bad;
fn can_fail(n: Int) -> Int raises Oops;

fn wrapper() -> Int raises Oops { can_fail(1)! }

fn main() -> Int { 0 }
",
    );
    assert!(
        found.iter().any(|m| m.contains("it can raise") && m.contains("errata 35")),
        "expected the tagged-return reason, got {found:?}"
    );
}

/// No body means nothing to specialize.
#[test]
fn a_foreign_function_may_not_be_generic() {
    rejects(
        "ffi_generic",
        "fn identity<A>(value: A) -> A;",
        "khora_print_int(identity(1));",
        "it is generic",
    );
}

/// A returned object is the same mistake in the other direction.
#[test]
fn a_foreign_function_may_not_return_an_object() {
    rejects(
        "ffi_returns_object",
        "fn makes_a_record() -> Pair;",
        "khora_print_int(makes_a_record().a);",
        "return type `Pair` cannot cross",
    );
}

/// **A `with` clause on a foreign function is a permission, not an argument.**
///
/// The caller must hold the capability — that is how the boundary is governed,
/// since nothing can reach the outside world without something `main` handed
/// down — but nothing is appended to the call, because a C function has no use
/// for a Khora record of closures. Decision 3 in `docs/design/ffi.md`.
///
/// The proof is that this *compiles*: the signature has a `with` row and takes
/// only scalars, so if evidence were being appended the boundary check would
/// have seen an extra parameter it could not carry.
#[test]
fn a_capability_is_required_but_not_passed() {
    let found = refused(
        "ffi_capability",
        "module t;
fn khora_print_int(value: Int);

export effect Fs { open: (Int) -> Int, }

fn sys_open(flags: I32) -> I32 with { fs: Fs };

fn run() -> Int with { fs: Fs } { I32::to_int(sys_open(0)) }

fn main() -> Int {
  with { fs: handler for Fs { open: fn n => n } } { khora_print_int(run()); }
  0
}

impl I32 { fn to_int(self) -> Int; }
",
    );
    assert!(
        found.iter().all(|m| !m.contains("foreign function")),
        "a `with` row is a permission and must not become a parameter: {found:?}"
    );
    assert!(
        found.iter().any(|m| m.contains("undefined symbol") || m.contains("link")),
        "expected only the missing symbol, got {found:?}"
    );
}

/// And the requirement has teeth: a caller that does not hold the capability
/// is refused before the boundary is ever reached.
#[test]
fn a_caller_without_the_capability_is_refused() {
    let found = refused(
        "ffi_capability_missing",
        "module t;
fn khora_print_int(value: Int);

export effect Fs { open: (Int) -> Int, }

fn sys_open(flags: I32) -> I32 with { fs: Fs };

fn main() -> Int { khora_print_int(I32::to_int(sys_open(0))); 0 }

impl I32 { fn to_int(self) -> Int; }
",
    );
    assert!(
        found.iter().any(|m| m.contains("fs") || m.contains("Fs")),
        "expected the missing capability to be named, got {found:?}"
    );
}

/// A `Ptr` is what makes the contract above more than a description of what is
/// forbidden: it is the one thing besides a number that may cross.
///
/// The declaration is accepted and only the symbol is missing, which is the
/// same shape as `scalars_cross`.
#[test]
fn a_pointer_crosses() {
    let found = refused(
        "ffi_ptr",
        "module t;
fn khora_print_int(value: Int);
fn sys_open(flags: I32) -> Ptr;
fn sys_read(handle: Ptr, into: Ptr, len: Int) -> Int;
fn sys_close(handle: Ptr) -> ();

fn main() -> Int {
  let handle = sys_open(0);
  if Ptr::is_null(handle) { 0 } else {
    khora_print_int(sys_read(handle, Ptr::null(), 8));
    sys_close(handle);
    0
  }
}

impl Ptr {
  fn null() -> Ptr;
  fn is_null(self) -> Bool;
}
",
    );
    assert!(
        found.iter().all(|m| !m.contains("foreign function")),
        "a pointer is allowed to cross: {found:?}"
    );
    assert!(
        found.iter().any(|m| m.contains("undefined symbol") || m.contains("link")),
        "expected only the missing symbols, got {found:?}"
    );
}

/// `Ptr::null` and `Ptr::is_null` are the whole of what a `Ptr` can do, and
/// they run.
#[test]
fn a_null_pointer_knows_it_is_null() {
    let exe = build(
        "ffi_null",
        "module t;
fn khora_print_int(value: Int);

impl Ptr {
  fn null() -> Ptr;
  fn is_null(self) -> Bool;
}

fn main() -> Int {
  khora_print_int(if Ptr::is_null(Ptr::null()) { 1 } else { 0 });
  0
}
",
    )
    .expect("a program that only makes and tests a null pointer");

    let output = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"), "1\n");
    assert_eq!(output.status.code(), Some(0));
}

/// There is no way to *make* a `Ptr` from a Khora value, which is what keeps a
/// dangling one impossible. A buffer is the harder question and is unanswered
/// on purpose — this pins that it stays unanswered rather than drifting into
/// existence.
#[test]
fn a_khora_object_cannot_be_turned_into_a_pointer() {
    let found = refused(
        "ffi_no_address_of",
        "module t;
fn khora_print_int(value: Int);

impl String {
  fn data(self) -> Ptr;
}

fn main() -> Int { khora_print_int(if Ptr::is_null(String::data(\"hi\")) { 1 } else { 0 }); 0 }

impl Ptr {
  fn is_null(self) -> Bool;
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("String::data")),
        "expected `String::data` to be unknown, got {found:?}"
    );
}
