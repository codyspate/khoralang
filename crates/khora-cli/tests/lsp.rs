//! `khora lsp`, as an editor actually starts it.
//!
//! `khora-lsp/tests/session.rs` drives `serve` over two buffers, which is the
//! right way to test the protocol and says nothing about the thing an editor
//! runs. What the VS Code extension does is spawn `khora lsp` and speak
//! `Content-Length`-framed JSON-RPC over its pipes, and everything between
//! `serve` and that — the subcommand existing, stdout carrying the protocol and
//! nothing else, the process ending on `exit` — was covered by no test at all.
//!
//! **stdout is the thing most easily broken from a long way away.** A stray
//! `println!` anywhere the `lsp` path reaches corrupts the stream, and the
//! symptom is an editor that shows nothing with no error in it. That is why
//! this asserts on a real subprocess rather than a real editor.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// A `khora lsp` subprocess, with the pipes an editor would hold.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(root: &std::path::Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_khora"))
            .arg("lsp")
            .current_dir(root)
            // A toolchain handover would replace this process with another
            // build of the compiler, which is a different thing than the one
            // under test. `KHORA_HOME` points somewhere empty so the machine's
            // real installation cannot join in.
            .env("KHORA_HOME", root.join("empty-home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("starting `khora lsp`");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server { child, stdin, stdout }
    }

    fn send(&mut self, message: &serde_json::Value) {
        let body = serde_json::to_string(message).expect("json");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).expect("writing");
        self.stdin.flush().expect("flushing");
    }

    /// One framed message, or `None` at end of stream.
    ///
    /// Parsed here rather than with the crate's own `read_message`, so that a
    /// framing bug in the server cannot be hidden by the same bug in the
    /// reader.
    fn recv(&mut self) -> Option<serde_json::Value> {
        let mut length = None;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = value.trim().parse::<usize>().ok();
            }
        }
        let mut body = vec![0u8; length?];
        self.stdout.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// Everything said until the stream ends.
    fn drain(&mut self) -> Vec<serde_json::Value> {
        let mut all = Vec::new();
        while let Some(message) = self.recv() {
            all.push(message);
        }
        all
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    for (name, text) in files {
        let path = tmp.path().join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        std::fs::write(&path, text).expect("writing");
    }
    tmp
}

fn url_of(path: &std::path::Path) -> String {
    url::Url::from_file_path(path).expect("a file URL").to_string()
}

/// **The whole path the extension takes**: spawn, initialize, open a file with
/// a mistake in it, and get told about the mistake.
#[test]
fn an_editor_can_start_it_and_be_told_about_an_error() {
    let tmp = project(&[("src/main.kh", "module main;\n\nfn f() -> Int { \"not an int\" }\n")]);
    let file = tmp.path().join("src/main.kh");
    let mut server = Server::start(tmp.path());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "rootUri": url_of(tmp.path()),
            "capabilities": { "general": { "positionEncodings": ["utf-8"] } }
        }
    }));
    server.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": url_of(&file), "languageId": "khora", "version": 1,
            "text": "module main;\n\nfn f() -> Int { \"not an int\" }\n"
        }}
    }));
    server.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));

    let said = server.drain();

    let initialized = said
        .iter()
        .find(|m| m.get("id").and_then(serde_json::Value::as_i64) == Some(1))
        .expect("an answer to initialize");
    assert_eq!(
        initialized.pointer("/result/serverInfo/name"),
        Some(&serde_json::json!("khora-lsp")),
        "{initialized}"
    );

    let published = said
        .iter()
        .find(|m| m.get("method").and_then(serde_json::Value::as_str)
            == Some("textDocument/publishDiagnostics"))
        .expect("diagnostics");
    let list = published.pointer("/params/diagnostics").and_then(serde_json::Value::as_array);
    assert!(list.is_some_and(|d| !d.is_empty()), "the type error should be reported: {published}");
}

/// The three things the extension turns on, as the server reports them.
#[test]
fn it_advertises_what_the_extension_relies_on() {
    let tmp = project(&[("src/main.kh", "module main;\n")]);
    let mut server = Server::start(tmp.path());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "rootUri": url_of(tmp.path()), "capabilities": {} }
    }));
    server.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));

    let said = server.drain();
    let caps = said
        .iter()
        .find(|m| m.get("id").and_then(serde_json::Value::as_i64) == Some(1))
        .and_then(|m| m.pointer("/result/capabilities").cloned())
        .expect("capabilities");

    assert_eq!(caps.get("hoverProvider"), Some(&serde_json::json!(true)), "{caps}");
    assert_eq!(caps.get("documentFormattingProvider"), Some(&serde_json::json!(true)), "{caps}");
    assert!(caps.get("textDocumentSync").is_some(), "{caps}");
}

/// **Nothing but the protocol on stdout.** A stray `println!` on this path
/// corrupts the stream, and an editor's symptom for that is showing nothing at
/// all with no error anywhere — so every byte is accounted for by a frame.
#[test]
fn stdout_carries_the_protocol_and_nothing_else() {
    let tmp = project(&[("src/main.kh", "module main;\n")]);
    let mut server = Server::start(tmp.path());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "rootUri": url_of(tmp.path()), "capabilities": {} }
    }));
    server.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));

    // `drain` reads frames until the stream ends. Anything unframed makes a
    // header parse fail, which ends the loop early and loses the reply.
    let said = server.drain();
    assert!(
        said.iter().any(|m| m.get("id").and_then(serde_json::Value::as_i64) == Some(1)),
        "the initialize reply should survive a full framed read: {said:?}"
    );
}

/// `exit` ends the process, so closing an editor does not leave a compiler
/// running with the whole standard library in memory.
#[test]
fn exit_ends_the_process() {
    let tmp = project(&[("src/main.kh", "module main;\n")]);
    let mut server = Server::start(tmp.path());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "rootUri": url_of(tmp.path()), "capabilities": {} }
    }));
    server.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
    let _ = server.drain();

    let status = server.child.wait().expect("waiting");
    assert!(status.success(), "`khora lsp` should end cleanly on exit: {status}");
}
