//! A language server over the salsa database.
//!
//! Roadmap phase 10.4. The compiler is already a set of incremental queries, so
//! this is mostly a translation layer: an edit becomes `set_text`, and salsa
//! decides what that invalidates.
//!
//! **That is the whole bet, and phase 10.0 is why it is safe to make.** An
//! editor asks the compiler a question after every keystroke, so a keystroke
//! that invalidates the world is a language server that recompiles the world.
//! It turned out that one did: `ItemMap` carried a span per item, so typing a
//! character in the first function of a file shifted every declaration below
//! it, and the module graph and every importer's scope were rebuilt.
//! `khora-hir/tests/incremental.rs` is what holds that closed now.
//!
//! # Shape
//!
//! One thread, one message at a time. No request is slow enough yet to need
//! cancellation or a worker pool, and a single thread means the database has
//! one owner and no locking — which is worth keeping until something measured
//! says otherwise. `serve` is generic over its streams so a test can drive it
//! with two buffers; an editor is a bad test harness.
//!
//! # What it answers
//!
//! Diagnostics, and hover. Both come from queries that already existed:
//! `khora_db::parse`, `khora_types::diagnostics`, `khora_lint::findings`, and
//! the checker's `BodyTypes`. Completion, rename and capability inlay hints are
//! the roadmap's list and are not here — each needs an index this does not
//! build yet, and a half-answering completion is worse than none.

#![deny(missing_docs)]

mod position;
mod transport;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use khora_db::{KhoraDatabase, Setter, SourceFile, SourceRoot};
use khora_manifest::LintLevel;
use lsp_types::{
    Diagnostic, DiagnosticSeverity, Hover, HoverContents, HoverProviderCapability,
    InitializeResult, MarkupContent, MarkupKind, PositionEncodingKind, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use serde_json::{json, Value};
use url::Url;

pub use position::{Encoding, LineIndex};
pub use transport::{read_message, write_message};

/// Runs until the client says to stop.
pub fn serve(input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    let mut server = Server::default();
    while let Some(text) = read_message(input)? {
        let message: Value = serde_json::from_str(&text).context("a message that is JSON")?;
        for reply in server.handle(&message) {
            write_message(output, &serde_json::to_string(&reply)?)?;
        }
        if server.finished {
            break;
        }
    }
    Ok(())
}

/// Everything the server knows.
pub struct Server {
    db: KhoraDatabase,
    /// Every file in the project, by the path the database knows it as.
    files: HashMap<PathBuf, SourceFile>,
    /// Line boundaries per open document, for translating positions.
    lines: HashMap<Url, LineIndex>,
    /// How this client counts a character offset.
    encoding: Encoding,
    /// How loud each lint is, from the workspace's manifest.
    levels: HashMap<String, LintLevel>,
    /// Set by `exit`, and by `shutdown` followed by a closed stream.
    pub finished: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            db: KhoraDatabase::new(),
            files: HashMap::new(),
            lines: HashMap::new(),
            encoding: Encoding::default(),
            levels: HashMap::new(),
            finished: false,
        }
    }
}

impl Server {
    /// Answers one message with zero or more of its own.
    ///
    /// A notification produces no reply and may still produce a notification of
    /// our own — `didChange` answers with `publishDiagnostics` — which is why
    /// this returns a list rather than an `Option`.
    pub fn handle(&mut self, message: &Value) -> Vec<Value> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match (method, id) {
            ("initialize", Some(id)) => vec![ok(id, self.initialize(&params))],
            ("shutdown", Some(id)) => vec![ok(id, Value::Null)],
            ("textDocument/hover", Some(id)) => {
                vec![ok(id, self.hover(&params).map_or(Value::Null, to_value))]
            }
            ("exit", _) => {
                self.finished = true;
                Vec::new()
            }
            ("textDocument/didOpen", _) => self.opened(&params),
            ("textDocument/didChange", _) => self.changed(&params),
            ("textDocument/didClose", _) => {
                if let Some(url) = url_of(&params) {
                    self.lines.remove(&url);
                }
                Vec::new()
            }
            // A request we do not implement must still be answered, or the
            // client waits forever. A notification we do not implement must be
            // ignored in silence, which is what the protocol says.
            (_, Some(id)) => vec![error(id, -32601, &format!("`{method}` is not implemented"))],
            _ => Vec::new(),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        // The client lists what it can count in, best first, and we take the
        // first we understand. Saying nothing means UTF-16, which is the
        // protocol's default and the expensive one.
        self.encoding = params
            .pointer("/capabilities/general/positionEncodings")
            .and_then(Value::as_array)
            .and_then(|offered| {
                offered.iter().filter_map(Value::as_str).find_map(|name| match name {
                    "utf-8" => Some(Encoding::Utf8),
                    "utf-16" => Some(Encoding::Utf16),
                    _ => None,
                })
            })
            .unwrap_or(Encoding::Utf16);

        if let Some(root) = workspace_root(params) {
            self.load(&root);
        }

        let result = InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(match self.encoding {
                    Encoding::Utf8 => PositionEncodingKind::UTF8,
                    Encoding::Utf16 => PositionEncodingKind::UTF16,
                }),
                // Full rather than incremental. The parser is fast and the
                // database backdates a reparse that produces the same tree, so
                // the saving from incremental sync is a few microseconds
                // against a whole class of desynchronisation bug.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "khora-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        };
        to_value(result)
    }

    /// Reads every `.kh` file under `root`, plus the standard library.
    ///
    /// All of it at once, because cross-file resolution needs one `SourceRoot`
    /// and a file that arrives later would not be in it. An editor opens one
    /// file and expects to be told about a name defined in another.
    fn load(&mut self, root: &Path) {
        let mut paths = Vec::new();
        gather(root, &mut paths);
        if let Some(std) = khora_db::standard_library() {
            gather(&std, &mut paths);
        }

        let mut files = Vec::new();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let file = SourceFile::new(&self.db, path.clone(), text);
            self.files.insert(path, file);
            files.push(file);
        }
        SourceRoot::new(&self.db, files);
        self.levels = lint_levels(root);
    }

    fn opened(&mut self, params: &Value) -> Vec<Value> {
        let Some(url) = params.pointer("/textDocument/uri").and_then(Value::as_str) else {
            return Vec::new();
        };
        let text = params
            .pointer("/textDocument/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.edit(url, text)
    }

    fn changed(&mut self, params: &Value) -> Vec<Value> {
        let Some(url) = params.pointer("/textDocument/uri").and_then(Value::as_str) else {
            return Vec::new();
        };
        // Full sync, so the last change carries the whole document.
        let Some(text) = params
            .pointer("/contentChanges")
            .and_then(Value::as_array)
            .and_then(|changes| changes.last())
            .and_then(|change| change.get("text"))
            .and_then(Value::as_str)
        else {
            return Vec::new();
        };
        self.edit(url, text.to_string())
    }

    /// Records new text for a document and says what is wrong with it.
    fn edit(&mut self, url: &str, text: String) -> Vec<Value> {
        let Ok(parsed) = Url::parse(url) else { return Vec::new() };
        let Ok(path) = parsed.to_file_path() else { return Vec::new() };

        self.lines.insert(parsed.clone(), LineIndex::new(&text));

        // A file the workspace scan did not see — a scratch buffer, or one
        // created since — joins the root rather than being ignored.
        match self.files.get(&path).copied() {
            Some(file) => {
                file.set_text(&mut self.db).to(text);
            }
            None => {
                let file = SourceFile::new(&self.db, path.clone(), text);
                self.files.insert(path.clone(), file);

                // `SourceRoot` is a salsa *singleton*: constructing a second
                // one panics. Adding a file is an ordinary input change, which
                // is the whole reason the file set is an input rather than a
                // filesystem walk — it invalidates exactly the queries that
                // depended on which files exist.
                let mut all: Vec<SourceFile> = self.files.values().copied().collect();
                all.sort_by_key(|f| f.path(&self.db).clone());
                match khora_db::source_root(&self.db) {
                    Some(root) => {
                        root.set_files(&mut self.db).to(all);
                    }
                    None => {
                        SourceRoot::new(&self.db, all);
                    }
                }
            }
        }

        let Some(file) = self.files.get(&path).copied() else { return Vec::new() };
        vec![notification(
            "textDocument/publishDiagnostics",
            json!({ "uri": url, "diagnostics": self.diagnostics(parsed, file) }),
        )]
    }

    /// Everything wrong with one file, as the client wants it.
    fn diagnostics(&self, url: Url, file: SourceFile) -> Vec<Diagnostic> {
        let Some(index) = self.lines.get(&url) else { return Vec::new() };
        let mut out = Vec::new();

        let parse = khora_db::parse(&self.db, file);
        for error in parse.errors() {
            out.push(Diagnostic {
                range: index.range(error.range, self.encoding),
                severity: Some(DiagnosticSeverity::ERROR),
                message: error.message.clone(),
                ..Diagnostic::default()
            });
        }
        // Type errors invented on top of a syntax error are noise, and the
        // command-line checker takes the same view.
        if !out.is_empty() {
            return out;
        }

        for error in khora_types::diagnostics(&self.db, file) {
            out.push(Diagnostic {
                range: index.range(error.range, self.encoding),
                severity: Some(DiagnosticSeverity::ERROR),
                message: error.message.clone(),
                ..Diagnostic::default()
            });
        }
        if !out.is_empty() {
            return out;
        }

        for finding in khora_lint::findings(&self.db, file) {
            let level = self.levels.get(finding.lint).copied().unwrap_or(LintLevel::Warn);
            if level == LintLevel::Allow {
                continue;
            }
            out.push(Diagnostic {
                range: index.range(finding.range, self.encoding),
                severity: Some(match level {
                    LintLevel::Deny => DiagnosticSeverity::ERROR,
                    _ => DiagnosticSeverity::WARNING,
                }),
                code: Some(lsp_types::NumberOrString::String(finding.lint.to_string())),
                message: finding.message.clone(),
                ..Diagnostic::default()
            });
        }
        out
    }

    /// The type of the smallest expression covering the cursor.
    ///
    /// Smallest because a cursor inside `f(x)` is more usefully told about `x`
    /// than about the call — the innermost thing under a cursor is the thing
    /// being pointed at.
    fn hover(&self, params: &Value) -> Option<Hover> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;

        let position: lsp_types::Position =
            serde_json::from_value(params.get("position")?.clone()).ok()?;
        let offset = index.offset(position, self.encoding);

        let checked = khora_types::checked(&self.db, file);
        let mut best: Option<(text_size::TextRange, String)> = None;

        for (name, body) in khora_hir::body::bodies(&self.db, file) {
            let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t)
            else {
                continue;
            };
            for (id, _) in body.exprs() {
                let range = body.range(id);
                if !range.contains(offset) {
                    continue;
                }
                let ty = types.of(id);
                if matches!(ty, khora_types::Type::Unknown) {
                    continue;
                }
                let smaller = best.as_ref().is_none_or(|(seen, _)| range.len() < seen.len());
                if smaller {
                    best = Some((range, ty.to_string()));
                }
            }
        }

        let (range, ty) = best?;
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```khora\n{ty}\n```"),
            }),
            range: Some(index.range(range, self.encoding)),
        })
    }
}

/// Every `.kh` file under a directory, skipping what a build skips.
fn gather(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target" || n == ".git") {
                continue;
            }
            gather(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh")
            && khora_db::selected_for_target(&path, khora_db::host_target())
        {
            out.push(path);
        }
    }
}

/// The `[lints]` levels for a workspace, or the defaults.
fn lint_levels(root: &Path) -> HashMap<String, LintLevel> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("khora.toml")) else { return out };
    let Ok(parsed) = khora_manifest::Manifest::parse(&text) else { return out };
    for (name, lint) in &parsed.manifest.lints {
        out.insert(name.clone(), lint.level);
    }
    out
}

/// Where the workspace is, from whichever of the three spellings the client
/// used. `rootUri` and `rootPath` are deprecated and still what several
/// editors send.
fn workspace_root(params: &Value) -> Option<PathBuf> {
    if let Some(folders) = params.get("workspaceFolders").and_then(Value::as_array) {
        if let Some(first) = folders.first().and_then(|f| f.get("uri")).and_then(Value::as_str) {
            if let Some(path) = Url::parse(first).ok().and_then(|u| u.to_file_path().ok()) {
                return Some(path);
            }
        }
    }
    if let Some(uri) = params.get("rootUri").and_then(Value::as_str) {
        if let Some(path) = Url::parse(uri).ok().and_then(|u| u.to_file_path().ok()) {
            return Some(path);
        }
    }
    params.get("rootPath").and_then(Value::as_str).map(PathBuf::from)
}

fn url_of(params: &Value) -> Option<Url> {
    Url::parse(params.pointer("/textDocument/uri")?.as_str()?).ok()
}

fn to_value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}
