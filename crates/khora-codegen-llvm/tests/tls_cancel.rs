#![cfg(feature = "llvm")]

//! What a cancelled fiber lets go of when it was holding a TLS session.
//!
//! **This was attempted once and abandoned**, and the reason is the shape of
//! the test rather than the runtime. A client that connects, is left waiting
//! and eventually times out looks *identical* whether the server released the
//! session or never finished the handshake at all — so the earlier attempt
//! measured a five-second wait and could not say what it had measured.
//!
//! Two things fix that, and both are here:
//!
//! - **The server says how far it got.** It prints when it is listening, when
//!   the handshake has completed, and when it is holding the session. A client
//!   that sees nothing is then a different failure from a client that sees a
//!   connection stay open, and the assertions can tell them apart.
//! - **The handshake happens before anything can be cancelled.** `accept` and
//!   `secure` run on the main fiber, and only the finished session is handed
//!   to the fiber that gets cancelled. There is no race between a client
//!   connecting and a cancellation arriving.
//!
//! # Cancelling from outside, which nothing could do before
//!
//! There is no `Fiber::cancel`. What there is, is a nursery: the first child's
//! failure cancels its siblings, which is a cancellation arriving from outside
//! at a fiber that is not expecting one — exactly the case a `khora_cancel` in
//! the fiber's own body cannot produce.
//!
//! **Adoption order is load-bearing.** A nursery notices failures in the order
//! it adopted, so the doomed child is adopted first; with the holder first its
//! sibling's failure is not looked at until the holder has finished, and
//! nothing is ever cancelled. `process_cancel.rs` has the same note, and found
//! it the same way.
//!
//! # What the client proves
//!
//! It completes a handshake, then reads. A released session sends
//! `close_notify` and closes the socket, so the read returns *nothing*
//! promptly. A leaked one leaves the client blocked until its own timeout.
//! The assertion is therefore a clock as well as a byte count: the read must
//! come back well inside the timeout it was given.

mod harness;

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The port this binds. Its own, so a parallel test is not a flake here.
const PORT: u16 = 18847;

/// What the client waits before giving up, in seconds.
///
/// The floor the broken behaviour hits. A released session closes as soon as
/// the cancellation lands, so anything near this is a leak.
const CLIENT_TIMEOUT: u64 = 8;

/// How long the server stays up after the cancellation, in milliseconds.
///
/// Longer than `CLIENT_TIMEOUT`, so that a leaked session is still open when
/// the client gives up. This is the difference between a test and a test that
/// passes with the release deleted.
const LINGER: u64 = 12_000;

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

fn fixture(name: &str) -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    std::fs::read_to_string(&path).expect("the test certificate is in tests/fixtures")
}

/// The server: everything on the fiber that gets cancelled.
///
/// **The handshake cannot happen anywhere else.** A `TlsConnection` can be
/// written, so the compiler refuses to let one cross a fiber boundary — two
/// fibers writing one session is a race, and it says so. Handing a finished
/// session to the holder was the first shape tried and it does not compile,
/// which is the type system doing its job.
///
/// So the holder binds, accepts, secures and holds, and the margin below is
/// what keeps that deterministic: the client connects as soon as it sees
/// `listening`, and the doomed sibling waits far longer than a loopback
/// handshake takes. If that margin were ever lost, the `secured` assertion
/// fails by name rather than the test quietly proving nothing.
fn server_program() -> String {
    let certificate = fixture("localhost.cert.pem").replace('\n', "\\n");
    let key = fixture("localhost.key.pem").replace('\n', "\\n");
    format!(
        "module demo::main;

import std::clock::{{Clock}};
import std::core::{{ChildFailed, Fiber, Nursery, Result, Scope, acquire, attempt, nursery, scoped}};
import std::net::socket::{{accept_on, invalid_handle, listen_on, shut as shut_socket, start}};
import std::net::tls::{{TlsConnection, TlsError, TlsServer, secure, server, shut}};

fn print(value: String);

pub type Oops = | Bad;

/// A fallible call, so that `!` is a cancellation point. It never fails.
fn mark() -> Int raises Oops {{ 1 }}

/// A loop whose back-edge is a cancellation point, so the stop is noticed
/// while the session is still held rather than after the program has ended.
fn waiting() -> () raises Oops {{
  let mut spins = 0;
  while spins < 600 {{
    let _ = mark()!;
    with {{ clock: Clock::real() }} {{ clock.sleep(10) }};
    spins = spins + 1;
  }}
}}

/// Binds, accepts, completes the handshake, and then holds the session.
///
/// The `acquire` is the whole subject: the release is registered with the
/// region, and the only way out of here is a cancellation, so a release that
/// does not run on that path does not run at all.
fn hold() -> () with {{ scope: Scope }} raises Oops {{
  let settings = attempt(fn () => server(\"{certificate}\", \"{key}\")!);
  match settings {{
    Result::Err(_) => print(\"the certificate was refused, so this proves nothing\"),
    Result::Ok(ready) => {{
      let listener = listen_on({PORT});
      if listener == invalid_handle() {{
        print(\"the port could not be bound, so this proves nothing\")
      }} else {{
        print(\"listening\");
        let socket = accept_on(listener);
        shut_socket(listener);
        if socket == invalid_handle() {{
          print(\"nothing connected, so this proves nothing\")
        }} else {{
          match attempt(fn () => secure(ready, socket)!) {{
            Result::Err(_) => print(\"the handshake failed, so this proves nothing\"),
            Result::Ok(connection) => {{
              print(\"secured\");
              let held = acquire(connection, fn c => shut(c));
              print(\"holding\");
              waiting()!;
              print(\"the hold returned, which is wrong\")
            }},
          }}
        }}
      }}
    }},
  }}
}}

/// Fails, and is what cancels the holder.
///
/// It waits first, and the wait is this test's reliability: the holder has to
/// have finished its handshake and be inside the loop, and a loopback
/// handshake is milliseconds against this.
fn doomed() -> () raises Oops {{
  with {{ clock: Clock::real() }} {{ clock.sleep(1500) }};
  raise Oops::Bad
}}

/// **Adoption order is load-bearing.** A nursery notices failures in the order
/// it adopted, so with the holder first the doomed child is not looked at
/// until the holder has finished, and nothing is ever cancelled.
fn both() -> () with {{ nursery: Nursery }} {{
  nursery.adopt(Fiber::spawn(fn () => doomed()!));
  nursery.adopt(Fiber::spawn(fn () => scoped(fn () => hold()!)!));
}}

pub fn main() -> Int {{
  // WSAStartup on Windows, and nothing anywhere else. Without it every socket
  // call fails and the program reports a port it could not bind.
  if start() {{}} else {{ print(\"no sockets\") }};
  let _ = attempt(fn () => nursery(fn () => both())!);
  print(\"the nursery closed\");
  // **Staying alive is what makes this test mean anything.** Written without
  // it, the program ends here, the operating system closes every socket it
  // held, and the client sees the connection close whether or not the release
  // ever ran -- which is exactly how this test passed with the `acquire`
  // deleted. `net_cancel.rs` learned the same lesson. Holding the process open
  // for longer than the client is willing to wait leaves only one thing that
  // can close the connection: the region.
  with {{ clock: Clock::real() }} {{ clock.sleep({LINGER}) }};
  print(\"done\");
  0
}}
"
    )
}

/// Builds `main` and starts it.
fn start(name: &str, main: &str) -> Child {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }
    Command::new(&exe)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("`{name}` should start: {e}"))
}

/// Connects, finishes a handshake, then reads until the connection closes.
///
/// Prints how long the read took and how many bytes came back, so that a
/// timeout and a prompt close are told apart by the caller rather than by a
/// process exit code.
fn client_watching_for_close(port: u16) -> (bool, f64) {
    let script = format!(
        "import socket, ssl, sys, time\n\
         c = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)\n\
         c.check_hostname = False\n\
         c.verify_mode = ssl.CERT_NONE\n\
         s = c.wrap_socket(socket.create_connection(('127.0.0.1', {port}), timeout=10),\n\
         \x20   server_hostname='localhost')\n\
         s.settimeout({CLIENT_TIMEOUT})\n\
         began = time.time()\n\
         closed = False\n\
         try:\n\
         \x20   while True:\n\
         \x20       chunk = s.recv(4096)\n\
         \x20       if not chunk:\n\
         \x20           closed = True\n\
         \x20           break\n\
         except (socket.timeout, TimeoutError):\n\
         \x20   closed = False\n\
         except ssl.SSLError:\n\
         \x20   closed = True\n\
         except OSError:\n\
         \x20   closed = True\n\
         sys.stdout.write('%s %.3f' % (closed, time.time() - began))\n"
    );
    let out = Command::new("python")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("python should run; it is what drives the TLS tests too");
    assert!(
        out.status.success(),
        "the python client failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    let mut parts = said.split_whitespace();
    let closed = parts.next() == Some("True");
    let seconds: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(f64::MAX);
    (closed, seconds)
}

/// Reads the server's lines until `wanted` appears or its stdout ends.
fn wait_for(reader: &mut impl BufRead, wanted: &str, seen: &mut Vec<String>) -> bool {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {
                let line = line.trim_end().to_string();
                let hit = line.contains(wanted);
                seen.push(line);
                if hit {
                    return true;
                }
            }
        }
    }
}

/// **A cancelled fiber gives back the TLS session it was holding.**
#[test]
fn a_cancelled_fiber_releases_the_tls_session() {
    let mut child = start("tls_cancel", &server_program());
    let mut seen: Vec<String> = Vec::new();
    let stdout = child.stdout.take().expect("a piped stdout");
    let mut reader = std::io::BufReader::new(stdout);

    if !wait_for(&mut reader, "listening", &mut seen) {
        let _ = child.kill();
        panic!("the server never listened: {seen:?}");
    }

    let began = Instant::now();
    let (closed, read_seconds) = client_watching_for_close(PORT);
    let whole = began.elapsed();

    // Read up to the nursery closing, which happens at the cancellation.
    // Not `done`, which is deliberately a lingering sleep away.
    let _ = wait_for(&mut reader, "the nursery closed", &mut seen);
    let _ = child.kill();
    let _ = child.wait();
    let said = seen.join(" | ");

    assert!(
        seen.iter().any(|l| l.contains("secured")),
        "the handshake never completed, so nothing here is about cancellation: {said}"
    );
    assert!(
        seen.iter().any(|l| l.contains("holding")),
        "the session was never held by the fiber that gets cancelled: {said}"
    );
    assert!(
        !seen.iter().any(|l| l.contains("which is wrong")),
        "the holder was never cancelled, so this proves nothing: {said}"
    );
    assert!(
        closed,
        "the client waited {read_seconds:.3}s and the connection never closed: the cancelled \
         fiber leaked the session. {said}"
    );
    assert!(
        read_seconds < CLIENT_TIMEOUT as f64 / 2.0,
        "the connection closed only after {read_seconds:.3}s, which is the client giving up \
         rather than the server releasing anything (whole run {whole:?}). {said}"
    );
}
