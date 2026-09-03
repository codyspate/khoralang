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

/// A reader that does something once, part-way through the script.
///
/// **For the notifications that are about the disk.** A watcher event says a
/// file appeared, and a test of it has to have the file appear *after*
/// `initialize` has read the workspace — otherwise the server already had it
/// and the notification proves nothing. The script is one buffer handed to
/// `serve`, so the only place to put a side effect between two messages is in
/// the reader that hands them over.
struct Interrupting<F: FnMut()> {
    data: Vec<u8>,
    at: usize,
    after: usize,
    done: bool,
    act: F,
}

impl<F: FnMut()> std::io::Read for Interrupting<F> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if !self.done && self.at >= self.after {
            self.done = true;
            (self.act)();
        }
        let left = self.data.len().saturating_sub(self.at);
        let n = left.min(out.len());
        out[..n].copy_from_slice(&self.data[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Runs a script, performing `act` once the first `before` messages have been
/// handed to the server.
fn session_interrupted(messages: &[Value], before: usize, act: impl FnMut()) -> Vec<Value> {
    let mut framed: Vec<Vec<u8>> = Vec::new();
    for message in messages {
        let mut one = Vec::new();
        write_message(&mut one, &serde_json::to_string(message).expect("json")).expect("writing");
        framed.push(one);
    }
    let after: usize = framed.iter().take(before).map(Vec::len).sum();
    let data: Vec<u8> = framed.concat();

    // Wrapped rather than implementing `BufRead` by hand: `serve` reads framed
    // messages, and a buffered reader over the interrupting one still asks it
    // for bytes in the order the script is in, which is what the side effect
    // is timed against.
    let mut input = std::io::BufReader::with_capacity(
        1,
        Interrupting { data, at: 0, after, done: false, act },
    );
    let mut output = Vec::new();
    khora_lsp::serve(&mut input, &mut output).expect("the server should not fail");

    let mut replies = Vec::new();
    let mut reading = output.as_slice();
    while let Some(text) = read_message(&mut reading).expect("reading a reply") {
        replies.push(serde_json::from_str(&text).expect("a reply that is JSON"));
    }
    replies
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
///
/// **This test has now moved twice**, and the pattern is the point: it named
/// `rename`, then `codeAction`, and each time the feature landed it began
/// asserting that an implemented request was unimplemented.
///
/// It names `foldingRange` now, which is on no roadmap list. If that ever
/// changes, move it again rather than deleting it — what it holds is that an
/// unanswered *request* hangs the client, which stays true of whatever is
/// unimplemented next.
#[test]
fn an_unimplemented_request_gets_an_error_rather_than_silence() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let replies = session(&[
        initialize(&w.root),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "textDocument/foldingRange", "params": {} }),
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
        ("src/library.kh", "module app::library;\npub fn two() -> Int { 2 }\n"),
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

// --- formatting -------------------------------------------------------------

fn formatting(path: &Path, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/formatting",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "options": { "tabSize": 4, "insertSpaces": true }
        }
    })
}

/// The result of the request with this id.
fn result_of(replies: &[Value], id: i64) -> Value {
    replies
        .iter()
        .find(|m| m.get("id").and_then(Value::as_i64) == Some(id))
        .and_then(|m| m.get("result").cloned())
        .unwrap_or(Value::Null)
}

#[test]
fn the_server_offers_to_format() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = result_of(&replies, 1);
    assert_eq!(
        caps.pointer("/capabilities/documentFormattingProvider"),
        Some(&json!(true)),
        "{caps}"
    );
}

/// **The whole file, as one edit.** A minimal diff is what preserves a cursor,
/// and computing one honestly needs a tree diff — so this returns the document
/// and lets the editor keep the cursor, which VS Code does well.
#[test]
fn formatting_returns_the_whole_file() {
    let path_text = "module main;\n\n\n\nfn f( ) ->Int{1}\n";
    let w = workspace(&[("src/main.kh", path_text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, path_text), formatting(&file, 2), exit()]);

    let edits = result_of(&replies, 2);
    let edits = edits.as_array().expect("a list of edits");
    assert_eq!(edits.len(), 1, "one edit over the whole document: {edits:?}");

    let edit = &edits[0];
    assert_eq!(edit.pointer("/range/start/line"), Some(&json!(0)));
    assert_eq!(edit.pointer("/range/start/character"), Some(&json!(0)));

    let new_text = edit.get("newText").and_then(Value::as_str).expect("newText");
    assert_eq!(new_text, khora_fmt::format(path_text).expect("it parses"));
    assert!(new_text.contains("fn f() -> Int"), "{new_text}");
}

/// A file already in canonical form produces nothing, so saving an untouched
/// file records no change and no undo step.
#[test]
fn formatting_an_already_formatted_file_is_no_edits() {
    let text = khora_fmt::format("module main;\n\nfn f() -> Int { 1 }\n").expect("it parses");
    let w = workspace(&[("src/main.kh", text.as_str())]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, &text), formatting(&file, 2), exit()]);
    assert_eq!(result_of(&replies, 2), json!([]), "nothing to do");
}

/// **A file that does not parse is left exactly as it is**, which is the same
/// decision `khora fmt` makes. Format-on-save runs while somebody is mid-edit,
/// when a brace is unbalanced more often than not, and a formatter that
/// rewrites a half-written file is one people turn off.
#[test]
fn formatting_a_broken_file_changes_nothing() {
    let broken = "module main;\n\nfn f( -> Int { 1\n";
    let w = workspace(&[("src/main.kh", broken)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, broken), formatting(&file, 2), exit()]);
    assert_eq!(result_of(&replies, 2), Value::Null, "no edits for a file that does not parse");

    // And the reason is on screen already, rather than only in this refusal.
    assert!(!last_diagnostics(&replies).is_empty(), "the parse error should be reported");
}

/// It formats what the client last *sent*, not what is on disk — which is the
/// whole point, since format-on-save runs against an unsaved buffer.
#[test]
fn formatting_uses_the_edited_buffer_rather_than_the_file() {
    let on_disk = "module main;\n\nfn f() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", on_disk)]);
    let file = w.root.join("src/main.kh");
    let edited = "module main;\n\nfn f( ) ->Int{2}\n";

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, on_disk),
        did_change(&file, edited),
        formatting(&file, 2),
        exit(),
    ]);

    let edits = result_of(&replies, 2);
    let new_text = edits
        .as_array()
        .and_then(|e| e.first())
        .and_then(|e| e.get("newText"))
        .and_then(Value::as_str)
        .expect("an edit");
    assert!(new_text.contains('2'), "the buffer, not the file: {new_text}");
}

// --- go to definition -------------------------------------------------------

fn definition(path: &Path, line: u32, character: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character }
        }
    })
}

#[test]
fn the_server_offers_to_go_to_definitions() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = result_of(&replies, 1);
    assert_eq!(caps.pointer("/capabilities/definitionProvider"), Some(&json!(true)), "{caps}");
}

/// **The case the feature exists for: another file.** A cursor on `helper::add`
/// in `main.kh` lands on `fn add` in `helper.kh`, and the range is read against
/// *that* file's text — a byte offset measured against the wrong source lands
/// somewhere plausible and wrong.
#[test]
fn a_path_into_another_module_finds_it_there() {
    let helper = "module helper;\n\npub fn add(a: Int, b: Int) -> Int { a + b }\n";
    let main = "module main;\n\nimport helper::{add};\n\nfn go() -> Int { helper::add(1, 2) }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    // Line 4, on the `add` of `helper::add`.
    let column = main.lines().nth(4).expect("a fifth line").find("add").expect("the call") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 4, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let uri = found.get("uri").and_then(Value::as_str).unwrap_or_default();
    assert!(uri.ends_with("helper.kh"), "it should cross the file boundary: {found}");

    // `pub fn add` is on line 2 of `helper.kh`, and the range must be read
    // against helper's text rather than main's.
    assert_eq!(found.pointer("/range/start/line"), Some(&json!(2)), "{found}");
}

/// A type is a declaration like any other.
#[test]
fn a_type_path_finds_its_declaration() {
    let shapes = "module shapes;\n\npub type Point = { x: Int, y: Int };\n";
    let main = "module main;\n\nimport shapes::{Point};\n\nfn go(p: shapes::Point) -> Int { p.x }\n";
    let w = workspace(&[("src/shapes.kh", shapes), ("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    let column = main.lines().nth(4).expect("a fifth line").find("Point").expect("it") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 4, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert!(
        found.get("uri").and_then(Value::as_str).is_some_and(|u| u.ends_with("shapes.kh")),
        "{found}"
    );
    assert_eq!(found.pointer("/range/start/line"), Some(&json!(2)), "{found}");
}

/// **A constructor lands on the type that declares it.** `khora_hir::Variant`
/// records a name and a type and no range, so there is nothing narrower to jump
/// to — and the type is where a reader wants to end up anyway.
#[test]
fn a_constructor_finds_the_type_that_declares_it() {
    let level = "module level;\n\npub type Risk =\n  | Low\n  | High;\n";
    let main = "module main;\n\nimport level::{Risk};\n\nfn go() -> Risk { level::Risk::Low }\n";
    let w = workspace(&[("src/level.kh", level), ("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    let column = main.lines().nth(4).expect("a fifth line").find("Low").expect("it") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 4, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert!(
        found.get("uri").and_then(Value::as_str).is_some_and(|u| u.ends_with("level.kh")),
        "{found}"
    );
}

/// Nothing under the cursor is a null result rather than a guess.
#[test]
fn asking_about_whitespace_finds_nothing() {
    let main = "module main;\n\nfn go() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 1, 0, 2),
        exit(),
    ]);
    assert_eq!(result_of(&replies, 2), Value::Null);
}

/// A name that does not resolve produces nothing, rather than the wrong thing.
#[test]
fn an_unresolved_name_finds_nothing() {
    let main = "module main;\n\nfn go() -> Int { nowhere::missing(1) }\n";
    let w = workspace(&[("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    let column = main.lines().nth(2).expect("a third line").find("missing").expect("it") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 2, column + 1, 2),
        exit(),
    ]);
    assert_eq!(result_of(&replies, 2), Value::Null);
}

// --- references, rename, symbols --------------------------------------------

fn references(path: &Path, line: u32, character: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/references",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        }
    })
}

fn prepare_rename(path: &Path, line: u32, character: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/prepareRename",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character }
        }
    })
}

fn rename(path: &Path, line: u32, character: u32, to: &str, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/rename",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character },
            "newName": to
        }
    })
}

/// The error for the request with this id, if it was answered with one.
fn error_of(replies: &[Value], id: i64) -> Option<String> {
    replies
        .iter()
        .find(|m| m.get("id").and_then(Value::as_i64) == Some(id))
        .and_then(|m| m.pointer("/error/message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[test]
fn the_server_offers_the_navigation_it_can_do() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = result_of(&replies, 1);
    assert_eq!(caps.pointer("/capabilities/referencesProvider"), Some(&json!(true)), "{caps}");
    assert_eq!(caps.pointer("/capabilities/documentSymbolProvider"), Some(&json!(true)), "{caps}");
    assert_eq!(caps.pointer("/capabilities/workspaceSymbolProvider"), Some(&json!(true)), "{caps}");
    assert_eq!(
        caps.pointer("/capabilities/renameProvider/prepareProvider"),
        Some(&json!(true)),
        "{caps}"
    );
}

// --- locals ---

const COUNTER: &str =
    "module main;\n\nfn go() -> Int {\n  let total = 1;\n  let other = 2;\n  total + total + other\n}\n";

/// A local declaration and both its uses, and nothing belonging to the binding
/// beside it.
#[test]
fn references_to_a_local_are_exactly_its_own() {
    let w = workspace(&[("src/main.kh", COUNTER)]);
    let file = w.root.join("src/main.kh");
    // Line 5 is `  total + total + other`; column 3 is inside the first name.
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, COUNTER),
        references(&file, 5, 3, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let list = found.as_array().expect("a list");
    // The `let total` and its two uses. `other` must not be in there.
    assert_eq!(list.len(), 3, "{found}");
    let lines: Vec<i64> =
        list.iter().filter_map(|l| l.pointer("/range/start/line")?.as_i64()).collect();
    assert_eq!(lines, vec![3, 5, 5], "the binding on line 3, two uses on line 5: {found}");
}

/// **Rename edits the binding as well as the uses.** One that changed the uses
/// and left the `let` alone would produce a program that does not compile,
/// which is the failure worth a test of its own.
#[test]
fn renaming_a_local_edits_the_binding_too() {
    let w = workspace(&[("src/main.kh", COUNTER)]);
    let file = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, COUNTER),
        rename(&file, 5, 3, "sum", 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let changes = found.pointer("/changes").and_then(Value::as_object).expect("changes");
    let edits = changes.values().next().and_then(Value::as_array).expect("edits");
    assert_eq!(edits.len(), 3, "the binding and both uses: {found}");
    assert!(edits.iter().all(|e| e.get("newText") == Some(&json!("sum"))), "{found}");
    assert!(
        edits.iter().any(|e| e.pointer("/range/start/line") == Some(&json!(3))),
        "the `let` on line 3 must be edited: {found}"
    );
}

/// The cursor on the `let` itself is as natural a place to ask from as a use.
#[test]
fn a_rename_can_be_asked_for_from_the_binding() {
    let w = workspace(&[("src/main.kh", COUNTER)]);
    let file = w.root.join("src/main.kh");
    // Line 3 is `  let total = 1;`.
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, COUNTER),
        prepare_rename(&file, 3, 7, 2),
        exit(),
    ]);
    let answer = result_of(&replies, 2);
    assert_eq!(answer.get("placeholder"), Some(&json!("total")), "{answer}");
}

/// **A declaration is refused with a reason, not with silence.** `null` from
/// `prepareRename` makes an editor say "cannot be renamed" and explain nothing.
#[test]
fn renaming_a_declaration_is_refused_and_says_why() {
    let helper = "module helper;\n\npub fn add(a: Int, b: Int) -> Int { a + b }\n";
    let main = "module main;\n\nimport helper::{add};\n\nfn go() -> Int { helper::add(1, 2) }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");
    let column = main.lines().nth(4).expect("a line").find("add").expect("it") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        prepare_rename(&file, 4, column + 1, 2),
        exit(),
    ]);

    let why = error_of(&replies, 2).expect("a refusal with a reason");
    assert!(why.contains("not supported yet"), "{why}");
    assert!(why.contains("local binding works"), "it should say what does work: {why}");
}

// --- items ---

/// **References to an item are found by resolution, not by text**, so they
/// cross files and cannot match a different declaration that happens to share
/// a name.
#[test]
fn references_to_an_item_cross_files_and_ignore_a_namesake() {
    let helper = "module helper;\n\npub fn add(a: Int, b: Int) -> Int { a + b }\n";
    // A second `add`, in another module, which must not be swept up.
    let other = "module other;\n\npub fn add(a: Int) -> Int { a }\n";
    let main =
        "module main;\n\nfn go() -> Int { helper::add(1, 2) + helper::add(3, 4) + other::add(5) }\n";
    let w =
        workspace(&[("src/helper.kh", helper), ("src/other.kh", other), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");
    let column = main.lines().nth(2).expect("a line").find("helper::add").expect("it") as u32 + 8;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        references(&file, 2, column, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let list = found.as_array().expect("a list");

    let in_other = list
        .iter()
        .filter(|l| l.get("uri").and_then(Value::as_str).is_some_and(|u| u.ends_with("other.kh")))
        .count();
    assert_eq!(in_other, 0, "a namesake in another module is not a reference: {found}");

    let in_main = list
        .iter()
        .filter(|l| l.get("uri").and_then(Value::as_str).is_some_and(|u| u.ends_with("main.kh")))
        .count();
    assert_eq!(in_main, 2, "both calls: {found}");

    assert!(
        list.iter().any(|l| l
            .get("uri")
            .and_then(Value::as_str)
            .is_some_and(|u| u.ends_with("helper.kh"))),
        "the declaration: {found}"
    );
}

// --- symbols ---

#[test]
fn the_outline_lists_what_a_file_declares() {
    let text =
        "module app;\n\npub type Point = { x: Int };\n\npub fn go() -> Int { 1 }\n\nconst LIMIT: Int = 3;\n";
    let w = workspace(&[("src/app.kh", text)]);
    let file = w.root.join("src/app.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": url_of(&file) } }
        }),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let names: Vec<&str> =
        found.as_array().expect("a list").iter().filter_map(|s| s.get("name")?.as_str()).collect();
    assert_eq!(names, vec!["Point", "go", "LIMIT"], "in declaration order: {found}");
}

/// Ctrl+T across the workspace, with the module named so three `parse`s can be
/// told apart.
#[test]
fn a_workspace_search_finds_by_substring_and_says_which_module() {
    let one = "module one;\n\npub fn parse_header() -> Int { 1 }\n";
    let w = workspace(&[("src/one.kh", one), ("src/two.kh", "module two;\n\npub fn unrelated() -> Int { 2 }\n")]);
    let file = w.root.join("src/one.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, one),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "workspace/symbol",
            "params": { "query": "parse" }
        }),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let list = found.as_array().expect("a list");
    assert!(list.iter().any(|s| s.get("name") == Some(&json!("parse_header"))), "{found}");
    assert!(
        !list.iter().any(|s| s.get("name") == Some(&json!("unrelated"))),
        "the substring should exclude it: {found}"
    );
    let entry = list.iter().find(|s| s.get("name") == Some(&json!("parse_header"))).expect("it");
    assert_eq!(entry.get("containerName"), Some(&json!("one")), "{entry}");
}

/// **The gap 14.3 left, closed.** A cursor on a use of a local lands on its
/// binding, in the same file.
#[test]
fn a_local_finds_its_binding() {
    let w = workspace(&[("src/main.kh", COUNTER)]);
    let file = w.root.join("src/main.kh");
    // Line 5 is `  total + total + other`.
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, COUNTER),
        definition(&file, 5, 3, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert_eq!(found.pointer("/range/start/line"), Some(&json!(3)), "the `let`: {found}");
    assert!(
        found.get("uri").and_then(Value::as_str).is_some_and(|u| u.ends_with("main.kh")),
        "{found}"
    );
}

/// **A type in a signature is not the parameter it annotates**, which is the
/// regression that made the lookup order load-bearing.
///
/// `khora_hir::Local::range` for a parameter covers `p: shapes::Point` entire,
/// so a binding check running before paths answers "the parameter `p`" for a
/// cursor on `Point` — and go-to-definition on a type in a signature lands
/// three characters to the left instead of in another file.
#[test]
fn a_type_in_a_signature_beats_the_parameter_it_annotates() {
    let shapes = "module shapes;\n\npub type Point = { x: Int };\n";
    let main = "module main;\n\nimport shapes::{Point};\n\nfn go(p: shapes::Point) -> Int { p.x }\n";
    let w = workspace(&[("src/shapes.kh", shapes), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");
    let column = main.lines().nth(4).expect("a line").find("shapes::Point").expect("it") as u32 + 9;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        definition(&file, 4, column, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert!(
        found.get("uri").and_then(Value::as_str).is_some_and(|u| u.ends_with("shapes.kh")),
        "the type, in the other file, not the parameter: {found}"
    );
}

/// And the parameter itself still answers, when that is what the cursor is on.
#[test]
fn a_parameter_finds_itself() {
    let main = "module main;\n\nfn go(count: Int) -> Int { count + 1 }\n";
    let w = workspace(&[("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");
    let column = main.lines().nth(2).expect("a line").rfind("count").expect("the use") as u32 + 1;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        definition(&file, 2, column, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert_eq!(found.pointer("/range/start/line"), Some(&json!(2)), "{found}");
    let start = found.pointer("/range/start/character").and_then(Value::as_i64).expect("a column");
    assert_eq!(start, 6, "the parameter, at `count` in the signature: {found}");
}

// --- completion -------------------------------------------------------------

fn completion(path: &Path, line: u32, character: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character }
        }
    })
}

/// The labels offered, for an assertion that reads.
fn labels(replies: &[Value], id: i64) -> Vec<String> {
    result_of(replies, id)
        .as_array()
        .map(|items| {
            items.iter().filter_map(|i| i.get("label")?.as_str().map(str::to_string)).collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_server_offers_completion_and_says_what_triggers_it() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = result_of(&replies, 1);
    let triggers = caps
        .pointer("/capabilities/completionProvider/triggerCharacters")
        .and_then(Value::as_array)
        .expect("trigger characters");
    assert!(triggers.contains(&json!(".")), "{caps}");
    assert!(triggers.contains(&json!(":")), "{caps}");
}

/// **The case that has to work in code that does not parse.** `s.` is a syntax
/// error and a request for the methods of `s` at the same moment.
#[test]
fn after_a_dot_the_methods_of_the_receiver() {
    // The import matters and is not decoration: `import_inherent` is what
    // brings a module's methods into a file's type map, so a file importing
    // nothing genuinely has no `String` methods to offer. Real code imports.
    let text =
        "module main;\n\nimport std::core::{print};\n\nfn go() -> Int {\n  let s = \"x\";\n  s.\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        // Line 6 is `  s.`; the cursor sits after the dot.
        completion(&file, 6, 4, 2),
        exit(),
    ]);

    let offered = labels(&replies, 2);
    assert!(!offered.is_empty(), "a broken line should still offer something");
    assert!(
        offered.iter().any(|l| l == "byte_length"),
        "String methods, from the checker rather than from the words on screen: {offered:?}"
    );
    // And nothing belonging to a different type.
    assert!(
        !offered.iter().any(|l| l == "push"),
        "an Array method has no business here: {offered:?}"
    );
}

/// After `Type::`, that type's constructors.
#[test]
fn after_a_type_and_colons_the_constructors() {
    let level = "module level;\n\npub type Risk =\n  | Low\n  | High;\n";
    let main = "module main;\n\nimport level::{Risk};\n\nfn go() -> Int {\n  Risk::\n}\n";
    let w = workspace(&[("src/level.kh", level), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        completion(&file, 5, 8, 2),
        exit(),
    ]);

    let offered = labels(&replies, 2);
    assert!(offered.iter().any(|l| l == "Low"), "{offered:?}");
    assert!(offered.iter().any(|l| l == "High"), "{offered:?}");
}

/// Inside an import list, what the module actually exports — and nothing it
/// keeps to itself.
#[test]
fn inside_an_import_list_the_modules_exports() {
    let helper =
        "module helper;\n\npub fn shown() -> Int { 1 }\n\nfn hidden() -> Int { 2 }\n";
    let main = "module main;\n\nimport helper::{};\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        // Just after the `{`.
        completion(&file, 2, 16, 2),
        exit(),
    ]);

    let offered = labels(&replies, 2);
    assert!(offered.iter().any(|l| l == "shown"), "{offered:?}");
    assert!(
        !offered.iter().any(|l| l == "hidden"),
        "a private declaration is not an export: {offered:?}"
    );
}

/// Otherwise: the locals of the body, and what the file declares.
#[test]
fn elsewhere_the_names_in_scope() {
    let text =
        "module main;\n\nfn helper() -> Int { 1 }\n\nfn go() -> Int {\n  let total = 1;\n  t\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        completion(&file, 6, 3, 2),
        exit(),
    ]);

    let offered = labels(&replies, 2);
    assert!(offered.iter().any(|l| l == "total"), "the local: {offered:?}");
    assert!(offered.iter().any(|l| l == "helper"), "a sibling function: {offered:?}");
    assert!(offered.iter().any(|l| l == "go"), "and this one: {offered:?}");
}

/// No prefix filtering here: an editor does that, and doing it twice means two
/// answers that disagree about which is best.
#[test]
fn nothing_is_filtered_by_what_has_been_typed() {
    let text = "module main;\n\nfn alpha() -> Int { 1 }\n\nfn go() -> Int {\n  zzz\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        completion(&file, 5, 5, 2),
        exit(),
    ]);
    assert!(
        labels(&replies, 2).iter().any(|l| l == "alpha"),
        "the editor filters, not the server"
    );
}

// --- semantic tokens --------------------------------------------------------

fn semantic_tokens(path: &Path, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/semanticTokens/full",
        "params": { "textDocument": { "uri": url_of(path) } }
    })
}

/// One decoded token: absolute line and column, and its type index.
#[derive(Debug, PartialEq, Eq)]
struct Decoded {
    line: u32,
    column: u32,
    length: u32,
    kind: u32,
    modifiers: u32,
}

/// Undoes the relative encoding, which is what an editor does.
///
/// Decoding rather than asserting on raw integers, because the raw form is
/// unreadable and the bug worth catching — a `deltaStart` not reset at a new
/// line — is invisible in it and obvious here.
fn decode(replies: &[Value], id: i64) -> Vec<Decoded> {
    let data: Vec<u32> = result_of(replies, id)
        .pointer("/data")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|n| n.as_u64().map(|n| n as u32)).collect())
        .unwrap_or_default();

    let mut out = Vec::new();
    let (mut line, mut column) = (0u32, 0u32);
    for chunk in data.chunks(5) {
        let [delta_line, delta_start, length, kind, modifiers] = chunk else { continue };
        if *delta_line > 0 {
            line += delta_line;
            column = *delta_start;
        } else {
            column += delta_start;
        }
        out.push(Decoded {
            line,
            column,
            length: *length,
            kind: *kind,
            modifiers: *modifiers,
        });
    }
    out
}

/// The legend's order is the wire encoding, so a token type is only meaningful
/// against the legend the same reply declared.
fn legend(replies: &[Value]) -> Vec<String> {
    result_of(replies, 1)
        .pointer("/capabilities/semanticTokensProvider/legend/tokenTypes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[test]
fn the_server_declares_a_legend() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let types = legend(&replies);
    assert!(types.contains(&"variable".to_string()), "{types:?}");
    assert!(types.contains(&"parameter".to_string()), "{types:?}");
    assert!(types.contains(&"method".to_string()), "{types:?}");
    assert!(types.contains(&"property".to_string()), "{types:?}");
    assert_eq!(
        result_of(&replies, 1).pointer("/capabilities/semanticTokensProvider/full"),
        Some(&json!(true))
    );
}

/// **The distinction a regular expression cannot make**: a parameter, a local,
/// and a function are three different things with the same shape.
#[test]
fn a_parameter_a_local_and_a_function_are_told_apart() {
    let text = "module main;\n\nfn helper() -> Int { 1 }\n\nfn go(count: Int) -> Int {\n  let total = count;\n  total\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, text), semantic_tokens(&file, 2), exit()]);

    let types = legend(&replies);
    let index = |name: &str| types.iter().position(|t| t == name).expect("in the legend") as u32;
    let found = decode(&replies, 2);
    assert!(!found.is_empty(), "nothing was classified");

    // Line 4 is `fn go(count: Int) -> Int {` — `count` at column 6 is a
    // parameter, and it is *declared* there.
    let parameter = found
        .iter()
        .find(|t| t.line == 4 && t.column == 6)
        .expect("the parameter declaration");
    assert_eq!(parameter.kind, index("parameter"), "{parameter:?}");
    assert_eq!(parameter.modifiers, 1, "declaration: {parameter:?}");

    // Line 5 is `  let total = count;` — `total` is a local, `count` a use of
    // the parameter, and they must not be the same colour.
    let local = found.iter().find(|t| t.line == 5 && t.column == 6).expect("the local");
    assert_eq!(local.kind, index("variable"), "{local:?}");
    let use_of_parameter =
        found.iter().find(|t| t.line == 5 && t.column == 14).expect("the use");
    assert_eq!(use_of_parameter.kind, index("parameter"), "{use_of_parameter:?}");
    assert_eq!(use_of_parameter.modifiers, 0, "a use, not a declaration");
}

/// **A field and a method have identical syntax**, and only the compiler knows
/// which is which.
#[test]
fn a_field_and_a_method_are_told_apart() {
    let text = "module main;\n\nimport std::core::{print};\n\npub type Box = { size: Int };\n\nfn go(b: Box, s: String) -> Int {\n  let n = b.size;\n  s.byte_length()\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, text), semantic_tokens(&file, 2), exit()]);

    let types = legend(&replies);
    let index = |name: &str| types.iter().position(|t| t == name).expect("in the legend") as u32;
    let found = decode(&replies, 2);

    // Line 7 is `  let n = b.size;` — `size` is a property.
    let property = found
        .iter()
        .find(|t| t.line == 7 && t.kind == index("property"))
        .expect("the field");
    assert_eq!(property.length, 4, "`size`: {property:?}");

    // Line 8 is `  s.byte_length()` — the same syntax, and a method.
    let method =
        found.iter().find(|t| t.line == 8 && t.kind == index("method")).expect("the method");
    assert_eq!(method.length, 11, "`byte_length`: {method:?}");
}

/// **Every token is sorted and non-overlapping**, which the protocol requires
/// and two independent passes make easy to get wrong.
#[test]
fn tokens_are_sorted_and_do_not_overlap() {
    let text = "module main;\n\nimport std::core::{print};\n\npub type Box = { size: Int };\n\nfn go(b: Box) -> Int {\n  let n = b.size;\n  n\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, text), semantic_tokens(&file, 2), exit()]);

    let found = decode(&replies, 2);
    assert!(found.len() > 3, "not enough to be a real check: {found:?}");
    for pair in found.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert!(
            (b.line, b.column) > (a.line, a.column),
            "out of order: {a:?} then {b:?}"
        );
        if a.line == b.line {
            assert!(a.column + a.length <= b.column, "overlapping: {a:?} then {b:?}");
        }
    }
}

/// A path's leading segments are modules and its last is what it resolves to,
/// which is the other thing a grammar guesses at by capitalisation.
#[test]
fn a_module_path_is_coloured_by_what_it_resolves_to() {
    let helper = "module helper;\n\npub fn add(a: Int) -> Int { a }\n";
    let main = "module main;\n\nfn go() -> Int { helper::add(1) }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, main), semantic_tokens(&file, 2), exit()]);

    let types = legend(&replies);
    let index = |name: &str| types.iter().position(|t| t == name).expect("in the legend") as u32;
    let found = decode(&replies, 2);

    // Line 2 is `fn go() -> Int { helper::add(1) }`.
    let module = found.iter().find(|t| t.line == 2 && t.column == 17).expect("`helper`");
    assert_eq!(module.kind, index("namespace"), "{module:?}");
    let function = found.iter().find(|t| t.line == 2 && t.column == 25).expect("`add`");
    assert_eq!(function.kind, index("function"), "{function:?}");
}

// --- inlay hints for rows ---------------------------------------------------

fn inlay_hints(path: &Path, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/inlayHint",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 9999, "character": 0 }
            }
        }
    })
}

/// The labels, trimmed, in the order they were sent.
fn hint_labels(replies: &[Value], id: i64) -> Vec<String> {
    result_of(replies, id)
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|h| Some(h.get("label")?.as_str()?.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_server_offers_inlay_hints() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    assert_eq!(
        result_of(&replies, 1).pointer("/capabilities/inlayHintProvider"),
        Some(&json!(true))
    );
}

/// **The roadmap's own example.** A call whose callee requires a capability
/// says so at the call, where nothing in the source does.
#[test]
fn a_call_shows_the_capability_it_needs() {
    let text = concat!(
        "module main;\n",
        "\n",
        "pub type Clock = { now: () -> Int };\n",
        "\n",
        "effect Timing {\n",
        "  fn tick() -> Int;\n",
        "}\n",
        "\n",
        "fn charge() -> Int with { timing: Timing } { timing.tick() }\n",
        "\n",
        "fn go() -> Int with { timing: Timing } { charge() }\n",
    );
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, text), inlay_hints(&file, 2), exit()]);

    let labels = hint_labels(&replies, 2);
    assert!(
        labels.iter().any(|l| l.contains("with") && l.contains("Timing")),
        "the call to `charge` needs `timing`, and nothing on that line says so: {labels:?}"
    );
}

/// And what it can raise, which is the other row.
#[test]
fn a_call_shows_what_it_can_raise() {
    let text = concat!(
        "module main;\n",
        "\n",
        "pub type Broken = { why: String };\n",
        "\n",
        "fn risky() -> Int raises Broken { raise Broken { why: \"no\" } }\n",
        "\n",
        "fn go() -> Int raises Broken { risky()! }\n",
    );
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, text), inlay_hints(&file, 2), exit()]);

    let labels = hint_labels(&replies, 2);
    assert!(
        labels.iter().any(|l| l.contains("raises") && l.contains("Broken")),
        "{labels:?}"
    );
}

/// **A call that costs nothing gets no hint**, which is most calls. A hint on
/// every line is a hint nobody reads; the point is that a marked line is one
/// where something crosses a boundary.
#[test]
fn an_ordinary_call_is_not_annotated() {
    let text = "module main;\n\nfn double(n: Int) -> Int { n + n }\n\nfn go() -> Int { double(2) }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&file, text), inlay_hints(&file, 2), exit()]);
    assert!(hint_labels(&replies, 2).is_empty(), "{:?}", hint_labels(&replies, 2));
}

// --- quick fixes ------------------------------------------------------------

/// A code action request carrying the diagnostics the client already has.
fn code_action(path: &Path, diagnostics: Vec<Value>, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 0 }
            },
            "context": { "diagnostics": diagnostics }
        }
    })
}

/// **The whole loop**: the server reports a diagnostic, the client hands it
/// back, and the server offers the edit its own message describes.
#[test]
fn the_renamed_keyword_is_offered_a_fix() {
    let text = "module main;\n\nexport fn go() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    // First, whatever the server actually said about it.
    let reported = session(&[initialize(&w.root), did_open(&file, text), exit()]);
    let found = last_diagnostics(&reported);
    let renamed = found
        .iter()
        .find(|d| {
            d.get("message").and_then(Value::as_str).is_some_and(|m| m.contains("spelled `pub`"))
        })
        .cloned()
        .expect("the rename diagnostic");

    // Then hand it back, the way an editor does.
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        code_action(&file, vec![renamed], 2),
        exit(),
    ]);

    let actions = result_of(&replies, 2);
    let list = actions.as_array().expect("a list");
    assert_eq!(list.len(), 1, "{actions}");
    assert_eq!(list[0].get("kind"), Some(&json!("quickfix")), "{actions}");

    let edits = list[0]
        .pointer("/edit/changes")
        .and_then(Value::as_object)
        .and_then(|c| c.values().next())
        .and_then(Value::as_array)
        .expect("edits");
    assert_eq!(edits[0].get("newText"), Some(&json!("pub")), "{actions}");
    // And it replaces the `export` on line 2, not something else.
    assert_eq!(edits[0].pointer("/range/start/line"), Some(&json!(2)), "{actions}");
}

/// **A diagnostic with no mechanical fix is offered nothing**, which is the
/// rule: an action is applied by somebody who read four words of the message,
/// so one that guesses is worse than none.
#[test]
fn a_type_error_is_offered_no_action() {
    let text = "module main;\n\nfn go() -> Int { \"not an int\" }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let reported = session(&[initialize(&w.root), did_open(&file, text), exit()]);
    let found = last_diagnostics(&reported);
    assert!(!found.is_empty(), "there should be a type error to ask about");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        code_action(&file, found, 2),
        exit(),
    ]);
    assert_eq!(result_of(&replies, 2), json!([]), "no guessing");
}

/// **A requirement that has to move outwards is offered the signature edit.**
///
/// The whole loop again, and this one is worth the round trip because the edit
/// is nowhere near the squiggle: the diagnostic is on the call and the change
/// belongs on the line above it. Nothing about that is visible to the unit
/// tests, which are handed a signature rather than finding one.
#[test]
fn a_missing_capability_is_offered_the_clause_it_names() {
    let text = "module main;\n\
                \n\
                effect Db {\n\
                  get: (String) -> String,\n\
                }\n\
                \n\
                fn load() -> String with { db: Db } { db.get(\"k\") }\n\
                \n\
                fn go() -> String { load() }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let reported = session(&[initialize(&w.root), did_open(&file, text), exit()]);
    let missing = last_diagnostics(&reported)
        .into_iter()
        .find(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("does not require"))
        })
        .expect("the missing-capability diagnostic");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        code_action(&file, vec![missing], 2),
        exit(),
    ]);

    let actions = result_of(&replies, 2);
    let list = actions.as_array().expect("a list");
    assert_eq!(list.len(), 1, "{actions}");

    let edits = list[0]
        .pointer("/edit/changes")
        .and_then(Value::as_object)
        .and_then(|c| c.values().next())
        .and_then(Value::as_array)
        .expect("edits");
    assert_eq!(edits[0].get("newText"), Some(&json!(" with { db: Db }")), "{actions}");
    // On `go`'s return type, which is not where the diagnostic is -- the
    // point of this fix, and the part only the server can work out.
    assert_eq!(edits[0].pointer("/range/start/line"), Some(&json!(8)), "{actions}");
    assert_eq!(edits[0].pointer("/range/start/character"), Some(&json!(17)), "{actions}");
    assert_eq!(edits[0].pointer("/range/end"), edits[0].pointer("/range/start"), "an insert");
}

// --- signature help ---------------------------------------------------------

fn signature_help(path: &Path, line: u32, character: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/signatureHelp",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character }
        }
    })
}

/// Parameter *names*, which `Signature` does not carry — they come from the
/// callee's lowered body.
#[test]
fn signature_help_shows_named_parameters() {
    let text = "module main;\n\nfn charge(account: Int, amount: Int) -> Int { account + amount }\n\nfn go() -> Int { charge(\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        signature_help(&file, 4, 24, 2),
        exit(),
    ]);

    let help = result_of(&replies, 2);
    let label = help.pointer("/signatures/0/label").and_then(Value::as_str).unwrap_or_default();
    assert!(label.contains("account: Int"), "names, not just types: {help}");
    assert!(label.contains("amount: Int"), "{help}");
    assert_eq!(help.get("activeParameter"), Some(&json!(0)), "{help}");
}

/// A comma moves to the next parameter.
#[test]
fn a_comma_advances_the_active_parameter() {
    let text = "module main;\n\nfn charge(account: Int, amount: Int) -> Int { account + amount }\n\nfn go() -> Int { charge(1, \n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        signature_help(&file, 4, 27, 2),
        exit(),
    ]);
    assert_eq!(result_of(&replies, 2).get("activeParameter"), Some(&json!(1)), "{:?}", result_of(&replies, 2));
}

/// **The unmatched paren, not the first one.** Inside `outer(inner(a, b), ` the
/// help is about `outer`, which is the case somebody typing a nested call
/// actually needs.
#[test]
fn a_nested_call_reports_the_outer_one() {
    let text = "module main;\n\nfn inner(a: Int) -> Int { a }\n\nfn outer(x: Int, y: Int) -> Int { x + y }\n\nfn go() -> Int { outer(inner(1), \n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        signature_help(&file, 6, 33, 2),
        exit(),
    ]);

    let help = result_of(&replies, 2);
    let label = help.pointer("/signatures/0/label").and_then(Value::as_str).unwrap_or_default();
    assert!(label.starts_with("outer("), "the outer call: {help}");
    assert_eq!(help.get("activeParameter"), Some(&json!(1)), "{help}");
}

// --- run lenses -------------------------------------------------------------

/// A lens above each `test` block, carrying what `--filter` needs.
#[test]
fn each_test_gets_a_run_lens() {
    let text = "module main;\n\ntest \"adds\" {\n  assert(1 + 1 == 2);\n}\n\ntest \"subtracts\" {\n  assert(2 - 1 == 1);\n}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": url_of(&file) } }
        }),
        exit(),
    ]);

    let lenses = result_of(&replies, 2);
    let list = lenses.as_array().expect("a list");
    assert_eq!(list.len(), 2, "one per test: {lenses}");

    let names: Vec<&str> = list
        .iter()
        .filter_map(|l| l.pointer("/command/arguments/0")?.as_str())
        .collect();
    assert_eq!(names, vec!["adds", "subtracts"], "{lenses}");
    assert_eq!(list[0].pointer("/command/command"), Some(&json!("khora.runTest")), "{lenses}");
}

/// A file with no tests gets no lenses, rather than an empty decoration.
#[test]
fn a_file_with_no_tests_gets_no_lenses() {
    let text = "module main;\n\nfn go() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": url_of(&file) } }
        }),
        exit(),
    ]);
    assert_eq!(result_of(&replies, 2), json!([]));
}

// --- what an edit does to the files it is not in ---------------------------

/// Every `publishDiagnostics` the server sent, as (uri, count).
fn published(replies: &[Value]) -> Vec<(String, usize)> {
    replies
        .iter()
        .filter(|r| {
            r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        })
        .filter_map(|r| {
            let uri = r.pointer("/params/uri")?.as_str()?.to_string();
            let n = r.pointer("/params/diagnostics")?.as_array()?.len();
            Some((uri, n))
        })
        .collect()
}

/// The last count published for one document, or `None` if it never was.
fn published_for(replies: &[Value], path: &Path) -> Option<usize> {
    let want = url_of(path);
    published(replies).into_iter().rev().find(|(uri, _)| *uri == want).map(|(_, n)| n)
}

/// **A build is whole-program, so an edit here is a diagnostic there.**
///
/// Breaking `library.kh` breaks `main.kh`, and the server used to publish only
/// for the file that changed — so the editor showed `main.kh` as it was before
/// the edit until somebody happened to touch it.
#[test]
fn an_edit_republishes_the_other_open_files() {
    let w = workspace(&[
        ("src/library.kh", "module p::library;\npub fn shared() -> Int { 1 }\n"),
        ("src/main.kh", "module p::main;\nimport p::library::{shared};\npub fn main() -> Int { shared() }\n"),
    ]);
    let library = w.root.join("src/library.kh");
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, &std::fs::read_to_string(&main).expect("main")),
        did_open(&library, &std::fs::read_to_string(&library).expect("library")),
        // The function `main.kh` imports goes away.
        did_change(&library, "module p::library;\n"),
        exit(),
    ]);

    assert!(
        published_for(&replies, &main).is_some_and(|n| n > 0),
        "main.kh imports a name that library.kh no longer defines, and it is open, \
         so the server has to say so without waiting to be asked: {:?}",
        published(&replies)
    );
}

/// Closing a file takes its squiggles with it.
///
/// A diagnostic is published against a URI and stays in the client until
/// something replaces it. Dropping the line index without publishing an empty
/// list left a closed file in the Problems panel for the rest of the session.
#[test]
fn closing_a_file_clears_its_diagnostics() {
    let w = workspace(&[("src/main.kh", "module p::main;\npub fn main() -> Int { nope() }\n")]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, "module p::main;\npub fn main() -> Int { nope() }\n"),
        json!({
            "jsonrpc": "2.0", "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": url_of(&main) } }
        }),
        exit(),
    ]);

    let seen = published(&replies);
    assert!(
        seen.iter().any(|(uri, n)| *uri == url_of(&main) && *n > 0),
        "the open file should have been reported as broken first: {seen:?}"
    );
    assert_eq!(
        seen.last().map(|(_, n)| *n),
        Some(0),
        "and closing it should take the report back: {seen:?}"
    );
}

/// A `.kh` file that appears outside the editor joins the source root.
///
/// The client already sends this — the extension watches `**/*.kh` — and the
/// server used to drop it, so a file written by `git checkout` or `khora new`
/// stayed invisible and every name it defined read as unresolved until
/// somebody restarted the server.
#[test]
fn a_file_created_outside_the_editor_joins_the_root() {
    let w = workspace(&[(
        "src/main.kh",
        "module p::main;\nimport p::later::{answer};\npub fn main() -> Int { answer() }\n",
    )]);
    let main = w.root.join("src/main.kh");
    let later = w.root.join("src/later.kh");

    let text = std::fs::read_to_string(&main).expect("main");
    let writing = later.clone();
    // Written once `initialize` and `didOpen` have gone through, so the
    // workspace scan genuinely did not see it. That is the situation the
    // notification exists for.
    let replies = session_interrupted(
        &[
            initialize(&w.root),
            did_open(&main, &text),
            json!({
                "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
                "params": { "changes": [{ "uri": url_of(&later), "type": 1 }] }
            }),
            exit(),
        ],
        2,
        move || {
            std::fs::write(&writing, "module p::later;\npub fn answer() -> Int { 42 }\n")
                .expect("writing the new file");
        },
    );

    let counts: Vec<usize> =
        published(&replies).into_iter().filter(|(uri, _)| *uri == url_of(&main)).map(|(_, n)| n).collect();
    assert!(
        counts.first().is_some_and(|n| *n > 0),
        "before the file arrived, the import cannot resolve: {counts:?}"
    );
    assert_eq!(
        counts.last(),
        Some(&0),
        "and once it has, the server should say the file is fine without a restart: {counts:?}"
    );
}

/// The same file, deleted, stops satisfying the import.
#[test]
fn a_file_deleted_outside_the_editor_leaves_the_root() {
    let w = workspace(&[
        ("src/library.kh", "module p::library;\npub fn shared() -> Int { 1 }\n"),
        ("src/main.kh", "module p::main;\nimport p::library::{shared};\npub fn main() -> Int { shared() }\n"),
    ]);
    let main = w.root.join("src/main.kh");
    let library = w.root.join("src/library.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, &std::fs::read_to_string(&main).expect("main")),
        json!({
            "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
            "params": { "changes": [{ "uri": url_of(&library), "type": 3 }] }
        }),
        exit(),
    ]);

    assert!(
        published_for(&replies, &main).is_some_and(|n| n > 0),
        "the file it imported from is gone: {:?}",
        published(&replies)
    );
}

/// A file open in the editor is not overwritten from disk.
///
/// The buffer is the truth; what is on disk is behind whatever has not been
/// saved. A watcher event for an open file has to be ignored or typing would
/// be undone by the last save.
#[test]
fn a_watcher_event_does_not_overwrite_an_open_buffer() {
    let w = workspace(&[("src/main.kh", "module p::main;\npub fn main() -> Int { 0 }\n")]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        // The buffer is broken; the file on disk is not.
        did_open(&main, "module p::main;\npub fn main() -> Int { nope() }\n"),
        json!({
            "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
            "params": { "changes": [{ "uri": url_of(&main), "type": 2 }] }
        }),
        exit(),
    ]);

    assert!(
        published_for(&replies, &main).is_some_and(|n| n > 0),
        "the unsaved buffer is what the author is looking at, so its error stands: {:?}",
        published(&replies)
    );
}

// --- document highlight ----------------------------------------------------

/// Every mention of the name under the cursor, in this file.
#[test]
fn the_name_under_the_cursor_is_highlighted() {
    let w = workspace(&[(
        "src/main.kh",
        "module p::main;\npub fn main() -> Int {\n  let count = 1;\n  count + count\n}\n",
    )]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, &std::fs::read_to_string(&main).expect("main")),
        json!({
            "jsonrpc": "2.0", "id": 9, "method": "textDocument/documentHighlight",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                // `count` in `let count = 1;`
                "position": { "line": 2, "character": 6 }
            }
        }),
        exit(),
    ]);

    let answer = replies
        .iter()
        .find(|r| r.get("id").and_then(Value::as_i64) == Some(9))
        .and_then(|r| r.pointer("/result"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert_eq!(
        answer.len(),
        3,
        "the binding and both reads: {}",
        serde_json::to_string(&answer).unwrap_or_default()
    );
    for hit in &answer {
        assert!(hit.get("range").is_some(), "a highlight is a range: {hit}");
        assert!(hit.get("uri").is_none(), "a highlight is in this file, so it carries no uri");
    }
}

// --- what a name is, not just what type it has -----------------------------

const DOCUMENTED: &str = "module p::main;\n\
/// Adds two numbers, which is all it does.\n\
///\n\
/// The second paragraph, so a joined block can be told from one line.\n\
pub fn add(a: Int, b: Int) -> Int { a + b }\n\
\n\
pub fn main() -> Int { add(1, 2) }\n";

/// Hovering a function shows the line that declares it and the prose above it.
///
/// It used to show the type of the expression under the cursor and nothing
/// else, which answers "what is this" while somebody hovering a name they have
/// not called before is asking "what is it for".
#[test]
fn hovering_a_function_shows_its_signature_and_documentation() {
    let w = workspace(&[("src/main.kh", DOCUMENTED)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, DOCUMENTED),
        json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                // `add` in the call on the last line.
                "position": { "line": 6, "character": 24 }
            }
        }),
        exit(),
    ]);

    let value = result_of(&replies, 7)
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    assert!(
        value.contains("pub fn add(a: Int, b: Int) -> Int"),
        "the declaration as its author wrote it: {value:?}"
    );
    assert!(!value.contains("{ a + b }"), "and not its body: {value:?}");
    assert!(
        value.contains("Adds two numbers"),
        "and the sentence explaining it: {value:?}"
    );
    assert!(
        value.contains("second paragraph"),
        "the whole block, not the first line: {value:?}"
    );
}

/// A name with no `///` still hovers: the signature alone is worth having, and
/// an empty answer would read as "this does not exist".
#[test]
fn hovering_an_undocumented_function_still_shows_its_signature() {
    let source = "module p::main;\npub fn plain(a: Int) -> Int { a }\npub fn main() -> Int { plain(1) }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                "position": { "line": 2, "character": 24 }
            }
        }),
        exit(),
    ]);

    let value = result_of(&replies, 7)
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("pub fn plain(a: Int) -> Int"), "got {value:?}");
}

/// An expression that names no declaration still hovers with its type, which
/// is the whole of what can be said about it.
#[test]
fn hovering_an_expression_still_shows_its_type() {
    let source = "module p::main;\npub fn main() -> Int { 1 + 2 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                // The `1` of `1 + 2`.
                "position": { "line": 1, "character": 23 }
            }
        }),
        exit(),
    ]);

    let value = result_of(&replies, 7)
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("Int"), "an arithmetic operand still has a type: {value:?}");
}

/// Completion carries the documentation, not only the word "function".
///
/// A name and its kind is enough to finish typing something already known; the
/// sentence its author wrote is what says whether it is the right one.
#[test]
fn completion_carries_the_documentation() {
    let w = workspace(&[("src/main.kh", DOCUMENTED)]);
    let main = w.root.join("src/main.kh");
    let typing = DOCUMENTED.replace("pub fn main() -> Int { add(1, 2) }", "pub fn main() -> Int { a }");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, DOCUMENTED),
        did_change(&main, &typing),
        json!({
            "jsonrpc": "2.0", "id": 8, "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                "position": { "line": 6, "character": 24 }
            }
        }),
        exit(),
    ]);

    let items = result_of(&replies, 8).as_array().cloned().unwrap_or_default();
    let add = items
        .iter()
        .find(|i| i.get("label").and_then(Value::as_str) == Some("add"))
        .unwrap_or_else(|| panic!("`add` should be offered: {}", serde_json::to_string(&items).unwrap_or_default()));

    let docs = add.pointer("/documentation/value").and_then(Value::as_str).unwrap_or_default();
    assert!(docs.contains("Adds two numbers"), "the prose comes with it: {add}");
    let detail = add.get("detail").and_then(Value::as_str).unwrap_or_default();
    assert!(
        detail.contains("pub fn add(a: Int, b: Int) -> Int"),
        "and the signature stands where the word `function` used to: {add}"
    );
}

/// A local has no declaration to explain, and gets no empty panel for one.
#[test]
fn a_local_is_offered_without_an_empty_documentation_panel() {
    let source = "module p::main;\npub fn main() -> Int {\n  let total = 1;\n  t\n}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "id": 8, "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                "position": { "line": 3, "character": 3 }
            }
        }),
        exit(),
    ]);

    let items = result_of(&replies, 8).as_array().cloned().unwrap_or_default();
    let total = items
        .iter()
        .find(|i| i.get("label").and_then(Value::as_str) == Some("total"))
        .unwrap_or_else(|| panic!("the local should be offered: {}", serde_json::to_string(&items).unwrap_or_default()));
    assert!(
        total.get("documentation").is_none(),
        "a local has nothing to document, so the field is absent: {total}"
    );
}

/// **The form people actually write.** `import helper::{add};` and then a
/// bare `add(1, 2)` is the whole point of a named import, and go-to-definition
/// on it used to answer nothing while the qualified `helper::add` worked. So
/// the feature was there and missing exactly where it is used.
#[test]
fn a_bare_imported_name_finds_its_declaration() {
    let helper = "module helper;\n\n/// Adds, and says so.\npub fn add(a: Int, b: Int) -> Int { a + b }\n";
    let main = "module main;\n\nimport helper::{add};\n\nfn go() -> Int { add(1, 2) }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    let column = main.lines().nth(4).expect("a fifth line").find("add").expect("the call") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 4, column + 1, 2),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main_path) },
                "position": { "line": 4, "character": column + 1 }
            }
        }),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let uri = found.get("uri").and_then(Value::as_str).unwrap_or_default();
    assert!(uri.ends_with("helper.kh"), "a bare imported name still names another file: {found}");

    let hovered = result_of(&replies, 3)
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(hovered.contains("pub fn add(a: Int, b: Int) -> Int"), "{hovered:?}");
    assert!(hovered.contains("Adds, and says so."), "and its documentation: {hovered:?}");
}

/// A method reached through its type: `Int::to_string`, which is the commonest
/// shape of call in the language and resolved to nothing at all.
///
/// The name resolver reads `Type::method` as one only when the type is
/// declared in the same file, and `Int` is declared nowhere — it is the
/// language's own. So this asks the impl blocks directly, after resolution has
/// had its turn.
#[test]
fn a_method_on_a_type_finds_the_impl_that_declares_it() {
    let source = "module main;\n\
pub type Celsius = { degrees: Int };\n\
\n\
impl Celsius {\n\
  /// Freezing, in this scale.\n\
  pub fn freezing() -> Celsius { { degrees: 0 } }\n\
}\n\
\n\
fn go() -> Celsius { Celsius::freezing() }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main_path = w.root.join("src/main.kh");

    let line = source.lines().nth(8).expect("the call");
    let column = line.find("freezing").expect("the method") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, source),
        definition(&main_path, 8, column + 1, 2),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main_path) },
                "position": { "line": 8, "character": column + 1 }
            }
        }),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert_eq!(
        found.pointer("/range/start/line"),
        Some(&json!(5)),
        "the `fn` inside the impl, not the impl block and not the type: {found}"
    );

    let hovered = result_of(&replies, 3)
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(hovered.contains("pub fn freezing() -> Celsius"), "{hovered:?}");
    assert!(hovered.contains("Freezing, in this scale."), "{hovered:?}");
}

/// An inherent method wins over a trait method of the same name, because that
/// is what the call resolves to.
#[test]
fn an_inherent_method_is_preferred_to_a_trait_one() {
    let source = "module main;\n\
import std::core::{Show};\n\
pub type Tag = { n: Int };\n\
\n\
impl Show for Tag {\n\
  fn show(self) -> String { \"trait\" }\n\
}\n\
\n\
impl Tag {\n\
  /// The one a call means.\n\
  pub fn show(self) -> String { \"inherent\" }\n\
}\n\
\n\
fn go(t: Tag) -> String { Tag::show(t) }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main_path = w.root.join("src/main.kh");

    let line = source.lines().nth(13).expect("the call");
    let column = line.rfind("show").expect("the method") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, source),
        definition(&main_path, 13, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    assert_eq!(
        found.pointer("/range/start/line"),
        Some(&json!(10)),
        "the inherent `show`, not the trait impl's: {found}"
    );
}

/// The same, with the module path a package actually uses.
///
/// A package's modules are `pkg::main` and `pkg::library`, not the bare
/// `main` and `helper` the tests above use, and a two-segment path is a
/// different lookup in the module graph.
#[test]
fn a_bare_import_from_a_nested_module_finds_its_declaration() {
    let library = "module probe::library;\n\n/// Doubles it.\npub fn twice(n: Int) -> Int { n * 2 }\n";
    // With an unrelated `std` import above it, which is what a real file has.
    let main = "module probe::main;\n\nimport std::core::{print};\nimport probe::library::{twice};\n\nfn go() -> Int { twice(21) }\n";
    let w = workspace(&[("src/library.kh", library), ("src/main.kh", main)]);
    let main_path = w.root.join("src/main.kh");

    let column = main.lines().nth(5).expect("a sixth line").find("twice").expect("the call") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main_path, main),
        definition(&main_path, 5, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2);
    let uri = found.get("uri").and_then(Value::as_str).unwrap_or_default();
    assert!(
        uri.ends_with("library.kh"),
        "a nested module path is still a module path: {found}"
    );
}
