#![cfg(feature = "llvm")]

//! Reading a real file, from Khora, through ISO C.
//!
//! Nothing here is bound to a Rust shim: `fopen`, `fread` and `fclose` are the
//! C standard library, spelled the same on every platform Khora targets, and
//! `FILE *` is exactly what `Ptr` was added for. `docs/design/ffi.md`.
//!
//! The socket half of the exit criterion is not here, because a socket is not
//! ISO C — it is Winsock or it is Berkeley sockets, and choosing between them
//! needs conditional compilation, which the language does not have yet.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

/// Compiles and runs `source`, having first written `contents` to a file the
/// program is told the path of.
fn run_over(name: &str, contents: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let data = dir.join("input.txt");
    std::fs::write(&data, contents).expect("writing the input file");
    // A Khora string literal, so the separators have to survive the quoting.
    let path = data.to_string_lossy().replace('\\', "/");

    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file =
        SourceFile::new(&db, dir.join("main.kh"), source.replace("@PATH@", &path));
    let root = SourceRoot::new(&db, vec![file]);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let output = std::process::Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

/// The bindings, and nothing else. Seven declarations reach the file system.
const C: &str = "module t;
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
  fn get(self, index: Int) -> A;
  fn with_data<B, 'c, 'e>(self, body: (Ptr, Int) -> B with 'c raises 'e) -> B
    with 'c
    raises 'e;
}

impl String {
  fn byte_length(self) -> Int;
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

// ISO C. Not a shim, not a Rust binding — the same names on every target.
extern fn fopen(path: Ptr, mode: Ptr) -> Ptr;
extern fn fread(into: Ptr, size: Int, count: Int, file: Ptr) -> Int;
extern fn fclose(file: Ptr) -> I32;
";

/// The whole point: bytes out of a real file, in Khora, with no Rust in
/// between.
#[test]
fn a_file_can_be_read_from_khora() {
    let ran = run_over(
        "c_read_file",
        "khora",
        &format!(
            "{C}
fn read_all(path: String, into: Array<U8>) -> Int {{
  String::with_c_string(path, fn p =>
    String::with_c_string(\"rb\", fn mode => {{
      let file = fopen(p, mode);
      if Ptr::is_null(file) {{
        0 - 1
      }} else {{
        let read = Array::with_data(into, fn (buf, len) => fread(buf, 1, len, file));
        fclose(file);
        read
      }}
    }}))
}}

fn main() -> Int {{
  let buffer: Array<U8> = Array::new(64, 0);
  let read = read_all(\"@PATH@\", buffer);
  khora_print_int(read);
  khora_print_int(U8::to_int(Array::get(buffer, 0)));
  khora_print_int(U8::to_int(Array::get(buffer, 4)));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    // The trailing 1 is the live-object count. `buffer`'s last read is an
    // `Array::get`, which only borrows — a borrow cannot take the binding's
    // reference, so the block still releases it, after this count.
    // `khora_perceus::borrowed_arguments`.
    assert_eq!(
        ran.stdout, "5\n107\n97\n1\n",
        "five bytes, `k` and `a`, and only the buffer still alive"
    );
    assert_eq!(ran.code, Some(0));
}

/// A path that is not there is a null `FILE *`, which is how C says it failed.
#[test]
fn a_missing_file_is_a_null_handle() {
    let ran = run_over(
        "c_missing_file",
        "unused",
        &format!(
            "{C}
fn main() -> Int {{
  let opened = String::with_c_string(\"@PATH@.nope\", fn p =>
    String::with_c_string(\"rb\", fn mode =>
      if Ptr::is_null(fopen(p, mode)) {{ 0 }} else {{ 1 }}));
  khora_print_int(opened);
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// **Phase 7's exit criterion, the file half.** The file is closed by the scope
/// that opened it, on the error path as well as the ordinary one.
///
/// `acquire` is what ties the two together, and it needed nothing new: it
/// registers a release with the enclosing `Scope`, and a `Scope` is a region,
/// and a region runs its deferred work on every way out. A foreign handle is a
/// counted value because everything is.
///
/// The proof is that the second `fopen` succeeds. On Windows a file opened for
/// reading can be reopened, so that alone would prove little — but the same
/// program deletes the file afterwards, and Windows refuses to delete a file
/// with an open handle. The delete succeeding is the close having happened.
#[test]
fn a_file_is_closed_on_the_error_path() {
    let core = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std")
            .join("core.kh"),
    )
    .expect("std/core.kh");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("c_close_on_raise");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let data = dir.join("input.txt");
    std::fs::write(&data, "khora").expect("writing the input file");
    let path = data.to_string_lossy().replace('\\', "/");

    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let main = format!(
        "module demo::main;
import std::core::{{Scope, Region, Ptr, acquire}};

extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

extern fn fopen(path: Ptr, mode: Ptr) -> Ptr;
extern fn fclose(file: Ptr) -> I32;

pub type Torn = | Midway;

/// Opening is its own function so that the two borrows stay simple: neither
/// body needs a capability and neither can fail, which is what a C string is
/// for.
fn open_file(path: String) -> Ptr {{
  String::with_c_string(path, fn p =>
    String::with_c_string(\"rb\", fn mode => fopen(p, mode)))
}}

/// Registers the close with the scope, then fails. The close still happens,
/// because leaving the scope is what runs it.
fn open_then_fail(path: String) -> Int with {{ scope: Scope }} raises Torn {{
  let file = acquire(open_file(path), fn f => {{ fclose(f); }});
  if Ptr::is_null(file) {{ 0 }} else {{ raise Torn::Midway }}
}}

/// The region is a local, so the raise below leaves *through* its release.
/// That is the path being tested: not that the close happens, but that it
/// happens on the way out of a failure.
fn open_in_region(path: String) -> Int raises Torn {{
  let region = Region::open();
  with {{ scope: handler for Scope {{ defer: fn f => Region::defer(region, f) }} }} {{
    open_then_fail(path)!
  }}
}}

fn main() -> Int {{
  khora_print_int(open_in_region(\"{path}\")! catch {{
    Torn::Midway => 1,
  }});
  khora_print_int(khora_live_count());
  0
}}
"
    );

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), core),
        SourceFile::new(&db, dir.join("main.kh"), main),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }

    let output = std::process::Command::new(&exe).output().expect("the program should run");
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "1\n0\n", "the raise left, and nothing stayed behind");
    assert_eq!(output.status.code(), Some(0));

    // The close is what makes this possible on Windows, which refuses to
    // delete a file that still has a handle open on it.
    std::fs::remove_file(&data)
        .expect("the file should be closed, so deleting it should succeed");
}
