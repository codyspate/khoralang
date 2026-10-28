#![cfg(feature = "llvm")]

//! `HttpClient`'s deadline over a whole call.
//!
//! **What this guards: a call that a slow server holds for as long as it
//! likes.** The client's only bound used to be a limit on each read, renewed
//! by every byte: a server that accepts and says nothing held the caller for
//! that limit, thirty seconds, and one that trickles a byte at a time held it
//! for ever. A deadline over the call ends both at the budget the caller
//! chose, with `CallError::TimedOut`, and closes the connection.
//!
//! The servers are Rust threads on ports the operating system picks. Each
//! reports when the client's side of the connection closed, which is how the
//! test sees that the timed-out call let go of its socket while the program
//! was still running.

use crate::harness;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

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

fn build(name: &str, source: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// A server that takes one connection, reads the request, and then either
/// says nothing or says one byte every `drip`, until the client goes away.
///
/// Sends on the channel when it sees the client's side close, so the caller
/// can tell a released socket from one the process exit closed.
fn slow_server(drip: Option<Duration>) -> (u16, mpsc::Receiver<Instant>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let (told, closed) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else { return };
        let mut chunk = [0u8; 4096];
        // The request, which has no body.
        let mut got = Vec::new();
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            match socket.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => got.extend_from_slice(&chunk[..n]),
            }
        }
        match drip {
            None => {
                // Nothing, and a read that returns only when the client closes.
                let _ = socket.read(&mut chunk);
            }
            Some(every) => {
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n");
                socket.set_read_timeout(Some(every)).expect("a read timeout");
                loop {
                    if socket.write_all(b"x").is_err() {
                        break;
                    }
                    match socket.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                        Err(_) => break,
                    }
                }
            }
        }
        let _ = told.send(Instant::now());
    });
    (port, closed)
}

/// **A call with a 200 ms deadline against a server that never answers, and
/// one that trickles, comes back `TimedOut` well inside two seconds, leaks
/// nothing, and closes its connection.**
///
/// On `main` the first waited out the thirty-second read limit and the
/// second never ended: each byte renewed the limit.
///
/// The program prints how long each call took and the live-count delta
/// around it, then stays up for three seconds, so the only thing that can
/// close the connection before the server's report is the call itself.
#[test]
fn a_call_past_its_deadline_times_out_and_lets_go() {
    for backend in ["threads", "scheduler"] {
        let (silent, silent_closed) = slow_server(None);
        let (dripping, dripping_closed) = slow_server(Some(Duration::from_millis(50)));
        let source = format!(
            "module main;
import std::core::{{Result, print}};
import std::clock::{{Clock}};
import std::net::http::{{Answer, Call, CallError, HttpClient}};

extern fn khora_live_count() -> Int;
extern fn khora_monotonic_millis() -> Int;

/// The status, or -1 for `TimedOut` after exactly the 200 ms asked for, or
/// -2 for anything else (printed). An `Int`, so nothing the call made is
/// still alive when the live count is read.
fn ask(url: String) -> Int {{
  with {{ http: HttpClient::within(200) }} {{
    match http.send(Call::get(url)) {{
      Result::Ok(answer) => answer.status,
      Result::Err(CallError::TimedOut(200)) => -1,
      Result::Err(why) => {{ print(\"failed ${{why.show()}}\"); -2 }},
    }}
  }}
}}

fn timed(name: String, url: String) -> () {{
  let before = khora_live_count();
  let began = khora_monotonic_millis();
  let answer = ask(url);
  let took = khora_monotonic_millis() - began;
  let delta = khora_live_count() - before;
  let text = if answer == -1 {{ \"timed out\" }} else {{ \"answered ${{answer}}\" }};
  print(\"${{name}}: ${{text}}, under two seconds ${{took < 2000}}, live ${{delta}}\");
  if took >= 2000 {{ print(\"${{name}} took ${{took}} ms\") }};
}}

pub fn main() -> Int {{
  timed(\"silent\", \"http://127.0.0.1:{silent}/\");
  timed(\"dripping\", \"http://127.0.0.1:{dripping}/\");
  print(\"lingering\");
  with {{ clock: Clock::real() }} {{ clock.sleep(3000) }};
  0
}}
"
        );
        let exe = build(&format!("http_deadline_{backend}"), &source);
        let mut child = Command::new(&exe)
            .env("KHORA_FIBERS", backend)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the program should run");
        let stdout = child.stdout.take().expect("piped");
        // On another thread, so a program that never prints the last line
        // fails this test at the bound instead of holding it on a `read`.
        let (lines, heard) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut chunk = [0u8; 256];
            while let Ok(n @ 1..) = stdout.read(&mut chunk) {
                if lines.send(String::from_utf8_lossy(&chunk[..n]).into_owned()).is_err() {
                    break;
                }
            }
        });
        let mut said = String::new();
        let started = Instant::now();
        // The two calls, then the line that says they are over. Thirty
        // seconds is far over the two the calls may take, and is what the
        // silent call alone took without a deadline.
        while !said.contains("lingering") {
            let left = Duration::from_secs(30).saturating_sub(started.elapsed());
            match heard.recv_timeout(left) {
                Ok(more) => said.push_str(&more),
                Err(_) => break,
            }
        }
        // Reported while the program is still lingering, or the call did not
        // close its connection and only the exit would.
        let silent_let_go = silent_closed.recv_timeout(Duration::from_secs(2)).is_ok();
        let dripping_let_go = dripping_closed.recv_timeout(Duration::from_secs(2)).is_ok();
        let _ = child.kill();
        let _ = child.wait();
        let said = said.replace("\r\n", "\n");
        assert_eq!(
            said,
            "silent: timed out, under two seconds true, live 0\n\
             dripping: timed out, under two seconds true, live 0\n\
             lingering\n",
            "`{backend}`"
        );
        assert!(silent_let_go, "`{backend}`: the call that timed out left the silent server's connection open");
        assert!(dripping_let_go, "`{backend}`: the call that timed out left the dripping server's connection open");
    }
}

/// **A refused connection is `Unreachable` with a deadline set, as without
/// one.** The deadline turns only running out of time into `TimedOut`; a
/// port with nothing on it fails at once and says where.
#[test]
fn a_refused_connection_within_a_deadline_is_still_unreachable() {
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        listener.local_addr().expect("an address").port()
    };
    let exe = build(
        "http_deadline_refused",
        &format!(
            "module main;
import std::core::{{Result, print}};
import std::net::http::{{Call, CallError, HttpClient}};

pub fn main() -> Int {{
  with {{ http: HttpClient::within(5000) }} {{
    match http.send(Call::get(\"http://127.0.0.1:{port}/\")) {{
      Result::Ok(_) => print(\"answered\"),
      Result::Err(why) => print(why.show()),
    }}
  }};
  0
}}
"
        ),
    );
    let out = Command::new(&exe).output().expect("the program should run");
    let said = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(said, format!("CallError::Unreachable(127.0.0.1:{port})\n"));
}

/// Runs `exe` on `backend` and gathers what it prints, until it prints
/// `until` or `patience` runs out, without waiting for it to exit.
fn heard_until(exe: &PathBuf, backend: &str, until: &str, patience: Duration) -> (String, std::process::Child) {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let stdout = child.stdout.take().expect("piped");
    let (lines, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut chunk = [0u8; 256];
        while let Ok(n @ 1..) = stdout.read(&mut chunk) {
            if lines.send(String::from_utf8_lossy(&chunk[..n]).into_owned()).is_err() {
                break;
            }
        }
    });
    let mut said = String::new();
    let started = Instant::now();
    while !said.contains(until) {
        let left = patience.saturating_sub(started.elapsed());
        match heard.recv_timeout(left) {
            Ok(more) => said.push_str(&more),
            Err(_) => break,
        }
    }
    (said.replace("\r\n", "\n"), child)
}

/// **A deadline longer than the per-read limit governs the reads too.** A
/// call given 40 s against a server that answers after 31 s gets its answer.
/// The two limits used to stack and the shorter won: the call was cut at the
/// thirty-second read limit and reported as `Closed(nothing came back)`, so
/// no `within` could wait for a slow first byte.
///
/// Thirty-one seconds, because the limit it proves absent is thirty. Both
/// backends run at once, so the test takes that long once.
#[test]
fn a_deadline_longer_than_the_read_limit_governs_the_reads() {
    let runs: Vec<_> = ["threads", "scheduler"]
        .into_iter()
        .map(|backend| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
            let port = listener.local_addr().expect("an address").port();
            std::thread::spawn(move || {
                let Ok((mut socket, _)) = listener.accept() else { return };
                let mut chunk = [0u8; 4096];
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => got.extend_from_slice(&chunk[..n]),
                    }
                }
                std::thread::sleep(Duration::from_secs(31));
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            });
            let exe = build(
                &format!("http_deadline_long_{backend}"),
                &format!(
                    "module main;
import std::core::{{Result, print}};
import std::net::http::{{Answer, Call, CallError, HttpClient}};

pub fn main() -> Int {{
  with {{ http: HttpClient::within(40000) }} {{
    match http.send(Call::get(\"http://127.0.0.1:{port}/\")) {{
      Result::Ok(answer) => print(\"answered ${{answer.status}}\"),
      Result::Err(why) => print(why.show()),
    }}
  }};
  print(\"done\");
  0
}}
"
                ),
            );
            std::thread::spawn(move || {
                let (said, mut child) = heard_until(&exe, backend, "done", Duration::from_secs(60));
                let _ = child.kill();
                let _ = child.wait();
                (backend, said)
            })
        })
        .collect();
    for run in runs {
        let (backend, said) = run.join().expect("a run");
        assert_eq!(said, "answered 200\ndone\n", "`{backend}`");
    }
}

/// **A canceled call closes its connection wherever the cancel lands.** A
/// thousand calls, each canceled after a spin of a different length, so the
/// cancel arrives before the connect, inside it, just after it and in the
/// read. The server never answers and counts the connections still open.
///
/// The one after the connect leaked: the handle came back to a caller that
/// registered its close one cancellation point later, so a cancel between the
/// two unwound past a socket nothing would close -- 35 to 47 per thousand on
/// the scheduler. The live count cannot see a socket, which is why the
/// server counts them.
#[test]
fn a_canceled_call_closes_its_connection_wherever_the_cancel_lands() {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::Arc;
    for backend in ["threads", "scheduler"] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let open = Arc::new(AtomicIsize::new(0));
        let accepted = Arc::new(AtomicIsize::new(0));
        {
            let (open, accepted) = (open.clone(), accepted.clone());
            std::thread::spawn(move || {
                for socket in listener.incoming() {
                    let Ok(mut socket) = socket else { continue };
                    open.fetch_add(1, Ordering::SeqCst);
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let open = open.clone();
                    std::thread::spawn(move || {
                        let mut chunk = [0u8; 4096];
                        // Reads the request and then whatever else, and never
                        // answers: the client's close is the only way out.
                        while let Ok(1..) = socket.read(&mut chunk) {}
                        open.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        }
        let exe = build(
            &format!("http_cancel_{backend}"),
            &format!(
                "module main;
import std::core::{{Fiber, Result, print}};
import std::clock::{{Clock}};
import std::net::http::{{Call, CallError, HttpClient}};

fn call() -> () {{
  with {{ http: HttpClient::real() }} {{
    match http.send(Call::get(\"http://127.0.0.1:{port}/\")) {{
      Result::Ok(_) => print(\"answered\"),
      Result::Err(why) => print(\"failed ${{why.show()}}\"),
    }}
  }}
}}

fn round(spin: Int) -> Int {{
  let f = Fiber::spawn(fn () => call());
  let mut i = 0;
  while i < spin {{ i = i + 1 }};
  Fiber::cancel(f);
  Fiber::wait(f);
  if Fiber::canceled(f) {{ 1 }} else {{ 0 }}
}}

pub fn main() -> Int {{
  let mut k = 0;
  let mut canceled = 0;
  while k < 1000 {{
    canceled = canceled + round((k * 37) % 20000);
    k = k + 1;
  }};
  print(\"canceled ${{canceled}}\");
  print(\"lingering\");
  with {{ clock: Clock::real() }} {{ clock.sleep(5000) }};
  0
}}
"
            ),
        );
        let (said, mut child) = heard_until(&exe, backend, "lingering", Duration::from_secs(120));
        // Room for the server's readers to see the closes that did happen.
        let quiet = Instant::now() + Duration::from_secs(3);
        while open.load(Ordering::SeqCst) > 0 && Instant::now() < quiet {
            std::thread::sleep(Duration::from_millis(20));
        }
        let still_open = open.load(Ordering::SeqCst);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(said, "canceled 1000\nlingering\n", "`{backend}`");
        assert_eq!(
            still_open,
            0,
            "`{backend}`: {still_open} of {} connections were left open by canceled calls",
            accepted.load(Ordering::SeqCst)
        );
    }
}
