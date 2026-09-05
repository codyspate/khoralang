#![cfg(feature = "llvm")]

//! TLS, against a client that is not ours.
//!
//! A handshake either interoperates or it does not, and the only way to know is
//! to run one against something written by somebody else. Python's `ssl` is
//! OpenSSL, which is the other implementation almost every real client is.
//!
//! `docs/design/ecosystem.md` decides that TLS is bound rather than written, so
//! what is under test here is not cryptography — it is the boundary: that a
//! certificate crosses as bytes, that a socket changes hands exactly once, and
//! that a connection is released when its scope ends.
//!
//! `tests/fixtures` holds a small chain: a CA, and a leaf for `localhost`
//! signed by it. A chain rather than one self-signed certificate because that
//! is what a real deployment has, and because the self-signed one this started
//! with was a *CA* certificate — `openssl req -x509` makes one by default —
//! which `rustls` correctly refuses as a server's leaf with
//! `CaUsedAsEndEntity`. Nothing caught it until a client actually verified,
//! which is the argument for having a client at all.
//!
//! They are **test** certificates with their private keys beside them in a
//! public repository. Anything using them in earnest deserves what it gets.

use crate::harness;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test.
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

/// Builds `main` and starts it, giving back the child.
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

/// Speaks TLS to `port` with Python's `ssl`, sends `send`, returns what came
/// back.
///
/// Deliberately another implementation. A handshake between two copies of
/// `rustls` proves that `rustls` agrees with itself.
fn python_tls_roundtrip(port: u16, send: &str) -> String {
    let script = format!(
        "import socket, ssl, sys\n\
         c = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)\n\
         c.check_hostname = False\n\
         c.verify_mode = ssl.CERT_NONE\n\
         s = c.wrap_socket(socket.create_connection(('127.0.0.1', {port}), timeout=10),\n\
         \x20   server_hostname='localhost')\n\
         s.sendall({send:?}.encode())\n\
         got = b''\n\
         while True:\n\
         \x20   chunk = s.recv(4096)\n\
         \x20   if not chunk: break\n\
         \x20   got += chunk\n\
         sys.stdout.write(got.decode())\n"
    );
    let out = Command::new(harness::python())
        .arg("-c")
        .arg(&script)
        .output()
        .expect("python should run; it is what drives the load generator too");
    assert!(
        out.status.success(),
        "the python client failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Waits for the server to say it is listening, so the client does not race it.
/// Hands the reader back rather than dropping it: taking `stdout` twice gives
/// `None` the second time, and a test that then unwraps it fails for a reason
/// with nothing to do with TLS.
fn wait_for_ready(child: &mut Child) -> BufReader<std::process::ChildStdout> {
    let stdout = child.stdout.take().expect("a piped stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("the server should announce itself");
    assert!(line.contains("ready"), "unexpected first line from the server: {line}");
    reader
}

/// The whole of it: a certificate, a handshake with OpenSSL on the other end,
/// an echo, and a clean close.
#[test]
fn a_khora_server_speaks_tls_to_an_openssl_client() {
    let port = 18971;
    let program = format!(
        "module main;

import std::core::{{Array, Scope, print}};
import std::net::socket::{{accept_on, invalid_handle, listen_on, start}};
import std::net::tls::{{TlsError, TlsServer, receive, secure, server, shut, transmit}};

const certificate = {:?};
const key = {:?};

/// One connection, secured, echoed back, closed.
fn answer(settings: TlsServer, socket: Int) -> () raises TlsError {{
  let connection = secure(settings, socket)!;
  let buffer: Array<U8> = Array::new(4096, 0);
  let read = receive(connection, buffer);
  if read > 0 {{
    let heard = String::from_bytes(Array::prefix(buffer, read));
    transmit(connection, \"you said: \" + heard);
  }} else {{
  }};
  shut(connection)
}}

pub fn main() -> () raises TlsError {{
  with {{ scope: Scope::root() }} {{
    start();
    let settings = server(certificate, key)!;
    let listener = listen_on({port});
    print(\"ready\");
    let taken = accept_on(listener);
    if taken == invalid_handle() {{ }} else {{ answer(settings, taken)! }}
  }}
}}
",
        fixture("localhost.cert.pem"),
        fixture("localhost.key.pem"),
    );

    let mut child = start("tls_echo", &program);
    let _reader = wait_for_ready(&mut child);
    let heard = python_tls_roundtrip(port, "hello over tls");
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(heard, "you said: hello over tls");
}

/// **Both ends, ours.** A Khora client connects to a Khora server, verifies its
/// certificate, and reads the answer.
///
/// Verification is real. The client is given the test certificate as an extra
/// root and reaches the server as `localhost`, which is the name that
/// certificate is for. Nothing here turns checking off, because `std::net::tls`
/// offers nothing to turn it off with.
///
/// It also exercises the reason `TlsClient` is `Share`: the configuration
/// crosses into the fiber that does the calling.
#[test]
fn a_khora_client_verifies_a_khora_server() {
    let said = run_and_read("tls_both_ends", BOTH_ENDS, 18973);
    assert!(
        said.contains("you said: hello from khora"),
        "the client should have read the server's answer, got: {said}"
    );
}

/// A name the certificate is not for is refused, which is the whole point of
/// checking one.
///
/// `127.0.0.1` reaches the very same server as `localhost` and is not what the
/// certificate says. It is the closest a test gets to staging a man in the
/// middle without one.
#[test]
fn a_certificate_for_the_wrong_name_is_refused() {
    let said = run_and_read("tls_wrong_name", WRONG_NAME, 18974);
    assert!(
        said.contains("refused: 127.0.0.1"),
        "a name the certificate does not cover must fail: {said}"
    );
}

/// Fills in the port and the certificate, runs it, and returns what it printed
/// after "ready".
fn run_and_read(name: &str, source: &str, port: u16) -> String {
    let program = source
        .replace("PORT", &port.to_string())
        .replace("CERT_PEM", &format!("{:?}", fixture("localhost.cert.pem")))
        .replace("KEY_PEM", &format!("{:?}", fixture("localhost.key.pem")))
        .replace("CA_PEM", &format!("{:?}", fixture("ca.cert.pem")));

    let mut child = start(name, &program);
    let reader = wait_for_ready(&mut child);
    let mut said = String::new();
    for line in reader.lines() {
        said.push_str(&line.expect("a line"));
        said.push('\n');
    }
    let _ = child.kill();
    let _ = child.wait();
    said
}

/// A client that is not speaking TLS is refused, and the server survives it.
///
/// This is the ordinary case on a public port — a scanner, or somebody typing
/// `http://` — so it has to be a raised `Handshake` rather than anything
/// louder.
#[test]
fn plain_bytes_on_a_tls_port_are_a_handshake_failure() {
    let port = 18972;
    let program = format!(
        "module main;

import std::core::{{Array, Result, Scope, attempt, print}};
import std::net::socket::{{accept_on, invalid_handle, listen_on, start}};
import std::net::tls::{{TlsError, secure, server, shut}};

const certificate = {:?};
const key = {:?};

pub fn main() -> () raises TlsError {{
  with {{ scope: Scope::root() }} {{
    start();
    let settings = server(certificate, key)!;
    let listener = listen_on({port});
    print(\"ready\");
    let taken = accept_on(listener);
    match attempt(fn () => secure(settings, taken)!) {{
      Result::Ok(connection) => {{ print(\"secured, which is wrong\"); shut(connection) }},
      Result::Err(why) => match why {{
        TlsError::Handshake => print(\"refused the handshake\"),
        TlsError::BadCertificate => print(\"bad certificate, which is wrong\"),
        TlsError::BadKey => print(\"bad key, which is wrong\"),
        TlsError::Unreachable(_) => print(\"unreachable, which is wrong\"),
      }},
    }}
  }}
}}
",
        fixture("localhost.cert.pem"),
        fixture("localhost.key.pem"),
    );

    let mut child = start("tls_plain", &program);
    let reader = wait_for_ready(&mut child);

    // Plain HTTP at an HTTPS port, which is what a browser sends by mistake.
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).expect("a connection");
    let _ = socket.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    drop(socket);

    let mut rest = String::new();
    for line in reader.lines() {
        rest.push_str(&line.expect("a line"));
        rest.push('\n');
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(rest.contains("refused the handshake"), "the server said: {rest}");
}

/// A certificate that is not one is refused where the server is configured,
/// rather than at the first connection.
#[test]
fn a_bad_certificate_is_refused_at_startup() {
    let program = "module main;

import std::core::{Result, Scope, attempt, print};
import std::net::tls::{TlsError, server};

pub fn main() -> () {
  with { scope: Scope::root() } {
    match attempt(fn () => server(\"not a certificate\", \"nor is this\")!) {
      Result::Ok(_) => print(\"opened, which is wrong\"),
      Result::Err(why) => match why {
        TlsError::BadCertificate => print(\"refused the certificate\"),
        TlsError::BadKey => print(\"bad key\"),
        TlsError::Handshake => print(\"handshake\"),
        TlsError::Unreachable(_) => print(\"unreachable\"),
      },
    }
  }
}
";
    let mut child = start("tls_bad_cert", program);
    let stdout = child.stdout.take().expect("a piped stdout");
    let mut said = String::new();
    for line in BufReader::new(stdout).lines() {
        said.push_str(&line.expect("a line"));
        said.push('\n');
    }
    let _ = child.wait();
    assert!(said.contains("refused the certificate"), "the program said: {said}");
}

const BOTH_ENDS: &str = r#"module main;

import std::core::{Array, Fiber, Result, Scope, attempt, print};
import std::net::socket::{accept_on, invalid_handle, listen_on, start};
import std::net::tls::{
  TlsClient, TlsError, TlsServer, connect, receive, secure, server, shut, transmit, trusting,
};

const certificate = CERT_PEM;
const key = KEY_PEM;
const authority = CA_PEM;

/// The client half, run on a fiber.
///
/// `caller` crosses into it, which is what `impl Share for TlsClient` is for: a
/// `rustls` configuration behind an `Arc` is safe for any number of readers,
/// and a client that could not reach the fiber doing the calling would not be
/// much of a client.
///
/// Answers for itself, because a fiber's body cannot raise — whatever went
/// wrong has nobody left to tell.
fn call(caller: TlsClient) -> () {
  match attempt(fn () => connect(caller, "localhost", PORT)!) {
    Result::Err(_) => print("the client could not connect"),
    Result::Ok(connection) => {
      transmit(connection, "hello from khora");
      let buffer: Array<U8> = Array::new(4096, 0);
      let read = receive(connection, buffer);
      if read > 0 {
        print(String::from_bytes(Array::prefix(buffer, read)));
      } else {
        print("the server said nothing");
      };
      shut(connection)
    },
  }
}

fn answer(settings: TlsServer, socket: Int) -> () raises TlsError {
  let connection = secure(settings, socket)!;
  let buffer: Array<U8> = Array::new(4096, 0);
  let read = receive(connection, buffer);
  if read > 0 {
    transmit(connection, "you said: " + String::from_bytes(Array::prefix(buffer, read)));
  } else {
  };
  shut(connection)
}

pub fn main() -> () raises TlsError {
  with { scope: Scope::root() } {
    start();
    let settings = server(certificate, key)!;
    let listener = listen_on(PORT);
    // The test certificate is not one this machine trusts, so it is handed over
    // as an extra root. That is the whole of the difference from calling a
    // public server, and a great deal better than a switch that stops checking.
    let caller = trusting(authority)!;
    print("ready");

    let calling = Fiber::spawn(fn () => call(caller));
    let taken = accept_on(listener);
    if taken == invalid_handle() { } else { answer(settings, taken)! };
    Fiber::join(calling)!
  }
}
"#;

const WRONG_NAME: &str = r#"module main;

import std::core::{Array, Fiber, Result, Scope, attempt, print};
import std::net::socket::{accept_on, invalid_handle, listen_on, start};
import std::net::tls::{
  TlsClient, TlsError, TlsServer, connect, secure, server, shut, trusting,
};

const certificate = CERT_PEM;
const key = KEY_PEM;
const authority = CA_PEM;

/// Reaches the same server by an address the certificate does not name.
fn call(caller: TlsClient) -> () {
  match attempt(fn () => connect(caller, "127.0.0.1", PORT)!) {
    Result::Ok(connection) => { print("connected, which is wrong"); shut(connection) },
    Result::Err(why) => match why {
      TlsError::Unreachable(host) => print("refused: " + host),
      TlsError::Handshake => print("handshake"),
      TlsError::BadCertificate => print("bad certificate"),
      TlsError::BadKey => print("bad key"),
    },
  }
}

pub fn main() -> () raises TlsError {
  with { scope: Scope::root() } {
    start();
    let settings = server(certificate, key)!;
    let listener = listen_on(PORT);
    let caller = trusting(authority)!;
    print("ready");
    let calling = Fiber::spawn(fn () => call(caller));
    let taken = accept_on(listener);
    // The handshake fails on the client's side, so this one fails too and that
    // is fine — the point is what the caller was told.
    if taken == invalid_handle() { } else { let _ = attempt(fn () => secure(settings, taken)!); };
    Fiber::join(calling)!
  }
}
"#;
