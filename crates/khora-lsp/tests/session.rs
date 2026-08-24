//! A whole session, driven over two buffers.
//!
//! No editor and no subprocess: `serve` is generic over its streams, so a test
//! writes a script of messages in and reads the replies out. That makes every
//! case here deterministic and instant, which is the only way a protocol
//! implementation gets tested at all.

use std::path::Path;

use khora_lsp::{read_message, write_message};
use serde_json::{json, Value};

/// A project on disk, since the server reads the workspace at `initialize`.
struct Workspace {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
}

fn workspace(files: &[(&str, &str)]) -> Workspace {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let root = tmp.path().to_path_buf();
    for (name, text) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a parent directory");
        }
        std::fs::write(&path, text).expect("writing");
    }
    Workspace { _tmp: tmp, root }
}

fn url_of(path: &Path) -> String {
    url::Url::from_file_path(path).expect("a file URL").to_string()
}

/// Runs a script and returns everything the server said.
fn session(messages: &[Value]) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        write_message(&mut input, &serde_json::to_string(message).expect("json"))
            .expect("writing");
    }
    let mut output = Vec::new();
    khora_lsp::serve(&mut input.as_slice(), &mut output).expect("the server should not fail");

    let mut replies = Vec::new();
    let mut reading = output.as_slice();
    while let Some(text) = read_message(&mut reading).expect("reading a reply") {
        replies.push(serde_json::from_str(&text).expect("a reply that is JSON"));
    }
    replies
}

fn initialize(root: &Path) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "rootUri": url_of(root),
            "capabilities": { "general": { "positionEncodings": ["utf-8", "utf-16"] } }
        }
    })
}

fn did_open(path: &Path, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": url_of(path), "languageId": "khora", "version": 1, "text": text
        }}
    })
}

fn did_change(path: &Path, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": url_of(path), "version": 2 },
            "contentChanges": [{ "text": text }]
        }
    })
}

fn exit() -> Value {
    json!({ "jsonrpc": "2.0", "method": "exit" })
}

/// The diagnostics from the last `publishDiagnostics` the server sent.
fn last_diagnostics(replies: &[Value]) -> Vec<Value> {
    replies
        .iter()
        .rfind(|r| {
            r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        })
        .and_then(|r| r.pointer("/params/diagnostics"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// --- the handshake ---------------------------------------------------------

#[test]
fn initialize_answers_with_what_the_server_can_do() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);

    let result = replies[0].pointer("/result").expect("a result");
    assert_eq!(result.pointer("/serverInfo/name").and_then(Value::as_str), Some("khora-lsp"));
    assert!(result.pointer("/capabilities/hoverProvider").is_some(), "{result}");
    // Full sync: `TextDocumentSyncKind::FULL` is 1.
    assert_eq!(result.pointer("/capabilities/textDocumentSync"), Some(&json!(1)));
}

/// The client offered UTF-8 first, so the server should take it and say so —
/// a server that silently keeps UTF-16 while the client counts bytes puts
/// every diagnostic in the wrong place on any line with an accent in it.
#[test]
fn the_negotiated_encoding_is_echoed_back() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    assert_eq!(
        replies[0].pointer("/result/capabilities/positionEncoding"),
        Some(&json!("utf-8"))
    );
}

/// A client that says nothing gets the protocol's default rather than our
/// preference.
#[test]
fn a_client_that_offers_nothing_gets_utf16() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "rootUri": url_of(&w.root), "capabilities": {} }
        }),
        exit(),
    ]);
    assert_eq!(
        replies[0].pointer("/result/capabilities/positionEncoding"),
        Some(&json!("utf-16"))
    );
}

/// A request nobody implements must still be answered, or the client waits
/// forever for a reply that is not coming.
#[test]
fn an_unimplemented_request_gets_an_error_rather_than_silence() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[
        initialize(&w.root),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "textDocument/rename", "params": {} }),
        exit(),
    ]);
    let reply = replies.iter().find(|r| r.get("id") == Some(&json!(7))).expect("a reply");
    assert_eq!(reply.pointer("/error/code"), Some(&json!(-32601)), "{reply}");
}

/// An unimplemented *notification* is ignored in silence, which is what the
/// protocol says and the opposite of the rule for requests.
#[test]
fn an_unimplemented_notification_is_ignored() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[
        initialize(&w.root),
        json!({ "jsonrpc": "2.0", "method": "$/setTrace", "params": { "value": "off" } }),
        exit(),
    ]);
    assert_eq!(replies.len(), 1, "only the initialize reply: {replies:?}");
}

// --- diagnostics -----------------------------------------------------------

#[test]
fn opening_a_clean_file_publishes_nothing_to_fix() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies =
        session(&[initialize(&w.root), did_open(&path, "module app::main;\n"), exit()]);
    assert!(last_diagnostics(&replies).is_empty(), "{replies:?}");
}

#[test]
fn a_syntax_error_is_published() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f( -> Int { 1 }\n"),
        exit(),
    ]);
    let found = last_diagnostics(&replies);
    assert!(!found.is_empty(), "a broken file should say so: {replies:?}");
    assert_eq!(found[0].get("severity"), Some(&json!(1)), "an error: {found:?}");
}

/// Type errors invented on top of a syntax error are noise, and the
/// command-line checker takes the same view. One broken file, one story.
#[test]
fn a_syntax_error_suppresses_the_type_errors() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f( -> Int { nope }\n"),
        exit(),
    ]);
    let found = last_diagnostics(&replies);
    assert!(
        found.iter().all(|d| d.get("message").and_then(Value::as_str).is_some_and(|m| !m.contains("nope"))),
        "{found:?}"
    );
}

#[test]
fn a_type_error_is_published() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f() -> Int { \"text\" }\n"),
        exit(),
    ]);
    assert!(!last_diagnostics(&replies).is_empty(), "{replies:?}");
}

/// A lint is a warning, and a warning is severity 2. It arriving as an error
/// would make a clean build look broken.
#[test]
fn a_lint_arrives_as_a_warning_with_its_name() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f(x: Int) -> Int { x + 1; x }\n"),
        exit(),
    ]);
    let found = last_diagnostics(&replies);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].get("severity"), Some(&json!(2)), "a warning: {found:?}");
    assert_eq!(found[0].get("code"), Some(&json!("dangling-expression")), "{found:?}");
}

/// The manifest's `[lints]` reaches the editor too, not just the CLI.
#[test]
fn the_manifest_decides_how_loud_a_lint_is() {
    let w = workspace(&[
        ("khora.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[lints]\ndangling-expression = \"allow\"\n"),
        ("src/main.kh", "module app::main;\n"),
    ]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f(x: Int) -> Int { x + 1; x }\n"),
        exit(),
    ]);
    assert!(last_diagnostics(&replies).is_empty(), "`allow` should be silent: {replies:?}");
}

/// Editing is the whole point: the second set of diagnostics must reflect the
/// second version of the file, not the first.
#[test]
fn an_edit_republishes() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f() -> Int { \"text\" }\n"),
        did_change(&path, "module app::main;\nfn f() -> Int { 1 }\n"),
        exit(),
    ]);

    let published: Vec<_> = replies
        .iter()
        .filter(|r| {
            r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        })
        .collect();
    assert_eq!(published.len(), 2, "one per edit: {replies:?}");
    assert!(
        !published[0].pointer("/params/diagnostics").unwrap().as_array().unwrap().is_empty(),
        "the broken version"
    );
    assert!(
        published[1].pointer("/params/diagnostics").unwrap().as_array().unwrap().is_empty(),
        "the fixed version must clear them: {published:?}"
    );
}

/// A file the workspace scan never saw — a scratch buffer — still gets
/// answers rather than silence.
#[test]
fn a_file_outside_the_workspace_scan_still_gets_diagnostics() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let scratch = w.root.join("src/scratch.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&scratch, "module scratch;\nfn f() -> Int { \"text\" }\n"),
        exit(),
    ]);
    assert!(!last_diagnostics(&replies).is_empty(), "{replies:?}");
}

/// Cross-file resolution: `main` uses a name `library` exports, and neither
/// file is wrong. A server that loaded one file at a time would report an
/// unresolved name here.
#[test]
fn a_name_from_another_file_resolves() {
    let w = workspace(&[
        ("src/library.kh", "module app::library;\nexport fn two() -> Int { 2 }\n"),
        ("src/main.kh", "module app::main;\nimport app::library::{two};\nfn f() -> Int { two() }\n"),
    ]);
    let path = w.root.join("src/main.kh");
    let text = std::fs::read_to_string(&path).expect("the file");
    let replies = session(&[initialize(&w.root), did_open(&path, &text), exit()]);
    assert!(last_diagnostics(&replies).is_empty(), "{replies:?}");
}

// --- hover -----------------------------------------------------------------

fn hover(path: &Path, line: u32, character: u32) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 42, "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character }
        }
    })
}

#[test]
fn hovering_a_binding_shows_its_type() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    //                                        0         1
    //                                        0123456789012345
    let text = "module app::main;\nfn f(x: Int) -> Int { x }\n";
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, text),
        hover(&path, 1, 22),
        exit(),
    ]);

    let reply = replies.iter().find(|r| r.get("id") == Some(&json!(42))).expect("a reply");
    let shown = reply
        .pointer("/result/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(shown.contains("Int"), "hovering `x` should say Int, said {reply}");
}

/// Nothing under the cursor is `null`, not an error. An editor asks about
/// every position the pointer passes over.
#[test]
fn hovering_nothing_is_a_null_result() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&path, "module app::main;\nfn f() -> Int { 1 }\n"),
        hover(&path, 0, 3),
        exit(),
    ]);
    let reply = replies.iter().find(|r| r.get("id") == Some(&json!(42))).expect("a reply");
    assert_eq!(reply.get("result"), Some(&Value::Null), "{reply}");
}

// --- lifecycle -------------------------------------------------------------

#[test]
fn shutdown_is_answered_and_exit_stops_the_loop() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[
        initialize(&w.root),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
        exit(),
        // Never read: the loop stops at `exit`.
        json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown" }),
    ]);
    assert!(replies.iter().any(|r| r.get("id") == Some(&json!(2))), "{replies:?}");
    assert!(
        !replies.iter().any(|r| r.get("id") == Some(&json!(3))),
        "nothing after `exit` should be answered: {replies:?}"
    );
}
