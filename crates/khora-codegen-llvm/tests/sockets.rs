#![cfg(feature = "llvm")]

//! Sockets, against a real connection.
//!
//! Everything under test is written in Khora: the Berkeley calls are `extern`
//! declarations, and the sixteen bytes of a `sockaddr_in` are laid out in an
//! `Array<U8>` and lent as a `Ptr`, because no struct crosses the C ABI.
//! `docs/design/ffi.md`.

mod harness;

use std::io::{Read, Write};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_sources(db: &KhoraDatabase, dir: &std::path::Path) -> Vec<SourceFile> {
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
                let name = path.file_name().expect("a name").to_string_lossy().into_owned();
                out.push(SourceFile::new(db, dir.join(name), text));
            }
        }
    }
    out
}

fn build(name: &str, main: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db, &dir);
    files.push(SourceFile::new(&db, dir.join("main.kh"), main.to_string()));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    exe
}

/// A Khora program that listens, accepts one connection, echoes what it reads,
/// and stops — with Rust on the other end of the wire.
#[test]
fn khora_can_accept_a_connection_and_answer() {
    let exe = build(
        "socket_echo",
        "module demo::main;
import std::core::{Array, Option};
import std::net::socket::{start, listen_on, accept_on, receive, transmit, shut, invalid_handle};

fn print(value: String);
extern fn khora_print_int(value: Int);

fn main() -> Int {
  if start() {
    let server = listen_on(18711);
    if server == invalid_handle() {
      print(\"could not listen\");
      1
    } else {
      // Printed so the test knows the port is open before it connects.
      print(\"listening\");
      let connection = accept_on(server);
      if connection == invalid_handle() {
        print(\"could not accept\");
        1
      } else {
        let buffer: Array<U8> = Array::new(64, 0);
        let read = receive(connection, buffer);
        khora_print_int(read);
        transmit(connection, \"khora says \" + Int::to_string(read));
        shut(connection);
        shut(server);
        0
      }
    }
  } else {
    print(\"no winsock\");
    1
  }
}
",
    );

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the program should start");

    // Wait for the listen to be up. Reading the first line is the handshake:
    // connecting before `listen` has returned is a race the test would lose
    // intermittently, which is worse than losing it every time.
    let mut stdout = child.stdout.take().expect("piped");
    let mut opened = [0u8; 10];
    read_exactly(&mut stdout, &mut opened);
    assert!(
        String::from_utf8_lossy(&opened).starts_with("listening"),
        "expected the program to reach `listen`, got {:?}",
        String::from_utf8_lossy(&opened)
    );

    let mut socket = connect_retrying(18711);
    socket.write_all(b"hello from rust").expect("writing to the Khora server");
    socket.flush().expect("flush");

    let mut answer = String::new();
    socket.read_to_string(&mut answer).expect("reading the Khora server's answer");
    assert_eq!(answer, "khora says 15", "fifteen bytes went, and it counted them");

    // **Close before waiting on the child.** `shut` on the Khora side does a
    // `shutdown` and then drains whatever the client still had to say, and a
    // client socket left open gives that drain nothing to end it — the program
    // cannot exit, so the `read_to_string` below cannot return. This test spent
    // two minutes of every suite run waiting for that to time out.
    drop(socket);

    let mut rest = String::new();
    stdout.read_to_string(&mut rest).expect("the rest of stdout");
    assert!(rest.contains("15"), "the program printed what it read: {rest:?}");

    let status = child.wait().expect("the program should finish");
    assert_eq!(status.code(), Some(0));
}

fn read_exactly(from: &mut impl Read, into: &mut [u8]) {
    let mut at = 0;
    while at < into.len() {
        match from.read(&mut into[at..]) {
            Ok(0) => panic!("the program stopped before it was listening"),
            Ok(n) => at += n,
            Err(e) => panic!("reading the program's output: {e}"),
        }
    }
}

/// The listen is up by the time the handshake line arrives, but the accept may
/// not be — a connection refused here is a lost race rather than a failure.
fn connect_retrying(port: u16) -> std::net::TcpStream {
    for _ in 0..100 {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(socket) => return socket,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    panic!("could not connect to the Khora server on {port}");
}

/// Khora dialling *out*, which nothing could do until phase 13.
///
/// Everything else in this module and in `std::net::socket` grew from serving:
/// `listen_on`, `accept_on`, and nothing that starts a conversation. A database
/// driver is the first caller that needs the other direction, so `connect_to`
/// exists and this is the proof it reaches something.
///
/// It also exercises `transmit_bytes`. `transmit` takes a `String`, which is
/// right for a protocol made of text and wrong for one framed with a length
/// nobody wrote as characters — which is every wire protocol, Postgres
/// included.
#[test]
fn khora_can_dial_out_and_send_bytes() {
    // A listener on this side, so the Khora program has something real to
    // reach. Port zero: the operating system picks one that is free, which
    // beats hoping a hard-coded number is.
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("a port to listen on");
    let port = listener.local_addr().expect("an address").port();

    let heard = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut got = [0u8; 5];
        stream.read_exact(&mut got).expect("the five bytes");
        // Answer, so the Khora side can prove the connection is two-way.
        stream.write_all(b"pong").expect("the answer");
        got
    });

    let exe = build(
        "socket_dial",
        &format!(
            "module demo::main;
import std::core::{{Array, Option}};
import std::net::socket::{{start, connect_to, transmit_bytes, receive, shut, invalid_handle}};

fn print(value: String);
extern fn khora_print_int(value: Int);

fn main() -> Int {{
  if start() {{
    let connection = connect_to(\"127.0.0.1\", {port});
    if connection == invalid_handle() {{
      print(\"could not connect\");
      1
    }} else {{
      // Five bytes that are not text: a zero and a high byte would both be
      // mangled by anything that went through a `String`.
      let message: Array<U8> = Array::new(5, 0);
      Array::set(message, 0, 1);
      Array::set(message, 1, 0);
      Array::set(message, 2, 255);
      Array::set(message, 3, 128);
      Array::set(message, 4, 42);
      let sent = transmit_bytes(connection, message);
      khora_print_int(sent);

      let buffer: Array<U8> = Array::new(16, 0);
      let read = receive(connection, buffer);
      khora_print_int(read);
      shut(connection);
      0
    }}
  }} else {{
    print(\"no winsock\");
    1
  }}
}}
"
        ),
    );

    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert!(ran.status.success(), "the dialler exited with {:?}: {out}", ran.status.code());
    assert_eq!(out, "5\n4\n", "five bytes out, four back: {out}");

    let got = heard.join().expect("the listener");
    assert_eq!(
        got,
        [1u8, 0, 255, 128, 42],
        "the bytes arrived unchanged — a zero and a high byte included"
    );
}
