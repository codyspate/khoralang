#![cfg(feature = "llvm")]

//! What a loop back-edge and a `!` cost when nothing has happened.
//!
//! **What this guards: a runtime call on the path every trip of every loop
//! takes.** A back-edge asks whether the fiber should give its worker back and
//! whether it has been cancelled, and a `!` asks the second. Both used to be
//! calls, made every time, and in a short loop they were most of the loop: a
//! function with a `raises` row ran its loops ten times slower than the same
//! function without one, and a program that called `Fiber::spawn` once paid as
//! much again at every back-edge, on the thread backend, where the safepoint
//! has nothing to do.
//!
//! The answer is one relaxed load of `khora_poll`, which the runtime keeps at
//! zero while nothing is cancelled and no scheduler pool exists, and the calls
//! behind a branch on it. A timing test would be the direct statement of that,
//! and would be flaky on every shared runner this suite has run on. So this
//! reads the IR: **every call to `khora_safepoint` or `khora_cancelled` in the
//! function must sit in a block that only the poll branch leads to.** That is
//! the structural fact the speed follows from, and it holds or fails the same
//! way on every machine.
//!
//! A separate binary for the reason `debugging.rs` is: `KHORA_EMIT_LLVM` is
//! process-wide.

mod harness;

use std::path::PathBuf;

use khora_codegen_llvm::Profile;
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Compiles `source` with its IR dumped, and returns the body of the function
/// whose symbol ends with `name`.
fn function_ir(test: &str, source: &str, name: &str) -> String {
    function_ir_as(test, source, name, Profile::Debug)
}

/// The same, at `profile`. A release build's IR is the optimised module
/// (`program.opt.ll`), which is where a hoisted load would show.
fn function_ir_as(test: &str, source: &str, name: &str, profile: Profile) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test);
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    // SAFETY-of-a-sort: process-wide, and this binary's tests hold the lock.
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("KHORA_EMIT_LLVM", "1") };
    let outcome = khora_codegen_llvm::compile_with(&db, root, &exe, profile);
    unsafe { std::env::remove_var("KHORA_EMIT_LLVM") };
    outcome.expect("it compiles");

    let dumped = match profile {
        Profile::Debug => ".ll",
        Profile::Release => ".opt.ll",
    };
    let mut path = exe.clone().into_os_string();
    path.push(dumped);
    let ir = std::fs::read_to_string(path).expect("the IR was dumped");
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&format!("{name}\"(")))
        .unwrap_or_else(|| panic!("no function ending `{name}` in the IR"));
    ir.lines().skip(start).take_while(|l| *l != "}").collect::<Vec<_>>().join("\n")
}

static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Each runtime call named in `calls`, with the block it is in.
fn calls_by_block<'a>(body: &'a str, calls: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut block = "entry";
    let mut found = Vec::new();
    for line in body.lines() {
        // A label is the only unindented line with a colon, after `define`.
        if !line.starts_with(' ') && !line.starts_with("define") {
            if let Some((label, _)) = line.split_once(':') {
                block = label;
                continue;
            }
        }
        for call in calls {
            if line.contains(&format!("@{call}()")) {
                found.push((*call, block));
            }
        }
    }
    found
}

/// Two loops, one in a function with a row and one without, in a program
/// that spawns. Both are cancellation points; the first also checks at its
/// `!`.
const LOOPS: &str = "module t;
pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> {
  fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;
  fn join(self) -> A raises 'r;
}
type Stop = | Stop

fn step(i: Int) -> Int raises Stop { i + 1 }

fn raising(n: Int) -> Int raises Stop {
  let mut i = 0;
  while i < n { i = step(i)!; };
  i
}

fn quiet(n: Int) -> Int {
  let mut i = 0;
  while i < n { i = i + 1; };
  i
}

fn main() -> Int raises Stop {
  let f = Fiber::spawn(fn () => quiet(3));
  Fiber::join(f);
  raising(3)!
}
";

/// A back-edge and a `!` in a function with a row: both questions, and every
/// call behind the one load.
#[test]
fn a_raising_loop_calls_the_runtime_only_behind_the_poll() {
    let body = function_ir("polls_raising", LOOPS, "raising");
    assert!(
        body.contains("load atomic i64, ptr @khora_poll monotonic"),
        "the poll word is read with a relaxed load:\n{body}"
    );
    let calls = calls_by_block(
        &body,
        &["khora_safepoint", "khora_cancelled", "khora_back_edge"],
    );
    assert!(
        calls.iter().any(|(c, _)| *c == "khora_back_edge")
            && calls.iter().any(|(c, _)| *c == "khora_cancelled"),
        "one call at the back-edge that asks both questions, and a check at the `!`: {calls:?}\n{body}"
    );
    for (call, block) in &calls {
        assert!(
            block.starts_with("poll.slow"),
            "`{call}` is in `{block}`, which every trip of the loop runs:\n{body}"
        );
    }
    // On the scheduler backend the slow path is taken on every trip, so a
    // back-edge that makes two calls there is slower than the two
    // unconditional calls it replaced. One call per slow block.
    for (call, block) in &calls {
        let sharing = calls.iter().filter(|(_, b)| b == block).count();
        assert_eq!(
            sharing, 1,
            "`{call}` shares `{block}` with another runtime call:\n{body}"
        );
    }
}

/// **The load has to stay in the loop once the optimiser has run.** The debug
/// test above reads IR nothing has touched, and debug is not what anyone
/// measures. A plain load of a global the loop never writes is one LLVM may
/// hoist out of the loop, and then the loop never sees a cancellation at all;
/// only a release build would show it.
///
/// Asserted structurally: in the optimised function some branch, from the
/// load's block or a block laid out after it, targets the load's block or one
/// laid out before it -- a back-edge around the load. A hoisted load sits in
/// the preheader, which nothing branches back to. Layout order stands in for
/// dominance, which holds for LLVM's printed order of a single loop and is
/// what makes this a check on this program rather than on any function.
#[test]
fn in_a_release_build_the_poll_is_read_inside_the_loop() {
    let body = function_ir_as("polls_release", LOOPS, "raising", Profile::Release);
    // (label, lines) for each block, in layout order.
    let mut blocks: Vec<(&str, Vec<&str>)> = vec![("entry", Vec::new())];
    for line in body.lines().skip(1) {
        if !line.starts_with(' ') {
            if let Some((label, _)) = line.split_once(':') {
                blocks.push((label, Vec::new()));
                continue;
            }
        }
        if let Some((_, lines)) = blocks.last_mut() {
            lines.push(line);
        }
    }
    let polled: Vec<usize> = (0..blocks.len())
        .filter(|&i| {
            blocks[i]
                .1
                .iter()
                .any(|l| l.contains(" load ") && l.contains("ptr @khora_poll"))
        })
        .collect();
    assert!(!polled.is_empty(), "no load of the poll word survived optimisation:\n{body}");
    for &at in &polled {
        let back_edge = blocks[at..].iter().any(|(_, lines)| {
            lines.iter().any(|l| {
                l.contains(" br ")
                    && blocks[..=at]
                        .iter()
                        .any(|(target, _)| l.contains(&format!("label %{target},")) || l.ends_with(&format!("label %{target}")))
            })
        });
        assert!(
            back_edge,
            "nothing branches back around the poll in `{}`, so it was hoisted out of the loop:\n{body}",
            blocks[at].0
        );
    }
}

/// A back-edge in a function with no row, in a program that spawns: a loop is
/// a cancellation point whatever the row says, so the one call is the
/// back-edge's, which asks both questions -- and it too behind the load.
#[test]
fn an_infallible_loop_in_a_spawning_program_calls_the_runtime_only_behind_the_poll() {
    let body = function_ir("polls_quiet", LOOPS, "quiet");
    let calls = calls_by_block(&body, &["khora_safepoint", "khora_cancelled", "khora_back_edge"]);
    assert_eq!(
        calls.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        vec!["khora_back_edge"],
        "one call at the back-edge that asks both questions:\n{body}"
    );
    for (call, block) in &calls {
        assert!(
            block.starts_with("poll.slow"),
            "`{call}` is in `{block}`, which every trip of the loop runs:\n{body}"
        );
    }
}

/// **The half of the poll word a spawning program cannot do without on the
/// scheduler: a fiber that never suspends still gives its worker back.**
///
/// On one worker, a fiber spins in an infallible loop until a second fiber,
/// queued behind it, sets a flag. The only thing that ever lets the second run
/// is the safepoint at the spinner's back-edge, and that is on the poll's slow
/// path -- so if the runtime left the word at zero while a pool was running,
/// the spinner would hold the worker for ever and this would time out.
///
/// Pinned to one CPU with `taskset`, because the pool sizes itself from the
/// affinity mask, and with two workers the setter would simply run on the
/// other one. Linux only for that reason.
///
/// Also asserts the spinner went round more than once per budget, so a green
/// run is not the setter having got in before the spinner started.
#[cfg(target_os = "linux")]
#[test]
fn a_spinning_fiber_on_the_scheduler_still_gives_its_worker_back() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("polls_preempt");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("program");
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let main = "module main;
import std::core::{Fiber, Shared, print};

fn spin(flag: Shared<Int>) -> Int {
  let mut turns = 0;
  while Shared::get(flag) == 0 { turns = turns + 1; };
  turns
}

pub fn main() -> () {
  let flag = Shared::of(0);
  let spinner = Fiber::spawn(fn () => spin(flag));
  let setter = Fiber::spawn(fn () => Shared::set(flag, 1));
  Fiber::join(setter);
  let turns = Fiber::join(spinner);
  print(if turns > 128 { \"the setter ran\" } else { \"the setter ran first\" });
}
";
    let root = SourceRoot::new(&db, with_std(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }

    let mut child = std::process::Command::new("taskset")
        .arg("-c")
        .arg(first_allowed_cpu())
        .arg(&exe)
        .env("KHORA_FIBERS", "scheduler")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("taskset and the program");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting") {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let status = status.expect(
        "the spinner held its only worker for 20 s: the back-edge never reached a safepoint",
    );
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().expect("stdout"), &mut out)
        .expect("reading stdout");
    assert!(status.success(), "exited {status:?}");
    assert_eq!(out.trim(), "the setter ran");
}

/// The lowest CPU this process may run on. Not always 0: a container's cpuset
/// may not include it, and `taskset -c 0` then fails with `EINVAL`.
#[cfg(target_os = "linux")]
fn first_allowed_cpu() -> String {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    let list = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .expect("a Cpus_allowed_list line");
    list.trim()
        .split([',', '-'])
        .next()
        .expect("at least one allowed CPU")
        .to_string()
}

/// Every `.kh` file of `std` for the host, plus the program.
fn with_std(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
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
