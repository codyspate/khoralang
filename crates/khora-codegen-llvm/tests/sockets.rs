#![cfg(feature = "llvm")]

//! Sockets, against a real connection.
//!
//! Everything under test is written in Khora: the Berkeley calls are `extern`
//! declarations, and the sixteen bytes of a `sockaddr_in` are laid out in an
//! `Array<U8>` and lent as a `Ptr`, because no struct crosses the C ABI.
//! `docs/design/ffi.md`.

use crate::harness;

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

/// **A client that resets its connection mid-response costs that response,
/// not the server.**
///
/// After a peer's RST (a killed browser tab, a load balancer, `kill -9`), a
/// `send` on the connection makes the kernel raise `SIGPIPE`, and a Khora
/// binary keeps that signal's default action, which ends the process. So one
/// client that vanished took down every other client's server, on both fiber
/// backends.
///
/// The server serves each connection in a fiber. The first client asks, then
/// resets; the server goes on writing its answer until writes fail, and
/// prints how that went. The second client must still be served, and the
/// server must exit 0 rather than die of signal 13.
///
/// Unix only: Windows has no `SIGPIPE`, and a send to a reset peer there
/// was always an ordinary failure.
#[cfg(unix)]
#[test]
fn a_client_that_resets_mid_response_does_not_stop_the_server() {
    const PORT: u16 = 18735;
    let exe = build(
        "socket_reset_peer",
        &format!(
            "module demo::main;
import std::core::{{Array, Fiber, print}};
import std::clock::{{Clock}};
import std::net::socket::{{start, listen_on, accept_on, receive, transmit, shut, invalid_handle}};

/// Writes to `connection` until two writes have failed, at most `tries` times.
///
/// Two, because the first write after a reset fails quietly with
/// `ECONNRESET`, and the next one is the write that raised `SIGPIPE`. A
/// handler that writes its answer in several pieces and checks at the end
/// makes that second write as a matter of course.
fn answer(connection: Int, tries: Int) -> Bool {{
  with {{ clock: Clock::real() }} {{
    let mut i = 0;
    let mut failures = 0;
    while i < tries && failures < 2 {{
      if transmit(connection, \"part of a long answer\\n\") < 0 {{ failures = failures + 1 }};
      clock.sleep(5);
      i = i + 1;
    }};
    failures == 2
  }}
}}

fn serve(connection: Int, n: Int) -> () {{
  let buffer: Array<U8> = Array::new(64, 0);
  let read = receive(connection, buffer);
  if n == 1 {{
    if answer(connection, 200) {{ print(\"1: the write failed\") }} else {{ print(\"1: every write went\") }}
  }} else {{
    transmit(connection, \"served \" + Int::to_string(read));
    print(\"2: served\")
  }};
  shut(connection)
}}

pub fn main() -> Int {{
  if start() {{}} else {{ print(\"no sockets\") }};
  let server = listen_on({PORT});
  if server == invalid_handle() {{
    print(\"could not listen\");
    1
  }} else {{
    print(\"listening\");
    let mut n = 1;
    while n <= 2 {{
      let connection = accept_on(server);
      let which = n;
      Fiber::join(Fiber::spawn(fn () => serve(connection, which)));
      n = n + 1;
    }};
    shut(server);
    0
  }}
}}
"
        ),
    );

    // Both backends run before anything is asserted, so a failure names every
    // backend it happens on.
    let mut failures = Vec::new();
    for backend in ["threads", "scheduler"] {
        let mut child = std::process::Command::new(&exe)
            .env("KHORA_FIBERS", backend)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the server should start");
        let mut stdout = child.stdout.take().expect("piped");
        let mut opened = [0u8; 10];
        read_exactly(&mut stdout, &mut opened);
        assert_eq!(&opened, b"listening\n", "`{backend}`: expected the server to listen");

        // The first client: asks, then resets without reading the answer.
        let first = connect_retrying(PORT);
        (&first).write_all(b"give me everything").expect("the first request");
        std::thread::sleep(std::time::Duration::from_millis(30));
        reset(first);

        // The second: an ordinary request, which must still be answered. A
        // server that has died may refuse it or reset it; either is recorded
        // rather than panicked on, so the exit status below is still read.
        let mut answer = String::new();
        if let Ok(mut second) = std::net::TcpStream::connect(("127.0.0.1", PORT)) {
            let _ = second.write_all(b"hello");
            let _ = second.read_to_string(&mut answer);
        }

        let mut rest = String::new();
        let _ = stdout.read_to_string(&mut rest);
        let status = child.wait().expect("the server should finish");
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            failures.push(format!("`{backend}`: killed by signal {signal} (13 is SIGPIPE); it printed {rest:?}"));
        } else if answer != "served 5" || rest != "1: the write failed\n2: served\n" || status.code() != Some(0) {
            failures.push(format!(
                "`{backend}`: exit {:?}, the second client got {answer:?}, the server printed {rest:?}",
                status.code()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Closes `stream` with an RST instead of a FIN: `SO_LINGER` on, timeout zero.
#[cfg(unix)]
fn reset(stream: std::net::TcpStream) {
    use std::os::fd::AsRawFd;
    let linger = libc::linger { l_onoff: 1, l_linger: 0 };
    // SAFETY: `linger` is a live `struct linger` and the length is its size;
    // `stream` owns the descriptor for the whole call.
    let set = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&raw const linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    assert_eq!(set, 0, "SO_LINGER 0 is what makes the close an RST");
    drop(stream);
}
