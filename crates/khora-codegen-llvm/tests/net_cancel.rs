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
//! in *this* file can: `khora_cancel` sets the flag on the running fiber and
//! these two programs hold no handle to the listener.
//!
//! **A handler cancelling itself is a third test, and it used to be a hole.**
//! The comment here said the cancellation reached the connection fiber's root,
//! "which the runtime still declines, and the process stops" — which was true,
//! and was the whole of the release blocker: one cancelled request took the
//! server down with status 134. `Router::serve_connection` catches `_` and has
//! no `raises` row, so it is the frame with nowhere to send one. It absorbs
//! now, and `a_handler_that_cancels_itself_stops_its_connection_and_not_the_server`
//! is the proof.
//!
//! Both would have failed before the fix, for the reason the fix exists:
//! `std::net::socket` registered no release at all, so a socket was closed only
//! by a normal return, which is the one exit a server never takes.
//!
//! # And it guards a second thing, which it found
//!
//! The peer here holds the connection open and says nothing, which is the shape
//! that made `shut` wait for the platform's own `FIN_WAIT_2` timeout — 120
//! seconds, `docs/errata.md` 78. So the last assertion is a clock: the program
//! must finish with the connection in seconds, against a floor of sixty that
//! the platform imposes on the old behaviour.

use crate::harness;

use std::io::Read;
use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Not 18732 — `http.rs` binds that, and the two run at once.
const LISTENER_PORT: u16 = 18961;
const CONNECTION_PORT: u16 = 18962;
/// And a third, for the server that keeps running after a cancelled request.
const HANDLER_PORT: u16 = 18963;

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
import std::core::{{Fiber, Scope, acquire, scoped}};
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
/// Cancelled with the connection open and nothing written, which a `catch`
/// cannot reach and only the region covers.
fn serve(server: Int) -> () with {{ scope: Scope }} raises Oops {{
  let connection = acquire(accept_on(server), fn c => shut(c));
  if connection == invalid_handle() {{
    print(\"nothing connected, so this proves nothing\")
  }} else {{
    khora_cancel();
    let _ = mark()!;
    print(\"the answer was written, which is wrong\")
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
    let f = Fiber::spawn(fn () => scoped(fn () => serve(server)!)!);
    Fiber::wait(f);
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

    // **Read to the end, and timed.** This was `child.kill()` for a while,
    // because a program that had accepted a connection did not reach its next
    // line for 120 seconds -- `docs/errata.md` 78, which was `shut` draining
    // with a read that waits while the peer above held the connection open and
    // said nothing. It is fixed, and reading to the end here is what keeps it
    // fixed: the peer is *still* open and silent at this point, which is
    // exactly the shape that used to hang.
    let finishing = std::time::Instant::now();
    let settled = wait_for(&mut stdout, &mut said, "the connection was let go");
    let ended = child.wait().expect("it should exit");
    let took = finishing.elapsed();

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
    assert!(settled, "it never finished with the connection: {said}");
    assert_eq!(ended.code(), Some(0), "{said}");
    // **Seconds, against a defect measured in minutes.** Generous enough that a
    // loaded machine does not fail it and tight enough that a `shut` waiting on
    // a silent peer cannot pass: the platform's own floor for that wait is 60
    // seconds on Linux and 120 on Windows.
    assert!(
        took < std::time::Duration::from_secs(20),
        "closing a connection whose peer is open and silent took {took:?}, which is          `docs/errata.md` 78 coming back"
    );
}

/// **A cancellation reaching a connection fiber's root stops that connection,
/// not the server.**
///
/// This is the shape `khora_cancel_stop`'s comment used to name, and it named
/// it because `Router::served` is a function with no `raises` row that catches
/// `_` -- "what a fiber runs, and it does not fail". A cancellation is in no
/// row, so the `_` arm cannot name it, and the frame has no tagged return to
/// pass it on with. Before, one cancelled request took the whole server down
/// with status 134.
///
/// A handler cancelling itself is the only way a *program* can reach that
/// frame from inside, and it stands in for every other way of getting there:
/// the nursery cancelling a sibling, or the listener being asked to stop with
/// connections in flight.
///
/// **The handler has to be able to raise**, and that is not a detail of the
/// test -- it is the rule. A router whose handlers raise nothing has no tagged
/// return anywhere between `Router::dispatch` and here, so there is no channel
/// for a cancellation to travel and the flag is simply never read. `Oops` is
/// never raised; declaring it is what puts the `!` in `dispatch`'s caller.
///
/// Three requests, and the shape of the answers is the assertion. `/health`
/// answers before and after; `/stop` does not answer at all, because its fiber
/// stopped where the cancellation reached it and the region shut the socket on
/// the way out. The second `/health` is the whole point: it proves the process
/// that served the cancelled request is still there.
#[test]
fn a_handler_that_cancels_itself_stops_its_connection_and_not_the_server() {
    let exe = build(
        "net_cancel_handler",
        &format!(
            "module demo::main;
import std::core::{{SharedFn, print}};
import std::net::http::{{Request, Response, Router}};

extern fn khora_cancel();

pub type Oops = | Bad;

/// A fallible call, so that `!` is a cancellation point. It never fails.
fn mark() -> Int raises Oops {{ 1 }}

/// Runs on the connection's own fiber, under `Router::served`.
fn stopping(_request: Request) -> Response raises Oops {{
  khora_cancel();
  let _ = mark()!;
  Response::text(200, \"the cancellation was not seen\")
}}

fn healthy(_request: Request) -> Response raises Oops {{
  Response::text(200, \"ok\")
}}

pub fn main() -> Int {{
  Router::listen(
    Router::get(
      Router::get(Router::new(), \"/health\", SharedFn::of(fn r => healthy(r)!)),
      \"/stop\",
      SharedFn::of(fn r => stopping(r)!),
    ),
    {HANDLER_PORT},
  )! catch {{
    _ => print(\"the listener stopped\"),
  }};
  0
}}
"
        ),
    );

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the program should start");
    let mut stdout = child.stdout.take().expect("piped");
    let mut said = String::new();
    if !wait_for(&mut stdout, &mut said, &format!("listening on {HANDLER_PORT}")) {
        let _ = child.kill();
        panic!("it never bound: {said}");
    }

    let before = ask(HANDLER_PORT, "/health");
    let cancelled = ask(HANDLER_PORT, "/stop");
    let after = ask(HANDLER_PORT, "/health");
    let alive = child.try_wait().expect("asking after the child").is_none();
    let _ = child.kill();
    let _ = child.wait();

    assert!(before.contains("200"), "the server was not answering to begin with: {before:?}");
    assert!(
        !cancelled.contains("the cancellation was not seen"),
        "the handler ran past its own cancellation point: {cancelled:?}"
    );
    assert!(
        after.contains("200"),
        "the server did not survive a cancelled request: {after:?} {said}"
    );
    assert!(alive, "the server was gone after the cancelled request: {said}");
}

/// One HTTP/1.1 GET, spoken by hand.
///
/// A raw socket rather than `std::net::http`'s own client, so that what is
/// under test is the server and not a second thing that could also be wrong.
/// An empty answer is a connection that was accepted and closed without a
/// reply, which is what a stopped connection fiber leaves behind.
fn ask(port: u16, path: &str) -> String {
    use std::io::Write;

    let Ok(mut socket) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return String::new();
    };
    socket.set_read_timeout(Some(DEADLINE)).expect("a read deadline");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if socket.write_all(request.as_bytes()).is_err() {
        return String::new();
    }
    let mut answer = Vec::new();
    let _ = socket.read_to_end(&mut answer);
    String::from_utf8_lossy(&answer).into_owned()
}
