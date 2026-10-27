#![cfg(feature = "llvm")]

//! A program's function named like a C function `std` declares.
//!
//! **What these guard: `fn bind()` in a program breaking `std`'s call to C's
//! `bind`.** `std::net::socket` declares `extern fn bind`, `listen`, `close`
//! and others, and its bodies are compiled into every program. The backend
//! declared each of those calls from the C signature and then took the result
//! apart by whichever signature a program-wide, bare-name map found first.
//! A namesake that can raise crashed the compiler ("expected the StructValue
//! variant"); one returning `()` where C returns `I32` was refused with a
//! message about a body the program did not write.
//!
//! **"First" is path order.** `khora build` sorts every file it compiles --
//! the program's, its dependencies', `std`'s -- by canonical path, and the map
//! keeps the first declaration of each bare name. So which namesake won
//! depended on where things sat on disk: a package whose path sorts before
//! `std`'s found its own function first, one whose path sorts after found
//! `std`'s extern first and built, and a dependency in the package's own
//! `dep/` directory sorted before the program's `src/` and won over the
//! program's `extern fn`. An import does not put a name in play: an imported
//! function is stored under the name it is imported as.
//!
//! The program's own calls were always right: they resolve to its
//! module-qualified function. So what decides these is the namesake's
//! signature, not how the program calls it -- directly, as a fiber body, as a
//! value or from a second module are here so that each stays compiled
//! against the program's function and not the C one.
//!
//! **Each test gives its files in the order that loses**: the namesake before
//! the C declaration. A fixture that appended the program after `std` would
//! find `std`'s declaration first and pass with the defect present.

use crate::harness;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The program's files, in the order given, and then every `.kh` file of `std`.
fn sources(db: &KhoraDatabase, dir: &std::path::Path, program: &[(&str, &str)]) -> Vec<SourceFile> {
    let mut out: Vec<SourceFile> = program
        .iter()
        .map(|(file, text)| SourceFile::new(db, dir.join(file), text.to_string()))
        .collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
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
    out
}

fn build(name: &str, program: &[(&str, &str)]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, program));
    // `catch_unwind` because the defect is a panic inside the backend: the
    // test should say which program crashed it rather than die with the
    // backend's own message alone.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        khora_codegen_llvm::compile(&db, root, &exe)
    }));
    match outcome {
        Ok(Ok(())) => exe,
        Ok(Err(errors)) => {
            let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
            panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
        }
        Err(_) => panic!("compiling `{name}` crashed the compiler"),
    }
}

/// Runs `exe` on `backend` under a 30 s watchdog, and hands back its stdout.
fn run(exe: &PathBuf, backend: &str) -> String {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    while child.try_wait().expect("waiting").is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("`{backend}`: {} did not finish in 30 s", exe.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    assert_eq!(status.code(), Some(0), "`{backend}` exited badly: {stderr}");
    stdout.replace("\r\n", "\n")
}

/// Builds `program` and checks its output on both fiber backends.
fn check(name: &str, program: &[(&str, &str)], expected: &str) {
    let exe = build(name, program);
    for backend in ["threads", "scheduler"] {
        assert_eq!(run(&exe, backend), expected, "`{backend}`");
    }
}

/// A fallible `bind`, called directly. This crashed the compiler.
#[test]
fn a_fallible_function_named_bind_is_called_as_itself() {
    check(
        "namesake_bind_direct",
        &[(
            "main.kh",
            r#"module main;
import std::core::{print};
type Oops = | Bad;
fn bind() -> Int raises Oops { raise Oops::Bad }
pub fn main() -> Int {
  let n = bind()! catch { Oops::Bad => 3 };
  print("${n}");
  0
}
"#,
        )],
        "3\n",
    );
}

/// The program the defect was found in: a fallible `bind` as a fiber's body,
/// canceled before it is waited for.
#[test]
fn a_fallible_function_named_bind_can_be_a_fiber_body() {
    check(
        "namesake_bind_fiber",
        &[(
            "main.kh",
            r#"module main;
import std::core::{Fiber, print};
type Oops = | Bad;
fn bind() -> Int raises Oops { raise Oops::Bad }
pub fn main() -> Int {
  let server = Fiber::spawn(fn () => bind()!);
  Fiber::cancel(server);
  print("done");
  0
}
"#,
        )],
        "done\n",
    );
}

/// A `bind` returning `()`, where C's returns `I32`. This was refused with
/// "this body does not produce the `I32` its signature promises", about a
/// body that promises nothing of the kind.
#[test]
fn a_function_named_bind_may_return_unit() {
    check(
        "namesake_bind_unit",
        &[(
            "main.kh",
            r#"module main;
import std::core::{print};
fn bind() -> () { print("bound") }
pub fn main() -> Int {
  bind();
  0
}
"#,
        )],
        "bound\n",
    );
}

/// **`std`'s `listen_on` calls C's `bind` and `listen` while the program has a
/// `listen` of its own**, and both answer: the socket opens, and the
/// program's `listen` raises its own error. Port 0 lets the system choose, so
/// nothing else on the machine can make this fail.
#[test]
fn std_listens_while_the_program_has_its_own_listen() {
    check(
        "namesake_listen_beside_std",
        &[(
            "main.kh",
            r#"module main;
import std::core::{print};
import std::net::socket::{start, listen_on, shut, invalid_handle};
type Oops = | Bad;
fn listen() -> Int raises Oops { raise Oops::Bad }
pub fn main() -> Int {
  if start() {
    let server = listen_on(0);
    if server == invalid_handle() { print("could not listen") } else {
      print("listening");
      shut(server);
    }
  } else { print("no sockets") }
  let mine = listen()! catch { Oops::Bad => 5 };
  print("${mine}");
  0
}
"#,
        )],
        "listening\n5\n",
    );
}

/// A fallible `bind` exported from a second module, and reached from `main`
/// directly, through a closure, as a function value and as a fiber body.
/// Every path calls the program's function.
#[test]
fn a_bind_from_another_module_is_called_as_itself_on_every_path() {
    check(
        "namesake_bind_module",
        &[
            (
                "main.kh",
                r#"module main;
import std::core::{Fiber, print};
import main::net::{bind, Oops};
pub fn main() -> Int {
  let direct = bind(1)! catch { Oops::Bad(n) => n };
  let through = fn () => bind(2)!;
  let closed = through()! catch { Oops::Bad(n) => n };
  let value = bind;
  let named = value(3)! catch { Oops::Bad(n) => n };
  let child = Fiber::spawn(fn () => bind(4)!);
  let joined = Fiber::join(child)! catch { Oops::Bad(n) => n };
  print("${direct} ${closed} ${named} ${joined}");
  0
}
"#,
            ),
            (
                "net.kh",
                r#"module main::net;
/// What `bind` raises.
pub type Oops = | Bad(n: Int);
/// Raises its argument back.
pub fn bind(n: Int) -> Int raises Oops { raise Oops::Bad(n) }
"#,
            ),
        ],
        "1 2 3 4\n",
    );
}

/// **The program's own `extern fn` beside its own Khora function of the same
/// name**, in two modules of one package. The C declaration is in the file
/// read second, so the Khora `abs` was found first and the C call was
/// refused with "this call can leave the function", pointing at the Khora
/// function's `raises`. `khora check` accepts this program, and is right
/// to: the rule for C symbols compares `extern fn` declarations with each
/// other, and a Khora function is not one. `abs` because it is ISO C, so
/// every platform's C library has it.
#[test]
fn a_program_extern_and_a_program_function_of_one_name_are_both_called() {
    check(
        "namesake_own_extern",
        &[
            (
                "main.kh",
                r#"module main;
import std::core::{print};
import main::util::{distance};
type Oops = | Bad;
fn abs() -> Int raises Oops { raise Oops::Bad }
pub fn main() -> Int {
  let mine = abs()! catch { Oops::Bad => 9 };
  print("${mine} ${distance(0 - 4)}");
  0
}
"#,
            ),
            (
                "util.kh",
                r#"module main::util;
extern fn abs(n: I32) -> I32;
/// How far `n` is from zero, from C.
pub fn distance(n: Int) -> Int { I32::to_int(abs(I32::of(n))) }
"#,
            ),
        ],
        "9 4\n",
    );
}

/// **A canceled `clock.sleep` stops the fiber while the program has a
/// `khora_sleep` of its own.** The one shape here that compiled before the
/// fix and answered wrong. Whether a runtime call gave up on a cancel is
/// checked only after a call to a C function, and the program's
/// `khora_sleep` -- same parameters, same result, but a Khora function --
/// made std's call to the runtime's look like one that was not, so the
/// sleep came back early and the statement after it ran.
#[test]
fn a_canceled_sleep_stops_while_the_program_has_its_own_khora_sleep() {
    check(
        "namesake_khora_sleep",
        &[(
            "main.kh",
            r#"module main;
import std::core::{print, Fiber, Channel};
import std::clock::{Clock};

fn khora_sleep(millis: Int) -> () { print("mine ${millis}") }

fn napper(ready: Channel<Int>) -> () with { clock: Clock } {
  Channel::send(ready, 1);
  clock.sleep(8000);
  print("statement after the sleep ran");
}

pub fn main() -> Int {
  khora_sleep(1);
  with { clock: Clock::real() } {
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => napper(ready));
    let _ = Channel::receive(ready);
    clock.sleep(100);
    Fiber::cancel(f);
    Fiber::wait(f);
    print("canceled: ${Fiber::canceled(f)}");
    0
  }
}
"#,
        )],
        "mine 1\ncanceled: true\n",
    );
}

/// **A C function used as a value, in a closure and as a fiber body, beside a
/// fallible Khora function of the same name read first.** A C function taken
/// as a value is wrapped in a forwarder whose one call is an ordinary call
/// site, so each of the three shapes reaches the same lookup a direct call
/// does, and each crashed the compiler.
#[test]
fn a_c_function_as_a_value_is_called_as_itself() {
    check(
        "namesake_extern_as_value",
        &[
            (
                "a_mine.kh",
                r#"module main::a_mine;
/// Raised.
pub type Oops = | Bad;
/// Fallible, named like C.
pub fn labs(n: Int) -> Int raises Oops { raise Oops::Bad }
"#,
            ),
            (
                "z_c.kh",
                r#"module main::z_c;
/// C labs.
pub extern fn labs(n: Int) -> Int;
"#,
            ),
            (
                "main.kh",
                r#"module main;
import std::core::{print, Fiber};
import main::a_mine::{Oops, labs as mine};
import main::z_c::{labs};
pub fn main() -> Int {
  let f = labs;
  let g = fn (n: Int) => labs(n);
  let child = Fiber::spawn(fn () => labs(0 - 9));
  let j = Fiber::join(child);
  let m = mine(1)! catch { Oops::Bad => 7 };
  print("${f(0 - 4)} ${g(0 - 5)} ${j} ${m}");
  0
}
"#,
            ),
        ],
        "4 5 9 7\n",
    );
}

/// **A dependency's fallible `abs` beside the program's own `extern fn
/// abs`.** Through the real `khora build`, because the order that matters is
/// the one it chooses: it sorts every file by path, and a dependency in the
/// package's `dep/` directory sorts before the package's `src/`, so the
/// dependency's Khora `abs` was found before the program's C one. That the
/// program imports it as `dabs` does not matter: an import is stored under the
/// name it is imported as.
#[test]
fn a_dependency_function_named_like_the_program_extern_is_called_as_itself() {
    let khora = harness::khora();
    harness::ensure_runtime();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("namesake_dependency");
    let _ = std::fs::remove_dir_all(&root);
    let files = [
        ("khora.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"0.4.0\"\n\n[dependencies]\ndep = { path = \"dep\" }\n"),
        (
            "src/main.kh",
            r#"module app::main;
import std::core::{print};
import dep::{Oops, abs as dabs};
extern fn abs(n: I32) -> I32;
pub fn main() -> Int {
  let n = dabs()! catch { Oops::Bad => 7 };
  print("${n} ${I32::to_int(abs(I32::of(0 - 4)))}");
  0
}
"#,
        ),
        ("dep/khora.toml", "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n"),
        (
            "dep/src/lib.kh",
            r#"module dep;
/// Raised.
pub type Oops = | Bad;
/// Named like C's `abs`.
pub fn abs() -> Int raises Oops { raise Oops::Bad }
"#,
        ),
    ];
    for (relative, text) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a fixture directory");
        std::fs::write(&path, text).expect("a fixture");
    }
    let built = Command::new(&khora).arg("build").arg(&root).output().expect("`khora` runs");
    assert!(
        built.status.success(),
        "`khora build` failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let exe = root.join("build").join(if cfg!(windows) { "app.exe" } else { "app" });
    for backend in ["threads", "scheduler"] {
        assert_eq!(run(&exe, backend), "7 4\n", "`{backend}`");
    }
}
