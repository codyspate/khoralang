#![cfg(feature = "llvm")]

//! What a trap inside a request handler does to the server.
//!
//! **The documentation makes a strong claim and nothing was holding it.** The
//! reference says a trap in a handler ends the process rather than the
//! request, and the HTTP cookbook now tells people to run more than one
//! instance because of it. That claim is the reason a service is architected
//! one way rather than another, and until this file there was no test of it
//! anywhere: every trap test ran a program with no server in it, and every
//! server test used handlers that did not trap.
//!
//! It is worth testing in both directions, because the code around a handler
//! reads as though the claim were false. `serve_connection` wraps the handler
//! in `catch { _ => 500 }`, which looks like a request-level safety net and is
//! one — for `raises`. A trap does not unwind, so it goes straight past.
//! Asserting only that the trap kills the server would leave the other half
//! untested and would pass just as well if the wrapper caught nothing at all.

use crate::harness;

use std::io::{Read, Write};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Its own port: `tests/http.rs` explains why two servers in one binary is a
/// bind failure and a connection reset rather than two results.
const PORT: u16 = 18733;

const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Every `.kh` file of `std`, plus the server below.
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

/// Three routes: one that answers, one that raises, one that traps.
///
/// `/raise` is the control. It fails in the way the language has a mechanism
/// for, so the router turns it into a 500 and the server carries on — which is
/// what makes `/trap` a claim about traps rather than about handlers that go
/// wrong in general.
const SERVER: &str = "module demo::main;
import std::core::{ChildFailed, Option, SharedFn};
import std::net::http::{HttpError, Request, Response, Router};

fn print(value: String);

pub type Broke = | Deliberately;

fn fine(_req: Request) -> Response {
  Response::text(200, \"fine\")
}

/// Fails the way the language provides for, so the wrapper answers 500.
fn raising(_req: Request) -> Response raises Broke {
  raise Broke::Deliberately
}

/// Overflows. Nothing catches this: it does not unwind.
///
/// The addend comes from the request so that the arithmetic cannot be folded
/// away at compile time -- a constant overflow is a compile error, and a
/// handler the optimiser proved unreachable would test nothing.
fn trapping(req: Request) -> Response {
  let step = String::byte_length(req.path);
  let mut n = 9223372036854775807;
  n = n + step;
  Response::text(200, Int::to_string(n))
}

pub fn main() raises HttpError + ChildFailed {
  Router::new()
    |> Router::get(\"/fine\", SharedFn::of(fine))
    |> Router::get(\"/raise\", SharedFn::of(fn req => raising(req)! catch { _ => Response::text(500, \"raised\") }))
    |> Router::get(\"/trap\", SharedFn::of(trapping))
    |> Router::listen(@PORT@)!
}
";

struct Server {
    child: std::process::Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Server {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("http_trap_server");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "server.exe" } else { "server" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let main = SERVER.replace("@PORT@", &PORT.to_string());
    let root = SourceRoot::new(&db, sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("the test server did not build:\n  {}", messages.join("\n  "));
    }

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the server should start");

    let mut stdout = child.stdout.take().expect("piped");
    let mut opened = [0u8; 9];
    let mut at = 0;
    while at < opened.len() {
        match stdout.read(&mut opened[at..]) {
            Ok(0) => {
                let _ = child.kill();
                panic!("the server stopped before it was listening");
            }
            Ok(read) => at += read,
            Err(e) => {
                let _ = child.kill();
                panic!("reading the server's output: {e}");
            }
        }
    }
    assert_eq!(&opened, b"listening", "expected the server to announce itself");
    Server { child }
}

fn connect() -> Option<std::net::TcpStream> {
    for _ in 0..100 {
        if let Ok(socket) = std::net::TcpStream::connect(("127.0.0.1", PORT)) {
            socket.set_read_timeout(Some(DEADLINE)).expect("a read deadline");
            socket.set_write_timeout(Some(DEADLINE)).expect("a write deadline");
            return Some(socket);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    None
}

/// Sends one request on a fresh connection and returns whatever came back,
/// which for a server that died mid-answer is nothing.
fn ask(path: &str) -> String {
    let Some(mut socket) = connect() else { return String::new() };
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    if socket.write_all(request.as_bytes()).is_err() {
        return String::new();
    }
    let _ = socket.flush();
    let mut answer = String::new();
    // Reading to the close: `Connection: close` means the server's own FIN is
    // the end of the message, and a server that trapped sends a reset instead,
    // which is an outcome here rather than a failure.
    let _ = socket.read_to_string(&mut answer);
    answer
}

/// The whole claim, in the order the reader of the documentation meets it.
///
/// One test rather than three: each step depends on the server still being in
/// the state the step before left it in, and the last step deliberately ends
/// the process. Splitting them would need three servers on three ports to
/// assert one thing each about the same server.
#[test]
fn a_trap_in_a_handler_ends_the_server_and_a_raise_does_not() {
    let mut server = start();

    // --- an ordinary request is answered
    let ok = ask("/fine");
    assert!(ok.starts_with("HTTP/1.1 200"), "expected an answer, got {ok:?}");
    assert!(ok.ends_with("fine"), "expected the body, got {ok:?}");

    // --- a raise is contained at the request, which is the control
    //
    // If this were the trap's behaviour too, the assertion below would be
    // testing nothing about traps.
    let raised = ask("/raise");
    assert!(raised.starts_with("HTTP/1.1 500"), "a raise is a 500, got {raised:?}");
    let after = ask("/fine");
    assert!(
        after.starts_with("HTTP/1.1 200"),
        "and the server is still serving afterwards, got {after:?}"
    );

    // --- a trap is not
    let trapped = ask("/trap");
    assert!(
        !trapped.starts_with("HTTP/1.1 200"),
        "the handler cannot have answered: {trapped:?}"
    );

    let status = server.child.wait().expect("the server should have ended");
    assert_eq!(
        status.code(),
        Some(134),
        "a trap in a handler ends the process with a trap's status, not the request"
    );

    let mut said = String::new();
    if let Some(mut stderr) = server.child.stderr.take() {
        let _ = stderr.read_to_string(&mut said);
    }
    assert!(
        said.contains("overflowed"),
        "and it says what happened on the way out: {said:?}"
    );
}
