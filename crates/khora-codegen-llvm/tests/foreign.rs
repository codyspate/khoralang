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

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let exe = match build(name, source) {
        Ok(exe) => exe,
        Err(messages) => panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  ")),
    };
    let output = std::process::Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
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
extern fn khora_print_int(value: Int);
pub type Pair = {{ a: Int, b: Int }};
pub type Oops = | Bad;

{declaration}

fn main() -> Int {{ {call} 0 }}
"
    );
    let found = refused(name, &source);
    assert!(
        found.iter().any(|m| m.contains(needle) && m.contains("crosses the C ABI")),
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
extern fn khora_print_int(value: Int);
extern fn takes_numbers(a: Int, b: U8, c: I32, d: Float, e: Bool) -> I64;

fn main() -> Int {
  khora_print_int(takes_numbers(1, 2, 3, 4.0, true));
  0
}
",
    );
    assert!(
        found.iter().all(|m| !m.contains("crosses the C ABI") && !m.contains("nothing to call")),
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
        "extern fn takes_a_record(p: Pair) -> Int;",
        "khora_print_int(takes_a_record({ a: 1, b: 2 }));",
        "`Pair` cannot cross",
    );
}

#[test]
fn a_string_may_not_cross() {
    rejects(
        "ffi_string",
        "extern fn takes_a_string(s: String) -> Int;",
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
        "extern fn takes_a_closure(f: (Int) -> Int) -> Int;",
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
extern fn khora_print_int(value: Int);
pub type Oops = | Bad;
extern fn can_fail(n: Int) -> Int raises Oops;

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
        "extern fn identity<A>(value: A) -> A;",
        "khora_print_int(identity(1));",
        "it is generic",
    );
}

/// A returned object is the same mistake in the other direction.
#[test]
fn a_foreign_function_may_not_return_an_object() {
    rejects(
        "ffi_returns_object",
        "extern fn makes_a_record() -> Pair;",
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
extern fn khora_print_int(value: Int);

pub effect Fs { open: (Int) -> Int, }

extern fn sys_open(flags: I32) -> I32 with { fs: Fs };

fn run() -> Int with { fs: Fs } { I32::to_int(sys_open(0)) }

fn main() -> Int {
  with { fs: handler for Fs { open: fn n => n } } { khora_print_int(run()); }
  0
}

impl I32 { fn to_int(self) -> Int; }
",
    );
    assert!(
        found.iter().all(|m| !m.contains("crosses the C ABI") && !m.contains("nothing to call")),
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
extern fn khora_print_int(value: Int);

pub effect Fs { open: (Int) -> Int, }

extern fn sys_open(flags: I32) -> I32 with { fs: Fs };

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
extern fn khora_print_int(value: Int);
extern fn sys_open(flags: I32) -> Ptr;
extern fn sys_read(handle: Ptr, into: Ptr, len: Int) -> Int;
extern fn sys_close(handle: Ptr) -> ();

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
        found.iter().all(|m| !m.contains("crosses the C ABI") && !m.contains("nothing to call")),
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
extern fn khora_print_int(value: Int);

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
extern fn khora_print_int(value: Int);

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

// --- lending a buffer -------------------------------------------------------

const LEND: &str = "module t;
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
  fn get(self, index: Int) -> A;
  fn set(self, index: Int, value: A) -> ();
  fn with_data<B, 'c, 'e>(self, body: (Ptr, Int) -> B with 'c raises 'e) -> B
    with 'c
    raises 'e;
}

impl String {
  fn byte_length(self) -> Int;
  fn with_data<B, 'c, 'e>(self, body: (Ptr, Int) -> B with 'c raises 'e) -> B
    with 'c
    raises 'e;
  fn with_c_string<B, 'c, 'e>(self, body: (Ptr) -> B with 'c raises 'e) -> B
    with 'c
    raises 'e;
}

impl Ptr {
  fn null() -> Ptr;
  fn is_null(self) -> Bool;
}

impl U8 {
  fn to_int(self) -> Int;
}

/// Stands in for a foreign function: it takes what a foreign function takes.
extern fn khora_sum_bytes(data: Ptr, len: Int) -> Int;
";

/// The body is handed a pointer and a count, and the count is the element
/// count — the same number `length` gives.
#[test]
fn a_buffer_is_lent_as_a_pointer_and_a_count() {
    let ran = run(
        "lend_array",
        &format!(
            "{LEND}
fn main() -> Int {{
  let bytes: Array<U8> = Array::new(5, 7);
  khora_print_int(Array::with_data(bytes, fn (p, n) => n));
  khora_print_int(Array::with_data(bytes, fn (p, n) => if Ptr::is_null(p) {{ 1 }} else {{ 0 }}));
  khora_print_int(Array::length(bytes));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    // The trailing 1 is the live-object count. `bytes`'s last read is an
    // `Array::length`, which only borrows — a borrow cannot take the binding's
    // reference, so the block still releases it, after this count.
    assert_eq!(
        ran.stdout, "5\n0\n5\n1\n",
        "five elements, a pointer that is not null, and the array still alive afterwards"
    );
    assert_eq!(ran.code, Some(0));
}

/// A string lends its bytes, and the count is the byte length.
#[test]
fn a_string_lends_its_bytes() {
    let ran = run(
        "lend_string",
        &format!(
            "{LEND}
fn main() -> Int {{
  khora_print_int(String::with_data(\"khora\", fn (p, n) => n));
  khora_print_int(String::with_data(\"\", fn (p, n) => n));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n0\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// The pointer really addresses the elements: a routine reading through it
/// sees what Khora wrote.
#[test]
fn what_is_lent_is_the_actual_data() {
    let ran = run(
        "lend_reads",
        &format!(
            "{LEND}
fn main() -> Int {{
  let bytes: Array<U8> = Array::new(4, 0);
  Array::set(bytes, 0, 10);
  Array::set(bytes, 1, 20);
  Array::set(bytes, 2, 30);
  Array::set(bytes, 3, 40);
  khora_print_int(Array::with_data(bytes, fn (p, n) => khora_sum_bytes(p, n)));
  khora_print_int(String::with_data(\"AB\", fn (p, n) => khora_sum_bytes(p, n)));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    // The trailing 0 is the live-object count, freed at the last read rather
    // than at the end of `main`, as above.
    assert_eq!(
        ran.stdout, "100\n131\n0\n",
        "10+20+30+40, then the bytes of `AB` which are 65 and 66"
    );
    assert_eq!(ran.code, Some(0));
}

/// The array outlives the call by construction, and is released afterwards
/// rather than leaked. Both halves matter: freed too early is a dangling
/// pointer, freed never is a leak.
#[test]
fn what_was_lent_is_released_afterwards() {
    let ran = run(
        "lend_releases",
        &format!(
            "{LEND}
fn borrow() -> Int {{
  let bytes: Array<U8> = Array::new(8, 1);
  Array::with_data(bytes, fn (p, n) => khora_sum_bytes(p, n))
}}

fn main() -> Int {{
  khora_print_int(borrow());
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "8\n0\n", "nothing is left alive once the borrow is over");
    assert_eq!(ran.code, Some(0));
}

/// And released on the *error* path too, which is why the array is held by a
/// scope rather than by a statement after the call. Errata 34, for the third
/// time.
#[test]
fn what_was_lent_is_released_when_the_body_raises() {
    let ran = run(
        "lend_raises",
        &format!(
            "{LEND}
pub type Oops = | Bad;

fn borrow() -> Int raises Oops {{
  let bytes: Array<U8> = Array::new(8, 1);
  Array::with_data(bytes, fn (p, n) => raise Oops::Bad)!
}}

fn main() -> Int {{
  khora_print_int(borrow()! catch {{
    Oops::Bad => 0,
  }});
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n0\n", "the raise left, and the buffer did not stay behind");
    assert_eq!(ran.code, Some(0));
}

/// An array of Khora objects holds reference-counted pointers, and handing
/// those across is the mistake the whole boundary exists to prevent.
#[test]
fn an_array_of_objects_cannot_be_lent() {
    let found = refused(
        "lend_boxed",
        &format!(
            "{LEND}
fn main() -> Int {{
  let names: Array<String> = Array::new(2, \"a\");
  khora_print_int(Array::with_data(names, fn (p, n) => n));
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|m| m.contains("reference-counted objects")),
        "expected a boundary error about counted elements, got {found:?}"
    );
}

/// A C string is the bytes plus a zero, and the proof is that C's own `strlen`
/// finds the same length Khora reports. Nothing in the test measures the
/// terminator directly — `strlen` is what a terminator is *for*.
#[test]
fn a_c_string_is_terminated() {
    let ran = run(
        "lend_c_string",
        &format!(
            "{LEND}
extern fn strlen(s: Ptr) -> Int;

fn main() -> Int {{
  khora_print_int(String::with_c_string(\"khora\", fn p => strlen(p)));
  khora_print_int(String::with_c_string(\"\", fn p => strlen(p)));
  khora_print_int(String::with_c_string(\"a\" + \"bc\", fn p => strlen(p)));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "5\n0\n3\n0\n",
        "the terminator is where C expects it, and the copy did not outlive the call"
    );
    assert_eq!(ran.code, Some(0));
}

/// The copy lives exactly as long as the call, on the failing path too.
#[test]
fn a_c_string_is_released_when_the_body_raises() {
    let ran = run(
        "lend_c_string_raises",
        &format!(
            "{LEND}
pub type Nope = | Bad;

fn borrow() -> Int raises Nope {{
  String::with_c_string(\"khora\", fn p => raise Nope::Bad)!
}}

fn main() -> Int {{
  khora_print_int(borrow()! catch {{ Nope::Bad => 1 }});
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n");
    assert_eq!(ran.code, Some(0));
}

// --- saying it out loud -----------------------------------------------------

/// A function with no body and no `extern` is a promise nobody has kept. The
/// checker takes it — a signature written ahead of its implementation is a
/// useful thing to have, and `std::net::http` is nothing but those — and the
/// code generator refuses to call it.
///
/// This is the whole reason the keyword exists. Before it, a misspelled name
/// became a C symbol nobody defines, and the only sign was `undefined symbol`
/// from the linker: no line, no file, no mention of Khora.
#[test]
fn a_body_less_function_is_not_silently_foreign() {
    let found = refused(
        "extern_missing",
        "module t;
extern fn khora_print_int(value: Int);

fn calculat_total(items: Int) -> Int;

fn main() -> Int {{ khora_print_int(calculat_total(3)); 0 }}
"
        .replace("{{", "{")
        .replace("}}", "}")
        .as_str(),
    );
    assert!(
        found.iter().any(|m| m.contains("calculat_total") && m.contains("nothing to call")),
        "expected the typo to be named, got {found:?}"
    );
    assert!(
        found.iter().all(|m| !m.contains("undefined symbol")),
        "the linker should never have been reached: {found:?}"
    );
}

/// Declaring it is not calling it. A module full of signatures ahead of their
/// implementations still compiles, which is what keeps `extern` from being a
/// tax on writing an interface first.
#[test]
fn a_body_less_function_nobody_calls_is_fine() {
    let exe = build(
        "extern_unused",
        "module t;
extern fn khora_print_int(value: Int);

fn not_written_yet(items: Int) -> Int;

fn main() -> Int { khora_print_int(1); 0 }
",
    )
    .expect("a declaration nobody calls is a promise nobody has come to collect");
    let output = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"), "1\n");
}

/// And `extern` is what makes a C symbol reachable. `strlen` is ISO C, so this
/// is a real call to a real library.
#[test]
fn extern_reaches_the_c_library() {
    let ran = run(
        "extern_works",
        "module t;
extern fn khora_print_int(value: Int);
extern fn strlen(s: Ptr) -> Int;

impl String {
  fn with_c_string<B, 'c, 'e>(self, body: (Ptr) -> B with 'c raises 'e) -> B
    with 'c
    raises 'e;
}

fn main() -> Int {
  khora_print_int(String::with_c_string(\"khora\", fn p => strlen(p)));
  0
}
",
    );
    assert_eq!(ran.stdout, "5\n");
    assert_eq!(ran.code, Some(0));
}
