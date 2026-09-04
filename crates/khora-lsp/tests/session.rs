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
    khora_lsp::serve(std::io::Cursor::new(input), &mut output).expect("the server should not fail");

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
    // Incremental sync: `TextDocumentSyncKind::INCREMENTAL` is 2.
    assert_eq!(result.pointer("/capabilities/textDocumentSync"), Some(&json!(2)));
    // **The kinds, so a client filling one menu asks for one menu.** A server
    // that answers `true` here is asked for everything every time, and the
    // assists are then computed and thrown away.
    assert_eq!(
        result.pointer("/capabilities/codeActionProvider/codeActionKinds"),
        Some(&json!(["quickfix", "refactor.rewrite", "refactor.extract"])),
        "{result}"
    );
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
        // Whatever is unimplemented next. `foldingRange` stood here until it
        // was implemented, which is the point: the rule outlives the example.
        json!({ "jsonrpc": "2.0", "id": 7, "method": "textDocument/documentLink", "params": {} }),
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

/// Editing is the whole point: the diagnostics must reflect the version of the
/// file that the edits settle on.
///
/// **Driven a batch at a time**, because `serve` reports once per batch and a
/// test that sends everything at once and then counts reports is asserting a
/// race rather than a behaviour.
#[test]
fn an_edit_republishes() {
    let w = workspace(&[("src/main.kh", "module app::main;\n")]);
    let path = w.root.join("src/main.kh");
    let mut server = khora_lsp::Server::default();

    batch(&mut server, &[initialize(&w.root)]);
    let broken = batch(
        &mut server,
        &[did_open(&path, "module app::main;\nfn f() -> Int { \"text\" }\n")],
    );
    let fixed = batch(
        &mut server,
        &[did_change(&path, "module app::main;\nfn f() -> Int { 1 }\n")],
    );

    let count = |replies: &[Value]| -> Option<usize> {
        replies
            .iter()
            .filter(|r| {
                r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            })
            .filter_map(|r| r.pointer("/params/diagnostics")?.as_array().map(Vec::len))
            .next_back()
    };

    assert!(count(&broken).is_some_and(|n| n > 0), "the broken version: {broken:?}");
    assert_eq!(count(&fixed), Some(0), "and then the fixed one: {fixed:?}");
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
/// **A rename now leaves the file it started in.** It refused to until the two
/// things that made it unsafe were answered: a declaration's range covers its
/// whole body, and an import list is not a `::` path so nothing looked at it.
/// Both are handled, so the edit reaches every file that names the thing.
#[test]
fn renaming_a_declaration_edits_every_file_that_names_it() {
    let helper = "module helper;\n\npub fn add(a: Int, b: Int) -> Int { a + b }\n";
    let main = "module main;\n\nimport helper::{add};\n\nfn go() -> Int { add(1, 2) }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");
    let column = main.lines().nth(4).expect("a line").find("add").expect("it") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        prepare_rename(&file, 4, column + 1, 2),
        rename(&file, 4, column + 1, "plus", 3),
        exit(),
    ]);

    assert!(
        result_of(&replies, 2).get("placeholder").is_some(),
        "prepareRename should accept it now: {}",
        result_of(&replies, 2)
    );

    let changes = result_of(&replies, 3).pointer("/changes").cloned().unwrap_or(Value::Null);
    let map = changes.as_object().cloned().unwrap_or_default();
    assert_eq!(map.len(), 2, "both files: {changes}");

    let in_helper = map
        .iter()
        .find(|(uri, _)| uri.ends_with("helper.kh"))
        .map(|(_, edits)| edits.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    assert_eq!(in_helper.len(), 1, "the declaration's name, once: {in_helper:?}");
    // `pub fn add` is on line 2, and the edit must cover `add` alone rather
    // than the declaration it belongs to.
    assert_eq!(in_helper[0].pointer("/range/start/line"), Some(&json!(2)), "{in_helper:?}");
    let start = in_helper[0].pointer("/range/start/character").and_then(Value::as_u64);
    let finish = in_helper[0].pointer("/range/end/character").and_then(Value::as_u64);
    assert_eq!(
        finish.zip(start).map(|(f, s)| f - s),
        Some(3),
        "three characters, not the whole declaration: {in_helper:?}"
    );

    let in_main = map
        .iter()
        .find(|(uri, _)| uri.ends_with("main.kh"))
        .map(|(_, edits)| edits.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    assert_eq!(
        in_main.len(),
        2,
        "the import that brings it in, and the call: {in_main:?}"
    );
    assert!(
        in_main.iter().any(|e| e.pointer("/range/start/line") == Some(&json!(2))),
        "**the import list is the one nothing else looks at**: {in_main:?}"
    );
}

/// `import m::{foo as bar}` renames the `foo` and leaves the `bar`, because
/// the alias is this file's own word for it.
#[test]
fn renaming_through_an_alias_leaves_the_alias_alone() {
    let helper = "module helper;\n\npub fn add(a: Int, b: Int) -> Int { a + b }\n";
    let main = "module main;\n\nimport helper::{add as plus};\n\nfn go() -> Int { plus(1, 2) }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/helper.kh");
    let column = helper.lines().nth(2).expect("a line").find("add").expect("it") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, helper),
        rename(&file, 2, column + 1, "sum", 3),
        exit(),
    ]);

    let map = result_of(&replies, 3)
        .pointer("/changes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let in_main = map
        .iter()
        .find(|(uri, _)| uri.ends_with("main.kh"))
        .map(|(_, edits)| edits.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();

    assert_eq!(in_main.len(), 1, "only the import's original name: {in_main:?}");
    let at = in_main[0].pointer("/range/start/character").and_then(Value::as_u64);
    let ends = in_main[0].pointer("/range/end/character").and_then(Value::as_u64);
    let line = main.lines().nth(2).expect("the import");
    assert_eq!(
        at.map(|a| line.len() as u64 - (line.len() as u64 - a)),
        Some(line.find("add").expect("the written name") as u64),
        "the `add`, not the `plus`: {in_main:?}"
    );
    assert_eq!(ends.zip(at).map(|(e, a)| e - a), Some(3), "{in_main:?}");
}

/// A trait member is refused, and says why: the name belongs to the trait and
/// to every impl of it, and editing one without the others does not compile.
#[test]
fn renaming_a_trait_member_is_refused_and_says_why() {
    let source = "module main;\n\
import std::core::{Show};\n\
pub type Tag = { n: Int };\n\
impl Show for Tag {\n\
  fn show(self) -> String { \"tag\" }\n\
}\n\
fn go(t: Tag) -> String { Tag::show(t) }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let file = w.root.join("src/main.kh");
    let line = source.lines().nth(6).expect("the call");
    let column = line.rfind("show").expect("the method") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, source),
        prepare_rename(&file, 6, column + 1, 2),
        exit(),
    ]);

    let why = error_of(&replies, 2).expect("a refusal with a reason");
    assert!(why.contains("trait member"), "{why}");
    assert!(why.contains("every impl"), "it should say why: {why}");
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

/// The completion items for a position, as (label, sortText, the import it
/// carries).
fn completions_at(
    w: &Workspace,
    file: &Path,
    text: &str,
    line: u32,
    character: u32,
) -> Vec<(String, String, Option<String>)> {
    let replies = session(&[
        initialize(&w.root),
        did_open(file, text),
        completion(file, line, character, 9),
        exit(),
    ]);
    result_of(&replies, 9)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            (
                item.get("label").and_then(Value::as_str).unwrap_or_default().to_string(),
                item.get("sortText").and_then(Value::as_str).unwrap_or_default().to_string(),
                item.pointer("/additionalTextEdits/0/newText")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect()
}

/// **The completion people actually use in a typed language.** You do not go
/// looking for `twice` in `helper`; you type it and expect the editor to write
/// the import. Until this existed a name had to be imported before it could be
/// completed, which is the wrong way round.
#[test]
fn a_name_from_another_module_completes_and_brings_its_import() {
    let helper = "module helper;\n\npub fn twice(a: Int) -> Int { a + a }\n";
    let main = "module main;\n\nfn go() -> Int {\n  1\n}\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let offered = completions_at(&w, &file, main, 3, 3);
    let (_, sort, edit) = offered
        .iter()
        .find(|(label, _, _)| label == "twice")
        .expect("`twice` should be offered from `helper`");
    assert_eq!(edit.as_deref(), Some("import helper::{twice};\n\n"), "{offered:?}");
    // Below everything in scope: a local must not be outranked by the
    // workspace.
    assert!(sort.starts_with('2'), "{offered:?}");
}

/// A name already imported is offered once, in scope, with no import attached
/// -- an edit there would write a second `import` for something that already
/// had one.
#[test]
fn an_imported_name_is_not_offered_a_second_import() {
    let helper = "module helper;\n\npub fn twice(a: Int) -> Int { a + a }\n";
    let main = "module main;\n\nimport helper::{twice};\n\nfn go() -> Int {\n  twice(1)\n}\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let offered = completions_at(&w, &file, main, 5, 3);
    let found: Vec<_> = offered.iter().filter(|(label, _, _)| label == "twice").collect();
    assert_eq!(found.len(), 1, "{offered:?}");
    assert_eq!(found[0].2, None, "already in scope: {offered:?}");
    assert!(found[0].1.starts_with('1'), "{offered:?}");
}

/// The completion items for a position, as (label, sortText, insertText).
fn insertions_at(
    w: &Workspace,
    file: &Path,
    text: &str,
    line: u32,
    character: u32,
) -> Vec<(String, String, String)> {
    let replies = session(&[
        initialize(&w.root),
        did_open(file, text),
        completion(file, line, character, 9),
        exit(),
    ]);
    result_of(&replies, 9)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let label =
                item.get("label").and_then(Value::as_str).unwrap_or_default().to_string();
            (
                label.clone(),
                item.get("sortText").and_then(Value::as_str).unwrap_or_default().to_string(),
                item.get("insertText").and_then(Value::as_str).unwrap_or(&label).to_string(),
            )
        })
        .collect()
}

/// **The completion no other language has to answer.** A `with` block is where
/// a capability stops being a requirement, and writing one means naming the
/// effect, then every operation it declares, then a closure of the right arity
/// for each -- all of it in a declaration somewhere else.
#[test]
fn a_with_row_offers_a_whole_handler() {
    let text = "module main;\n\
                \n\
                pub effect Chime {\n\
                \x20 now: () -> Int,\n\
                \x20 at: (Int, Int) -> Int,\n\
                }\n\
                \n\
                fn go() -> Int {\n\
                \x20 with { } { 1 }\n\
                }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    // The cursor just inside the `with {`.
    let offered = insertions_at(&w, &file, text, 8, 9);
    let (_, _, insert) = offered
        .iter()
        .find(|(label, _, _)| label == "Chime")
        .unwrap_or_else(|| panic!("a handler for `Chime`: {offered:?}"));
    assert_eq!(
        insert,
        "chime: handler for Chime { now: fn () => todo(), at: fn (a, b) => todo() }",
        "{offered:?}"
    );
}

/// **The case this is actually asked in.** `with {` and nothing after it is a
/// syntax error, and the node that would say what the brace belongs to is
/// exactly the node that does not exist yet -- so the row is found by reading
/// tokens backwards rather than by walking up the tree.
#[test]
fn a_half_typed_with_row_still_knows_what_it_is() {
    let text = "module main;\n\
                \n\
                pub effect Chime {\n\
                \x20 now: () -> Int,\n\
                }\n\
                \n\
                fn go() -> Int {\n\
                \x20 with {\n\
                }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let offered = insertions_at(&w, &file, text, 7, 8);
    let (_, _, insert) = offered
        .iter()
        .find(|(label, _, _)| label == "Chime")
        .unwrap_or_else(|| panic!("a handler for `Chime`: {offered:?}"));
    assert!(insert.starts_with("chime: handler for Chime"), "{offered:?}");
}

/// One entry further along the same row, which arrives on a comma rather than
/// a brace and has to walk back over what is already written to find it.
#[test]
fn a_second_entry_in_a_with_row_is_still_a_with_row() {
    let text = "module main;\n\
                \n\
                pub effect Chime {\n\
                \x20 now: () -> Int,\n\
                }\n\
                \n\
                pub type Db;\n\
                \n\
                fn go() -> Int {\n\
                \x20 with { db: Db, } { 1 }\n\
                }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let offered = insertions_at(&w, &file, text, 9, 17);
    let (_, _, insert) = offered
        .iter()
        .find(|(label, _, _)| label == "Chime")
        .unwrap_or_else(|| panic!("a handler for `Chime`: {offered:?}"));
    assert!(insert.starts_with("chime: handler for Chime"), "{offered:?}");
}

/// **The label is the requirement's, not one made up from the type.** `std`
/// installs `LLMService` as `ai`, which no rule derives; the call inside the
/// block is what says so, and an entry that answers it has to be spelled the
/// way it asked.
#[test]
fn the_handler_is_labelled_the_way_the_requirement_asked() {
    let text = "module main;\n\
                \n\
                pub effect Chime {\n\
                \x20 now: () -> Int,\n\
                }\n\
                \n\
                fn stamp() -> Int with { ticker: Chime } { ticker.now() }\n\
                \n\
                fn go() -> Int {\n\
                \x20 with { } { stamp() }\n\
                }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let offered = insertions_at(&w, &file, text, 9, 9);
    let (_, sort, insert) = offered
        .iter()
        .find(|(label, _, _)| label == "Chime")
        .unwrap_or_else(|| panic!("a handler for `Chime`: {offered:?}"));
    assert!(insert.starts_with("ticker: handler for Chime"), "{offered:?}");
    // And first: it is not one of several plausible entries, it is the one.
    assert!(sort.starts_with('0'), "{offered:?}");
}

/// **A signature's `with` holds types, not handlers.** The same three
/// characters open both rows, and a handler skeleton offered in a signature is
/// nonsense.
#[test]
fn a_signature_row_offers_the_type_rather_than_a_handler() {
    let text = "module main;\n\
                \n\
                pub effect Chime {\n\
                \x20 now: () -> Int,\n\
                }\n\
                \n\
                fn go() -> Int with { } { 1 }\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let offered = insertions_at(&w, &file, text, 6, 22);
    let (_, _, insert) = offered
        .iter()
        .find(|(label, _, _)| label == "Chime")
        .unwrap_or_else(|| panic!("`Chime` as a type: {offered:?}"));
    assert_eq!(insert, "chime: Chime", "{offered:?}");
}

/// An effect from another module comes with the `import` that brings it in,
/// the same as any other name from elsewhere.
#[test]
fn an_effect_from_another_module_brings_its_import() {
    let chimes = "module chimes;\n\npub effect Chime {\n  now: () -> Int,\n}\n";
    let main = "module main;\n\nfn go() -> Int {\n  with { } { 1 }\n}\n";
    let w = workspace(&[("src/chimes.kh", chimes), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&file, main),
        completion(&file, 3, 9, 9),
        exit(),
    ]);
    let item = result_of(&replies, 9)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|item| item.get("label") == Some(&json!("Chime")))
        .expect("`Chime` from `chimes`");
    assert_eq!(
        item.pointer("/additionalTextEdits/0/newText").and_then(Value::as_str),
        Some("import chimes::{Chime};\n\n"),
        "{item}"
    );
}

/// **The `///` arrives when the item is looked at, not when the list is built.**
/// Reading the documentation of every public name in a workspace to fill a list
/// where one of them gets read cost 100ms a keystroke against something the
/// size of `std`; on resolve it costs a lookup, once, for the one item the
/// reader highlighted.
#[test]
fn a_completion_from_elsewhere_gets_its_documentation_on_resolve() {
    let helper =
        "module helper;\n\n/// Twice as much as it was.\npub fn twice(a: Int) -> Int { a + a }\n";
    let main = "module main;\n\nfn go() -> Int {\n  1\n}\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let mut server = khora_lsp::Server::default();
    batch(&mut server, &[initialize(&w.root), did_open(&file, main)]);
    let listed = batch(&mut server, &[completion(&file, 3, 3, 9)]);
    let item = result_in(&listed, 9)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|item| item.get("label") == Some(&json!("twice")))
        .expect("`twice` should be offered");

    // The list itself says nothing about it beyond where it comes from.
    assert_eq!(item.get("documentation"), None, "{item}");
    assert_eq!(item.pointer("/data/module"), Some(&json!("helper")), "{item}");

    let resolved = batch(
        &mut server,
        &[json!({ "jsonrpc": "2.0", "id": 10, "method": "completionItem/resolve", "params": item })],
    );
    let filled = result_in(&resolved, 10);
    assert_eq!(
        filled.pointer("/documentation/value").and_then(Value::as_str),
        Some("Twice as much as it was."),
        "{filled}"
    );
    // **Whole, not just the new parts.** A client merges the reply over what it
    // sent, so an item that came back without its import would insert the name
    // and forget the `import`.
    assert_eq!(filled.get("label"), Some(&json!("twice")), "{filled}");
    assert!(filled.get("additionalTextEdits").is_some(), "{filled}");
}

/// An item this knows nothing more about comes back exactly as it arrived,
/// rather than as an error or a null the client would merge over its own.
#[test]
fn resolving_an_item_with_nothing_to_add_returns_it_unchanged() {
    let w = workspace(&[("src/main.kh", "module main;\n")]);
    let mut server = khora_lsp::Server::default();
    batch(&mut server, &[initialize(&w.root)]);

    let sent = json!({ "label": "whatever", "kind": 3 });
    let replies = batch(
        &mut server,
        &[json!({ "jsonrpc": "2.0", "id": 7, "method": "completionItem/resolve", "params": sent })],
    );
    assert_eq!(result_in(&replies, 7), json!({ "label": "whatever", "kind": 3 }));
}

/// Nothing from elsewhere after a `.`, where the question is "what can this
/// value do" and an unrelated free function is not an answer to it.
#[test]
fn a_method_position_is_not_filled_with_the_workspace() {
    let helper = "module helper;\n\npub fn twice(a: Int) -> Int { a + a }\n";
    let main = "module main;\n\nfn go(s: String) -> Int {\n  s.\n}\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");

    let offered = completions_at(&w, &file, main, 3, 4);
    assert!(!offered.iter().any(|(label, _, _)| label == "twice"), "{offered:?}");
}

// --- quick fixes ------------------------------------------------------------

/// A code action request for a selection, with no diagnostics attached.
fn assist_at(path: &Path, line: u32, from: u32, to: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": url_of(path) },
            "range": {
                "start": { "line": line, "character": from },
                "end": { "line": line, "character": to }
            },
            "context": { "diagnostics": [] }
        }
    })
}

/// The assists offered for a selection, as (title, kind).
fn assists_for(text: &str, line: u32, from: u32, to: u32) -> Vec<(String, String)> {
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        assist_at(&file, line, from, to, 4),
        exit(),
    ]);
    result_of(&replies, 4)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|a| {
            Some((
                a.get("title")?.as_str()?.to_string(),
                a.get("kind")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// The one assist's edits applied to `text`, and its title.
fn assist_applied(text: &str, line: u32, from: u32, to: u32) -> (String, String) {
    chosen_assist(text, line, from, to, None)
}

/// [`assist_applied`], choosing among several by a word in the title.
fn assist_named(text: &str, line: u32, from: u32, to: u32, want: &str) -> (String, String) {
    chosen_assist(text, line, from, to, Some(want))
}

fn chosen_assist(
    text: &str,
    line: u32,
    from: u32,
    to: u32,
    wanted: Option<&str>,
) -> (String, String) {
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        assist_at(&file, line, from, to, 4),
        exit(),
    ]);
    let offered = result_of(&replies, 4);
    let list = offered.as_array().expect("a list");
    // **One selection may be offered several assists**, which it could not be
    // when the only extraction was the `let`. Extracting a selection into a
    // binding and into a function are both reasonable answers to the same
    // gesture, so the caller says which one it meant.
    let chosen = match wanted {
        Some(want) => list
            .iter()
            .find(|a| a.get("title").and_then(Value::as_str).is_some_and(|t| t.contains(want)))
            .unwrap_or_else(|| panic!("no assist matching `{want}`: {offered}")),
        None => {
            assert_eq!(list.len(), 1, "exactly one assist: {offered}");
            &list[0]
        }
    };
    let title = chosen.get("title").and_then(Value::as_str).expect("a title").to_string();
    let edits = chosen
        .pointer("/edit/changes")
        .and_then(Value::as_object)
        .and_then(|c| c.values().next())
        .and_then(Value::as_array)
        .expect("edits")
        .clone();

    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|edit| {
            let at = |which: &str| {
                let l = edit
                    .pointer(&format!("/range/{which}/line"))
                    .and_then(Value::as_u64)
                    .expect("a line") as usize;
                let c = edit
                    .pointer(&format!("/range/{which}/character"))
                    .and_then(Value::as_u64)
                    .expect("a character") as usize;
                text.split_inclusive('\n').take(l).map(str::len).sum::<usize>() + c
            };
            let new = edit.get("newText").and_then(Value::as_str).unwrap_or_default().to_string();
            (at("start"), at("end"), new)
        })
        .collect();
    // Later edits first, so an earlier one's offsets stay true.
    spans.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut out = text.to_string();
    for (start, end, new) in spans {
        out.replace_range(start..end, &new);
    }
    (title, out)
}

/// A binding whose type the source does not say: the initializer's name is
/// `origin`, not `Point`, so nothing on the line answers "what is `p`".
const INFERRED: &str = "module main;\n\
\n\
type Point = { x: Int };\n\
\n\
fn origin() -> Point { { x: 0 } }\n\
\n\
fn go() -> Int {\n\
\x20 let p = origin();\n\
\x20 p.x\n\
}\n";

/// **The inlay hint made text.** A hint cannot be copied, disappears when the
/// setting is off, and is absent from the diff a reviewer reads; writing it
/// down is the difference between the compiler knowing a type and the file
/// saying it.
#[test]
fn a_binding_with_no_annotation_is_offered_its_inferred_type() {
    let (title, after) = assist_named(INFERRED, 7, 6, 6, "inferred type");
    assert!(title.contains("`Point`"), "{title}");
    assert!(after.contains("let p: Point = origin();"), "{after}");
}

/// A binding whose type the author already wrote gets nothing: the assist
/// exists to say something the file does not, not to rewrite what it does.
#[test]
fn a_binding_that_says_its_type_is_offered_no_annotation() {
    let text = INFERRED.replace("let p =", "let p: Point =");
    let offered = assists_for(&text, 7, 6, 6);
    // Inlining is offered here and is a different question: what this pins is
    // that an annotation somebody wrote is not offered a second one.
    assert!(
        !offered.iter().any(|(title, _)| title.contains("inferred type")),
        "an annotated binding has nothing to write: {offered:?}"
    );
}

/// **A selected expression, lifted into a `let` above its statement**, with the
/// statement's own indentation copied onto the new line.
#[test]
fn a_selected_expression_is_offered_an_extraction() {
    let text = "module main;\n\nfn go(a: Int, b: Int) -> Int {\n  a + (b + 1)\n}\n";
    let (title, after) = assist_named(text, 3, 6, 13, "`let`");
    assert!(title.contains("Extract into a `let`"), "{title}");
    assert!(after.contains("  let extracted = (b + 1);\n  a + extracted"), "{after}");
}

/// Every diagnostic in `text`, which is how the tests below check that an
/// extraction produced a program rather than a plausible-looking one.
fn complaints(text: &str) -> Vec<String> {
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");
    let replies = session(&[initialize(&w.root), did_open(&file, text), exit()]);
    last_diagnostics(&replies)
        .iter()
        .filter_map(|d| d.get("message").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// **The extracted program still compiles**, which is the assertion the others
/// are shorthand for.
///
/// An assist that writes a signature can be wrong in a way no string
/// comparison catches: a capability left out of the row, a `!` missing at the
/// call, a parameter type that does not name a type. So this one applies the
/// edit and hands the result back to the server, and asserts it has nothing to
/// say about it.
#[test]
fn an_extracted_function_compiles() {
    let text = concat!(
        "module main;\n\n",
        "pub effect Log { record: (String) -> () }\n\n",
        "fn note(m: String) -> () with { log: Log } { log.record(m) }\n\n",
        "fn go(n: Int) -> Int with { log: Log } {\n",
        "  note(\"x\");\n",
        "  n\n",
        "}\n",
    );
    assert!(complaints(text).is_empty(), "the fixture itself must compile: {:?}", complaints(text));

    let (_, after) = assist_named(text, 7, 2, 11, "function");
    let said = complaints(&after);
    assert!(said.is_empty(), "the extraction did not compile:\n{after}\n{said:?}");
}

/// **A selection that can fail gets a `raises` clause and a `!` at the call.**
///
/// The failure has to go somewhere, and where it was going before is the
/// enclosing function. An extraction that wrote the clause and forgot the `!`
/// would compile the new function and break the old one.
#[test]
fn an_extracted_function_that_can_fail_says_so_and_is_called_with_a_mark() {
    let text = concat!(
        "module main;

",
        "pub type Oops = | Bad;

",
        "fn risky() -> Int raises Oops { 1 }

",
        "fn go() -> Int raises Oops {
",
        "  risky()!
",
        "}
",
    );
    assert!(complaints(text).is_empty(), "the fixture must compile: {:?}", complaints(text));

    let (_, after) = assist_named(text, 7, 2, 10, "function");
    assert!(after.contains("raises Oops"), "the clause should be written: {after}");
    assert!(after.contains("extracted()!"), "the call needs the mark: {after}");
    let said = complaints(&after);
    assert!(said.is_empty(), "the extraction did not compile:
{after}
{said:?}");
}

// --- the last five ---------------------------------------------------------

/// **`!(a && b)` becomes `!a || !b`**, and the `||` is the whole point: the
/// common mistake is distributing the `!` and leaving the `&&` alone, which
/// agrees with the original exactly half the time.
#[test]
fn a_negated_conjunction_distributes_properly() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Bool, b: Bool) -> Bool {\n",
        "  !(a && b)\n",
        "}\n",
    );
    let (title, after) = assist_named(text, 3, 2, 2, "Distribute");
    assert!(title.contains("`||`"), "the operator turns over: {title}");
    assert!(after.contains("!a || !b"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// The other direction, `!(a || b)` to `!a && !b`.
#[test]
fn a_negated_disjunction_distributes_properly() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Bool, b: Bool) -> Bool {\n",
        "  !(a || b)\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 2, 2, "Distribute");
    assert!(after.contains("!a && !b"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **`mut` added**, which is unconditional where removing it is careful: the
/// cheap mistake is the one to make easy to fix.
#[test]
fn a_binding_can_be_made_mutable() {
    let text = concat!("module main;\n\n", "fn go() -> Int {\n", "  let n = 1;\n", "  n\n", "}\n");
    let (_, after) = assist_named(text, 3, 6, 6, "Make it `mut`");
    assert!(after.contains("let mut n = 1;"), "{after}");
}

/// **The import lines put in order**, which is separate from ordering the
/// names inside one.
#[test]
fn the_import_lines_can_be_sorted() {
    let text = concat!(
        "module main;\n\n",
        "import std::json::{Json};\n",
        "import std::core::{Option};\n\n",
        "fn go() -> Int { 1 }\n",
    );
    let (_, after) = assist_named(text, 2, 5, 5, "Sort the imports");
    let core = after.find("std::core").expect("core");
    let json = after.find("std::json").expect("json");
    assert!(core < json, "core comes first: {after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **An arm's body given a block**, which is where the second statement goes.
#[test]
fn a_match_arm_can_get_a_block_body() {
    let text = concat!(
        "module main;\n\n",
        "fn go(ready: Bool) -> Int {\n",
        "  match ready {\n",
        "    true => 1,\n",
        "    false => 2,\n",
        "  }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 4, 6, 6, "block body");
    assert!(after.contains("true => { 1 },"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

// --- statements, loops and tests -------------------------------------------

/// **`while c { .. }` becomes a `loop` with the break at the top**, which is
/// where a hand-written one goes about half the time.
#[test]
fn a_while_can_become_a_loop() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> () {\n",
        "  let mut n = 0;\n",
        "  while n < 3 {\n",
        "    n = n + 1;\n",
        "  }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 4, 3, 3, "as a `loop`");
    assert!(after.contains("loop {"), "{after}");
    assert!(after.contains("if !(n < 3) { break };"), "the break goes first: {after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A `test` block named after the function**, in the sentence form the rest
/// of the tree uses.
#[test]
fn a_function_is_offered_a_test() {
    let text = concat!("module main;\n\n", "fn charge() -> Int { 1 }\n");
    let (title, after) = assist_named(text, 2, 4, 4, "test for");
    assert!(title.contains("`charge`"), "{title}");
    assert!(after.contains("test \"charge "), "{after}");
}

/// **And a benchmark**, which is a different question with the same shape: a
/// measurement with an `assert` in it measures the assertion.
#[test]
fn a_function_is_offered_a_benchmark() {
    let text = concat!("module main;\n\n", "fn charge() -> Int { 1 }\n");
    let (_, after) = assist_named(text, 2, 4, 4, "benchmark for");
    assert!(after.contains("bench \"charge\""), "{after}");
    assert!(!after.contains("bench \"charge\" {\n  assert"), "a bench asserts nothing: {after}");
}

/// **A discarded answer given a name.** The opposite of inlining, for when a
/// call's result turns out to matter.
#[test]
fn a_discarded_call_can_be_bound() {
    let text = concat!(
        "module main;\n\n",
        "fn work() -> Int { 1 }\n\n",
        "fn go() -> () {\n",
        "  work();\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 5, 3, 3, "Bind the answer");
    assert!(after.contains("let answer = work();"), "{after}");
}

// --- types and what is generated from them ---------------------------------

/// **A whole handler written from the effect declaration.**
///
/// Every operation, with a closure of the right arity. The list comes from the
/// declaration, which is the difference between this and typing it out.
#[test]
fn an_effect_is_offered_a_handler_written_from_its_operations() {
    let text = concat!(
        "module main;\n\n",
        "pub effect Clock {\n",
        "  now: () -> Int,\n",
        "  sleep: (Int) -> (),\n",
        "}\n",
    );
    let (title, after) = assist_named(text, 2, 12, 12, "handler for");
    assert!(title.contains("`Clock`"), "{title}");
    assert!(after.contains("handler for Clock"), "{after}");
    assert!(after.contains("now:"), "every operation: {after}");
    assert!(after.contains("sleep:"), "every operation: {after}");
}

/// **An `impl` block, with the type's own parameters carried over.**
#[test]
fn a_generic_type_is_offered_an_impl_with_its_parameters() {
    let text = concat!("module main;\n\n", "pub type Box<A> = { held: A };\n");
    let (_, after) = assist_named(text, 2, 10, 10, "`impl` block");
    assert!(after.contains("impl<A> Box<A> {"), "the parameters come too: {after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A case added to a variant type, with the `|` that introduces it.**
#[test]
fn a_variant_type_is_offered_another_case() {
    let text = concat!("module main;\n\n", "pub type Colour = | Red | Green;\n");
    let (_, after) = assist_named(text, 2, 20, 20, "Add a case");
    assert!(after.contains("| Case"), "{after}");
}

/// **A field added to a record type, with the comma decided by what is there.**
#[test]
fn a_record_type_is_offered_another_field() {
    let text = concat!("module main;\n\n", "pub type Point = { x: Int, y: Int };\n");
    let (_, after) = assist_named(text, 2, 20, 20, "Add a field");
    assert!(after.contains("y: Int, field: Int"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **`derive(Show)` above a type that has none**, and nothing for one that has.
#[test]
fn a_type_without_a_derive_is_offered_one() {
    let text = concat!("module main;\n\n", "pub type Point = { x: Int };\n");
    let (_, after) = assist_named(text, 2, 10, 10, "Derive");
    assert!(after.contains("derive(Show)\npub type Point"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));

    let already = "module main;\n\nderive(Show)\npub type Point = { x: Int };\n";
    let offered = assists_for(already, 3, 10, 10);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Derive")),
        "it has one: {offered:?}"
    );
}

// --- calls and pipelines ---------------------------------------------------

/// **`f(x)` becomes `x |> f`.**
#[test]
fn a_one_argument_call_can_become_a_pipeline() {
    let text = concat!(
        "module main;\n\n",
        "fn double(n: Int) -> Int { n * 2 }\n\n",
        "fn go(n: Int) -> Int {\n",
        "  double(n)\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 5, 3, 3, "as a pipeline");
    assert!(after.contains("n |> double"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A call of two arguments is not**, because the pipe fills the first
/// position and the rest would have to stay behind.
#[test]
fn a_two_argument_call_is_not_offered_a_pipeline() {
    let text = concat!(
        "module main;\n\n",
        "fn add(a: Int, b: Int) -> Int { a + b }\n\n",
        "fn go(n: Int) -> Int {\n",
        "  add(n, 1)\n",
        "}\n",
    );
    let offered = assists_for(text, 5, 3, 3);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("as a pipeline")),
        "the second argument has nowhere to go: {offered:?}"
    );
}

/// **And back**, for a pipeline one stage long.
#[test]
fn a_one_stage_pipeline_can_become_a_call() {
    let text = concat!(
        "module main;\n\n",
        "fn double(n: Int) -> Int { n * 2 }\n\n",
        "fn go(n: Int) -> Int {\n",
        "  n |> double\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 5, 4, 4, "pipeline as a call");
    assert!(after.contains("double(n)"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A lambda body gets braces**, which is where the second statement goes.
#[test]
fn a_lambda_body_can_become_a_block() {
    let text = concat!(
        "module main;\n\n",
        "fn apply(f: (Int) -> Int, n: Int) -> Int { f(n) }\n\n",
        "fn go(n: Int) -> Int {\n",
        "  apply(fn x => x * 2, n)\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 5, 14, 14, "block body");
    assert!(after.contains("fn x => { x * 2 }"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **An underscore parameter gets a name**, for a lambda that grew a use.
#[test]
fn an_unnamed_lambda_parameter_can_be_named() {
    let text = concat!(
        "module main;\n\n",
        "fn apply(f: (Int) -> Int, n: Int) -> Int { f(n) }\n\n",
        "fn go(n: Int) -> Int {\n",
        "  apply(fn _ => 1, n)\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 5, 12, 12, "Name the parameter");
    assert!(after.contains("fn value => 1"), "{after}");
    // **The result warns, and that is right.** A parameter named and not yet
    // used is exactly what "bound and never read" is for, and it is the state
    // somebody is in for the two seconds between naming it and using it. So
    // this asserts the edit rather than silence.
    let said = complaints(&after);
    assert!(
        said.iter().all(|line| line.contains("never read")),
        "nothing but the unused warning: {said:?}"
    );
}

// --- literals --------------------------------------------------------------

/// **`"a " + name + "!"` becomes `"a ${name}!"`.**
///
/// The whole chain at once, not the innermost `+`: a three-piece message is
/// one edit, which is what somebody asking for it meant.
#[test]
fn a_concatenated_message_becomes_one_interpolated_string() {
    let text = concat!(
        "module main;\n\n",
        "fn go(name: String) -> String {\n",
        "  \"hello \" + name + \"!\"\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 12, 12, "interpolated");
    assert!(after.contains("\"hello ${name}!\""), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **Arithmetic is not a message.** A `+` chain with no string literal in it is
/// left alone, or every sum in the file would be offered a rewrite into
/// nonsense.
#[test]
fn adding_numbers_is_not_offered_an_interpolation() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Int, b: Int) -> Int {\n",
        "  a + b\n",
        "}\n",
    );
    let offered = assists_for(text, 3, 4, 4);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("interpolated")),
        "there is no string here: {offered:?}"
    );
}

/// **A long number gets its digits grouped**, which is the one typo that looks
/// exactly like the right answer.
#[test]
fn a_long_number_can_have_its_digits_grouped() {
    let text = concat!("module main;\n\n", "fn go() -> Int {\n", "  1000000\n", "}\n");
    let (title, after) = assist_named(text, 3, 4, 4, "Group the digits");
    assert!(title.contains("1_000_000"), "{title}");
    assert!(after.contains("  1_000_000"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// A short one is not: three digits need no counting.
#[test]
fn a_short_number_is_not_offered_grouping() {
    let text = concat!("module main;\n\n", "fn go() -> Int {\n", "  100\n", "}\n");
    let offered = assists_for(text, 3, 4, 4);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Group the digits")),
        "nothing to count: {offered:?}"
    );
}

// --- declarations ----------------------------------------------------------

/// **`pub` added, and the title says what it costs.** Everything a module
/// exports is a promise it cannot take back quietly.
#[test]
fn a_declaration_can_be_exported_and_the_title_says_what_that_means() {
    let text = concat!("module main;\n\n", "fn go() -> Int { 1 }\n");
    let (title, after) = assist_named(text, 2, 3, 3, "Export it");
    assert!(title.contains("promise"), "{title}");
    assert!(after.contains("pub fn go()"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// And taken off again, which is always safe.
#[test]
fn an_exported_declaration_can_stop_being_one() {
    let text = concat!("module main;\n\n", "pub fn go() -> Int { 1 }\n");
    let (_, after) = assist_named(text, 2, 7, 7, "Stop exporting");
    assert!(after.contains("fn go()") && !after.contains("pub fn"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// A declaration already public is not offered `pub` again.
#[test]
fn an_exported_declaration_is_not_offered_pub_twice() {
    let text = concat!("module main;\n\n", "pub fn go() -> Int { 1 }\n");
    let offered = assists_for(text, 2, 7, 7);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Export it")),
        "it already is: {offered:?}"
    );
}

/// **A `///` block started, and left empty for somebody to write.**
#[test]
fn an_undocumented_declaration_is_offered_a_comment() {
    let text = concat!("module main;\n\n", "fn go() -> Int { 1 }\n");
    let (_, after) = assist_named(text, 2, 3, 3, "documentation comment");
    assert!(after.contains("/// \nfn go()"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// One that already has a block is left alone, however short it is.
#[test]
fn a_documented_declaration_is_not_offered_another_comment() {
    let text = concat!("module main;\n\n", "/// Answers.\nfn go() -> Int { 1 }\n");
    let offered = assists_for(text, 3, 3, 3);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("documentation comment")),
        "it has one: {offered:?}"
    );
}

/// **The clauses go after the return type**, which is the placement somebody
/// writing one by hand gets wrong when the return type contains a brace.
#[test]
fn a_with_clause_lands_after_a_record_return_type() {
    let text = concat!("module main;\n\n", "fn go() -> { n: Int } { { n: 1 } }\n");
    let (_, after) = assist_named(text, 2, 3, 3, "`with` clause");
    assert!(after.contains("-> { n: Int } with {  } {"), "the record type is not the body: {after}");
}

/// A signature that already says `raises` is not offered a second clause.
#[test]
fn a_signature_with_raises_is_not_offered_another() {
    let text = concat!(
        "module main;\n\n",
        "pub type Oops = | Bad;\n\n",
        "fn go() -> Int raises Oops { 1 }\n",
    );
    let offered = assists_for(text, 4, 3, 3);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("`raises` clause")),
        "it has one: {offered:?}"
    );
}

// --- imports ---------------------------------------------------------------

/// **Two imports of one module fold into one**, keeping the order the names
/// were written in.
#[test]
fn two_imports_of_one_module_merge() {
    let text = concat!(
        "module main;\n\n",
        "import std::core::{Option};\n",
        "import std::core::{Result};\n\n",
        "fn go() -> Int { 1 }\n",
    );
    let (title, after) = assist_named(text, 2, 5, 5, "Merge");
    assert!(title.contains("std::core"), "{title}");
    assert!(after.contains("import std::core::{Option, Result};"), "{after}");
    assert!(!after.contains("import std::core::{Result};"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// A file with one import of a module has nothing to merge, and says nothing.
#[test]
fn a_lone_import_is_not_offered_a_merge() {
    let text = concat!(
        "module main;\n\n",
        "import std::core::{Option};\n\n",
        "fn go() -> Int { 1 }\n",
    );
    let offered = assists_for(text, 2, 5, 5);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Merge")),
        "there is only one: {offered:?}"
    );
}

/// **Names out of order are offered an order**, and names already in one are
/// not offered an edit that changes nothing.
#[test]
fn names_out_of_order_can_be_sorted() {
    let text = concat!(
        "module main;\n\n",
        "import std::core::{Result, Option};\n\n",
        "fn go() -> Int { 1 }\n",
    );
    let (_, after) = assist_named(text, 2, 5, 5, "Sort");
    assert!(after.contains("{Option, Result}"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));

    let tidy = text.replace("{Result, Option}", "{Option, Result}");
    let offered = assists_for(&tidy, 2, 5, 5);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Sort")),
        "already sorted: {offered:?}"
    );
}

/// **One import per name**, for a list that has grown past reading.
#[test]
fn an_import_list_can_be_split() {
    let text = concat!(
        "module main;\n\n",
        "import std::core::{Option, Result};\n\n",
        "fn go() -> Int { 1 }\n",
    );
    let (_, after) = assist_named(text, 2, 5, 5, "Split into one");
    assert!(after.contains("import std::core::{Option};"), "{after}");
    assert!(after.contains("import std::core::{Result};"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

// --- matching --------------------------------------------------------------

/// **A two-arm `match` on a `Bool` is an `if` written the long way.**
#[test]
fn a_boolean_match_can_become_an_if() {
    let text = concat!(
        "module main;\n\n",
        "fn go(ready: Bool) -> Int {\n",
        "  match ready {\n",
        "    true => 1,\n",
        "    false => 2,\n",
        "  }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 3, 3, "as an `if`");
    assert!(after.contains("if ready { 1 } else { 2 }"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **Written the other way round, the branches swap with it.**
#[test]
fn a_boolean_match_written_false_first_keeps_its_answers() {
    let text = concat!(
        "module main;\n\n",
        "fn go(ready: Bool) -> Int {\n",
        "  match ready {\n",
        "    false => 2,\n",
        "    true => 1,\n",
        "  }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 3, 3, "as an `if`");
    assert!(after.contains("if ready { 1 } else { 2 }"), "the arms follow their patterns: {after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A `match` over constructors is not an `if`**, and rewriting it as one
/// would throw away the exhaustiveness the compiler is about to check.
#[test]
fn a_match_over_constructors_is_not_offered_an_if() {
    let text = concat!(
        "module main;\n\n",
        "pub type Colour = | Red | Green;\n\n",
        "fn go(c: Colour) -> Int {\n",
        "  match c {\n",
        "    Colour::Red => 1,\n",
        "    Colour::Green => 2,\n",
        "  }\n",
        "}\n",
    );
    let offered = assists_for(text, 5, 3, 3);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("as an `if`")),
        "two constructors are not two booleans: {offered:?}"
    );
}

/// **And an `if` becomes a `match`**, which is where it goes when the
/// condition is about to become three cases.
#[test]
fn an_if_can_become_a_match() {
    let text = concat!(
        "module main;\n\n",
        "fn go(ready: Bool) -> Int {\n",
        "  if ready { 1 } else { 2 }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 3, 3, "as a `match`");
    assert!(after.contains("match ready {"), "{after}");
    assert!(after.contains("true => 1,"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// An arm added below the cursor's, with the comma the last one was missing.
#[test]
fn an_arm_can_be_added_below_the_cursor() {
    let text = concat!(
        "module main;\n\n",
        "pub type Colour = | Red | Green;\n\n",
        "fn go(c: Colour) -> Int {\n",
        "  match c {\n",
        "    Colour::Red => 1,\n",
        "    Colour::Green => 2,\n",
        "  }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 6, 8, 8, "arm below");
    assert!(after.contains("_ => todo()"), "{after}");
}

// --- capabilities and failures ---------------------------------------------

/// The head every effect-assist fixture shares.
const RAISING: &str = concat!(
    "module main;\n\n",
    "import std::core::{Result, attempt, todo};\n\n",
    "pub type Oops = | Bad;\n\n",
    "fn risky() -> Int raises Oops { 1 }\n\n",
);

/// **A `!` is offered a `catch` with the arm that covers everything.**
#[test]
fn a_failing_call_is_offered_a_catch() {
    let text = format!("{RAISING}fn go() -> Int raises Oops {{\n  risky()!\n}}\n");
    let (title, after) = assist_named(&text, 9, 4, 4, "catch");
    assert!(title.contains("catch"), "{title}");
    assert!(after.contains("risky()! catch { failure => todo() }"), "{after}");
}

/// **And `attempt`, which is the other way out.** `!` sends the failure up;
/// `attempt` turns it into a value to branch on here.
#[test]
fn a_failing_call_is_offered_attempt() {
    let text = format!("{RAISING}fn go() -> Int raises Oops {{\n  risky()!\n}}\n");
    let (_, after) = assist_named(&text, 9, 4, 4, "attempt");
    assert!(after.contains("attempt(fn () => risky()!)"), "{after}");
}

/// **A call already inside `attempt` is not offered another**, which is the
/// check that stops the assist stacking wrappers on each keystroke.
#[test]
fn a_call_already_attempted_is_not_offered_attempt_again() {
    let text = format!(
        "{RAISING}fn go() -> Result<Int, Oops> {{\n  attempt(fn () => risky()!)\n}}\n"
    );
    let offered = assists_for(&text, 9, 20, 20);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("attempt")),
        "it is already attempted: {offered:?}"
    );
}

/// **The way back out of `attempt`.**
#[test]
fn an_attempt_can_become_a_raise() {
    let text = format!(
        "{RAISING}fn go() -> Result<Int, Oops> {{\n  attempt(fn () => risky()!)\n}}\n"
    );
    let (_, after) = assist_named(&text, 9, 3, 3, "Raise it");
    assert!(after.contains("  risky()!"), "{after}");
    assert!(!after.contains("attempt(fn"), "{after}");
}

/// **A postfix `with` becomes a block**, which is what somebody wants the
/// moment there is more than one expression under it.
#[test]
fn a_postfix_with_can_become_a_block() {
    let text = concat!(
        "module main;\n\n",
        "pub effect Log { record: (String) -> () }\n\n",
        "fn quiet() -> Log { handler for Log { record: fn _m => () } }\n\n",
        "fn note() -> () with { log: Log } { log.record(\"x\") }\n\n",
        "fn go() -> () {\n",
        "  note() with { log: quiet() }\n",
        "}\n",
    );
    assert!(complaints(text).is_empty(), "the fixture must compile: {:?}", complaints(text));
    let (_, after) = assist_named(text, 9, 10, 10, "as a block");
    assert!(after.contains("with { log: quiet() } {"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **And a block holding one expression becomes postfix.**
#[test]
fn a_with_block_around_one_expression_can_become_postfix() {
    let text = concat!(
        "module main;\n\n",
        "pub effect Log { record: (String) -> () }\n\n",
        "fn quiet() -> Log { handler for Log { record: fn _m => () } }\n\n",
        "fn note() -> () with { log: Log } { log.record(\"x\") }\n\n",
        "fn go() -> () {\n",
        "  with { log: quiet() } { note() }\n",
        "}\n",
    );
    assert!(complaints(text).is_empty(), "the fixture must compile: {:?}", complaints(text));
    let (_, after) = assist_named(text, 9, 3, 3, "after the expression");
    assert!(after.contains("note() with { log: quiet() }"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

// --- bindings --------------------------------------------------------------

/// **A binding used once is replaced by what it held.**
#[test]
fn a_binding_used_once_can_be_inlined() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Int, b: Int) -> Int {\n",
        "  let sum = a + b;\n",
        "  sum * 2\n",
        "}\n",
    );
    let (title, after) = assist_named(text, 3, 6, 6, "Inline");
    assert!(title.contains("Inline `sum`"), "{title}");
    assert!(after.contains("  (a + b) * 2"), "the brackets matter: {after}");
    assert!(!after.contains("let sum"), "the binding should be gone: {after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A binding used twice is not**, unless what it holds costs nothing to
/// repeat. `count()` twice is two calls, and whether that is the same program
/// depends on what `count` does.
#[test]
fn a_binding_used_twice_is_not_inlined_when_it_holds_work() {
    let text = concat!(
        "module main;\n\n",
        "fn count() -> Int { 1 }\n\n",
        "fn go() -> Int {\n",
        "  let n = count();\n",
        "  n + n\n",
        "}\n",
    );
    let offered = assists_for(text, 5, 6, 6);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Inline")),
        "two uses would be two calls: {offered:?}"
    );
}

/// A literal costs nothing to repeat, so two uses are fine.
#[test]
fn a_binding_holding_a_literal_is_inlined_however_often_it_is_used() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> Int {\n",
        "  let n = 2;\n",
        "  n + n\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 6, 6, "Inline");
    assert!(after.contains("  2 + 2"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **A `mut` binding is not inlined**, because a binding that is written to is
/// not its initializer.
#[test]
fn a_mutable_binding_is_not_inlined() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> Int {\n",
        "  let mut n = 1;\n",
        "  n = n + 1;\n",
        "  n\n",
        "}\n",
    );
    let offered = assists_for(text, 3, 10, 10);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Inline")),
        "a binding that is written to is not what it started as: {offered:?}"
    );
}

/// **A type the checker would have inferred is offered for removal**, which is
/// the way back from `Write the inferred type`.
#[test]
fn a_redundant_type_annotation_can_be_removed() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> Int {\n",
        "  let n: Int = 1;\n",
        "  n\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 6, 6, "inferred");
    assert!(after.contains("  let n = 1;"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **`mut` that nothing uses is offered for removal.**
#[test]
fn an_unused_mut_can_be_removed() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> Int {\n",
        "  let mut n = 1;\n",
        "  n\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 10, 10, "Remove `mut`");
    assert!(after.contains("  let n = 1;"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **And `mut` that something writes to is not.** The control for the test
/// above: without it, that one passes just as well if the assist never
/// recognised a `let` at all.
#[test]
fn a_mut_that_is_written_to_is_kept() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> Int {\n",
        "  let mut n = 1;\n",
        "  n = n + 1;\n",
        "  n\n",
        "}\n",
    );
    let offered = assists_for(text, 3, 10, 10);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Remove `mut`")),
        "something assigns to it: {offered:?}"
    );
}

/// **`_name` back to `name`**, for a binding that grew a use.
#[test]
fn an_underscored_binding_can_be_renamed_back() {
    let text = concat!(
        "module main;\n\n",
        "fn go() -> Int {\n",
        "  let _n = 1;\n",
        "  2\n",
        "}\n",
    );
    let (title, after) = assist_named(text, 3, 7, 7, "Rename to");
    assert!(title.contains("`n`"), "{title}");
    assert!(after.contains("let n = 1;"), "{after}");
}

// --- control flow ----------------------------------------------------------

/// **An inverted `if` runs the same branch for the same input.**
///
/// The condition flips to its opposite comparison rather than growing a `!`,
/// because a negation somebody has to read is worse than the one they wrote.
#[test]
fn an_if_with_both_branches_can_be_inverted() {
    let text = concat!(
        "module main;\n\n",
        "fn go(n: Int) -> Int {\n",
        "  if n < 10 { 1 } else { 2 }\n",
        "}\n",
    );
    let (title, after) = assist_named(text, 3, 5, 5, "Invert");
    assert!(title.contains("Invert"), "{title}");
    assert!(after.contains("if n >= 10 { 2 } else { 1 }"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// `<` inverts to `>=` and not to `>`, because the third case is what makes it
/// a comparison rather than a coin toss.
#[test]
fn inverting_a_comparison_keeps_the_case_it_did_not_mention() {
    let text = concat!(
        "module main;\n\n",
        "fn go(n: Int) -> Int {\n",
        "  if n <= 10 { 1 } else { 2 }\n",
        "}\n",
    );
    let (_, after) = assist_named(text, 3, 5, 5, "Invert");
    assert!(after.contains("if n > 10"), "`<=` inverts to `>`: {after}");
}

/// **Two `if`s that hold nothing else become one `&&`**, which tests the same
/// things in the same order and skips the same work.
#[test]
fn nested_ifs_merge_into_one_condition() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Bool, b: Bool) -> Int {\n",
        "  if a { if b { 1 } else { 0 } } else { 0 }\n",
        "}\n",
    );
    // The inner `if` has an `else`, so the merge would change which code runs.
    let offered = assists_for(text, 3, 5, 5);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Merge")),
        "an inner `else` belongs to the inner condition: {offered:?}"
    );
}

/// The shape that does merge: neither has an `else`, and the outer holds only
/// the inner.
#[test]
fn a_bare_nested_if_merges() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Bool, b: Bool) -> () {\n",
        "  if a { if b { report() } }\n",
        "}\n\n",
        "fn report() -> () { () }\n",
    );
    let (_, after) = assist_named(text, 3, 5, 5, "Merge");
    assert!(after.contains("if a && b {"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **`a < b` flips to `b > a`**, operator turned round with the operands so it
/// answers the same question.
#[test]
fn a_comparison_can_be_read_the_other_way_round() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Int, b: Int) -> Bool {\n",
        "  a < b\n",
        "}\n",
    );
    let (title, after) = assist_named(text, 3, 4, 4, "Flip");
    assert!(title.contains("`>`"), "{title}");
    assert!(after.contains("  b > a"), "{after}");
    assert!(complaints(&after).is_empty(), "{after}\n{:?}", complaints(&after));
}

/// **`+` is not offered**, even though the arithmetic commutes: the operands
/// are expressions, and swapping them swaps the order two calls happen in.
#[test]
fn addition_is_not_offered_a_flip() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Int, b: Int) -> Int {\n",
        "  a + b\n",
        "}\n",
    );
    let offered = assists_for(text, 3, 4, 4);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("Flip")),
        "swapping operands swaps evaluation order: {offered:?}"
    );
}

/// An `if` with no `else` is offered one, in the right place.
#[test]
fn an_if_without_an_else_is_offered_one() {
    let text = concat!(
        "module main;\n\n",
        "fn go(a: Bool) -> () {\n",
        "  if a { report() }\n",
        "}\n\n",
        "fn report() -> () { () }\n",
    );
    let (_, after) = assist_named(text, 3, 5, 5, "else");
    assert!(after.contains("} else {"), "{after}");
}

/// **A block is what people actually extract**, and it is the kind the `let`
/// deliberately leaves out.
#[test]
fn a_selected_block_becomes_a_function() {
    let text = concat!(
        "module main;

",
        "fn go(a: Int) -> Int {
",
        "  { let doubled = a + a; doubled + 1 }
",
        "}
",
    );
    assert!(complaints(text).is_empty(), "the fixture must compile: {:?}", complaints(text));

    let (_, after) = assist_named(text, 3, 2, 38, "function");
    assert!(after.contains("fn extracted(a: Int) -> Int"), "{after}");
    let said = complaints(&after);
    assert!(said.is_empty(), "the extraction did not compile:
{after}
{said:?}");
}

/// **A block that writes to a binding it did not declare is refused.**
///
/// A parameter is a value, so the write would land on a copy and the original
/// would quietly stop changing -- an extraction that compiles and is wrong,
/// which is the worst kind. The closure route to the same mistake is already a
/// compile error; this is the one that is not.
#[test]
fn a_block_that_assigns_to_an_outer_binding_is_not_extracted() {
    let text = concat!(
        "module main;

",
        "fn go() -> Int {
",
        "  let mut n = 0;
",
        "  { n = n + 1; n }
",
        "}
",
    );
    assert!(complaints(text).is_empty(), "the fixture must compile: {:?}", complaints(text));

    let offered = assists_for(text, 4, 2, 18);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("function")),
        "the write would land on a parameter: {offered:?}"
    );

    // **The control, and it is not optional.** The assertion above passes just
    // as well if the block was never recognised at all, which would make it a
    // test of nothing. The same block with the write removed has to be
    // offered, and the only difference between the two is the assignment.
    let readable = text.replace("{ n = n + 1; n }", "{ let m = n + 1; m }");
    assert!(complaints(&readable).is_empty(), "{:?}", complaints(&readable));
    let offered = assists_for(&readable, 4, 2, 22);
    assert!(
        offered.iter().any(|(title, _)| title.contains("function")),
        "a block that only reads is extractable, so the refusal above is about the write and          not about blocks: {offered:?}"
    );
}

/// **A selected expression becomes a function, and the signature is written.**
///
/// The parameter is the one local the selection uses and does not bind, and
/// its type is the checker's. `a` is not a parameter because the selection
/// does not mention it.
#[test]
fn a_selected_expression_is_offered_a_function() {
    let text = "module main;\n\nfn go(a: Int, b: Int) -> Int {\n  a + (b + 1)\n}\n";
    let (title, after) = assist_named(text, 3, 6, 13, "function");
    assert!(title.contains("Extract into a function"), "{title}");
    assert!(after.contains("fn extracted(b: Int) -> Int {"), "{after}");
    assert!(after.contains("  a + extracted(b)"), "{after}");
}

/// **The capability row is written from what the calls inside demanded.**
///
/// This is the half a Rust equivalent has no analogue for: the extracted
/// function needs `with { log: Log }` and nothing in the selection says so --
/// the checker recorded it at the call while it was type-checking, and the
/// assist reads it back.
#[test]
fn an_extracted_function_declares_the_capability_it_needs() {
    let text = concat!(
        "module main;\n\n",
        "pub effect Log { record: (String) -> () }\n\n",
        "fn note(m: String) -> () with { log: Log } { log.record(m) }\n\n",
        "fn go(n: Int) -> Int with { log: Log } {\n",
        "  note(\"x\");\n",
        "  n\n",
        "}\n",
    );
    let line = 7;
    let (_, after) = assist_named(text, line, 2, 11, "function");
    assert!(
        after.contains("with { log: Log }") && after.contains("fn extracted("),
        "the extracted function should declare the capability: {after}"
    );
}

/// **A selection containing a `with` block is refused.**
///
/// A handler *discharges* a row, and the rows are unioned from what each call
/// demanded before anything answered it -- so the signature would declare a
/// capability the extracted code supplies itself. Refused exactly rather than
/// guessed at.
#[test]
fn a_selection_that_installs_a_capability_is_not_extracted() {
    let text = concat!(
        "module main;\n\n",
        "pub effect Log { record: (String) -> () }\n\n",
        "fn quiet() -> Log { handler for Log { record: fn _m => () } }\n\n",
        "fn note(m: String) -> () with { log: Log } { log.record(m) }\n\n",
        "fn go() -> Int {\n",
        "  with { log: quiet() } { note(\"x\") };\n",
        "  0\n",
        "}\n",
    );
    let offered = assists_for(text, 9, 2, 36);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("function")),
        "a selection that answers its own row has no honest signature: {offered:?}"
    );
}

/// **Offered where the `let` is refused**, which is the point of having both.
///
/// Hoisting the right side of `&&` runs code the `&&` exists to skip. A *call*
/// left where the expression was runs at exactly the same moment, so there is
/// nothing to refuse.
#[test]
fn a_function_is_offered_where_a_binding_would_reorder_the_program() {
    let text =
        "module main;\n\nfn go(a: Bool, b: Bool) -> Bool {\n  let both = a && (b && a);\n  both\n}\n";
    let offered = assists_for(text, 3, 18, 26);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("into a `let`")),
        "the binding still reorders: {offered:?}"
    );
    assert!(
        offered.iter().any(|(title, _)| title.contains("into a function")),
        "a call does not reorder anything, so it should be offered: {offered:?}"
    );
}

/// **The refusal is the feature.** Hoisting the right side of `&&` above the
/// statement makes code run that the `&&` exists to skip, so no *binding* is
/// offered there rather than an edit that quietly changes the program.
///
/// Extracting a function is offered, and that is not an exception to this: a
/// call left where the expression was runs exactly when the expression did.
/// `a_function_is_offered_where_a_binding_would_reorder_the_program` is the
/// other half, and the pair is the whole distinction.
#[test]
fn an_expression_that_may_not_run_is_offered_no_extraction() {
    let text =
        "module main;\n\nfn go(a: Bool, b: Bool) -> Bool {\n  let both = a && (b && a);\n  both\n}\n";
    let offered = assists_for(text, 3, 18, 26);
    assert!(
        !offered.iter().any(|(title, _)| title.contains("into a `let`")),
        "the far side of `&&` is not always run: {offered:?}"
    );
}

/// A client that asks only for quick fixes is not answered with refactorings,
/// which it would filter out after paying for them to be computed.
#[test]
fn a_client_asking_only_for_quick_fixes_gets_no_assists() {
    let text = INFERRED;
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&file, text),
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "textDocument/codeAction",
            "params": {
                "textDocument": { "uri": url_of(&file) },
                "range": {
                    "start": { "line": 7, "character": 6 },
                    "end": { "line": 7, "character": 6 }
                },
                "context": { "diagnostics": [], "only": ["quickfix"] }
            }
        }),
        exit(),
    ]);
    assert_eq!(result_of(&replies, 4), json!([]));
}

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

/// The one action offered for the first diagnostic whose message says `needle`,
/// with its edits applied to `text`.
///
/// **Applied rather than inspected**, because a range that is off by a comma
/// produces an edit that reads correctly in JSON and source that does not
/// parse. The result is the thing a person would see in their editor, so that
/// is what the assertions are written against.
fn applied(text: &str, needle: &str) -> (String, String) {
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");
    fix_in(&w, &file, text, needle)
}

/// The half of `applied` that does not decide what else is in the workspace.
fn fix_in(w: &Workspace, file: &Path, text: &str, needle: &str) -> (String, String) {
    let reported = session(&[initialize(&w.root), did_open(file, text), exit()]);
    let found = last_diagnostics(&reported);
    let about = found
        .iter()
        .find(|d| d.get("message").and_then(Value::as_str).is_some_and(|m| m.contains(needle)))
        .unwrap_or_else(|| panic!("no diagnostic mentioning {needle:?} in {found:#?}"))
        .clone();

    let replies = session(&[
        initialize(&w.root),
        did_open(file, text),
        code_action(file, vec![about], 2),
        exit(),
    ]);
    let actions = result_of(&replies, 2);
    let list = actions.as_array().expect("a list");
    assert_eq!(list.len(), 1, "exactly one action: {actions}");
    let title = list[0].get("title").and_then(Value::as_str).expect("a title").to_string();

    let edits = list[0]
        .pointer("/edit/changes")
        .and_then(Value::as_object)
        .and_then(|c| c.values().next())
        .and_then(Value::as_array)
        .expect("edits")
        .clone();

    // Later edits first, so that an earlier one's offsets stay true.
    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|edit| {
            let at = |which: &str| {
                let line = edit
                    .pointer(&format!("/range/{which}/line"))
                    .and_then(Value::as_u64)
                    .expect("a line") as usize;
                let character = edit
                    .pointer(&format!("/range/{which}/character"))
                    .and_then(Value::as_u64)
                    .expect("a character") as usize;
                // The tests here are all ASCII, so a UTF-16 unit is a byte.
                text.split_inclusive('\n').take(line).map(str::len).sum::<usize>() + character
            };
            let new = edit.get("newText").and_then(Value::as_str).unwrap_or_default().to_string();
            (at("start"), at("end"), new)
        })
        .collect();
    spans.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));

    let mut out = text.to_string();
    for (start, end, new) in spans {
        out.replace_range(start..end, &new);
    }
    (title, out)
}

/// `applied`, with a `helper` module beside the file for imports to name.
fn applied_with_helper(text: &str, needle: &str) -> (String, String) {
    let helper = "module helper;\n\npub fn add(a: Int, b: Int) -> Int { a + b }\n\n\
                  pub fn twice(a: Int) -> Int { a + a }\n";
    let w = workspace(&[("src/helper.kh", helper), ("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");
    fix_in(&w, &file, text, needle)
}

/// **The signature is the whole value of this one.** The message names `cmp`
/// and nothing else; what it takes and what it answers are in another file,
/// written against `Self`. Transcribing that by hand is where a second error
/// about a mismatched signature comes from.
#[test]
fn an_impl_missing_a_member_is_offered_the_signature() {
    let shapes = "module shapes;\n\n\
                  pub type Ordering = | Less | Same | More;\n\n\
                  pub trait Rank {\n\
                  \x20 fn cmp(self, other: Self) -> Ordering;\n\
                  }\n";
    let main = "module main;\n\n\
                import shapes::{Rank, Ordering};\n\n\
                type Point = { x: Int };\n\n\
                impl Rank for Point {\n\
                }\n";
    let w = workspace(&[("src/shapes.kh", shapes), ("src/main.kh", main)]);
    let file = w.root.join("src/main.kh");
    let (title, after) = fix_in(&w, &file, main, "this impl is missing `cmp`");

    assert!(title.contains("`Rank`"), "{title}");
    assert!(
        after.contains("fn cmp(self, other: Point) -> Ordering { todo() }"),
        "`Self` becomes the type being implemented: {after}"
    );
}

/// **An unused import takes its comma with it**, which is the whole difficulty:
/// the diagnostic covers the name alone, and deleting exactly that leaves
/// `{List, , print}`.
#[test]
fn an_unused_import_is_removed_with_its_separator() {
    let (title, after) = applied_with_helper(
        "module main;\n\nimport helper::{add, twice};\n\nfn go() -> Int { add(1, 2) }\n",
        "`twice` is imported and never used",
    );
    assert!(title.contains("`twice`"), "{title}");
    assert!(after.contains("import helper::{add};"), "{after}");
}

/// The same, for a name that is not last: the comma after it goes instead.
#[test]
fn an_unused_import_in_front_takes_the_comma_after_it() {
    let (_, after) = applied_with_helper(
        "module main;\n\nimport helper::{twice, add};\n\nfn go() -> Int { add(1, 2) }\n",
        "`twice` is imported and never used",
    );
    assert!(after.contains("import helper::{add};"), "{after}");
}

/// **An unused binding is offered the rename and not the deletion**, though the
/// message names both. Deleting means deciding what to do with the initializer,
/// which may be the call that does the work; the prefix is one token.
#[test]
fn an_unused_binding_is_offered_the_underscore() {
    let text = "module main;\n\nfn go() -> Int {\n  let spare = 1;\n  2\n}\n";
    let (title, after) = applied(text, "`spare` is bound and never read");
    assert!(title.contains("`_spare`"), "{title}");
    assert!(after.contains("let _spare = 1;"), "{after}");
}

/// **The suggestion in the message becomes the edit.** The checker already did
/// the work of finding the near name; this saves retyping it.
#[test]
fn a_misspelling_is_offered_the_name_it_meant() {
    let text = "module main;\n\nfn go() -> Int {\n  let count = 1;\n  cout\n}\n";
    let (title, after) = applied(text, "did you mean `count`?");
    assert!(title.contains("`count`"), "{title}");
    assert!(after.contains("\n  count\n"), "{after}");
}

/// **A record missing a field is offered the field, with `todo()` in it.**
///
/// The value is a hole for the reason the match arms are: a plausible default
/// would be a wrong answer where the error was a refusal.
#[test]
fn a_missing_record_field_is_offered_a_hole() {
    let text = "module main;\n\ntype Point = { x: Int, y: Int };\n\n\
                fn go() -> Point { { x: 1 } }\n";
    let (title, after) = applied(text, "is missing `y`");
    assert!(title.contains("`y`"), "{title}");
    assert!(after.contains("y: todo()"), "{after}");
    assert!(after.contains("x: 1,"), "the field it already had keeps its comma: {after}");
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
///
/// **Driven a batch at a time**, which is also what lets the file be created
/// at the right moment: between two calls rather than by timing a side effect
/// against a stream.
#[test]
fn a_file_created_outside_the_editor_joins_the_root() {
    let w = workspace(&[(
        "src/main.kh",
        "module p::main;\nimport p::later::{answer};\npub fn main() -> Int { answer() }\n",
    )]);
    let main = w.root.join("src/main.kh");
    let later = w.root.join("src/later.kh");
    let text = std::fs::read_to_string(&main).expect("main");

    let mut server = khora_lsp::Server::default();
    let before = batch(&mut server, &[initialize(&w.root), did_open(&main, &text)]);

    let count = |replies: &[Value]| -> Option<usize> {
        replies
            .iter()
            .filter(|r| {
                r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            })
            .filter_map(|r| r.pointer("/params/diagnostics")?.as_array().map(Vec::len))
            .next_back()
    };

    assert!(
        count(&before).is_some_and(|n| n > 0),
        "before the file arrived, the import cannot resolve: {before:?}"
    );

    std::fs::write(&later, "module p::later;\npub fn answer() -> Int { 42 }\n")
        .expect("writing the new file");

    let after = batch(
        &mut server,
        &[json!({
            "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
            "params": { "changes": [{ "uri": url_of(&later), "type": 1 }] }
        })],
    );

    assert_eq!(
        count(&after),
        Some(0),
        "and once it has, the server says the file is fine without a restart: {after:?}"
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

// --- incremental sync ------------------------------------------------------

/// A ranged change, as a client sends one under incremental sync.
fn did_change_range(
    path: &Path,
    (from_line, from_col): (u32, u32),
    (to_line, to_col): (u32, u32),
    text: &str,
) -> Value {
    json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": url_of(path), "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": from_line, "character": from_col },
                    "end": { "line": to_line, "character": to_col }
                },
                "text": text
            }]
        }
    })
}

/// The server declares incremental sync, so a client sends edits rather than
/// the file. Full sync sent the whole document on every keystroke, which for a
/// large module is the cost that grows with the file.
#[test]
fn the_server_asks_for_incremental_sync() {
    let w = workspace(&[("src/main.kh", "module p::main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = replies
        .iter()
        .find(|r| r.get("id").and_then(Value::as_i64) == Some(1))
        .and_then(|r| r.pointer("/result/capabilities/textDocumentSync"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(caps, json!(2), "2 is Incremental in the protocol: {caps}");
}

/// One edit in the middle of a line, applied to the text the server holds.
#[test]
fn a_ranged_edit_is_applied_to_the_document() {
    let source = "module p::main;\npub fn main() -> Int { nope() }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let column = source.lines().nth(1).expect("a line").find("nope").expect("the call") as u32;
    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        // Replace `nope` with `1`, which makes the file correct.
        did_change_range(&main, (1, column), (1, column + 6), "1"),
        exit(),
    ]);

    // The script arrives as one batch, so what is reported is the state the
    // edits settle on -- which is the whole point of batching, and is
    // checked directly in `a_run_of_edits_is_reported_once`.
    let seen = published(&replies);
    assert_eq!(
        seen.last().map(|(_, n)| *n),
        Some(0),
        "the edit fixes the file, which it can only do if the splice landed: {seen:?}"
    );
}

/// Several edits in one notification, each measured against the document the
/// one before it left. A multi-cursor edit is the everyday case.
#[test]
fn several_edits_in_one_notification_apply_in_order() {
    // `zz` rather than `a`, so searching for it cannot land inside `main`.
    let source = "module p::main;\npub fn main() -> Int { zz }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");
    let column = source.lines().nth(1).expect("a line").find("zz").expect("the name") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": url_of(&main), "version": 2 },
                "contentChanges": [
                    // `zz` becomes `ab`, then `ab` becomes `1`.
                    { "range": { "start": { "line": 1, "character": column },
                                 "end": { "line": 1, "character": column + 2 } }, "text": "ab" },
                    { "range": { "start": { "line": 1, "character": column },
                                 "end": { "line": 1, "character": column + 2 } }, "text": "1" }
                ]
            }
        }),
        exit(),
    ]);

    assert_eq!(
        published(&replies).last().map(|(_, n)| *n),
        Some(0),
        "the second edit is measured against what the first left: {:?}",
        published(&replies)
    );
}

/// A change with no range is still the whole document, which a client sends
/// when it re-synchronizes.
#[test]
fn a_change_with_no_range_replaces_everything() {
    let source = "module p::main;\npub fn main() -> Int { nope() }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        did_change(&main, "module p::main;\npub fn main() -> Int { 1 }\n"),
        exit(),
    ]);

    assert_eq!(published(&replies).last().map(|(_, n)| *n), Some(0), "{:?}", published(&replies));
}

/// **A range the server cannot honour is dropped, not guessed at.** A client
/// and a server that disagree about an offset should cost a wrong document
/// until the next full change, rather than a panic that takes the session with
/// it.
#[test]
fn an_impossible_range_does_not_kill_the_server() {
    let source = "module p::main;\npub fn main() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        // Past the end of the document, and inverted.
        did_change_range(&main, (99, 0), (99, 5), "x"),
        did_change_range(&main, (1, 10), (1, 2), "x"),
        // The session is still alive and still answering.
        json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                "position": { "line": 1, "character": 23 }
            }
        }),
        exit(),
    ]);

    assert!(
        replies.iter().any(|r| r.get("id").and_then(Value::as_i64) == Some(7)),
        "the server should still be answering after two impossible edits"
    );
}

/// An edit that spans lines, which is what deleting a block sends.
#[test]
fn an_edit_across_lines_is_applied() {
    let source = "module p::main;\npub fn gone() -> Int {\n  nope()\n}\npub fn main() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        // Delete the whole broken function, lines 1 to 3 inclusive.
        did_change_range(&main, (1, 0), (4, 0), ""),
        exit(),
    ]);

    assert_eq!(
        published(&replies).last().map(|(_, n)| *n),
        Some(0),
        "removing the broken function leaves a clean file: {:?}",
        published(&replies)
    );
}

// --- type hints on bindings ------------------------------------------------

/// Every inlay hint in a file, as (line, label).
fn hints_in(replies: &[Value], id: i64) -> Vec<(u64, String)> {
    result_of(replies, id)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|h| {
            let line = h.pointer("/position/line")?.as_u64()?;
            let label = match h.get("label")? {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p.get("value").and_then(Value::as_str))
                    .collect::<String>(),
                _ => return None,
            };
            Some((line, label.trim().to_string()))
        })
        .collect()
}

/// **A `let` with no annotation is the one place a Khora type is hidden.**
/// Parameters and returns are written out by design, so a hint there repeats
/// the screen; a binding's type is on nobody's screen until now.
#[test]
fn a_binding_without_an_annotation_shows_its_type() {
    let source = "module p::main;\n\
import std::core::{List};\n\
\n\
pub fn go() -> Int {\n\
  let counted = List::length(List::Cons(1, List::Nil));\n\
  counted\n\
}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&main, source), inlay_hints(&main, 5), exit()]);

    let hints = hints_in(&replies, 5);
    assert!(
        hints.iter().any(|(line, label)| *line == 4 && label == ": Int"),
        "the inferred type of `counted`: {hints:?}"
    );
}

/// An annotation is the author saying it, so nothing is added.
#[test]
fn an_annotated_binding_gets_no_hint() {
    let source = "module p::main;\n\
pub fn go() -> Int {\n\
  let n: Int = 1 + 1;\n\
  n\n\
}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&main, source), inlay_hints(&main, 5), exit()]);
    assert!(
        !hints_in(&replies, 5).iter().any(|(line, _)| *line == 2),
        "the line already says `Int`: {:?}",
        hints_in(&replies, 5)
    );
}

/// **A hint that repeats the line it is on is worse than no hint.** A literal
/// is its own type, and a call through a path whose head is the type it
/// returns has already said the word.
#[test]
fn a_binding_whose_line_already_says_the_type_gets_no_hint() {
    let source = "module p::main;\n\
import std::core::{List};\n\
\n\
pub fn go() -> Int {\n\
  let n = 1;\n\
  let xs = List::Cons(n, List::Nil);\n\
  List::length(xs)\n\
}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&main, source), inlay_hints(&main, 5), exit()]);
    let hints = hints_in(&replies, 5);
    assert!(!hints.iter().any(|(line, _)| *line == 4), "a literal says `Int`: {hints:?}");
    assert!(
        !hints.iter().any(|(line, _)| *line == 5),
        "`List::Cons` says `List` on the line: {hints:?}"
    );
}

/// But a call through a path whose head is *not* the type still gets one:
/// `List::length` says `List` and answers `Int`, and that is exactly the hint
/// worth having.
#[test]
fn a_call_that_returns_something_else_still_gets_a_hint() {
    let source = "module p::main;\n\
import std::core::{List};\n\
\n\
pub fn go() -> String {\n\
  let xs = List::Cons(1, List::Nil);\n\
  let described = Int::to_string(List::length(xs));\n\
  described\n\
}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&main, source), inlay_hints(&main, 5), exit()]);
    let hints = hints_in(&replies, 5);
    assert!(
        hints.iter().any(|(line, label)| *line == 5 && label == ": String"),
        "`Int::to_string` says `Int` and answers `String`: {hints:?}"
    );
}

/// A binding nobody reads is already a lint. Two voices for one thing is one
/// too many.
#[test]
fn an_underscore_binding_gets_no_hint() {
    let source = "module p::main;\n\
import std::core::{List};\n\
\n\
pub fn go() -> Int {\n\
  let _counted = List::length(List::Nil);\n\
  0\n\
}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies =
        session(&[initialize(&w.root), did_open(&main, source), inlay_hints(&main, 5), exit()]);
    assert!(
        !hints_in(&replies, 5).iter().any(|(line, _)| *line == 4),
        "{:?}",
        hints_in(&replies, 5)
    );
}

// --- go to the type, and go to the implementations -------------------------

fn request_at(method: &str, path: &Path, line: u32, character: u32, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": method,
        "params": {
            "textDocument": { "uri": url_of(path) },
            "position": { "line": line, "character": character }
        }
    })
}

const SHAPES: &str = "module p::main;\n\
import std::core::{Show};\n\
\n\
pub type Colour = { red: Int };\n\
\n\
impl Show for Colour {\n\
  fn show(self) -> String { \"colour\" }\n\
}\n\
\n\
impl Colour {\n\
  pub fn make() -> Colour { { red: 1 } }\n\
}\n\
\n\
pub fn go() -> Int {\n\
  let mixed = Colour::make();\n\
  mixed.red\n\
}\n";

/// **A different question from "where is this declared".** On
/// `let mixed = Colour::make()`, go-to-definition lands on `make` and this
/// lands on `Colour`, which is what somebody chasing an unfamiliar return
/// value wants.
#[test]
fn go_to_type_definition_finds_the_type_not_the_function() {
    let w = workspace(&[("src/main.kh", SHAPES)]);
    let main = w.root.join("src/main.kh");
    let line = SHAPES.lines().nth(14).expect("the binding");
    let column = line.find("make").expect("the call") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, SHAPES),
        request_at("textDocument/typeDefinition", &main, 14, column + 1, 2),
        request_at("textDocument/definition", &main, 14, column + 1, 3),
        exit(),
    ]);

    assert_eq!(
        result_of(&replies, 2).pointer("/range/start/line"),
        Some(&json!(3)),
        "the type declaration: {}",
        result_of(&replies, 2)
    );
    assert_eq!(
        result_of(&replies, 3).pointer("/range/start/line"),
        Some(&json!(10)),
        "and go-to-definition still lands on the function: {}",
        result_of(&replies, 3)
    );
}

/// Every `impl` written for the type under the cursor, one result per block
/// rather than one per method.
#[test]
fn go_to_implementation_lists_every_impl_of_a_type() {
    let w = workspace(&[("src/main.kh", SHAPES)]);
    let main = w.root.join("src/main.kh");
    let column = SHAPES.lines().nth(3).expect("the type").find("Colour").expect("the name") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, SHAPES),
        request_at("textDocument/implementation", &main, 3, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2).as_array().cloned().unwrap_or_default();
    assert_eq!(
        found.len(),
        2,
        "the `Show` impl and the inherent one, once each: {}",
        serde_json::to_string(&found).unwrap_or_default()
    );
}

/// A trait answers with who implements it, which is the question asked about a
/// trait far more often than where it is declared.
#[test]
fn go_to_implementation_on_a_trait_finds_its_implementors() {
    let w = workspace(&[("src/main.kh", SHAPES)]);
    let main = w.root.join("src/main.kh");
    let column = SHAPES.lines().nth(5).expect("the impl").find("Show").expect("the trait") as u32;

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, SHAPES),
        request_at("textDocument/implementation", &main, 5, column + 1, 2),
        exit(),
    ]);

    let found = result_of(&replies, 2).as_array().cloned().unwrap_or_default();
    assert!(
        !found.is_empty(),
        "`Show` is implemented here: {}",
        serde_json::to_string(&found).unwrap_or_default()
    );
}

/// Both are advertised, or no client will ask.
#[test]
fn the_navigation_capabilities_are_declared() {
    let w = workspace(&[("src/main.kh", "module p::main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = replies[0].pointer("/result/capabilities").cloned().unwrap_or(Value::Null);
    assert!(caps.get("typeDefinitionProvider").is_some(), "{caps}");
    assert!(caps.get("implementationProvider").is_some(), "{caps}");
}

// --- folding and expand-selection ------------------------------------------

const FOLDABLE: &str = "module p::main;\n\
import std::core::{List};\n\
import std::env::{Env};\n\
\n\
/* a block\n\
   comment */\n\
pub fn go(n: Int) -> Int {\n\
  match n {\n\
    0 => 1,\n\
    _ => 2,\n\
  }\n\
}\n";

/// Folds by (startLine, endLine, kind).
fn folds_in(replies: &[Value], id: i64) -> Vec<(u64, u64, String)> {
    result_of(replies, id)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|f| {
            Some((
                f.get("startLine")?.as_u64()?,
                f.get("endLine")?.as_u64()?,
                f.get("kind").and_then(Value::as_str).unwrap_or("region").to_string(),
            ))
        })
        .collect()
}

/// A run of imports folds as one region, which nothing gives you by accident:
/// each import is its own declaration, so no node spans them.
#[test]
fn a_run_of_imports_folds_together() {
    let w = workspace(&[("src/main.kh", FOLDABLE)]);
    let main = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&main, FOLDABLE),
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/foldingRange",
            "params": { "textDocument": { "uri": url_of(&main) } }
        }),
        exit(),
    ]);

    let folds = folds_in(&replies, 5);
    assert!(
        folds.iter().any(|(start, _, kind)| *start == 1 && kind == "imports"),
        "the two imports on lines 2 and 3: {folds:?}"
    );
    assert!(
        folds.iter().any(|(start, _, kind)| *start == 4 && kind == "comment"),
        "the block comment: {folds:?}"
    );
    assert!(
        folds.iter().any(|(start, end, _)| *start == 6 && *end >= 11),
        "the function body: {folds:?}"
    );
}

/// **A fold that starts and ends on one line is not a fold.** An editor asked
/// to draw one puts a chevron beside a line that cannot collapse.
#[test]
fn a_single_line_declaration_is_not_folded() {
    let source = "module p::main;\npub fn one() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/foldingRange",
            "params": { "textDocument": { "uri": url_of(&main) } }
        }),
        exit(),
    ]);
    assert!(folds_in(&replies, 5).is_empty(), "{:?}", folds_in(&replies, 5));
}

/// Expand-selection widens through the tree, each step containing the one
/// before it, and never repeats a range — a step that does not widen makes the
/// key press look broken.
#[test]
fn expanding_a_selection_widens_every_step() {
    let source = "module p::main;\npub fn go() -> Int {\n  let n = 1 + 2;\n  n\n}\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/selectionRange",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                // On the `1` of `1 + 2`.
                "positions": [{ "line": 2, "character": 10 }]
            }
        }),
        exit(),
    ]);

    let first = result_of(&replies, 5).as_array().and_then(|a| a.first().cloned()).unwrap_or(Value::Null);
    assert!(first.get("range").is_some(), "a chain starts with a range: {first}");

    // Walk out, checking each parent strictly contains its child.
    let mut step = first;
    let mut steps = 0;
    while let Some(parent) = step.get("parent").cloned() {
        let inner = step.pointer("/range").cloned().expect("a range");
        let outer = parent.pointer("/range").cloned().expect("a parent range");
        let inner_start = inner.pointer("/start/character").and_then(Value::as_u64);
        let outer_start = outer.pointer("/start/character").and_then(Value::as_u64);
        let inner_line = inner.pointer("/start/line").and_then(Value::as_u64);
        let outer_line = outer.pointer("/start/line").and_then(Value::as_u64);
        assert!(
            (outer_line, outer_start) <= (inner_line, inner_start),
            "a parent begins no later than its child: {outer} vs {inner}"
        );
        assert_ne!(inner, outer, "a step that does not widen is a key press that does nothing");
        step = parent;
        steps += 1;
    }
    assert!(steps >= 3, "the literal, the sum, the binding, the block: {steps} step(s)");
}

/// Both are advertised, or no client will ask.
#[test]
fn the_structure_capabilities_are_declared() {
    let w = workspace(&[("src/main.kh", "module p::main;\n")]);
    let replies = session(&[initialize(&w.root), exit()]);
    let caps = replies[0].pointer("/result/capabilities").cloned().unwrap_or(Value::Null);
    assert!(caps.get("foldingRangeProvider").is_some(), "{caps}");
    assert!(caps.get("selectionRangeProvider").is_some(), "{caps}");
}

// --- batching, and the cancellation it makes possible ----------------------

/// The `result` of the reply with this id, from a batch's replies.
fn result_in(replies: &[Value], id: i64) -> Value {
    replies
        .iter()
        .find(|r| r.get("id") == Some(&json!(id)))
        .and_then(|r| r.get("result").cloned())
        .unwrap_or(Value::Null)
}

/// One batch, handed to the server as a batch.
///
/// **`serve` batches whatever has already arrived, which depends on when the
/// client sent it.** That is right for a server and wrong for a test: asserting
/// "exactly one report" through the socket asserts a race. `handle_batch` is
/// the unit that decides, so it is what these drive.
fn batch(server: &mut khora_lsp::Server, messages: &[Value]) -> Vec<Value> {
    let framed: Vec<String> =
        messages.iter().map(|m| serde_json::to_string(m).expect("json")).collect();
    server.handle_batch(&framed).expect("the server should not fail")
}

/// **Nine of ten answers are obsolete before they are written.** Typing ten
/// characters is ten `didChange` notifications; every edit has to be applied,
/// because each is measured against the one before it, but only the last is
/// worth type-checking.
#[test]
fn a_run_of_edits_is_reported_once() {
    let source = "module p::main;\npub fn go() -> Int { 0 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");
    let at = source.lines().nth(1).expect("a line").find('0').expect("the literal") as u32;

    let mut server = khora_lsp::Server::default();
    batch(&mut server, &[initialize(&w.root), did_open(&main, source)]);

    let edits: Vec<Value> = (0..6)
        .map(|i| did_change_range(&main, (1, at + i), (1, at + i), "7"))
        .collect();
    let replies = batch(&mut server, &edits);

    let reports: Vec<&Value> = replies
        .iter()
        .filter(|r| {
            r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        })
        .collect();
    assert_eq!(reports.len(), 1, "six edits, one report: {reports:?}");
}

/// Each edit is still applied, or the document would be wrong.
///
/// Proved by building a name out of the edits: `t()` becomes `total()`, which
/// resolves only if all four landed. A missed edit leaves an unresolved name,
/// which is a diagnostic rather than a silent difference.
#[test]
fn every_edit_in_the_run_is_still_applied() {
    let source = "module p::main;\npub fn total() -> Int { 1 }\npub fn go() -> Int { t() }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");
    let at = source.lines().nth(2).expect("a line").find("t()").expect("the call") as u32;

    let mut server = khora_lsp::Server::default();
    batch(&mut server, &[initialize(&w.root), did_open(&main, source)]);

    let edits: Vec<Value> = "otal"
        .chars()
        .enumerate()
        .map(|(i, letter)| {
            let column = at + 1 + i as u32;
            did_change_range(&main, (2, column), (2, column), &letter.to_string())
        })
        .collect();
    let replies = batch(&mut server, &edits);

    let left = replies
        .iter()
        .filter(|r| {
            r.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        })
        .filter_map(|r| r.pointer("/params/diagnostics")?.as_array().map(Vec::len))
        .next_back();
    assert_eq!(left, Some(0), "`total()` resolves only if every edit landed: {replies:?}");
}

/// **A cancel that arrives with the request it cancels is honoured.** In a
/// strictly serial loop it never could be: the cancel is always read after the
/// work it wanted to stop had already been done.
///
/// Driven through `handle_batch` for the reason the tests above it are: through
/// the socket, whether the cancel lands in the same batch as the request it
/// cancels is decided by how fast the reader thread ran, so a test that asserts
/// it there asserts a race. It passed nearly always, which is the worse way for
/// a race to fail.
#[test]
fn a_cancelled_request_is_answered_with_the_cancellation_error() {
    let source = "module p::main;\npub fn go() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let mut server = khora_lsp::Server::default();
    batch(&mut server, &[initialize(&w.root), did_open(&main, source)]);
    let replies = batch(
        &mut server,
        &[
            json!({
                "jsonrpc": "2.0", "id": 11, "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": url_of(&main) },
                    "position": { "line": 1, "character": 23 }
                }
            }),
            json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": 11 } }),
        ],
    );

    let reply = replies.iter().find(|r| r.get("id") == Some(&json!(11))).expect("an answer");
    assert_eq!(
        reply.pointer("/error/code"),
        Some(&json!(-32800)),
        "the protocol's RequestCancelled, so the client is not left waiting: {reply}"
    );
}

/// A request nobody cancelled is answered normally, even when a cancel for
/// something else arrives in the same batch.
#[test]
fn a_cancel_for_another_request_leaves_this_one_alone() {
    let source = "module p::main;\npub fn go() -> Int { 1 }\n";
    let w = workspace(&[("src/main.kh", source)]);
    let main = w.root.join("src/main.kh");

    let replies = session(&[
        initialize(&w.root),
        did_open(&main, source),
        json!({
            "jsonrpc": "2.0", "id": 12, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": url_of(&main) },
                "position": { "line": 1, "character": 23 }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": 999 } }),
        exit(),
    ]);

    let reply = replies.iter().find(|r| r.get("id") == Some(&json!(12))).expect("an answer");
    assert!(reply.get("error").is_none(), "this one was not cancelled: {reply}");
}

// --- the assists the diagnostics already half-write ------------------------

/// The diagnostic whose message contains `needle`, and the actions offered for
/// it, in one round trip each.
fn actions_for(root: &Path, file: &Path, text: &str, needle: &str) -> Vec<Value> {
    let reported = session(&[initialize(root), did_open(file, text), exit()]);
    let found = last_diagnostics(&reported)
        .iter()
        .find(|d| d.get("message").and_then(Value::as_str).is_some_and(|m| m.contains(needle)))
        .cloned()
        .unwrap_or_else(|| panic!("no diagnostic mentioning {needle:?}"));

    let replies = session(&[
        initialize(root),
        did_open(file, text),
        code_action(file, vec![found], 2),
        exit(),
    ]);
    result_of(&replies, 2).as_array().cloned().unwrap_or_default()
}

/// The `newText` of every edit an action carries.
fn edits_of(action: &Value) -> Vec<String> {
    action
        .pointer("/edit/changes")
        .and_then(Value::as_object)
        .map(|changes| {
            changes
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(|e| e.get("newText")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `` the call needs `!` `` names one edit in one place, so it is one.
#[test]
fn a_call_that_can_leave_the_function_is_offered_the_mark() {
    let text = "module main;\n\
pub type Broke = | Badly;\n\
fn risky() -> Int raises Broke { raise Broke::Badly }\n\
pub fn go() -> Int raises Broke {\n\
  risky()\n\
}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let actions = actions_for(&w.root, &file, text, "so the call needs `!`");
    let offered = actions
        .iter()
        .find(|a| a.get("title").and_then(Value::as_str).is_some_and(|t| t.contains("`!`")))
        .unwrap_or_else(|| panic!("no mark offered: {actions:?}"));
    assert_eq!(edits_of(offered), vec!["!".to_string()], "{offered}");
}

/// **A bare constructor name is a binding, not a match.** The checker names the
/// missing cases unqualified, and an arm reading `Green => ..` binds every
/// remaining value to a new local called `Green`. The program then compiles and
/// is wrong. So the qualification is copied from the arms already written.
#[test]
fn missing_arms_are_written_the_way_the_match_writes_them() {
    let text = "module main;\n\
pub type Colour = | Red | Green | Blue;\n\
pub fn name(c: Colour) -> String {\n\
  match c {\n\
    Colour::Red => \"red\",\n\
  }\n\
}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let actions = actions_for(&w.root, &file, text, "not exhaustive");
    let offered = actions
        .iter()
        .find(|a| a.get("title").and_then(Value::as_str).is_some_and(|t| t.contains("missing")))
        .unwrap_or_else(|| panic!("no arms offered: {actions:?}"));

    let written = edits_of(offered).join("");
    assert!(written.contains("Colour::Green =>"), "qualified like its neighbour: {written:?}");
    assert!(written.contains("Colour::Blue =>"), "and both of them: {written:?}");
    assert!(
        !written.contains("\n  Green"),
        "never bare, which would bind rather than match: {written:?}"
    );
}

/// Both missing cases in one action, rather than the same action twice with a
/// recompile between.
#[test]
fn every_missing_arm_is_written_at_once() {
    let text = "module main;\n\
pub type Colour = | Red | Green | Blue;\n\
pub fn name(c: Colour) -> String {\n\
  match c {\n\
    Colour::Red => \"red\",\n\
  }\n\
}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let actions = actions_for(&w.root, &file, text, "not exhaustive");
    let offered = actions
        .iter()
        .find(|a| a.get("title").and_then(Value::as_str).is_some_and(|t| t.contains("missing")))
        .expect("an action");
    assert_eq!(
        offered.get("title").and_then(Value::as_str),
        Some("Add the 2 missing arms"),
        "{offered}"
    );
}

/// The body is `()` rather than an invented `todo`, so the error that remains
/// is the type checker's own and names what the arm has to produce.
#[test]
fn a_written_arm_leaves_a_hole_the_checker_describes() {
    let text = "module main;\n\
pub type Colour = | Red | Green;\n\
pub fn name(c: Colour) -> String {\n\
  match c {\n\
    Colour::Red => \"red\",\n\
  }\n\
}\n";
    let w = workspace(&[("src/main.kh", text)]);
    let file = w.root.join("src/main.kh");

    let actions = actions_for(&w.root, &file, text, "not exhaustive");
    let offered = actions
        .iter()
        .find(|a| a.get("title").and_then(Value::as_str).is_some_and(|t| t.contains("missing")))
        .expect("an action");
    let written = edits_of(offered).join("");
    assert!(written.contains("=> todo(),"), "the case is marked, not answered: {written:?}");
}

// --- the lens for what a function absorbs ----------------------------------

/// Every code lens in a file, as (line, title).
fn lenses_in(replies: &[Value], id: i64) -> Vec<(u64, String)> {
    result_of(replies, id)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|l| {
            Some((
                l.pointer("/range/start/line")?.as_u64()?,
                l.pointer("/command/title")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

const ABSORBS: &str = "module p::main;\n\
import std::core::{print};\n\
\n\
pub type Broke = | Badly;\n\
\n\
fn risky() -> Int raises Broke { raise Broke::Badly }\n\
\n\
fn passes_it_on() -> Int raises Broke { risky()! }\n\
\n\
pub fn takes_it_on() -> Int {\n\
  risky()! catch { _ => 0 }\n\
}\n";

/// **A signature says what a function asks of its caller; nothing said what it
/// takes on itself.** `takes_it_on` catches the failure, so its signature
/// mentions no error at all, and that is the line worth marking.
#[test]
fn a_function_that_catches_says_so_in_a_lens() {
    let w = workspace(&[("src/main.kh", ABSORBS)]);
    let main = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&main, ABSORBS),
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": url_of(&main) } }
        }),
        exit(),
    ]);

    let lenses = lenses_in(&replies, 5);
    assert!(
        lenses.iter().any(|(line, title)| *line == 9 && title == "catches Broke"),
        "the function that catches: {lenses:?}"
    );
}

const INSTALLS: &str = "module p::main;\n\
\n\
pub effect Clock {\n\
  now: () -> Int,\n\
}\n\
\n\
fn stamp() -> Int with { clock: Clock } { clock.now() }\n\
\n\
fn passes_it_on() -> Int with { clock: Clock } { stamp() }\n\
\n\
pub fn takes_it_on() -> Int {\n\
  with { clock: handler for Clock { now: fn () => 0 } } { stamp() }\n\
}\n";

/// **The capability half of the same idea**, and the half the type checker
/// makes hardest to see: `requires` is what a call *still* owes, so inside the
/// `with` block it is empty, and the discharged capability is invisible at
/// exactly the call that discharged it. `CallRows::declared` keeps the
/// callee's own row from before that subtraction, and the difference is this.
#[test]
fn a_function_that_installs_a_capability_says_so_in_a_lens() {
    let w = workspace(&[("src/main.kh", INSTALLS)]);
    let main = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&main, INSTALLS),
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": url_of(&main) } }
        }),
        exit(),
    ]);

    let lenses = lenses_in(&replies, 5);
    assert!(
        lenses.iter().any(|(line, title)| *line == 10 && title == "installs { clock }"),
        "the function that installs: {lenses:?}"
    );
    // And not on the one that only passes the requirement outwards, whose
    // signature already says it.
    assert!(
        !lenses.iter().any(|(line, _)| *line == 8),
        "nothing on the function that declares it: {lenses:?}"
    );
}

/// A function that passes the failure on is absorbing nothing, and its
/// signature already says everything. A lens there would repeat the line above
/// it, which is the noise this deliberately does not add.
#[test]
fn a_function_that_declares_its_failure_gets_no_lens() {
    let w = workspace(&[("src/main.kh", ABSORBS)]);
    let main = w.root.join("src/main.kh");
    let replies = session(&[
        initialize(&w.root),
        did_open(&main, ABSORBS),
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": url_of(&main) } }
        }),
        exit(),
    ]);

    let lenses = lenses_in(&replies, 5);
    assert!(
        !lenses.iter().any(|(line, _)| *line == 7),
        "`passes_it_on` declares `raises Broke`: {lenses:?}"
    );
    assert!(
        !lenses.iter().any(|(line, _)| *line == 5),
        "and so does `risky`: {lenses:?}"
    );
}

