#![cfg(feature = "llvm")]

//! `std::net::http` is layered, and this is the proof.
//!
//! **A framework of a different shape, using nothing `Router` has to itself.**
//! The server below is a middleware chain — the shape hapi and Koa have and
//! Khora's `Router` does not — built from `Connection`, `Incoming`, `parse`,
//! `matches`, `Response` and `over_socket`. It does not import `Router` at all.
//!
//! **And it does not spawn.** One connection at a time, on the thread that
//! started. `Router` gives each connection a fiber in its accept loop, and this
//! is what says that decision belongs to the accept loop rather than to
//! anything underneath it: an author who wants a synchronous server, a bounded
//! pool, or something else again writes their own loop and gives up nothing.
//!
//! What it gets for free is the part that is the same whatever shape the
//! framework has, and the part that fails in production rather than in testing:
//! reading until a request is whole, honouring `Content-Length` across packet
//! boundaries, holding what a pipelining client sent early, and refusing one
//! that will not fit. Before the split those lived inside `Router` and a second
//! framework would have re-derived them — which is a truncated request, or two
//! requests read as one, in somebody else's library.
//!
//! One `#[test]`, for the reason `http.rs` gives: a server needs a port, and
//! cargo runs a binary's tests concurrently.

mod harness;

use std::io::{Read, Write};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Its own, because `http.rs` and `reference.rs` bind theirs at the same time.
const PORT: u16 = 18733;

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

/// A middleware chain. No `Router`, no `Fiber`, no `Nursery`.
const CHAIN: &str = "module demo::main;

import std::core::{List, Option, Scope, print};
import std::net::socket::{accept_on, invalid_handle, listen_on, set_receive_timeout, start};
import std::net::http::{Connection, Incoming, Request, Response, matches, over_socket};

fn print(value: String);

/// A stage sees the request and may answer, or pass.
pub type Stage = { run: (Request) -> Option<Response> };

fn through(stages: List<Stage>, request: Request) -> Response {
  match stages {
    List::Nil => Response::text(404, \"no stage answered\"),
    List::Cons(stage, rest) => match stage.run(request) {
      Option::Some(answer) => answer,
      Option::None => through(rest, request),
    },
  }
}

fn greet() -> Stage {
  { run: fn request => match matches(\"/hi/:who\", request.path) {
      Option::Some(params) => match params.get(\"who\") {
        Option::Some(who) => Option::Some(Response::text(200, \"hello ${who}\")),
        Option::None => Option::None,
      },
      Option::None => Option::None,
    } }
}

fn measure() -> Stage {
  { run: fn request => if request.path == \"/size\" {
      Option::Some(Response::text(200, Int::to_string(String::byte_length(request.body))))
    } else {
      Option::None
    } }
}

/// The whole server. Framing, keep-alive and refusals come from `Connection`.
fn serve(stages: List<Stage>, socket: Int) -> () {
  let connection = Connection::over(over_socket(socket));
  let mut going = true;
  while going {
    match Connection::next(connection) {
      Incoming::Ended => going = false,
      Incoming::Rejected(response) => {
        Connection::reply(connection, response, false);
        going = false
      },
      Incoming::Arrived(request, keep) => {
        Connection::reply(connection, through(stages, request), keep);
        going = keep
      },
    }
  };
  Connection::shut(connection)
}

pub fn main() -> () {
  with { scope: Scope::root() } {
    start();
    let stages = [greet(), measure()];
    let server = listen_on(@PORT@);
    print(\"listening\");
    let mut rounds = 0;
    while rounds < 6 {
      let taken = accept_on(server);
      if taken == invalid_handle() { } else {
        set_receive_timeout(taken, 10000);
        serve(stages, taken)
      };
      rounds = rounds + 1
    }
  }
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
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("http_layers");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "chain.exe" } else { "chain" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let main = CHAIN.replace("@PORT@", &PORT.to_string());
    let root = SourceRoot::new(&db, sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("the middleware server did not build:\n  {}", messages.join("\n  "));
    }

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
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
    assert_eq!(&opened, b"listening");
    Server { child }
}

const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

fn connect() -> std::net::TcpStream {
    for _ in 0..100 {
        if let Ok(socket) = std::net::TcpStream::connect(("127.0.0.1", PORT)) {
            socket.set_read_timeout(Some(DEADLINE)).expect("a read deadline");
            socket.set_write_timeout(Some(DEADLINE)).expect("a write deadline");
            return socket;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("could not reach the server");
}

/// Everything the server sends before it closes.
fn drain(socket: &mut std::net::TcpStream) -> String {
    let mut said = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => said.extend_from_slice(&chunk[..read]),
        }
    }
    String::from_utf8_lossy(&said).into_owned()
}

/// Reads exactly one response, leaving anything after it on the socket.
fn one_response(socket: &mut std::net::TcpStream) -> String {
    let mut said = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match socket.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                said.push(byte[0]);
                let text = String::from_utf8_lossy(&said);
                if let Some(head) = text.find("\r\n\r\n") {
                    let length: usize = text
                        .to_ascii_lowercase()
                        .split("content-length:")
                        .nth(1)
                        .and_then(|rest| rest.split("\r\n").next())
                        .and_then(|n| n.trim().parse().ok())
                        .unwrap_or(0);
                    if said.len() >= head + 4 + length {
                        break;
                    }
                }
            }
        }
    }
    String::from_utf8_lossy(&said).into_owned()
}

fn body_of(answer: &str) -> &str {
    answer.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or("")
}

/// One request, one connection, closed on the way out.
///
/// **Closing matters.** `shut` on the server side does a `shutdown` and then
/// drains whatever the client still had to say, and a client socket left open
/// makes that drain wait out its ten-second deadline before the next
/// connection is accepted. `curl` closes promptly, which is why the
/// conformance script never notices.
fn exchange(request: &[u8]) -> String {
    let mut socket = connect();
    socket.write_all(request).expect("writing a request");
    let answered = drain(&mut socket);
    let _ = socket.shutdown(std::net::Shutdown::Both);
    answered
}

#[test]
fn a_framework_that_is_not_the_router_gets_http_right() {
    let _server = start();

    // **Two requests in one packet.** Keep-alive is `Connection`'s: this
    // framework never looks at the `Connection` header, it obeys the flag
    // `Incoming::Arrived` carries.
    //
    // This is the case that found a bug. `read_request` returns immediately
    // when the carry is already a whole request, and the loop above it read
    // "nothing new arrived" as "the client has gone" — so the second request
    // was dropped. `Router` had the same loop and the same bug, and no test
    // with `curl` in it could see it, because `curl` waits for each answer
    // before sending the next and the carry is therefore always empty.
    let (first, second) = {
        let mut socket = connect();
        socket
            .write_all(
                b"GET /hi/world HTTP/1.1\r\nHost: t\r\n\r\n\
                  GET /hi/again HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
            )
            .expect("writing two requests at once");
        let first = one_response(&mut socket);
        let second = drain(&mut socket);
        let _ = socket.shutdown(std::net::Shutdown::Both);
        (first, second)
    };
    assert_eq!(body_of(&first), "hello world", "the first of two in one packet");
    assert!(
        second.contains("hello again"),
        "the second arrived before the first was answered, and must not be dropped: {second:?}"
    );

    // **A body that does not arrive in one packet.** `Content-Length` framing
    // is `Connection`'s too; a single `receive` truncates this.
    let body = "x".repeat(3000);
    let answered = exchange(
        format!(
            "POST /size HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
    assert_eq!(body_of(&answered), "3000", "the whole body must arrive: {answered:?}");

    // **More than the buffer holds is a 413**, not a crash and not a truncated
    // request answered as though it were whole.
    let huge = "x".repeat(20000);
    let refused = exchange(
        format!(
            "POST /size HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{huge}",
            huge.len()
        )
        .as_bytes(),
    );
    assert!(refused.starts_with("HTTP/1.1 413"), "expected a 413, got: {refused:?}");

    // And an unmounted path reaches the chain's own fallback rather than
    // anything `std` decided.
    let missed = exchange(b"GET /nowhere HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n");
    assert!(missed.starts_with("HTTP/1.1 404"), "expected a 404, got: {missed:?}");
    assert_eq!(body_of(&missed), "no stage answered", "the framework's own message");
}
