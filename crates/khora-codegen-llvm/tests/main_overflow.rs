#![cfg(feature = "llvm")]
#![cfg(unix)]

//! A main thread that runs out of stack while it starts a thread.
//!
//! **What this prevents: an overflow that ends the process with nothing on
//! stderr.** The runtime's `SIGSEGV` handler prints "the stack ran out", but a
//! handler runs only if the signal can be delivered, and glibc's
//! `pthread_create` blocks every signal while it builds the new thread. A
//! stack fault in that window is a `SIGSEGV` the thread has blocked, and Linux
//! answers it by restoring the default action and killing the process: no
//! handler, no message. A Khora HTTP server whose main thread ran out of stack
//! died exactly there, silently, at every stack size from 256 KB to 8 MB.
//!
//! The program recurses on `main`'s thread and starts a fiber at every level,
//! on the thread backend, so each level's deepest point is `pthread_create`.
//! Without the runtime's guard (`khora-rt`'s `stack::before_a_start`) it was
//! silent in 20 of 20 runs at each size below. Each run is under a lowered
//! stack limit, set with `setrlimit` in the child, so the overflow comes after
//! thousands of levels rather than a hundred thousand; each size runs [`RUNS`]
//! times, because the stack's start address, and with it where the last level
//! faults, changes from run to run.
//!
//! **Only the thread backend.** The scheduler starts its threads once, when the
//! first fiber is spawned, and a fiber on it is a coroutine rather than a
//! thread -- so a recursion on `main` never starts a thread deep in its stack,
//! and the same program passed there with the guard removed.
//!
//! **What it depends on: an allocator built optimized.** On a runtime whose
//! `mimalloc` was compiled at `opt-level = 0`, its frames are large enough that
//! the deepest point of a spawn was an allocator refill, outside the blocked
//! window, and this passed with the guard removed. The workspace builds
//! `libmimalloc-sys` optimized in every profile for that reason; see the root
//! `Cargo.toml`.

use crate::harness;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Stack limits the program is run under, in kilobytes.
const STACKS_KB: [u64; 4] = [256, 384, 512, 1024];

/// How many times each limit is run; the message is required every time.
const RUNS: usize = 20;

/// Unbounded recursion with a live frame under each call, starting and
/// joining a fiber at every level.
const SOURCE: &str = "module t;
fn print(value: Int);

pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> {
  fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;
  fn join(self) -> A raises 'r;
}

fn quiet() -> () { }

// In a function of its own, so the handle is released before the next level.
fn once() -> () {
  let f = Fiber::spawn(fn () => quiet());
  Fiber::join(f)
}

fn down(n: Int) -> Int {
  once();
  if n <= 0 { 0 } else { 1 + down(n - 1) }
}

fn main() -> Int { print(down(100000000)); 0 }
";

fn build() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("main_overflow");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("program");
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), SOURCE.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on the thread backend under a stack of `kb` kilobytes; what it
/// wrote to stderr, and how it ended.
fn run_under(exe: &PathBuf, kb: u64) -> (String, std::process::ExitStatus) {
    let bytes = kb * 1024;
    let mut command = Command::new(exe);
    command.env("KHORA_FIBERS", "threads");
    // SAFETY: runs in the forked child before `exec`, and calls only
    // `setrlimit`, which is async-signal-safe and touches no memory the parent
    // shares; the limit is a stack value that outlives the call.
    unsafe {
        command.pre_exec(move || {
            let limit = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
            if libc::setrlimit(libc::RLIMIT_STACK, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().expect("the program should run");
    (String::from_utf8_lossy(&output.stderr).into_owned(), output.status)
}

#[test]
fn a_main_thread_overflow_while_starting_a_thread_says_so() {
    let exe = build();
    let mut silent = Vec::new();
    for kb in STACKS_KB {
        let mut quiet = 0;
        for _ in 0..RUNS {
            let (said, status) = run_under(&exe, kb);
            assert!(!status.success(), "the recursion is unbounded and must not finish");
            if !said.contains("khora: the stack ran out") {
                quiet += 1;
            }
        }
        if quiet > 0 {
            silent.push(format!("{kb} KB: silent {quiet} of {RUNS}"));
        }
    }
    assert!(silent.is_empty(), "an overflow on main said nothing: {}", silent.join("; "));
}
