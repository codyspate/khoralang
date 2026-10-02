#![cfg(feature = "llvm")]
#![cfg(unix)]

//! How many connections a `Router` server accepts before it stops.
//!
//! **What this prevents: a server that dies of `SIGSEGV` after a fixed number
//! of connections.** `Router::serve_forever` and `Router::serve_secured` took
//! the next connection by calling themselves, one stack frame per accepted
//! connection, on the thread that runs `main`. Khora does not promise tail
//! calls, so the frames stayed: an 8 MB main stack was gone after about
//! 130,000 connections, on either fiber backend. That is minutes for a busy
//! service whose clients do not keep connections alive, and the process ended
//! with nothing on stderr. A cancel-and-abort storm against a server on the
//! packaged toolchain found it as a SIGSEGV at 130,957 frames.
//!
//! Each server here runs under a 256 KB stack limit, so a frame per
//! connection runs out within a few thousand rather than after a hundred
//! thousand: the recursive loop died at 2,500-3,900 connections in
//! measurement, and each test asks for 20,000. Unix only, because the limit
//! is set with `setrlimit` in the child before it starts.

use crate::harness;

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Its own ports. `tests/traps_in_a_server.rs` lists the ones in use.
const PLAIN_PORT: u16 = 18736;
const TLS_PORT: u16 = 18737;

/// A stack small enough that a frame per connection shows within seconds,
/// and large enough for everything else the server does.
const STACK: u64 = 256 * 1024;

/// More connections than the recursive loop survived under [`STACK`] by a
/// factor of five.
const CONNECTIONS: usize = 20_000;

const PLAIN: &str = "module demo::main;
import std::core::{ChildFailed, SharedFn};
import std::net::http::{HttpError, Response, Router};

pub fn main() raises HttpError + ChildFailed {
  Router::new()
    |> Router::get(\"/\", SharedFn::of(fn _request => Response::text(200, \"ok\")))
    |> Router::listen(@PORT@)!
}
";

const SECURED: &str = "module demo::main;
import std::core::{ChildFailed, Scope, SharedFn};
import std::net::http::{HttpError, Response, Router};
import std::net::tls::{TlsError};

pub fn main() raises HttpError + TlsError + ChildFailed {
  with { scope: Scope::root() } {
    Router::new()
      |> Router::get(\"/\", SharedFn::of(fn _request => Response::text(200, \"ok\")))
      |> Router::listen_tls(@PORT@, @CERT@, @KEY@)!
  }
}
";

/// Every `.kh` file of `std`, plus the server.
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

/// Killed on every path out, so a failed run does not leave the port held.
struct Server {
    child: std::process::Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Builds `main`, starts it under [`STACK`], and waits for it to announce
/// that it is listening.
fn start(name: &str, main: &str) -> Server {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("server");
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("the test server did not build:\n  {}", messages.join("\n  "));
    }

    let mut command = Command::new(&exe);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: runs in the forked child before `exec`, and calls only
    // `setrlimit`, which is async-signal-safe and touches no memory the parent
    // shares; the limit is a stack value that outlives the call.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit { rlim_cur: STACK, rlim_max: STACK };
            if libc::setrlimit(libc::RLIMIT_STACK, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut server = Server { child: command.spawn().expect("the server should start") };

    // The announcement is the handshake: connecting before `listen` has bound
    // the port is a race.
    let mut stdout = server.child.stdout.take().expect("piped");
    let mut opened = [0u8; 9];
    stdout.read_exact(&mut opened).expect("the server should announce itself");
    assert_eq!(&opened, b"listening");
    server
}

/// Sends `request` on a fresh connection and reads until the server closes
/// it. `None` when the connection could not be made at all.
fn exchange(port: u16, request: &[u8]) -> Option<Vec<u8>> {
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    let deadline = Some(std::time::Duration::from_secs(10));
    socket.set_read_timeout(deadline).ok()?;
    socket.set_write_timeout(deadline).ok()?;
    let _ = socket.write_all(request);
    let mut answer = Vec::new();
    let _ = socket.read_to_end(&mut answer);
    Some(answer)
}

/// Fails with what the server said if it stopped before `served` reached
/// [`CONNECTIONS`].
fn assert_served_all(server: &mut Server, served: usize) {
    if served == CONNECTIONS {
        return;
    }
    let status = server.child.try_wait().ok().flatten();
    let mut stderr = String::new();
    if status.is_some() {
        if let Some(mut err) = server.child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }
    }
    panic!(
        "the server stopped answering after {served} of {CONNECTIONS} connections \
         (exit: {status:?}); it said: {stderr}"
    );
}

#[test]
fn a_server_accepts_any_number_of_connections() {
    let main = PLAIN.replace("@PORT@", &PLAIN_PORT.to_string());
    let mut server = start("http_accept_plain", &main);
    let request = b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let mut served = 0;
    while served < CONNECTIONS
        && exchange(PLAIN_PORT, request).is_some_and(|a| a.starts_with(b"HTTP/1.1 200"))
    {
        served += 1;
    }
    assert_served_all(&mut server, served);
}

/// The TLS loop, driven by plain bytes: each one is accepted, refused at the
/// handshake and closed by the server, which takes an accept without a TLS
/// client. A dead server refuses the connection instead.
#[test]
fn a_tls_server_accepts_any_number_of_connections() {
    let main = SECURED
        .replace("@PORT@", &TLS_PORT.to_string())
        .replace("@CERT@", &format!("{:?}", fixture("localhost.cert.pem")))
        .replace("@KEY@", &format!("{:?}", fixture("localhost.key.pem")));
    let mut server = start("http_accept_tls", &main);
    let mut served = 0;
    while served < CONNECTIONS && exchange(TLS_PORT, b"not a handshake\r\n\r\n").is_some() {
        served += 1;
    }
    assert_served_all(&mut server, served);
    assert!(
        server.child.try_wait().ok().flatten().is_none(),
        "the server ended while it was being connected to"
    );
}
