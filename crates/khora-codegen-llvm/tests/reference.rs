#![cfg(feature = "llvm")]

//! **Phase 8's exit criterion: the reference application serves a request.**
//!
//! `examples/risk_analyzer` is the program the whole design was written
//! against, and this builds it and talks to it over a real socket. Everything
//! between the wire and the answer is Khora: the Berkeley calls are `extern`
//! declarations, the `sockaddr_in` is sixteen bytes laid out in an
//! `Array<U8>`, the request parser and the router are `std::net::http`, and the
//! handler runs with the `ai` and `ledger` capabilities its signature asked
//! for.

mod harness;

use std::io::{Read, Write};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std` and the reference application, as one program.
fn sources(db: &KhoraDatabase) -> Vec<SourceFile> {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut out = Vec::new();
    let mut stack = vec![repo.join("std"), repo.join("examples").join("risk_analyzer")];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable directory") {
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
    out
}

/// A port unlikely to be anybody else's. The application hard-codes 8080, so
/// this is the one thing the test cannot choose — it kills whatever is left of
/// a previous run instead of failing on a port it does not own.
const PORT: u16 = 8080;

#[test]
fn the_reference_application_serves_a_request() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("risk_analyzer");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "analyzer.exe" } else { "analyzer" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("the reference application did not build:\n  {}", messages.join("\n  "));
    }

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the analyzer should start");
    let mut stdout = child.stdout.take().expect("piped");

    // The server says so when the port is open. Reading that line is the
    // handshake: connecting earlier is a race the test would lose sometimes,
    // which is worse than losing it always.
    let mut opened = [0u8; 9];
    if let Err(why) = read_exactly(&mut stdout, &mut opened) {
        let _ = child.kill();
        panic!("the analyzer never reached `listen`: {why}");
    }
    assert_eq!(&opened, b"listening", "expected the server to announce itself");

    let answer = ask(&mut child);
    let _ = child.kill();
    let _ = child.wait();

    let answer = answer.expect("the analyzer should have answered");
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "status line:\n{answer}");
    assert!(
        answer.contains("Content-Type: application/json\r\n"),
        "the handler returned `Response::json`:\n{answer}"
    );
    // The report is encoded through `Encode`, so the body is the JSON a
    // client reads rather than what `Show` printed: keys sorted, and the
    // variant tagged with `type` and keyed by its payload name.
    assert!(
        answer.contains("\r\n\r\n{\"account_id\":\"acc_9921\""),
        "the body is the report the model produced:\n{answer}"
    );
    assert!(
        answer.contains("\"risk\":{\"action_required\":\"Immediate fund freeze\",\"type\":\"Critical\"}"),
        "the risk level came through the whole stack:\n{answer}"
    );

    // The declared `Content-Length` is the body's actual length. Getting this
    // wrong is how a client hangs, and it is the one header that has to be
    // computed rather than written down.
    let (head, body) = answer.split_once("\r\n\r\n").expect("a blank line");
    let declared: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .expect("a Content-Length")
        .trim()
        .parse()
        .expect("a number");
    assert_eq!(declared, body.len(), "the length header and the body agree");
}

/// Connects, sends one request, and reads the answer to the end.
fn ask(child: &mut std::process::Child) -> Option<String> {
    let mut socket = connect_retrying()?;
    socket
        .write_all(b"POST /analyze/acc_9921 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
        .ok()?;
    socket.flush().ok()?;
    let mut answer = String::new();
    match socket.read_to_string(&mut answer) {
        Ok(_) => Some(answer),
        Err(_) => {
            let _ = child.kill();
            None
        }
    }
}

fn read_exactly(from: &mut impl Read, into: &mut [u8]) -> Result<(), String> {
    let mut at = 0;
    while at < into.len() {
        match from.read(&mut into[at..]) {
            Ok(0) => return Err("it stopped first".to_string()),
            Ok(n) => at += n,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn connect_retrying() -> Option<std::net::TcpStream> {
    for _ in 0..100 {
        if let Ok(socket) = std::net::TcpStream::connect(("127.0.0.1", PORT)) {
            return Some(socket);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    None
}
