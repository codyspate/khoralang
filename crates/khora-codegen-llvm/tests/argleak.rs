#![cfg(feature = "llvm")]

//! An operand that leaves early releases the operands evaluated before it.
//!
//! **What these guard: a leak on every early exit from the middle of an
//! argument list.** In `f(acc, load()!)`, `acc` has been evaluated and is
//! held for the call when `load` raises. The binding it came from already
//! handed its reference over, so no block releases it, and the early exit
//! has to. Every exit kind (`!`, `raise`, `return`, `break`, `continue`, a
//! cancellation) and every shape that evaluates several operands before using
//! them (a call, a method call, a constructor, a pipe, a closure call, a
//! record, a list, a tuple, an interpolation, `+` on strings, nested calls)
//! is counted by the live-object delta over repeated calls, which is 0 when
//! nothing leaks, on both fiber backends.
//!
//! Compiled against `std`, because the shapes call `List` methods, and
//! run with a watchdog because the cancellation program is a hang if the
//! fiber never stops.

use crate::harness;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test.
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

fn build(name: &str, source: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
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

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Declarations every program below shares.
const PRELUDE: &str = r#"module main;
import std::core::{Iterator, Step, List, Range, Fiber, Channel, print};

extern fn khora_live_count() -> Int;

type Bad = | Bad(n: Int);
type Two = | Two(a: List<Int>, b: List<Int>);
type P = { a: List<Int>, b: List<Int> };

fn faill(n: Int, at: Int) -> List<Int> raises Bad { if n == at { raise Bad::Bad(n) } else { [n, n] } }
fn fails(n: Int, at: Int) -> String raises Bad { if n == at { raise Bad::Bad(n) } else { "t${n}" } }
fn pair(a: List<Int>, b: List<Int>) -> Int { List::length(a) + List::length(b) }
"#;

/// Builds `PRELUDE` + `body`, runs it on both backends, and checks each line
/// `name delta` reads `name 0`. `expected` is the whole of stdout.
fn no_leaks(name: &str, body: &str, expected: &str) {
    let exe = build(name, &format!("{PRELUDE}{body}"));
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), expected, "`{backend}`");
    }
}

/// **`!` in an argument**, across every call shape. The delta is counted
/// over ten calls; before, each shape leaked the one list it had built for
/// the argument to the raising one's left, 10 or 20 per line.
#[test]
fn a_raise_in_an_argument_releases_the_arguments_before_it() {
    no_leaks(
        "argleak_raise",
        r#"
fn r_let(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); pair(acc, faill(4, at)!) }
fn r_fresh(xs: List<Int>, at: Int) -> Int raises Bad { pair(List::Cons(0, xs), faill(4, at)!) }
fn r_mut(xs: List<Int>, at: Int) -> List<Int> raises Bad {
  let mut acc = List::Nil;
  acc = List::Cons(0, acc);
  acc = List::reverse_onto(xs, acc);
  List::reverse_onto(acc, faill(4, at)!)
}
fn r_method(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); List::length(acc.reverse_onto(faill(4, at)!)) }
fn r_ctor(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); match Two::Two(acc, faill(4, at)!) { Two::Two(a, b) => pair(a, b) } }
fn r_cons(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); List::length(List::Cons(acc, List::Cons(faill(4, at)!, List::Nil))) }
fn r_pipe(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); acc |> pair(faill(4, at)!) }
fn r_closure(xs: List<Int>, at: Int) -> Int raises Bad {
  let acc = List::Cons(0, xs);
  let k = fn (a: List<Int>, b: List<Int>) => pair(a, b) + List::length(xs);
  k(acc, faill(4, at)!)
}
fn r_record(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); let p: P = { a: acc, b: faill(4, at)! }; pair(p.a, p.b) }
fn r_list(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); List::length([acc, faill(4, at)!]) }
fn r_tuple(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); let (p, q) = (acc, faill(4, at)!); pair(p, q) }
fn r_interp(xs: List<Int>, at: Int) -> Int raises Bad { let s = "s${List::length(xs)}"; String::byte_length("${s}${fails(4, at)!}") }
fn r_plus(xs: List<Int>, at: Int) -> Int raises Bad { let s = "s${List::length(xs)}"; String::byte_length(s + fails(4, at)!) }
fn r_nested(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); pair(List::reverse_onto(acc, faill(4, at)!), List::Cons(1, xs)) }
fn r_raise(xs: List<Int>, at: Int) -> Int raises Bad { let acc = List::Cons(0, xs); pair(acc, if at == 4 { raise Bad::Bad(4) } else { xs }) }
fn r_caught(xs: List<Int>, at: Int) -> Int { let acc = List::Cons(0, xs); pair(acc, faill(4, at)!) catch { Bad::Bad(_) => 0 } }

pub fn main() -> Int {
  let xs = [1, 2, 3, 4, 5];
  let mut j = 0;
  let mut sink = 0;
  let mut b = khora_live_count();
  j = 0; while j < 10 { sink = sink + (r_let(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; let mut a = khora_live_count(); print("let ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_fresh(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("fresh ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + List::length(r_mut(xs, 4)! catch { Bad::Bad(_) => [] }); j = j + 1; }; a = khora_live_count(); print("mut ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_method(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("method ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_ctor(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("ctor ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_cons(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("cons ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_pipe(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("pipe ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_closure(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("closure ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_record(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("record ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_list(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("list ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_tuple(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("tuple ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_interp(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("interp ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_plus(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("plus ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_nested(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("nested ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + (r_raise(xs, 4)! catch { Bad::Bad(_) => 0 }); j = j + 1; }; a = khora_live_count(); print("raise ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + r_caught(xs, 4); j = j + 1; }; a = khora_live_count(); print("caught ${a - b}");
  let n = (r_let(xs, 9)! catch { Bad::Bad(_) => 0 }) + (r_method(xs, 9)! catch { Bad::Bad(_) => 0 })
    + (r_closure(xs, 9)! catch { Bad::Bad(_) => 0 }) + (r_record(xs, 9)! catch { Bad::Bad(_) => 0 })
    + (r_tuple(xs, 9)! catch { Bad::Bad(_) => 0 }) + (r_interp(xs, 9)! catch { Bad::Bad(_) => 0 })
    + (r_plus(xs, 9)! catch { Bad::Bad(_) => 0 }) + r_caught(xs, 9);
  print("sink ${sink} values ${n}");
  0
}
"#,
        "let 0\nfresh 0\nmut 0\nmethod 0\nctor 0\ncons 0\npipe 0\nclosure 0\nrecord 0\n\
         list 0\ntuple 0\ninterp 0\nplus 0\nnested 0\nraise 0\ncaught 0\nsink 0 values 61\n",
    );
}

/// **`return`, `break` and `continue` in an argument.** Each takes its own
/// path out -- the whole frame, the loop, the turn -- and each leaked the
/// argument to its left.
#[test]
fn a_return_break_or_continue_in_an_argument_releases_the_arguments_before_it() {
    no_leaks(
        "argleak_jumps",
        r#"
fn t_call(xs: List<Int>, stop: Bool) -> Int { let acc = List::Cons(0, xs); pair(acc, if stop { return 0 } else { xs }) }
fn t_ctor(xs: List<Int>, stop: Bool) -> Int { let acc = List::Cons(0, xs); match Two::Two(acc, if stop { return 0 } else { xs }) { Two::Two(a, b) => pair(a, b) } }
fn t_plus(xs: List<Int>, stop: Bool) -> Int { let s = "s${List::length(xs)}"; String::byte_length(s + (if stop { return 0 } else { "z" })) }
fn b_call(xs: List<Int>) -> Int {
  let mut n = 0;
  loop { let acc = List::Cons(n, xs); n = n + pair(acc, if n > 2 { break n } else { xs }) - 10; }
}
fn b_record(xs: List<Int>) -> Int {
  let mut n = 0;
  loop { let acc = List::Cons(n, xs); let p: P = { a: acc, b: if n > 2 { break n } else { xs } }; n = n + pair(p.a, p.b) - 10; }
}
fn c_call(xs: List<Int>) -> Int {
  let mut n = 0;
  let mut total = 0;
  while n < 6 { n = n + 1; let acc = List::Cons(n, xs); total = total + pair(acc, if n % 2 == 0 { continue } else { xs }); };
  total
}

pub fn main() -> Int {
  let xs = [1, 2, 3, 4, 5];
  let mut j = 0;
  let mut sink = 0;
  let mut b = khora_live_count();
  j = 0; while j < 10 { sink = sink + t_call(xs, true); j = j + 1; }; let mut a = khora_live_count(); print("return call ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + t_ctor(xs, true); j = j + 1; }; a = khora_live_count(); print("return ctor ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + t_plus(xs, true); j = j + 1; }; a = khora_live_count(); print("return plus ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + b_call(xs); j = j + 1; }; a = khora_live_count(); print("break call ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + b_record(xs); j = j + 1; }; a = khora_live_count(); print("break record ${a - b}");
  b = khora_live_count(); j = 0; while j < 10 { sink = sink + c_call(xs); j = j + 1; }; a = khora_live_count(); print("continue call ${a - b}");
  print("sink ${sink} values ${t_call(xs, false)} ${t_ctor(xs, false)} ${t_plus(xs, false)}");
  0
}
"#,
        "return call 0\nreturn ctor 0\nreturn plus 0\nbreak call 0\nbreak record 0\n\
         continue call 0\nsink 390 values 11 11 3\n",
    );
}

/// **A cancellation inside an argument.** `spin` is a cancellation point that
/// never returns on its own, so the fiber is always stopped while `acc` is
/// held for `pair`. Twenty rounds; each leaked one list before.
#[test]
fn a_cancellation_in_an_argument_releases_the_arguments_before_it() {
    no_leaks(
        "argleak_cancel",
        r#"
fn spin(ready: Channel<Int>) -> List<Int> { Channel::send(ready, 1); let mut i = 0; loop { i = i + 1; } }
fn held(xs: List<Int>, ready: Channel<Int>) -> Int { let acc = List::Cons(0, xs); pair(acc, spin(ready)) }

pub fn main() -> Int {
  let xs = [1, 2, 3, 4, 5];
  let before = khora_live_count();
  let mut k = 0;
  let mut stopped = 0;
  while k < 20 {
    let ready: Channel<Int> = Channel::bounded(1);
    let f = Fiber::spawn(fn () => held(xs, ready));
    let _ = Channel::receive(ready);
    Fiber::cancel(f);
    Fiber::wait(f);
    if Fiber::canceled(f) { stopped = stopped + 1; };
    k = k + 1;
  };
  let after = khora_live_count();
  print("stopped ${stopped} delta ${after - before}");
  0
}
"#,
        "stopped 20 delta 0\n",
    );
}

/// **Left to right, each once**, and an operand to the right of the one that
/// leaves is not evaluated. Holding the evaluated operands for the exit path
/// must not move or repeat any evaluation.
#[test]
fn operands_are_still_evaluated_left_to_right_once() {
    no_leaks(
        "argleak_order",
        r#"
fn noisy(tag: String, n: Int) -> List<Int> { print(tag); [n] }
fn noisy_s(tag: String) -> String { print(tag); tag }
fn three(a: List<Int>, b: List<Int>, c: List<Int>) -> Int { pair(a, b) + List::length(c) }
fn go(at: Int) -> Int raises Bad {
  let t = (noisy("t1", 1), noisy("t2", 2));
  let r: P = { a: noisy("r1", 1), b: noisy("r2", 2) };
  let s = noisy_s("s1") + noisy_s("s2");
  print("${String::byte_length(s)}");
  let k = fn (a: List<Int>, b: List<Int>) => pair(a, b);
  let _ = k(noisy("k1", 1), noisy("k2", 2));
  let _ = Two::Two(noisy("c1", 1), noisy("c2", 2));
  three(noisy("a", 1), faill(4, at)!, noisy("c", 3))
}

pub fn main() -> Int {
  let n = go(9)! catch { Bad::Bad(_) => 0 - 1 };
  print("n ${n}");
  let m = go(4)! catch { Bad::Bad(_) => 0 - 1 };
  print("m ${m}");
  0
}
"#,
        "t1\nt2\nr1\nr2\ns1\ns2\n4\nk1\nk2\nc1\nc2\na\nc\nn 4\n\
         t1\nt2\nr1\nr2\ns1\ns2\n4\nk1\nk2\nc1\nc2\na\nm -1\n",
    );
}
