//! A session, driven the way an agent's client would drive one.
//!
//! Newline-delimited JSON in, newline-delimited JSON out. The tests that matter
//! are the ones exercising the loop this server exists for: write Khora that is
//! wrong in a way a model would get wrong, and check that the compiler's answer
//! comes back in a form the agent can act on.

use serde_json::{json, Value};

/// Runs a script and returns the replies.
fn session(messages: &[Value]) -> Vec<Value> {
    let mut input = String::new();
    for message in messages {
        input.push_str(&serde_json::to_string(message).expect("json"));
        input.push('\n');
    }
    let mut output = Vec::new();
    khora_mcp::serve(&mut input.as_bytes(), &mut output).expect("the server should not fail");

    String::from_utf8(output)
        .expect("utf-8")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a reply that is JSON"))
        .collect()
}

fn initialize() -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })
}

fn call(id: u32, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
}

/// The text of a `tools/call` reply.
fn text(reply: &Value) -> String {
    reply
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no content in {reply}"))
        .to_string()
}

fn find(replies: &[Value], id: u32) -> &Value {
    replies.iter().find(|r| r.get("id") == Some(&json!(id))).expect("a reply")
}

// --- the handshake ---------------------------------------------------------

#[test]
fn initialize_says_what_this_is() {
    let replies = session(&[initialize()]);
    let result = replies[0].pointer("/result").expect("a result");
    assert_eq!(result.pointer("/serverInfo/name").and_then(Value::as_str), Some("khora-mcp"));
    assert!(result.get("protocolVersion").is_some(), "{result}");
    assert!(result.pointer("/capabilities/tools").is_some(), "{result}");
}

/// The first thing an agent with no training data needs is to be told it has
/// none, and that there is a compiler here to ask.
#[test]
fn initialize_tells_the_agent_it_does_not_know_this_language() {
    let replies = session(&[initialize()]);
    let instructions = replies[0]
        .pointer("/result/instructions")
        .and_then(Value::as_str)
        .expect("instructions");
    assert!(instructions.contains("not in your training data"), "{instructions}");
    assert!(instructions.contains("khora_check"), "{instructions}");
    assert!(instructions.contains("with {"), "capabilities should be named: {instructions}");
    assert!(instructions.contains("raises"), "failures should be named: {instructions}");
}

#[test]
fn every_tool_has_a_schema() {
    let replies = session(&[json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" })]);
    let tools = find(&replies, 9).pointer("/result/tools").and_then(Value::as_array).expect("tools");
    assert!(tools.len() >= 5, "{tools:?}");
    for tool in tools {
        assert!(tool.get("name").is_some(), "{tool}");
        assert!(tool.get("description").is_some(), "{tool}");
        assert!(tool.pointer("/inputSchema/type").is_some(), "{tool}");
    }
}

/// A notification has no id and must never be answered — not even to say the
/// method is unknown, which would be a reply to something that asked nothing.
#[test]
fn a_notification_gets_no_reply() {
    let replies = session(&[
        initialize(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ]);
    assert_eq!(replies.len(), 1, "{replies:?}");
}

#[test]
fn an_unknown_method_is_an_error_not_a_silence() {
    let replies = session(&[json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/list" })]);
    assert_eq!(find(&replies, 5).pointer("/error/code"), Some(&json!(-32601)));
}

#[test]
fn a_line_that_is_not_json_is_answered_rather_than_fatal() {
    let mut input = String::from("this is not json\n");
    input.push_str(&serde_json::to_string(&initialize()).unwrap());
    input.push('\n');

    let mut output = Vec::new();
    khora_mcp::serve(&mut input.as_bytes(), &mut output).expect("it should keep going");
    let text = String::from_utf8(output).expect("utf-8");
    assert!(text.contains("-32700"), "{text}");
    assert!(text.contains("khora-mcp"), "and the next message still works: {text}");
}

// --- the loop this exists for ----------------------------------------------

#[test]
fn checking_correct_khora_says_so() {
    let replies =
        session(&[initialize(), call(2, "khora_check", json!({ "source": "module m;\n" }))]);
    assert!(text(find(&replies, 2)).contains("No errors"), "{:?}", text(find(&replies, 2)));
}

/// The mistake a model actually makes: using a capability without declaring it,
/// because no other language it knows requires that.
#[test]
fn a_missing_capability_comes_back_as_a_diagnostic() {
    let source = "module m;\n\n\
                  import std::random::{Random};\n\n\
                  pub fn roll() -> Int {\n  rng.int()\n}\n";
    let replies = session(&[initialize(), call(2, "khora_check", json!({ "source": source }))]);
    let answer = text(find(&replies, 2));
    assert!(answer.contains("error"), "{answer}");
    assert!(answer.contains("rng"), "it should name what is wrong: {answer}");
    assert!(answer.contains("5:") || answer.contains("6:"), "and where: {answer}");
}

/// And the same code with the capability declared compiles — which is what
/// makes the diagnostic teachable rather than just discouraging.
#[test]
fn the_same_code_with_the_capability_declared_compiles() {
    let source = "module m;\n\n\
                  import std::random::{Random};\n\n\
                  pub fn roll() -> Int with { rng: Random } {\n  rng.int()\n}\n";
    let replies = session(&[initialize(), call(2, "khora_check", json!({ "source": source }))]);
    assert!(text(find(&replies, 2)).contains("No errors"), "{}", text(find(&replies, 2)));
}

#[test]
fn a_syntax_error_says_it_does_not_parse() {
    let replies = session(&[
        initialize(),
        call(2, "khora_check", json!({ "source": "module m;\nfn f( -> Int { 1 }\n" })),
    ]);
    assert!(text(find(&replies, 2)).contains("does not parse"), "{}", text(find(&replies, 2)));
}

/// A tool failure has to arrive as content with `isError`, not as a JSON-RPC
/// error — an agent can read the first and usually cannot see the second.
#[test]
fn a_bad_argument_comes_back_as_readable_content() {
    let replies = session(&[initialize(), call(2, "khora_check", json!({}))]);
    let reply = find(&replies, 2);
    assert_eq!(reply.pointer("/result/isError"), Some(&json!(true)), "{reply}");
    assert!(text(reply).contains("source"), "{}", text(reply));
    assert!(reply.get("error").is_none(), "not a protocol error: {reply}");
}

// --- finding what exists ---------------------------------------------------

#[test]
fn searching_finds_a_standard_library_item_with_its_documentation() {
    let replies =
        session(&[initialize(), call(2, "khora_std_search", json!({ "query": "nursery" }))]);
    let answer = text(find(&replies, 2));
    assert!(answer.contains("Nursery"), "{answer}");
    assert!(answer.contains("std::core"), "the module is how you import it: {answer}");
}

/// The most valuable answer this can give: no, that does not exist, and do not
/// assume it does because another language has it.
#[test]
fn searching_for_something_absent_says_so_plainly() {
    let replies = session(&[
        initialize(),
        call(2, "khora_std_search", json!({ "query": "unwrap_or_default" })),
    ]);
    let answer = text(find(&replies, 2));
    assert!(answer.contains("Nothing in `std` matches"), "{answer}");
    assert!(answer.contains("Do not assume"), "{answer}");
}

#[test]
fn the_grammar_comes_back_whole() {
    let replies = session(&[initialize(), call(2, "khora_grammar", json!({}))]);
    let answer = text(find(&replies, 2));
    assert!(answer.contains("LambdaExpr"), "{}", &answer[..answer.len().min(200)]);
}

#[test]
fn design_notes_can_be_listed_and_read() {
    let replies = session(&[
        initialize(),
        call(2, "khora_design_doc", json!({})),
        call(3, "khora_design_doc", json!({ "name": "effects" })),
    ]);
    assert!(text(find(&replies, 2)).contains("effects"), "{}", text(find(&replies, 2)));
    assert!(text(find(&replies, 3)).len() > 500, "the note itself should come back");
}

/// This server answers whatever an agent asks it, so a name with a separator in
/// it must not reach outside `docs/`.
#[test]
fn a_document_name_cannot_escape_the_docs_directory() {
    for name in ["../../Cargo", "..\\..\\Cargo", "design/effects"] {
        let replies =
            session(&[initialize(), call(2, "khora_design_doc", json!({ "name": name }))]);
        let reply = find(&replies, 2);
        assert_eq!(reply.pointer("/result/isError"), Some(&json!(true)), "{name}: {reply}");
    }
}

#[test]
fn formatting_returns_canonical_source() {
    let replies = session(&[
        initialize(),
        call(2, "khora_format", json!({ "source": "module m;\nfn f() {    1 }\n" })),
    ]);
    let answer = text(find(&replies, 2));
    assert!(!answer.contains("    1"), "it should be tidied: {answer:?}");
}

/// **An agent's formatter has to agree with the project's.**
///
/// This formatted with the defaults while `khora fmt` and the language server
/// both read `[fmt]` from the manifest, so an agent editing a four-space
/// project produced two-space output that the project's own `--check` then
/// rejected. `path` says which package the source belongs to.
#[test]
fn formatting_honours_the_packages_own_settings() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let root = tmp.path();
    std::fs::write(
        root.join("khora.toml"),
        "[package]\nname = \"wide\"\nversion = \"0.1.0\"\n\n[fmt]\nindent-width = 4\n",
    )
    .expect("writing a manifest");

    let source = "module m;\npub fn f() -> Int {\n1\n}\n";
    let replies = session(&[
        initialize(),
        call(2, "khora_format", json!({ "source": source })),
        call(3, "khora_format", json!({ "source": source, "path": root.to_str().expect("a path") })),
    ]);

    let bare = text(find(&replies, 2));
    let in_package = text(find(&replies, 3));
    assert!(bare.contains("\n  1"), "with no package, the default two spaces: {bare:?}");
    assert!(
        in_package.contains("\n    1"),
        "inside a package asking for four, four: {in_package:?}"
    );
}
