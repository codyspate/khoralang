//! A Model Context Protocol server, so an agent can learn Khora from the
//! compiler rather than from training data that does not exist.
//!
//! Every model has read a great deal of Rust and no Khora at all. An agent
//! asked to write some will produce something that looks right — Khora borrows
//! enough syntax from Rust and enough ideas from Effect that a plausible guess
//! is easy — and be wrong in ways it cannot detect. The interesting failures
//! are the ones with no analogue elsewhere: a capability that has to appear in
//! a `with` row, an error that has to appear in a `raises` row, `Share` on
//! anything crossing into a fiber.
//!
//! So the point of this server is not documentation. It is
//! [`tools::CHECK`]: **the agent writes Khora, the compiler answers, and the
//! agent learns from the answer.** That loop needs no training data and cannot
//! go stale, because the thing giving the answers is the thing that will
//! compile the code. Everything else here — the standard library's surface, the
//! grammar, the design notes — exists so the first guess is a good one.
//!
//! # Version
//!
//! `khora mcp` goes through the same toolchain shim every other subcommand
//! does, so a project pinning a version gets that version's compiler answering
//! these questions, and its `std`. The version is in the `initialize` reply.
//!
//! # Transport
//!
//! Newline-delimited JSON-RPC on stdin and stdout, which is what MCP's stdio
//! transport is — and is *not* what `khora-lsp` uses. The Language Server
//! Protocol frames with `Content-Length` headers. Two protocols, two framings,
//! one process model; the difference is small and silent, and getting it wrong
//! means a client that hangs on the first message.

#![deny(missing_docs)]

mod surface;
mod tools;

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde_json::{json, Value};

pub use surface::Entry;
// **The same index, for a person.** `khora_std_search` was the most accurate
// reference Khora had and the only one a human could not reach: four
// evaluators reading the website hit six APIs that did not exist in the
// compiler they had, and the agent-facing tool was the thing that unstuck the
// one who found it. Exported so `khora std search` can answer out of exactly
// the same data rather than out of a second copy that drifts.
pub use tools::{read_std_surface, search};

/// The MCP revision this speaks.
///
/// Answered verbatim rather than negotiated: a client asking for something else
/// gets told what this is, and can decide. Pretending to agree is worse.
const PROTOCOL: &str = "2024-11-05";

/// Runs until stdin closes.
pub fn serve(input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    let mut server = Server::new()?;
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line).context("reading a message")? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            // A malformed line is answered rather than fatal: the id is
            // unknown, so this is the one reply that has to guess at null.
            Err(e) => {
                let reply = error(Value::Null, -32700, &format!("not JSON: {e}"));
                writeln!(output, "{}", serde_json::to_string(&reply)?)?;
                output.flush()?;
                continue;
            }
        };
        if let Some(reply) = server.handle(&message) {
            writeln!(output, "{}", serde_json::to_string(&reply)?)?;
            output.flush()?;
        }
    }
}

/// Everything the server knows, which is a standard library and how to compile.
pub struct Server {
    /// `std`'s public surface, read once.
    surface: Vec<Entry>,
}

impl Server {
    /// Reads the standard library's surface, so a tool call can answer from it.
    pub fn new() -> Result<Server> {
        Ok(Server { surface: tools::read_std_surface() })
    }

    /// Answers one message, or not — a notification gets no reply.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        // No id means a notification, and a notification is never answered —
        // not even to say the method is unknown.
        let id = id?;

        Some(match method {
            "initialize" => ok(id, self.initialize()),
            "ping" => ok(id, json!({})),
            "tools/list" => ok(id, json!({ "tools": tools::describe() })),
            "tools/call" => match self.call(&params) {
                Ok(content) => ok(id, content),
                // **A tool failure is a result, not a protocol error.** An
                // agent can read and act on `isError` with the message in it;
                // a JSON-RPC error is a client-level failure it usually cannot
                // see. Diagnostics from `check` are the whole point, so they
                // must arrive as content.
                Err(why) => ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": format!("{why:#}") }],
                        "isError": true
                    }),
                ),
            },
            other => error(id, -32601, &format!("`{other}` is not implemented")),
        })
    }

    fn initialize(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "khora-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
            // Not part of the specification, and worth sending anyway: the
            // first thing an agent with no training data needs is to be told
            // that it has none.
            "instructions": tools::ORIENTATION,
        })
    }

    fn call(&self, params: &Value) -> Result<Value> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let text = tools::run(name, &arguments, &self.surface)?;
        Ok(json!({ "content": [{ "type": "text", "text": text }] }))
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
