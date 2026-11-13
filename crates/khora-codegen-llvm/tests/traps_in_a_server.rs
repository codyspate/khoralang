#![cfg(feature = "llvm")]

//! What a trap inside a request handler does to the server.
//!
//! **What this guards: one bad request ending every request the server is
//! serving.** `Router` runs each request inside `std::core::contain`, so a
//! handler that overflows answers 500, the trap is printed to stderr with
//! `(contained; the fiber continues)`, and the requests beside it -- and the
//! ones after it -- are answered. `Router::uncontained` is the opt-out, and
//! with it the first trap ends the process with 134, which is the other half
//! of the claim and the reason a supervisor still matters.
//!
//! Tested in both directions, because each half alone passes against a
//! server that does the other: a 500 for `/trap` says nothing unless a
//! router that opted out dies, and a death says nothing unless the default
//! survives. `/raise` is the control for both: a raise is a 500 either way.

use crate::harness;

use std::io::{Read, Write};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Its own port: `tests/http.rs` explains why two servers in one binary is a
/// bind failure and a connection reset rather than two results.
///
/// **It was 18733, which is `http_layers.rs`'s.** Both files said "its own
/// port" in a comment and both picked the same number, which is what happens
/// when the way to choose one is to read every other file and remember. They
/// did not collide often: `http_layers` is in `.config/nextest.toml`'s
/// `networked` group and this is not, so whether they overlap is a scheduling
/// accident rather than a rule. The symptom when they do is this file failing
/// to bind and reporting a connection reset -- a server bug that is not one.
///
/// The ports in use, so the next one is chosen by looking in one place:
/// 18732 `http`, 18733 `http_layers`, 18734 and 18740 to 18742 here, 18736 and 18737
/// `http_accept`, 18738 and 18739 `long_inputs`, 18847 `tls_cancel`, 18961
/// and 18962 `net_cancel`, 18971 and 18972 `tls`, 47821 `reference`.
const PORT: u16 = 18734;

/// The server that opted out, on a port of its own so the two tests can run
/// in either order.
const UNCONTAINED_PORT: u16 = 18740;

/// The load test's, one per fiber backend.
const LOAD_PORTS: [(&str, u16); 2] = [("threads", 18741), ("scheduler", 18742)];

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

/// Four routes: one that answers, one that raises, one that traps, and one
/// that answers slowly, so a trap can arrive while others are in flight.
///
/// `/raise` is the control. It fails in the way the language has a mechanism
/// for, so the router turns it into a 500 and the server carries on — which is
/// what makes `/trap` a claim about traps rather than about handlers that go
/// wrong in general. `@ROUTER@` is `Router::new()` or, for the opt-out,
/// `Router::new() |> Router::uncontained`.
const SERVER: &str = "module demo::main;
import std::core::{ChildFailed, Option, SharedFn};
import std::clock::{Clock};
import std::net::http::{HttpError, Request, Response, Router};

fn print(value: String);

pub type Broke = | Deliberately;

fn fine(_req: Request) -> Response {
  Response::text(200, \"fine\")
}

/// Answers after 300 ms, holding a fiber in a handler meanwhile.
fn slow(_req: Request) -> Response {
  with { clock: Clock::real() } { clock.sleep(300) };
  Response::text(200, \"slow\")
}

/// Fails the way the language provides for, so the wrapper answers 500.
fn raising(_req: Request) -> Response raises Broke {
  raise Broke::Deliberately
}

/// Overflows. Nothing catches this: it does not unwind.
///
/// The addend comes from the request so that the arithmetic cannot be folded
/// away at compile time -- a constant overflow is a compile error, and a
/// handler the optimizer proved unreachable would test nothing.
fn trapping(req: Request) -> Response {
  let step = String::byte_length(req.path);
  let mut n = 9223372036854775807;
  n = n + step;
  Response::text(200, Int::to_string(n))
}

pub fn main() raises HttpError + ChildFailed {
  @ROUTER@
    |> Router::get(\"/fine\", SharedFn::of(fine))
    |> Router::get(\"/slow\", SharedFn::of(slow))
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

fn start(router: &str, port: u16) -> Server {
    start_on(router, port, "threads")
}

/// [`start`], on the fiber backend `fibers`.
fn start_on(router: &str, port: u16, fibers: &str) -> Server {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("http_trap_server_{port}"));
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "server.exe" } else { "server" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let main = SERVER.replace("@PORT@", &port.to_string()).replace("@ROUTER@", router);
    let root = SourceRoot::new(&db, sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("the test server did not build:\n  {}", messages.join("\n  "));
    }

    let mut child = std::process::Command::new(&exe)
        .env("KHORA_FIBERS", fibers)
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

fn connect(port: u16) -> Option<std::net::TcpStream> {
    for _ in 0..100 {
        if let Ok(socket) = std::net::TcpStream::connect(("127.0.0.1", port)) {
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
fn ask(port: u16, path: &str) -> String {
    let Some(mut socket) = connect(port) else { return String::new() };
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
/// One test rather than several: each step depends on the server still being
/// in the state the step before left it in.
#[test]
fn a_trap_in_a_handler_answers_500_and_the_server_goes_on() {
    let mut server = start("Router::new()", PORT);

    // --- an ordinary request is answered
    let ok = ask(PORT, "/fine");
    assert!(ok.starts_with("HTTP/1.1 200"), "expected an answer, got {ok:?}");
    assert!(ok.ends_with("fine"), "expected the body, got {ok:?}");

    // --- a raise is a 500, which is the control
    let raised = ask(PORT, "/raise");
    assert!(raised.starts_with("HTTP/1.1 500"), "a raise is a 500, got {raised:?}");

    // --- a trap is a 500 too, with requests in flight beside it
    let slow: Vec<_> = (0..8).map(|_| std::thread::spawn(|| ask(PORT, "/slow"))).collect();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let trapped = ask(PORT, "/trap");
    assert!(
        trapped.starts_with("HTTP/1.1 500"),
        "a trap in a handler is that request's 500: {trapped:?}"
    );
    for (i, waiting) in slow.into_iter().enumerate() {
        let answer = waiting.join().expect("a client thread");
        assert!(
            answer.starts_with("HTTP/1.1 200") && answer.ends_with("slow"),
            "request {i}, in flight when the trap came, is answered: {answer:?}"
        );
    }

    // --- and so is everything after it
    for _ in 0..3 {
        let after = ask(PORT, "/fine");
        assert!(after.starts_with("HTTP/1.1 200"), "the server goes on serving: {after:?}");
    }
    assert!(
        server.child.try_wait().expect("asking about the server").is_none(),
        "the process is still alive"
    );

    // The trap is not silent: it is on stderr, with what the server goes on
    // to do. Read once the server is stopped, so the pipe has an end.
    let _ = server.child.kill();
    let _ = server.child.wait();
    let mut said = String::new();
    if let Some(mut stderr) = server.child.stderr.take() {
        let _ = stderr.read_to_string(&mut said);
    }
    assert!(
        said.contains("Int addition overflowed") && said.contains("(contained; the fiber continues)"),
        "the contained trap is reported on stderr: {said:?}"
    );
}

/// `Router::uncontained` is the opt-out: the first trap ends the process with
/// a trap's status, as it does with no `contain` anywhere.
#[test]
fn an_uncontained_router_ends_the_server_on_a_trap() {
    let mut server = start("(Router::new() |> Router::uncontained)", UNCONTAINED_PORT);

    let ok = ask(UNCONTAINED_PORT, "/fine");
    assert!(ok.starts_with("HTTP/1.1 200"), "expected an answer, got {ok:?}");
    let raised = ask(UNCONTAINED_PORT, "/raise");
    assert!(raised.starts_with("HTTP/1.1 500"), "a raise is still a 500, got {raised:?}");

    let trapped = ask(UNCONTAINED_PORT, "/trap");
    assert!(!trapped.starts_with("HTTP/1.1"), "nothing answers the trap: {trapped:?}");

    let status = server.child.wait().expect("the server should have ended");
    assert_eq!(status.code(), Some(134), "a trap ends an uncontained server with 134");

    let mut said = String::new();
    if let Some(mut stderr) = server.child.stderr.take() {
        let _ = stderr.read_to_string(&mut said);
    }
    assert!(said.contains("overflowed"), "and says what happened: {said:?}");
    assert!(!said.contains("contained;"), "and did not contain it: {said:?}");
}

/// **What this guards: one request's trap ending the forty beside it.** Forty
/// `/slow` requests are parked in their sleep when a `/trap` arrives, and
/// every one of them must still answer 200 on both fiber backends: the
/// trap's unwind runs on one fiber while the others are suspended, and on
/// the scheduler they share workers with it. Then 200 more traps, mixed with
/// good requests from eight client threads, and the server still answers.
///
/// The shape of the design round's `load.py`, smaller for a test run.
#[test]
fn forty_requests_in_flight_survive_a_trapping_request() {
    for (fibers, port) in LOAD_PORTS {
        let mut server = start_on("Router::new()", port, fibers);

        let slow: Vec<_> = (0..40).map(|_| std::thread::spawn(move || ask(port, "/slow"))).collect();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let trapped = ask(port, "/trap");
        assert!(trapped.starts_with("HTTP/1.1 500"), "{fibers}: the trap is a 500: {trapped:?}");
        let answered = slow
            .into_iter()
            .map(|waiting| waiting.join().expect("a client thread"))
            .filter(|answer| answer.starts_with("HTTP/1.1 200") && answer.ends_with("slow"))
            .count();
        assert_eq!(answered, 40, "{fibers}: every request in flight is answered");

        let clients: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    let mut wrong = Vec::new();
                    for _ in 0..25 {
                        let bad = ask(port, "/trap");
                        if !bad.starts_with("HTTP/1.1 500") {
                            wrong.push(format!("trap: {bad:?}"));
                        }
                        let good = ask(port, "/fine");
                        if !(good.starts_with("HTTP/1.1 200") && good.ends_with("fine")) {
                            wrong.push(format!("fine: {good:?}"));
                        }
                    }
                    wrong
                })
            })
            .collect();
        let wrong: Vec<String> =
            clients.into_iter().flat_map(|c| c.join().expect("a client thread")).collect();
        assert!(wrong.is_empty(), "{fibers}: {} wrong answers, first {:?}", wrong.len(), wrong.first());
        assert!(
            server.child.try_wait().expect("asking about the server").is_none(),
            "{fibers}: alive after 201 contained traps"
        );
    }
}
