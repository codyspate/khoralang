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
//! down several connections is what the server is built for anyway.

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
import std::core::{Option, SharedFn};
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
  // No `with` block: `listen` opens the nursery it needs itself, and these
  // handlers want no capabilities. `SharedFn::of` is what each mount says
  // instead — the certificate that lets the whole router cross into the fiber
  // that answers a connection.
  Router::new()
    |> Router::get(\"/echo/:who\", SharedFn::of(echo))
    |> Router::get(\"/tagged\", SharedFn::of(tagged))
    |> Router::post(\"/size\", SharedFn::of(body_size))
    |> Router::listen(@PORT@)!
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

/// Reads exactly one answer: the headers, then the body `Content-Length`
/// promises.
///
/// **Not to end-of-file**, which is what this did while every response said
/// `Connection: close`. The server keeps connections open now, so reading
/// until it hangs up means waiting out the ten-second deadline — and a test
/// that reads to EOF is one that would not notice keep-alive being silently
/// dropped, which is the thing worth checking.
fn read_message(socket: &mut std::net::TcpStream) -> String {
    let mut got: Vec<u8> = Vec::new();
    let head = loop {
        if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&got).into_owned(),
            Ok(n) => got.extend_from_slice(&chunk[..n]),
        }
    };
    let length: usize = String::from_utf8_lossy(&got[..head])
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().to_string())
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while got.len() < head + length {
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&got).into_owned()
}

/// Sends one raw request on a connection of its own and reads the answer.
fn ask(request: &[u8]) -> String {
    let mut socket = connect().expect("could not reach the server");
    socket.write_all(request).expect("writing the request");
    socket.flush().expect("flush");
    read_message(&mut socket)
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
    read_message(&mut socket)
}

fn body_of(answer: &str) -> &str {
    answer.split_once("\r\n\r\n").expect("a blank line").1
}

#[test]
fn the_server_reads_what_a_client_actually_sends() {
    let _server = start();

    // --- several requests down one connection
    //
    // Reuse is the whole of the throughput story: 2,568 requests a second on a
    // fresh connection each time, 142,991 on one held open. Everything else
    // measured here — the parser, the router, the runtime — is inside that
    // second number, which is why this is the property to pin rather than any
    // of them.
    let mut kept = connect().expect("could not reach the server");
    for round in 1..=3 {
        kept.write_all(b"GET /tagged HTTP/1.1\r\nHost: x\r\n\r\n").expect("a request");
        kept.flush().expect("flush");
        let answer = read_message(&mut kept);
        assert!(
            answer.starts_with("HTTP/1.1 200 OK\r\n"),
            "request {round} on a reused connection: {answer}"
        );
        assert_eq!(body_of(&answer), "tagged", "request {round}: {answer}");
        assert!(
            answer.contains("Connection: keep-alive"),
            "the server should say it is staying: {answer}"
        );
    }

    // --- and a client that asks to close gets closed
    let mut closing = connect().expect("could not reach the server");
    closing
        .write_all(b"GET /tagged HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .expect("a request");
    closing.flush().expect("flush");
    let answer = read_message(&mut closing);
    assert!(answer.contains("Connection: close"), "the opt-out is honoured: {answer}");
    let mut nothing = String::new();
    closing.read_to_string(&mut nothing).expect("the server should hang up");
    assert!(nothing.is_empty(), "nothing should follow: {nothing:?}");


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
    //
    // Four gigabytes is more than the buffer, so it is refused on the spot —
    // and *on the spot* is the point. The reader waits for a body that was
    // promised, so a promise it can never satisfy has to be recognised rather
    // than waited out, or one lying client holds a connection for the whole
    // ten-second deadline.
    //
    // This asserted `200` with a body of `9` until the reader learned to wait:
    // the server read the nine bytes that came, stopped because the read was
    // short, and answered about them. That was the short-read heuristic
    // showing through rather than a decision — a request claiming four
    // gigabytes is too large, and 413 is what that is called.
    let answer = ask(b"POST /size HTTP/1.1\r\nContent-Length: 4000000000\r\n\r\nnine byte");
    assert!(
        answer.starts_with("HTTP/1.1 413 Payload Too Large\r\n"),
        "a promise past the buffer is refused, not waited for: {answer}"
    );

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

    // --- a slow client does not hold up the next one
    //
    // No clock and no sleep: this connection is opened and nothing is sent on
    // it, so whichever fiber accepted it is parked inside its very first
    // `recv` until the last two lines of this test. A server that answered one
    // connection at a time would be inside that read too, and everything below
    // would wait behind it until the deadline.
    //
    // Before `SharedFn` this could not be written. A `Router` holds its
    // handlers, a closure's captures are not in its type, and so the router
    // could not be handed to the fiber that makes this work — the server
    // served one caller at a time and said so in a comment.
    let mut silent = connect().expect("could not reach the server");

    let answer = ask(b"GET /tagged HTTP/1.1\r\n\r\n");
    assert_eq!(
        body_of(&answer),
        "tagged",
        "a second client was answered while the first had not said anything: {answer}"
    );

    // The parked fiber was waiting, not lost.
    silent.write_all(b"GET /tagged HTTP/1.1\r\n\r\n").expect("the request, at last");
    silent.flush().expect("flush");
    let late = read_message(&mut silent);
    assert_eq!(body_of(&late), "tagged", "the connection that waited was answered too: {late}");

    // --- a body that arrives after its headers
    //
    // A client is allowed to write the two separately, and most do — .NET's
    // `HttpClient` does, and so does anything setting a body from a stream.
    // The reader used to stop at the first read shorter than its buffer, so
    // the pause between them read as the end of the request and the server
    // answered `400` about a body that had not arrived yet, *before* the
    // client had finished sending it. curl never showed it, because curl
    // writes a small request in one go.
    let mut split = connect().expect("could not reach the server");
    split
        .write_all(b"POST /size HTTP/1.1\r\nHost: x\r\nContent-Length: 9\r\n\r\n")
        .expect("the headers");
    split.flush().expect("flush");
    // Long enough that the server has certainly returned from its first read.
    std::thread::sleep(std::time::Duration::from_millis(300));
    split.write_all(b"nine byte").expect("the body, late");
    split.flush().expect("flush");
    let late = read_message(&mut split);
    assert!(late.starts_with("HTTP/1.1 200 OK\r\n"), "{late}");
    assert_eq!(body_of(&late), "9", "the whole body arrived: {late}");

    // --- and it is still serving after all of that
    let answer = ask(b"GET /tagged HTTP/1.1\r\n\r\n");
    assert_eq!(body_of(&answer), "tagged", "the server survived every one of those");

    // --- the deadline that makes reading-until-complete safe
    //
    // Reading until the request is complete and reading forever are the same
    // loop when a client stops talking, so a connection that promises a body
    // and never sends it has to be let go of rather than parking a fiber for
    // the life of the process.
    //
    // Last, and slow on purpose: the timeout `serve_connection` sets is ten
    // seconds, and the alternative to waiting for it is not testing the thing
    // that keeps a server up.
    let mut silent = connect().expect("could not reach the server");
    silent
        .write_all(b"POST /size HTTP/1.1\r\nHost: x\r\nContent-Length: 99\r\n\r\n")
        .expect("the headers and no body");
    silent.flush().expect("flush");
    let started = std::time::Instant::now();
    let _ = read_message(&mut silent);
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(8),
        "the deadline should have held the connection open: {waited:?}"
    );
    assert!(
        waited < std::time::Duration::from_secs(25),
        "the deadline should have let it go: {waited:?}"
    );

    let answer = ask(b"GET /tagged HTTP/1.1\r\n\r\n");
    assert_eq!(body_of(&answer), "tagged", "the server outlived the silent client");
}
