#![cfg(feature = "llvm")]

//! `Handoff<A>`, end to end: a value that can be written, given to another
//! fiber.
//!
//! **What these guard: two fibers holding one writable object.** A hand-off's
//! send tests, by reference count, that nothing outside the value holds
//! anything writable in it, and traps naming the type when something does.
//! If the test answers "unique" wrongly, the failure is a use-after-free far
//! from its cause, so the debug owner check is the second guard: it traps the
//! first time a fiber counts a local object another fiber made, and a send
//! hands the objects it moves to the receiver.
//!
//! - [`a_value_held_only_by_itself_moves_on_both_backends`]: the connection's
//!   shape crosses, is written by the receiver, and comes back, with the owner
//!   check on and nothing leaked.
//! - [`a_kept_reference_traps_naming_the_type`]: the lockstep probe.
//! - [`the_owner_check_catches_a_walk_that_answers_unique`]: the mutation
//!   probe. The walk is told to pass what it should refuse, and the owner
//!   check must trap.
//! - [`an_aliased_share_part_sends_and_is_marked`]: a `Share` part held
//!   outside too.
//! - [`a_match_binding_sent_on_traps_and_says_why`]: the natural spelling the
//!   design review's own pool tripped on.
//! - [`a_handoffs_handle_is_lent_to_its_operations`]: `send`, `receive` and
//!   `close` borrow the handle.
//!
//! Every program runs on both fiber backends.

use crate::harness;

use std::path::PathBuf;
use std::process::{Command, Output};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The connection's shape, as `postgres::conn` has it: writable fields, byte
/// buffers, a `Map`, a list of names that are `Share`, and an option of a
/// record. About eleven objects once a statement is prepared.
const HEAD: &str = "module main;

import std::core::{print, Array, Fiber, Handoff, Channel, Map, List, Option, Result, Shared};

extern fn khora_live_count() -> Int;

type Session = { mut step: Int, first: String };

type Prepared = { name: String, names: List<String>, mut used: Int };

type Conn = {
  handle: Int,
  mut pending: Array<U8>,
  mut start: Int,
  mut scram: Option<Session>,
  mut broken: Bool,
  inbox: Array<U8>,
  prepared: Map<String, Prepared>,
  mut uses: Int,
  mut closing: List<String>,
};

/// The pool's `Lent`: a record holding the connection, held inline.
type Lent = { c: Conn };

fn connect(handle: Int) -> Conn {
  let c: Conn = {
    handle: handle, pending: Array::new(16, 0), start: 0,
    scram: Option::Some({ step: 1, first: \"n,,n=user\" }), broken: false,
    inbox: Array::new(64, 0), prepared: Map::new(), uses: 0, closing: List::Nil,
  };
  Map::insert(c.prepared, \"select 1\", { name: \"khora_1\", names: List::Cons(\"id\", List::Nil), used: 0 });
  c
}

/// What a borrower does with a connection: writes it.
fn use_it(c: Conn) -> Int {
  c.uses = c.uses + 1;
  c.pending = Array::new(8, 7);
  c.closing = List::Cons(\"khora_${c.uses}\", c.closing);
  c.uses
}

/// The payload of a `Some`, consuming the option, so no scrutinee holds it.
fn taken(o: Option<Lent>) -> Lent {
  match o {
    Option::Some(v) => v,
    Option::None => taken(o),
  }
}
";

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

/// Builds `body` after [`HEAD`] in `profile`.
fn build(name: &str, body: &str, profile: khora_codegen_llvm::Profile) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("handoff_{name}"));
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, &format!("{HEAD}\n{body}")));
    if let Err(errors) = khora_codegen_llvm::compile_with(&db, root, &exe, profile) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

fn run(exe: &PathBuf, backend: &str, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(exe);
    command.env("KHORA_FIBERS", backend);
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().expect("the program should run")
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Requires `expected` on stdout and a clean exit, on both backends.
fn runs_clean(name: &str, exe: &PathBuf, expected: &str) {
    for backend in BACKENDS {
        let out = run(exe, backend, &[]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stdout.trim() == expected,
            "`{name}` on `{backend}`: {:?}, stdout {stdout:?}, stderr {stderr:?}",
            out.status.code()
        );
    }
}

/// Requires a trap with every one of `said` in its message, on both backends.
fn traps(name: &str, exe: &PathBuf, env: &[(&str, &str)], said: &[&str]) {
    for backend in BACKENDS {
        let out = run(exe, backend, env);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(134),
            "`{name}` on `{backend}` should trap: stdout {stdout:?}, stderr {stderr:?}"
        );
        for part in said {
            assert!(stderr.contains(part), "`{name}` on `{backend}`: {part:?} not in {stderr:?}");
        }
    }
}

/// A pool of one: a fiber borrows the connection, writes it, and gives it
/// back, twenty times, while the lender waits. The `Lent` is taken out of
/// its `Option` through `taken` before each send.
const ROUND_TRIPS: &str = "
fn borrower(idle: Handoff<Lent>, back: Handoff<Lent>) -> Int {
  let mut total = 0;
  let mut going = true;
  while going {
    let got = Handoff::receive(idle);
    match got {
      Option::None => going = false,
      Option::Some(_) => {
        let lent = taken(got);
        total = total + use_it(lent.c);
        Handoff::send(back, lent);
        ()
      },
    }
  };
  total
}

fn lend() -> Int {
  let idle: Handoff<Lent> = Handoff::bounded(1);
  let back: Handoff<Lent> = Handoff::bounded(1);
  let f = Fiber::spawn(fn () => borrower(idle, back));
  let mut lent: Lent = { c: connect(5) };
  let mut i = 0;
  while i < 20 {
    Handoff::send(idle, lent);
    lent = taken(Handoff::receive(back));
    i = i + 1
  };
  Handoff::close(idle);
  let total = Fiber::join(f);
  total + lent.c.uses
}

pub fn main() -> Int {
  let n = lend();
  let live = khora_live_count();
  print(\"total ${n} live ${live}\");
  0
}
";

/// **The connection's shape moves and comes back, on both backends, with the
/// owner check on**, and nothing leaks. `1 + 2 + .. + 20` counted by the
/// borrower, and 20 read back by the lender.
///
/// The owner check is what makes this a test of the move rather than of a
/// program that happens to work: the borrower's first write to a connection
/// the lender made would trap if the send had not handed the objects over,
/// and the lender's first read after the round trip would trap if the
/// return had not.
#[test]
fn a_value_held_only_by_itself_moves_on_both_backends() {
    let exe = build("round_trips", ROUND_TRIPS, khora_codegen_llvm::Profile::Debug);
    runs_clean("round_trips", &exe, "total 230 live 0");
}

/// The same, built optimized, where no object records an owner: the walk
/// still has to pass, and the counts have to come out even.
#[test]
fn a_value_held_only_by_itself_moves_in_a_release_build() {
    let exe = build("round_trips_release", ROUND_TRIPS, khora_codegen_llvm::Profile::Release);
    runs_clean("round_trips_release", &exe, "total 230 live 0");
}

/// A sender that keeps a writable part of what it sends: the connection's
/// `pending` buffer, read after the send.
const KEEPS_A_PART: &str = "
fn receiver(h: Handoff<Conn>) -> Int {
  match Handoff::receive(h) {
    Option::Some(c) => use_it(c),
    Option::None => 0,
  }
}

pub fn main() -> Int {
  let h: Handoff<Conn> = Handoff::bounded(1);
  let f = Fiber::spawn(fn () => receiver(h));
  let c = connect(5);
  let kept = c.pending;
  Handoff::send(h, c);
  let n = Fiber::join(f);
  print(\"sent ${n}, kept ${Array::length(kept)}\");
  0
}
";

/// **The lockstep probe: one extra reference to a writable part, and the
/// send traps naming the type**, in a release build as well as a debug one.
/// Marking the value instead, as a channel does, would let the receiver
/// write `pending` while the sender reads it.
#[test]
fn a_kept_reference_traps_naming_the_type() {
    for profile in [khora_codegen_llvm::Profile::Debug, khora_codegen_llvm::Profile::Release] {
        let name = format!("keeps_a_part_{profile:?}");
        let exe = build(&name, KEEPS_A_PART, profile);
        traps(
            &name,
            &exe,
            &[],
            &["a `Handoff` send of `Conn` found `Array<U8>` still held outside the value"],
        );
    }
}

/// The same program's shape where the extra holder is a whole binding the
/// sender reads afterwards, for the mutation probe: `c` itself.
const KEEPS_THE_WHOLE: &str = "
fn receiver(h: Handoff<Conn>) -> Int {
  match Handoff::receive(h) {
    Option::Some(c) => {
      let mut i = 0;
      let mut n = 0;
      while i < 200 { n = use_it(c); i = i + 1 };
      n
    },
    Option::None => 0,
  }
}

pub fn main() -> Int {
  let h: Handoff<Conn> = Handoff::bounded(1);
  let f = Fiber::spawn(fn () => receiver(h));
  let c = connect(5);
  let kept = c.pending;
  Handoff::send(h, c);
  let mut i = 0;
  let mut seen = 0;
  while i < 200 { seen = seen + Array::length(kept); i = i + 1 };
  let n = Fiber::join(f);
  print(\"sent ${n}, saw ${seen}\");
  0
}
";

/// **The mutation probe: a walk that answers "unique" when it is not must be
/// caught by the debug owner check.** `KHORA_HANDOFF_MUTANT=unique` makes the
/// send skip its trap, as a wrong walk would. The sender then counts the
/// `pending` buffer it kept, which the send handed to the receiver, and the
/// owner check traps on that count.
///
/// The control is [`a_kept_reference_traps_naming_the_type`]: without the
/// switch the same shape traps at the send.
#[test]
fn the_owner_check_catches_a_walk_that_answers_unique() {
    let exe = build("mutant", KEEPS_THE_WHOLE, khora_codegen_llvm::Profile::Debug);
    traps(
        "mutant",
        &exe,
        &[("KHORA_HANDOFF_MUTANT", "unique")],
        &["was counted on fiber", "without being shared"],
    );
}

/// The same mistake, counted before anybody receives: the value sits in the
/// queue while the sender reads the part it kept.
const KEEPS_BEFORE_THE_RECEIVE: &str = "
fn width(a: Array<U8>) -> Int { Array::length(a) }

pub fn main() -> Int {
  let h: Handoff<Conn> = Handoff::bounded(1);
  let c = connect(5);
  let kept = c.pending;
  Handoff::send(h, c);
  // Each call takes its own reference to `kept`: a count, on this fiber,
  // of an object the send gave away, while it is still in the queue.
  let n = width(kept) + width(kept);
  print(\"kept ${n}\");
  Handoff::close(h);
  // A fiber, so the build is one with the owner check; it never receives.
  let f = Fiber::spawn(fn () => 0);
  Fiber::join(f)
}
";

/// **A part the sender kept is caught before the receive, too.** Between a
/// send and its receive nobody owns what moved, and a debug build writes an
/// owner no fiber has, so the sender's own next count traps. Clearing the
/// owner instead -- what the design review's prototype did -- turns the
/// check off for that window, and this program then runs to the end.
///
/// A program that spawns nothing is built without the owner check at all
/// (it has one fiber), so this one spawns a fiber that does nothing.
#[test]
fn the_owner_check_catches_a_kept_part_before_the_receive() {
    let exe = build("before_receive", KEEPS_BEFORE_THE_RECEIVE, khora_codegen_llvm::Profile::Debug);
    traps(
        "before_receive",
        &exe,
        &[("KHORA_HANDOFF_MUTANT", "unique")],
        &["object made on fiber 4194303 was counted on fiber"],
    );
}

/// A value with a `Share` part the sender keeps: the prepared statement's
/// `names`, a list of strings, read by both fibers at once.
const ALIASED_SHARE: &str = "
fn receiver(h: Handoff<Conn>) -> Int {
  match Handoff::receive(h) {
    Option::Some(c) => {
      let n = use_it(c);
      let mut i = 0;
      let mut total = 0;
      while i < 2000 {
        total = total + match Map::get(c.prepared, \"select 1\") {
          Option::Some(p) => List::length(p.names),
          Option::None => 0,
        };
        i = i + 1
      };
      total + n
    },
    Option::None => 0,
  }
}

pub fn main() -> Int {
  let h: Handoff<Conn> = Handoff::bounded(1);
  let f = Fiber::spawn(fn () => receiver(h));
  let c = connect(5);
  let names = match Map::get(c.prepared, \"select 1\") {
    Option::Some(p) => p.names,
    Option::None => List::Nil,
  };
  Handoff::send(h, c);
  let mut i = 0;
  let mut mine = 0;
  while i < 2000 { mine = mine + List::length(names); i = i + 1 };
  let theirs = Fiber::join(f);
  print(\"theirs ${theirs} mine ${mine}\");
  0
}
";

/// **A value with an aliased `Share` part sends, and the part is marked.**
/// Both fibers read `names` two thousand times while the other does. The
/// mark is seen through the owner check: an unmarked list made on the sender
/// traps on the receiver's first count of it. (The runtime's own test,
/// `khora-rt`'s `an_aliased_share_part_is_marked_and_the_rest_moves`, reads
/// the bit directly.)
#[test]
fn an_aliased_share_part_sends_and_is_marked() {
    let exe = build("aliased_share", ALIASED_SHARE, khora_codegen_llvm::Profile::Debug);
    runs_clean("aliased_share", &exe, "theirs 2001 mine 2000");
}

/// The spelling the design review's pool used first: send the binding a
/// `match` arm made, straight out of a receive.
const MATCH_BINDING: &str = "
fn relay(from: Handoff<Lent>, to: Handoff<Lent>) -> Int {
  match Handoff::receive(from) {
    Option::Some(lent) => { Handoff::send(to, lent); () },
    Option::None => (),
  };
  match Handoff::receive(to) {
    Option::Some(lent) => use_it(lent.c),
    Option::None => 0,
  }
}

fn relayed() -> Int {
  let a: Handoff<Lent> = Handoff::bounded(1);
  let b: Handoff<Lent> = Handoff::bounded(1);
  Handoff::send(a, { c: connect(1) });
  let f = Fiber::spawn(fn () => relay(a, b));
  Fiber::join(f)
}

pub fn main() -> Int {
  let n = relayed();
  let live = khora_live_count();
  print(\"relayed ${n} live ${live}\");
  0
}
";

/// **Sending a `match` arm's binding passes when the value matched on is not
/// used again.** The arm takes its own reference and the scrutinee is
/// released at the head of the arm, before the body runs, so the binding is
/// the only holder by the send. The design review's prototype trapped on
/// this spelling; this pins that the tree the hand-off landed on does not,
/// and the std documentation says so.
#[test]
fn a_match_binding_sent_on_moves() {
    let exe = build("match_binding", MATCH_BINDING, khora_codegen_llvm::Profile::Debug);
    runs_clean("match_binding", &exe, "relayed 1 live 0");
}

/// The shape that does trap: the value the sent one was taken out of is
/// read again after the send, so it still holds it.
const READ_AGAIN: &str = "
fn open(ok: Bool) -> Result<Conn, String> {
  if ok { Result::Ok(connect(1)) } else { Result::Err(\"down\") }
}

fn lend(to: Handoff<Conn>) -> Int {
  let got = open(true);
  match got {
    Result::Ok(c) => { Handoff::send(to, c); () },
    Result::Err(_) => (),
  };
  match got { Result::Ok(_) => 1, Result::Err(_) => 0 }
}

pub fn main() -> Int {
  let h: Handoff<Conn> = Handoff::bounded(1);
  let f = Fiber::spawn(fn () => lend(h));
  print(\"lent ${Fiber::join(f)}\");
  0
}
";

/// **Sending a part of a value that is read again traps, naming the type
/// and the fiber, and saying what kind of holder to look for.** The runtime
/// sees a count, not the holders, so it cannot say which binding it is.
#[test]
fn a_value_read_again_after_its_part_is_sent_traps() {
    let exe = build("read_again", READ_AGAIN, khora_codegen_llvm::Profile::Debug);
    traps(
        "read_again",
        &exe,
        &[],
        &[
            "a `Handoff` send of `Conn` found it still held outside the value",
            "a value it was taken out of that is read again later on fiber ",
        ],
    );
}

/// Each function's last use of its handle is one hand-off operation, and
/// the live count straight after it says whether the call took the
/// binding's reference (the handle is gone) or looked at it (the binding
/// still holds it, and its block releases it on the way out).
const LAST_USE: &str = "
fn sends() -> Int {
  let base = khora_live_count();
  let h: Handoff<Int> = Handoff::bounded(1);
  Handoff::send(h, 7);
  khora_live_count() - base
}

fn receives() -> Int {
  let base = khora_live_count();
  let h: Handoff<Int> = Handoff::bounded(1);
  Handoff::send(h, 7);
  let got = Handoff::receive(h);
  let n = khora_live_count() - base;
  match got { Option::Some(_) => n, Option::None => 0 - 1 }
}

fn closes() -> Int {
  let base = khora_live_count();
  let h: Handoff<Int> = Handoff::bounded(1);
  Handoff::close(h);
  khora_live_count() - base
}

pub fn main() -> Int {
  let s = sends();
  let r = receives();
  let c = closes();
  let live = khora_live_count();
  print(\"send ${s} receive ${r} close ${c} live ${live}\");
  0
}
";

/// **A hand-off's handle is lent to `send`, `receive` and `close`, as a
/// channel's is.** The operations only look at the handle, so a caller that
/// handed each one an owned reference paid a count up before the call and
/// down inside it: two atomic operations per call, on a handle every lease
/// crosses twice. The value `send` is given is not lent; the queue takes it.
///
/// Taken, the handle is freed by the call, and the count straight after is
/// `send 0 receive 0 close 0` (the `7` is a machine word, so the queue holds
/// no object). Lent, the binding still holds it until its block ends:
/// `1 1 1`.
#[test]
fn a_handoffs_handle_is_lent_to_its_operations() {
    let exe = build("last_use", LAST_USE, khora_codegen_llvm::Profile::Debug);
    runs_clean("last_use", &exe, "send 1 receive 1 close 1 live 0");
}
