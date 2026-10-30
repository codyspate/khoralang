#![cfg(feature = "llvm")]

//! A canceled HTTP server closes every socket it opened: the connections it
//! had accepted and its listener.
//!
//! **What these guard: descriptors a server shutdown leaves behind for the
//! life of the process.** A server is canceled when it is shut down,
//! restarted on a config reload, or stopped by a supervisor, and each of
//! those left sockets open:
//!
//! - an accepted connection whose fiber was stopped before it registered the
//!   close, or never started: the accept loop took the connection and then
//!   handed a plain handle across a spawn, and every entry check and the
//!   nursery's admission wait on the way could drop it. 1,000 canceled
//!   servers left 423 to 667 connections open on the thread backend and 16
//!   to 28 on the scheduler, the server's end of each in `CLOSE-WAIT`;
//! - `Router::listen_tls`'s listener, which nothing registered a close for,
//!   so its port stayed bound after a cancel.
//!
//! The accepted-connection tests count with `khora_net_accepted_open`, the
//! runtime's exact count of accepted sockets not yet closed, which works on
//! every system, and with `/dev/fd` where the system lists it.
//!
//! **No fixed ports.** The accepted-connection tests bind port 0 in every
//! round and ask the runtime which port they got; the `listen_tls` test,
//! which needs a port number before its program runs, binds port 0 in the
//! harness, reads the port back and lets it go, and a test that loses that
//! race to another test fails to bind and says so rather than passing.

use std::time::Duration;

use crate::claims::{on_both, Ran};

/// Far over what any of these needs on a loaded runner, and short of the
/// suite's own timeout.
const PATIENCE: Duration = Duration::from_secs(240);

/// A port nothing is listening on, found by binding port 0 and letting it go.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port()
}

/// The program both accepted-connection tests run: `rounds` rounds of
/// binding a listener, starting a server on it, connecting, canceling the
/// server and closing the listener. `before` is how many clients connect
/// before the server starts and `after` how many connect once it has;
/// `spread` bounds the spin between the last connect and the cancel.
///
/// The server is `Router::serve_forever` in a `bounded_nursery` of 256,
/// which is what `Router::listen` runs once it has bound the port. **A
/// fresh listener each round, closed by the test**, so a connection the
/// server never accepted is reset with it: one listener for every round
/// collected those in its backlog until it was full, and a connect to a
/// full backlog on Linux waits for ever.
///
/// **And a fresh port each round.** Each listener binds port 0, and the
/// program asks the runtime which port it got (`khora_net_local_port`): a
/// Windows runner refused to rebind one port on the second round, before
/// anything was counted, as Linux does once `SO_REUSEADDR` is taken away,
/// because the connections a canceled server closed sit in `TIME_WAIT` on
/// that port.
fn rounds(rounds: i64, before: i64, after: i64, spread: i64, room: i64) -> String {
    let serve = format!(
        r#"fn serve(server: Int) -> () {{
  let _ = attempt(fn () => bounded_nursery({room}, fn () =>
    Router::new()
      |> Router::get("/x", SharedFn::of(fn _ => Response::text(200, "ok")))
      |> Router::serve_forever(server)!)!);
  ()
}}

/// The clients that connect once the server has started; `then` is the
/// cancel. Answers how many of them completed a TLS handshake: none here.
fn late(port: Int, n: Int, then: () -> ()) -> Int {{
  let plain = clients(port, {after}, n);
  then();
  shut_all(plain);
  0
}}"#
    );
    serving(rounds, before, spread, &serve)
}

/// [`rounds`], with `serve(server: Int) -> ()` and `late(port, n, then) ->
/// Int` given: `Router::serve_forever` and plain clients, or
/// `Router::serve_secured` and a TLS client as well.
fn serving(rounds: i64, before: i64, spread: i64, serve: &str) -> String {
    format!(
        r#"module main;

import std::core::{{ChildFailed, Fiber, List, Result, Scope, SharedFn, attempt, bounded_nursery, print, scoped}};
import std::fs::{{FsRead}};
import std::net::http::{{Response, Router}};
import std::net::socket::{{connect_to, invalid_handle, listen_on, shut, start, transmit}};
import std::net::tls::{{server as tls_server, shut as tls_shut, transmit as tls_transmit, trusting, upgrade as tls_upgrade}};

extern fn khora_net_accepted_open() -> Int;
extern fn khora_net_failed_closes() -> Int;
extern fn khora_net_local_port(socket: Int) -> Int;
extern fn khora_monotonic_millis() -> Int;

{serve}

fn spin(k: Int) -> Int {{
  let mut i = 0;
  let mut s = 0;
  while i < k {{ s = s + i; i = i + 1 }};
  s
}}

/// Descriptors this process holds, on a system that lists them; -1 where
/// it does not.
fn fds() -> Int {{
  with {{ reads: FsRead::real() }} {{
    match attempt(fn () => reads.read_dir("/dev/fd")!) {{
      Result::Ok(names) => List::length(names),
      Result::Err(_) => -1,
    }}
  }}
}}

/// Connects `count` clients, sending a request on every other one, and
/// answers the list of their sockets.
fn clients(port: Int, count: Int, n: Int) -> List<Int> {{
  let mut out = List::Nil;
  let mut k = 0;
  while k < count {{
    let c = connect_to("127.0.0.1", port);
    if c >= 0 && (n + k) % 2 == 0 {{ let _ = transmit(c, "GET /x HTTP/1.1\r\nHost: a\r\n\r\n"); () }} else {{}};
    out = List::Cons(c, out);
    k = k + 1
  }};
  out
}}

fn shut_all(sockets: List<Int>) -> () {{
  match sockets {{
    List::Nil => (),
    List::Cons(c, rest) => {{
      if c >= 0 {{ shut(c) }} else {{}};
      shut_all(rest)
    }},
  }}
}}

fn round(n: Int, before: Int, spread: Int) -> Int raises ChildFailed {{
  at(n, "listen");
  let server = listen_on(0);
  if server == invalid_handle() {{ return -1 }} else {{}};
  let port = khora_net_local_port(server);
  if port <= 0 {{ shut(server); return -1 }} else {{}};
  at(n, "connect early clients");
  let early = clients(port, before, n);
  at(n, "start the server");
  let f = Fiber::spawn(fn () => serve(server));
  let secured = late(port, n, fn () => {{
    let _ = spin((n * 7919) % spread);
    at(n, "cancel");
    Fiber::cancel(f);
    at(n, "wait for the server");
    Fiber::wait(f)! catch {{ _ => () }};
    at(n, "server stopped");
  }});
  at(n, "shut the listener and early clients");
  shut(server);
  shut_all(early);
  secured
}}

/// One progress line, flushed by its newline, so a run killed for hanging
/// says which round and which step it was in, and the milliseconds since
/// the program started say whether it was stuck or only slow.
fn at(n: Int, what: String) -> () {{
  print("at ${{Int::to_string(n)}} ${{what}} (${{Int::to_string(khora_monotonic_millis())}} ms)")
}}

pub fn main() -> Int raises ChildFailed {{
  // Winsock: every socket call on Windows fails until this has run.
  if start() {{}} else {{ print("could not start sockets"); return 1 }};
  let mut k = 0;
  while k < 10 {{
    print("warm-up ${{Int::to_string(k)}}");
    if round(0, {before}, {spread})! < 0 {{ print("could not bind a listener"); return 1 }} else {{}};
    k = k + 1
  }};
  print("counted rounds");
  let base = fds();
  let open = khora_net_accepted_open();
  let refused = khora_net_failed_closes();
  let mut n = 0;
  let mut secured = 0;
  while n < {rounds} {{
    let got = round(n, {before}, {spread})!;
    if got < 0 {{ print("could not bind a listener"); return 1 }} else {{}};
    secured = secured + got;
    n = n + 1
  }};
  let fd_delta = if base < 0 {{ 0 }} else {{ fds() - base }};
  print("rounds ${{Int::to_string(n)}} accepted ${{Int::to_string(khora_net_accepted_open() - open)}} fds ${{Int::to_string(fd_delta)}} secured ${{Int::to_string(secured)}} reclosed ${{Int::to_string(khora_net_failed_closes() - refused)}}");
  0
}}
"#
    )
}

/// The last lines a run printed: its progress lines say which round and step
/// it reached, and the tally, and a run of a thousand rounds prints thousands.
fn tail(stdout: &str) -> String {
    let lines: Vec<&str> = stdout.lines().collect();
    lines[lines.len().saturating_sub(12)..].join("\n")
}

/// The `rounds R accepted A fds F` line, as three numbers, having checked its
/// `reclosed` word is 0.
///
/// **`reclosed` is the runtime's count of socket closes refused because the
/// socket was not open** (`khora_net_failed_closes`): a socket closed twice.
/// Neither a leak count nor `/dev/fd` can see one, and if the number had been
/// given to a new connection in between, the second close shut that one. In
/// the TLS server this is the order of the hand-over to the handshake: the
/// server's close disowned after the handshake has taken the socket, rather
/// than before, shut sockets a canceled handshake had already closed.
///
/// **A hang says where.** The program prints a line before each step of each
/// round (`at N step`), each flushed by its newline, so a run killed by the
/// watchdog shows the last step it started.
fn tally(backend: &str, ran: &Ran) -> (i64, i64, i64) {
    let last = tail(&ran.stdout);
    assert!(!ran.hung, "{backend}: the program hung; its last lines:\n{last}\n{}", ran.stderr);
    assert_eq!(ran.code, Some(0), "{backend}: its last lines:\n{last}\n{}", ran.stderr);
    let line = ran
        .stdout
        .lines()
        .find(|l| l.starts_with("rounds "))
        .unwrap_or_else(|| panic!("{backend}: no tally; its last lines:\n{last}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    let at = |key: &str| -> i64 {
        let i = words.iter().position(|w| *w == key).unwrap_or_else(|| panic!("no `{key}` in `{line}`"));
        words[i + 1].parse().unwrap_or_else(|_| panic!("`{key}` is not a number in `{line}`"))
    };
    assert_eq!(at("reclosed"), 0, "{backend}: sockets were closed twice: {line}");
    (at("rounds"), at("accepted"), at("fds"))
}

/// **1,000 servers canceled right after a connect leave no accepted
/// connection open.** The review's shape: start the server, connect a
/// client (half of them send a request), and cancel the server after a spin
/// that varies by round, so the cancel lands everywhere from before the
/// accept to inside the answer.
#[test]
fn a_canceled_server_leaks_no_accepted_connection() {
    let program = rounds(1000, 0, 1, 200_000, 256);
    for (backend, ran) in on_both("server_cancel_accepted", &program, PATIENCE) {
        let (n, accepted, fds) = tally(backend, &ran);
        assert_eq!(n, 1000, "{backend}: {}", tail(&ran.stdout));
        assert_eq!(accepted, 0, "{backend}: canceled servers left accepted connections open: {}", tail(&ran.stdout));
        assert_eq!(fds, 0, "{backend}: canceled servers left descriptors open: {}", tail(&ran.stdout));
    }
}

/// **A server canceled with connections queued for it, before their fibers
/// can start, leaks none of them.** Eight clients connect before the server
/// does anything, so its first moves are accepts, and the cancel comes
/// within a short spin. The nursery holds two children, so the third
/// accepted connection waits for room: the cancel finds children admitted
/// and not yet run, and a spawn still waiting for admission, whose body is
/// let go of without ever starting.
///
/// The `serve` program's server is given that bound by `rounds`: `room`
/// is 2 here and 256, `Router::listen`'s, in the test above.
#[test]
fn a_nursery_canceled_before_its_connections_start_leaks_none() {
    let program = rounds(200, 8, 0, 20_000, 2);
    for (backend, ran) in on_both("server_cancel_unstarted", &program, PATIENCE) {
        let (n, accepted, fds) = tally(backend, &ran);
        assert_eq!(n, 200, "{backend}: {}", tail(&ran.stdout));
        assert_eq!(accepted, 0, "{backend}: unstarted connections were left open: {}", tail(&ran.stdout));
        assert_eq!(fds, 0, "{backend}: unstarted connections left descriptors open: {}", tail(&ran.stdout));
    }
}

/// A file in `tests/fixtures`, with its newlines escaped for a Khora string.
fn fixture(name: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    std::fs::read_to_string(&path)
        .expect("the test certificate is in tests/fixtures")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// **A canceled TLS server leaks no accepted connection either.**
/// `Router::serve_secured` had the same window as the plain server, with a
/// handshake added on the way. Each round has two clients once the server
/// is up: a plain one that never speaks TLS, so a fiber sits in the
/// handshake until the cancel cuts it short, and a TLS client that finishes
/// its handshake and sends a request, so the server holds a session when
/// the cancel lands. Every connection is closed through the session or by
/// the server's own close of the socket, never both, and the count of
/// rounds that held a session says the second shape was reached.
#[test]
fn a_canceled_tls_server_leaks_no_accepted_connection() {
    let serve = format!(
        r#"fn serve(server: Int) -> () {{
  let _ = attempt(fn () => scoped(fn () => {{
    let settings = tls_server("{cert}", "{key}")!;
    bounded_nursery(256, fn () =>
      Router::new()
        |> Router::get("/x", SharedFn::of(fn _ => Response::text(200, "ok")))
        |> Router::serve_secured(server, settings)!)!
  }})!);
  ()
}}

/// A plain client that never speaks TLS, held mid-handshake, and a TLS client
/// that finishes its handshake and sends a request, so the server holds a
/// session when the cancel lands. Answers 1 if the handshake completed.
///
/// **The TLS client dials `127.0.0.1` and upgrades, rather than connecting to
/// `localhost` by name.** The certificate is for `localhost`, so that is the
/// name the handshake checks. Resolving it for the dial can give `::1` first,
/// where nothing listens, and a refused connect there costs a round its
/// retry on any system that is slow to refuse; with a 2 s refusal put in on
/// Linux, three hundred rounds ran past the test's patience and it was
/// killed as hung, exactly the Windows CI failure. The dial is not what this
/// test is about, so it does not depend on one.
fn late(port: Int, n: Int, then: () -> ()) -> Int {{
  at(n, "connect a plain client");
  let plain = clients(port, 1, 1);
  at(n, "tls connect and handshake");
  let raw = connect_to("127.0.0.1", port);
  let secured = if raw < 0 {{ at(n, "tls connect failed"); then(); 0 }} else {{
    match attempt(fn () => scoped(fn () => {{
      let session = tls_upgrade(trusting("{ca}")!, raw, "localhost")!;
      at(n, "tls send");
      let _ = tls_transmit(session, "GET /x HTTP/1.1\r\nHost: a\r\n\r\n");
      then();
      at(n, "tls shut");
      tls_shut(session);
      1
    }})!) {{
      Result::Ok(one) => one,
      // A handshake that raised leaves the socket the caller's.
      Result::Err(_) => {{ at(n, "tls handshake failed"); shut(raw); then(); 0 }},
    }}
  }};
  at(n, "shut the plain client");
  shut_all(plain);
  secured
}}"#,
        cert = fixture("localhost.cert.pem"),
        key = fixture("localhost.key.pem"),
        ca = fixture("ca.cert.pem"),
    );
    let program = serving(300, 1, 200_000, &serve);
    for (backend, ran) in on_both("server_cancel_tls_accepted", &program, PATIENCE) {
        let (n, accepted, fds) = tally(backend, &ran);
        assert_eq!(n, 300, "{backend}: {}", tail(&ran.stdout));
        let secured = secured_count(&ran.stdout);
        assert!(secured >= 150, "{backend}: too few rounds held a session at the cancel to prove anything: {}", tail(&ran.stdout));
        assert_eq!(accepted, 0, "{backend}: a canceled TLS server left accepted connections open: {}", tail(&ran.stdout));
        assert_eq!(fds, 0, "{backend}: a canceled TLS server left descriptors open: {}", tail(&ran.stdout));
    }
}

/// The `secured N` word of the tally line.
fn secured_count(stdout: &str) -> i64 {
    let line = stdout.lines().find(|l| l.starts_with("rounds ")).unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().collect();
    words
        .iter()
        .position(|w| *w == "secured")
        .and_then(|i| words.get(i + 1))
        .and_then(|w| w.parse().ok())
        .unwrap_or_else(|| panic!("no `secured` in `{line}`"))
}

/// **A canceled `Router::listen_tls` lets go of its port.** The server is
/// started on a fiber, the test waits until a connect gets through, which
/// is the listener being up, and cancels it; binding the port again must
/// then succeed. Its listener had no close registered anywhere, so the
/// port stayed bound until the process ended.
#[test]
fn a_canceled_listen_tls_releases_its_port() {
    let port = free_port();
    let program = format!(
        r#"module main;

import std::core::{{ChildFailed, Fiber, SharedFn, attempt, print, scoped}};
import std::net::http::{{Response, Router}};
import std::net::socket::{{connect_to, invalid_handle, listen_on, shut}};

fn spin(k: Int) -> Int {{
  let mut i = 0;
  let mut s = 0;
  while i < k {{ s = s + i; i = i + 1 }};
  s
}}

fn serve(port: Int) -> () {{
  let _ = attempt(fn () => scoped(fn () =>
    Router::new()
      |> Router::get("/x", SharedFn::of(fn _ => Response::text(200, "ok")))
      |> Router::listen_tls(port, "{cert}", "{key}")!)!);
  ()
}}

pub fn main() -> Int raises ChildFailed {{
  let port = {port};
  let f = Fiber::spawn(fn () => serve(port));
  let mut c = connect_to("127.0.0.1", port);
  let mut tries = 0;
  while c < 0 && tries < 2000 {{
    let _ = spin(100000);
    c = connect_to("127.0.0.1", port);
    tries = tries + 1
  }};
  if c < 0 {{ print("the server never listened") }} else {{ shut(c) }};
  Fiber::cancel(f);
  Fiber::wait(f)! catch {{ _ => () }};
  let again = listen_on(port);
  if again == invalid_handle() {{ print("rebinding refused") }} else {{ print("rebinding ok"); shut(again) }};
  0
}}
"#,
        cert = fixture("localhost.cert.pem"),
        key = fixture("localhost.key.pem"),
    );
    for (backend, ran) in on_both("server_cancel_tls_port", &program, Duration::from_secs(120)) {
        assert!(!ran.hung, "{backend}: the program hung: {}{}", ran.stdout, ran.stderr);
        assert_eq!(ran.code, Some(0), "{backend}: {}{}", ran.stdout, ran.stderr);
        assert!(
            ran.stdout.contains("rebinding ok"),
            "{backend}: the canceled server still holds its port: {}{}",
            ran.stdout,
            ran.stderr
        );
    }
}
