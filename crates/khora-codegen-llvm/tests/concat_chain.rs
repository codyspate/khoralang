#![cfg(feature = "llvm")]

//! A `+` chain on strings, which is what `"a ${x} b ${y}"` desugars to, is
//! built as one string.
//!
//! **What these guard: a chain lowered as one allocation that is not the
//! string the pairwise `+` would have built, or that leaks a piece.** The
//! result's length is the sum of every piece's and each piece is copied at the
//! running offset, so an offset or a length summed wrong shows up as a wrong
//! byte, not as a crash. A piece is held for the concatenation until the last
//! one is evaluated, so an operand that leaves early has to release the pieces
//! before it.

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

/// Runs `exe` on `backend` under a 30 s watchdog, and hands back its stdout
/// as bytes, so a wrong byte is not hidden by a lossy decode.
fn run(exe: &PathBuf, backend: &str) -> Vec<u8> {
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
    let (mut stdout, mut stderr) = (Vec::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_end(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    assert_eq!(status.code(), Some(0), "`{backend}` exited badly: {stderr}");
    stdout.retain(|b| *b != b'\r');
    stdout
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

fn expect(exe: &PathBuf, expected: &str) {
    for backend in BACKENDS {
        let out = run(exe, backend);
        assert_eq!(
            out,
            expected.as_bytes(),
            "`{backend}`:\n{}\nexpected:\n{expected}",
            String::from_utf8_lossy(&out)
        );
    }
}

/// **Chains of 2, 3 and 10 pieces give the bytes the pairwise `+` gave**,
/// with empty pieces, two-, three- and four-byte characters, pieces that are
/// literals and pieces allocated at run time, a parenthesized middle, and a
/// chain that consumes its own accumulator in a loop. Each line prints the
/// byte length too, so a length field summed wrong is caught even where the
/// bytes happen to print the same.
#[test]
fn a_chain_builds_the_same_bytes_as_pairwise_concatenation() {
    let exe = build(
        "concat_chain_bytes",
        r#"module main;
import std::core::{Iterator, Step, List, Range, print};

fn line(label: String, s: String) { print("${label} ${String::byte_length(s)} [${s}]"); }

pub fn main() -> Int {
  let empty = "";
  let e = "é";
  let snow = "☃";
  let n = 42;
  let heap = "h${n}";
  line("two", empty + e);
  line("two-hole", "${e}${heap}");
  line("three", e + empty + snow);
  line("three-empty", empty + empty + empty);
  line("three-hole", "x${empty}y");
  line("ten", "a" + e + empty + "日本語" + heap + "🎉" + empty + snow + "${n}" + "z");
  line("ten-hole", "${n}é${empty}☃${heap}${n}${empty}🎉${e}!");
  line("nested", e + (snow + heap) + empty + "!");
  let mut acc = "";
  let mut i = 0;
  while i < 4 { acc = acc + "," + "${i}"; i = i + 1; };
  line("loop", acc);
  0
}
"#,
    );
    expect(
        &exe,
        "two 2 [é]\n\
         two-hole 5 [éh42]\n\
         three 5 [é☃]\n\
         three-empty 0 []\n\
         three-hole 2 [xy]\n\
         ten 25 [aé日本語h42🎉☃42z]\n\
         ten-hole 19 [42é☃h4242🎉é!]\n\
         nested 9 [é☃h42!]\n\
         loop 8 [,0,1,2,3]\n",
    );
}

/// **A five-piece interpolation allocates one string**, not four; a
/// ten-piece one allocates one, not nine. The pieces here are already built,
/// so the only allocation between the two counter reads is the result.
#[test]
fn a_five_piece_interpolation_allocates_one_string() {
    let exe = build(
        "concat_chain_count",
        r#"module main;
import std::core::{print};

extern fn khora_alloc_count() -> Int;

pub fn main() -> Int {
  let n = 7;
  let a = "a${n}";
  let b = "b";
  let c0 = khora_alloc_count();
  let five = "<${a}|${b}>";
  let c1 = khora_alloc_count();
  let ten = a + b + a + b + a + b + a + b + a + b;
  let c2 = khora_alloc_count();
  print("${c1 - c0} ${five}");
  print("${c2 - c1} ${ten}");
  0
}
"#,
    );
    expect(&exe, "1 <a7|b>\n1 a7ba7ba7ba7ba7b\n");
}

/// **An operand that leaves from the middle of a chain releases the pieces
/// already built.** The pieces to its left are held for the one allocation at
/// the end, and nothing else owns them: `${n}` was allocated for this chain
/// alone. Counted as the live-object delta over ten calls each, for a raise
/// inside an interpolation, a raise inside an explicit `+` chain, and a
/// `return` from the middle of one.
#[test]
fn a_raise_in_the_middle_of_a_chain_releases_the_pieces_before_it() {
    let exe = build(
        "concat_chain_raise",
        r#"module main;
import std::core::{print};

extern fn khora_live_count() -> Int;

type Bad = | Bad(n: Int);

fn fails(n: Int, at: Int) -> String raises Bad { if n == at { raise Bad::Bad(n) } else { "t${n}" } }
fn interp(n: Int, at: Int) -> Int raises Bad { let s = "s${n}"; String::byte_length("<${n}|${s}|${fails(n, at)!}|${n}>") }
fn plus(n: Int, at: Int) -> Int raises Bad { let s = "s${n}"; String::byte_length("a" + "${n}" + s + fails(n, at)! + "z") }
fn ret(n: Int, stop: Bool) -> Int { let s = "s${n}"; String::byte_length("<${n}" + s + (if stop { return 0 } else { "z" }) + "y") }

pub fn main() -> Int {
  let mut j = 0;
  let mut sink = 0;
  let mut b = khora_live_count();
  while j < 10 { sink = sink + (interp(3, 3)! catch { Bad::Bad(_) => 0 }); j = j + 1; };
  let mut a = khora_live_count();
  print("interp ${a - b}");
  b = khora_live_count(); j = 0;
  while j < 10 { sink = sink + (plus(3, 3)! catch { Bad::Bad(_) => 0 }); j = j + 1; };
  a = khora_live_count();
  print("plus ${a - b}");
  b = khora_live_count(); j = 0;
  while j < 10 { sink = sink + ret(3, true); j = j + 1; };
  a = khora_live_count();
  print("return ${a - b}");
  let v1 = interp(3, 9)! catch { Bad::Bad(_) => 0 - 1 };
  let v2 = plus(3, 9)! catch { Bad::Bad(_) => 0 - 1 };
  print("sink ${sink} values ${v1} ${v2} ${ret(3, false)}");
  0
}
"#,
    );
    expect(&exe, "interp 0\nplus 0\nreturn 0\nsink 0 values 11 7 6\n");
}
