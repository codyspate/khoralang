#![cfg(feature = "llvm")]

//! What a cancelled fiber lets go of when it was holding a socket.
//!
//! **Not "does not crash".** `fs.rs` proves its claim by *deleting* the file,
//! which Windows refuses while a handle is open. A socket has two equivalents
//! and both are here: a port that binds again, and a connection the peer sees
//! close.
//!
//! # What these pin, and what they cannot
//!
//! They pin the *shape* `std::net::http` now uses — `acquire` inside a region,
//! rather than a close written after the loop — against a real cancellation,
//! and they check it where the outside world can see it.
//!
//! What they cannot do is cancel `Router::listen`'s own fiber, because nothing
//! in Khora can: `khora_cancel` sets the flag on the *running* fiber and there
//! is no `Fiber::cancel`. Nor can a handler usefully cancel itself — the
//! cancellation reaches the connection fiber's root, which the runtime still
//! declines, and the process stops. So the call sites are covered by review and
//! the mechanism they rest on is covered here.
//!
//! Both would have failed before the fix, for the reason the fix exists:
//! `std::net::socket` registered no release at all, so a socket was closed only
//! by a normal return, which is the one exit a server never takes.

mod harness;

use std::io::Read;
use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Not 18732 — `http.rs` binds that, and the two run at once.
const LISTENER_PORT: u16 = 18961;
const CONNECTION_PORT: u16 = 18962;

/// A test that hangs is worse than one that fails: the failure at least says
/// what happened.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Every `.kh` file of `std`, plus the program under test, compiled.
fn build(name: &str, main: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let mut files = Vec::new();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                files.push(SourceFile::new(&db, path, text));
            }
        }
    }
    files.push(SourceFile::new(&db, dir.join("main.kh"), main.to_string()));

    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Reads whole lines from the program until one of them is `line`.
///
/// **Not a fixed-length prefix.** The program says several things before the
/// one being waited for, and a read of exactly nine bytes would take the first
/// nine of the wrong sentence.
fn wait_for(stdout: &mut impl Read, seen: &mut String, line: &str) -> bool {
    loop {
        if seen.lines().any(|each| each.trim_end() == line) {
            return true;
        }
        let mut chunk = [0u8; 256];
        match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&chunk[..n])),
        }
    }
}

/// **A cancelled fiber gives back both a port and a connection.**
///
/// Two claims and one program, because building `std` twice to make two
/// assertions costs two minutes and proves nothing extra.
///
/// The first half binds a port, is cancelled holding it, and binds again —
/// which is the situation `Router::held_open` is in permanently, since its loop
/// has no exit to hang a close on. `listen_on` sets no `SO_REUSEADDR`, so a
/// second bind succeeds only if the first listener was closed.
///
/// The second half is what the release is worth to the other end. It accepts a
/// connection, registers the close the way `Router::served` now does, and is
/// cancelled with nothing written. **The assertion is a read that returns**,
/// not a read that returns something in particular: a closed socket answers
/// end-of-file or a reset, and a leaked one answers nothing at all and waits
/// out the deadline — which is what a client of the unfixed server saw.
#[test]
fn a_cancelled_fiber_gives_back_the_socket_it_was_holding() {
    let exe = build(
        "net_cancel",
        &format!(
            "module demo::main;
import std::core::{{Fiber, Scope, acquire, attempt, scoped}};
import std::net::socket::{{accept_on, invalid_handle, listen_on, shut, start}};

fn print(value: String);
extern fn khora_cancel();

pub type Oops = | Bad;

/// A fallible call, so that `!` is a cancellation point. It never fails.
fn mark() -> Int raises Oops {{ 1 }}

/// What `Router::held_open` does, written out: take the port, register the
/// close, then never reach a line that mentions it again.
fn hold() -> () with {{ scope: Scope }} raises Oops {{
  let server = acquire(listen_on({LISTENER_PORT}), fn s => shut(s));
  if server == invalid_handle() {{
    print(\"the fiber could not bind, so this proves nothing\")
  }} else {{
    khora_cancel();
    let _ = mark()!;
    print(\"the hold returned, which is wrong\")
  }}
}}

/// What `Router::served` does: register the close, then answer. It leaves by
/// raising, before a byte is written -- the exit `serve_connection`'s `catch`
/// does not cover and only the region does.
///
/// **Raising rather than cancelling, here.** The port half above is the
/// cancellation test; this half is about what the release is worth to the peer,
/// and it must not `Fiber::wait` on a fiber that has called `accept_on`, which
/// takes 120 seconds -- `docs/errata.md` 78.
fn serve(server: Int) -> () with {{ scope: Scope }} raises Oops {{
  let connection = acquire(accept_on(server), fn c => shut(c));
  if connection == invalid_handle() {{
    print(\"nothing connected, so this proves nothing\")
  }} else {{
    raise Oops::Bad
  }}
}}

/// Cancelled holding a listening socket, then the port asked for again.
fn the_port() -> () {{
  let f = Fiber::spawn(fn () => scoped(fn () => hold()!)!);
  Fiber::wait(f);
  let again = listen_on({LISTENER_PORT});
  if again == invalid_handle() {{
    print(\"the port is still held\")
  }} else {{
    shut(again);
    print(\"the port was released\")
  }}
}}

/// Left holding an accepted connection, with a peer watching.
fn the_connection() -> () {{
  let server = listen_on({CONNECTION_PORT});
  if server == invalid_handle() {{
    print(\"could not bind, so this proves nothing\")
  }} else {{
    print(\"listening\");
    let _ = attempt(fn () => scoped(fn () => serve(server)!)!);
    print(\"the connection was let go\");
    shut(server)
  }}
}}

pub fn main() -> Int {{
  if start() {{}} else {{ print(\"no sockets\") }};
  the_port();
  the_connection();
  0
}}
"
        ),
    );

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the program should start");
    let mut stdout = child.stdout.take().expect("piped");
    let mut said = String::new();

    // The port half finishes before the connection half announces itself.
    if !wait_for(&mut stdout, &mut said, "listening") {
        let _ = child.kill();
        panic!("it stopped before it was listening: {said}");
    }

    let mut opened = None;
    for _ in 0..100 {
        if let Ok(connected) = std::net::TcpStream::connect(("127.0.0.1", CONNECTION_PORT)) {
            connected.set_read_timeout(Some(DEADLINE)).expect("a read deadline");
            opened = Some(connected);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut socket = match opened {
        Some(socket) => socket,
        None => {
            let _ = child.kill();
            panic!("could not reach it on {CONNECTION_PORT}: {said}");
        }
    };

    let mut got = Vec::new();
    let closed = match socket.read_to_end(&mut got) {
        Ok(_) => true,
        Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset,
    };

    // **Nothing more is read from it.** A Khora program that has accepted a
    // connection does not reach its next line for 120 seconds -- `docs/errata.md`
    // 78, a defect of its own and not anything this test is about. Everything
    // asserted below is already in hand: the port half printed before the
    // announcement, and the connection half is proved at this end.
    let _ = child.kill();
    let _ = child.wait();

    assert!(!said.contains("proves nothing"), "nothing was tested: {said}");
    assert!(!said.contains("which is wrong"), "a cancelled fiber ran on: {said}");
    assert!(
        said.contains("the port was released"),
        "a cancelled fiber left the port bound: {said}"
    );
    assert!(
        closed,
        "the cancelled fiber left the connection open; the read waited out its deadline: {said}"
    );
}
