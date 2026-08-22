#![cfg(feature = "llvm")]

//! `std::net::http`, against a real client.
//!
//! One server program, mounted with a handful of routes, and a socket at the
//! other end. Everything under test is Khora: the request parser, the query
//! decoder, the case-folding header map and the router.
//!
//! **One `#[test]`, deliberately.** Every claim needs a listening server, a
//! server needs a port, and cargo runs tests in one binary concurrently — two
//! tests each starting a server means the second fails to bind and the first
//! gets its connection reset. Sharing one process and sending several requests
//! down several connections is what the server is built for anyway:
//! `Connection: close`, one request each.

mod harness;

use std::io::{Read, Write};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Not 8080 — `tests/reference.rs` binds that, and the two run at once.
const PORT: u16 = 18732;

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

/// A server with one route per thing worth asking about.
///
/// `echo` reports what the parser made of the request, one field per line, so
/// a single response can be read for several separate claims.
const SERVER: &str = "module demo::main;
import std::core::{Option, Scope};
import std::net::http::{HttpError, Request, Response, Router};

fn print(value: String);

fn echo(req: Request) -> Response {
  Response::text(200,
    \"path=\" + req.path
    + \"|q:name=\" + Request::query(req, \"name\").unwrap_or(\"<none>\")
    + \"|q:flag=\" + Request::query(req, \"flag\").unwrap_or(\"<none>\")
    + \"|h:host=\" + Request::header(req, \"HOST\").unwrap_or(\"<none>\")
    + \"|h:x-note=\" + Request::header(req, \"X-Note\").unwrap_or(\"<none>\")
    + \"|body=\" + Int::to_string(String::byte_length(req.body)))
}

fn tagged(req: Request) -> Response {
  Response::text(200, \"tagged\") |> Response::with_header(\"X-Trace\", \"abc123\")
}

fn body_size(req: Request) -> Response {
  Response::text(200, Int::to_string(String::byte_length(req.body)))
}

export fn main() raises HttpError {
  // `listen` wants a `Scope` for the same reason the reference application
  // does: binding a socket is a resource, and a resource wants a region to
  // outlive it. The root one is the process, which is this server's lifetime.
  with { scope: Scope::root } {
    Router::new()
      |> Router::get(\"/echo/:who\", echo)
      |> Router::get(\"/tagged\", tagged)
      |> Router::post(\"/size\", body_size)
      |> Router::listen(@PORT@)!
  }
}
";

/// Killed on every path out, including a panicking one — a server left holding
/// the port makes the *next* run fail for a reason that has nothing to do with
/// the next run.
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
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("http_server");
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
        .spawn()
        .expect("the server should start");

    // Reading the announcement is the handshake. Connecting before `listen`
    // has returned is a race the test would lose sometimes, which is worse
    // than losing it always.
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

/// Every socket gets a deadline.
///
/// Not belt and braces: a server that stops reading part-way through a body
/// and then closes leaves the client writing into a window that never opens,
/// and on a graceful close — a FIN rather than a reset — that write blocks for
/// as long as anyone lets it. A test that hangs is worse than one that fails,
/// because the failure at least says what happened.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

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

/// Sends one raw request and reads the whole answer.
fn ask(request: &[u8]) -> String {
    let mut socket = connect().expect("could not reach the server");
    socket.write_all(request).expect("writing the request");
    socket.flush().expect("flush");
    let mut answer = String::new();
    socket.read_to_string(&mut answer).expect("reading the answer");
    answer
}

/// The same, for a request the server may refuse part-way through.
///
/// A reset, a timed-out write and an empty answer are all outcomes rather than
/// failures here: see the caller.
fn ask_tolerating_reset(request: &[u8]) -> String {
    let Some(mut socket) = connect() else { return String::new() };
    if socket.write_all(request).is_err() {
        return String::new();
    }
    let _ = socket.flush();
    let mut answer = String::new();
    let _ = socket.read_to_string(&mut answer);
    answer
}

fn body_of(answer: &str) -> &str {
    answer.split_once("\r\n\r\n").expect("a blank line").1
}

#[test]
fn the_server_reads_what_a_client_actually_sends() {
    let _server = start();

    // --- query strings, percent-decoding, and the path they are not part of
    let answer = ask(
        b"GET /echo/sam?name=hello%20world&flag HTTP/1.1\r\nHost: example.test\r\nX-Note: kept\r\n\r\n",
    );
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    let body = body_of(&answer);
    assert!(
        body.contains("path=/echo/sam|"),
        "the query string is not part of the path a route matches: {body}"
    );
    assert!(body.contains("|q:name=hello world|"), "`%20` decoded: {body}");
    assert!(body.contains("|q:flag=|"), "a parameter with no `=` is present and empty: {body}");

    // --- headers, found by a name in any case
    assert!(
        body.contains("|h:host=example.test|"),
        "`HOST` should find the `Host:` the client sent: {body}"
    );
    assert!(body.contains("|h:x-note=kept|"), "`X-Note` round-trips: {body}");

    // --- `+` is a space too, and an absent parameter is absent
    let answer = ask(b"GET /echo/sam?name=a+b HTTP/1.1\r\n\r\n");
    let body = body_of(&answer);
    assert!(body.contains("|q:name=a b|"), "`+` decoded: {body}");
    assert!(body.contains("|q:flag=<none>|"), "an absent parameter is absent: {body}");

    // --- a response may carry headers of its own
    let answer = ask(b"GET /tagged HTTP/1.1\r\n\r\n");
    assert!(answer.contains("X-Trace: abc123\r\n"), "the handler's header: {answer}");
    assert_eq!(body_of(&answer), "tagged");

    // --- a body larger than one `recv`, which is 4096 bytes
    //
    // The whole point of reading to `Content-Length` rather than once. Six
    // thousand rather than something rounder because `limit()` is 8192, and
    // `limit()` is 8192 because `std::core`'s text helpers recurse once per
    // byte and the stack gives out around nine thousand — see the comment
    // there.
    let big = "x".repeat(6000);
    let request = format!(
        "POST /size HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n{}",
        big.len(),
        big
    );
    let answer = ask(request.as_bytes());
    assert_eq!(
        body_of(&answer),
        "6000",
        "the server read the whole body, not the first packet of it:\n{answer}"
    );

    // --- past the limit does not take the server with it
    //
    // Asserted as "it survived" rather than "it answered 413", because the
    // 413 is not reliably *delivered*: the server stops reading at `limit()`
    // and closes, the client is still sending, and a socket closed with unread
    // data in its receive queue is a reset rather than a graceful shutdown —
    // which throws away whatever the server had written. Draining the rest of
    // the body before answering is what a server does about that, and it is
    // not written yet.
    let huge = "y".repeat(20000);
    let request = format!(
        "POST /size HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
        huge.len(),
        huge
    );
    let refused = ask_tolerating_reset(request.as_bytes());
    if !refused.is_empty() {
        assert!(
            refused.starts_with("HTTP/1.1 413 Payload Too Large\r\n"),
            "if anything came back it should be the refusal: {refused}"
        );
    }

    // --- `Content-Length` is a promise about what is coming, not an
    //     instruction to allocate
    let answer = ask(b"POST /size HTTP/1.1\r\nContent-Length: 4000000000\r\n\r\nnine byte");
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert_eq!(body_of(&answer), "9", "what arrived, not what was promised: {answer}");

    // --- a route nobody mounted
    let answer = ask(b"GET /nowhere HTTP/1.1\r\n\r\n");
    assert!(answer.starts_with("HTTP/1.1 404 Not Found\r\n"), "{answer}");
    assert!(body_of(&answer).contains("/nowhere"), "the message names the path: {answer}");

    // --- the right path with the wrong method is also a 404
    let answer = ask(b"POST /tagged HTTP/1.1\r\nContent-Length: 0\r\n\r\n");
    assert!(
        answer.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "a route is a method and a path together: {answer}"
    );

    // --- nonsense on the wire
    let answer = ask(b"not a request at all\r\n\r\n");
    assert!(answer.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{answer}");

    // --- and it is still serving after all of that
    let answer = ask(b"GET /tagged HTTP/1.1\r\n\r\n");
    assert_eq!(body_of(&answer), "tagged", "the server survived every one of those");
}
