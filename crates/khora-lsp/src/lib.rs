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
//! Diagnostics, hover, formatting, go to definition, find references, rename,
//! and symbols. All of them come from things that already existed:
//! `khora_db::parse`, `khora_types::diagnostics`, `khora_lint::findings`, the
//! checker's `BodyTypes`, `khora_fmt`, `khora_hir::resolve_path` and
//! `khora_hir::item_map`.
//!
//! **Rename covers locals only**, and refuses a declaration with a reason
//! rather than editing one badly — `references` has the argument. Completion
//! and capability inlay hints are not here at all.

#![deny(missing_docs)]

mod completion;
mod definition;
mod fixes;
mod hints;
mod position;
mod references;
mod semantic;
mod signature;
mod symbols;
mod transport;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use khora_db::{KhoraDatabase, Setter, SourceFile, SourceRoot};
use khora_manifest::LintLevel;
use lsp_types::{
    Diagnostic, DiagnosticSeverity, Hover, HoverContents, HoverProviderCapability,
    InitializeResult, MarkupContent, MarkupKind, OneOf, Position, PositionEncodingKind, Range,
    ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
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
            ("textDocument/formatting", Some(id)) => {
                vec![ok(id, self.formatting(&params).map_or(Value::Null, to_value))]
            }
            ("textDocument/definition", Some(id)) => {
                vec![ok(id, self.definition(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/signatureHelp", Some(id)) => {
                vec![ok(id, self.signature_help(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/codeLens", Some(id)) => {
                vec![ok(id, self.code_lenses(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/codeAction", Some(id)) => {
                vec![ok(id, self.code_actions(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/inlayHint", Some(id)) => {
                vec![ok(id, self.inlay_hints(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/semanticTokens/full", Some(id)) => {
                vec![ok(id, self.semantic_tokens(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/completion", Some(id)) => {
                vec![ok(id, self.completion(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/references", Some(id)) => {
                vec![ok(id, self.references(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/documentSymbol", Some(id)) => {
                vec![ok(id, self.document_symbols(&params).unwrap_or(Value::Null))]
            }
            ("workspace/symbol", Some(id)) => {
                vec![ok(id, self.workspace_symbols(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/prepareRename", Some(id)) => match self.prepare_rename(&params) {
                Ok(value) => vec![ok(id, value)],
                // **A refusal, not an empty answer.** `null` from
                // `prepareRename` makes VS Code say "the element can't be
                // renamed" with no reason; an error carries the sentence
                // explaining which part is missing.
                Err(why) => vec![error(id, -32803, &why)],
            },
            ("textDocument/rename", Some(id)) => match self.rename(&params) {
                Ok(value) => vec![ok(id, value)],
                Err(why) => vec![error(id, -32803, &why)],
            },
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
                document_formatting_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                completion_provider: Some(lsp_types::CompletionOptions {
                    // The two characters that change what is on offer. Without
                    // them an editor only asks after a letter, and `s.` with
                    // nothing typed after it -- which is when somebody wants
                    // the list most -- would offer nothing.
                    trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                    ..Default::default()
                }),
                semantic_tokens_provider: Some(
                    lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                        lsp_types::SemanticTokensOptions {
                            legend: lsp_types::SemanticTokensLegend {
                                token_types: semantic::TOKEN_TYPES
                                    .iter()
                                    .map(|name| lsp_types::SemanticTokenType::new(name))
                                    .collect(),
                                token_modifiers: semantic::TOKEN_MODIFIERS
                                    .iter()
                                    .map(|name| lsp_types::SemanticTokenModifier::new(name))
                                    .collect(),
                            },
                            // Whole-document only. Range and delta are both
                            // optimisations for a file large enough to notice,
                            // and neither is worth a second code path until
                            // something measured says so.
                            full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                            range: Some(false),
                            ..Default::default()
                        },
                    ),
                ),
                code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
                code_lens_provider: Some(lsp_types::CodeLensOptions { resolve_provider: Some(false) }),
                signature_help_provider: Some(lsp_types::SignatureHelpOptions {
                    // `(` opens the popup and `,` moves it to the next
                    // parameter. Without the comma it would show parameter one
                    // for the whole call.
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    ..Default::default()
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                // `prepareProvider`, so an editor asks before it opens the box
                // and hears the refusal as a message rather than as an edit
                // that does nothing.
                rename_provider: Some(OneOf::Right(lsp_types::RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
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

    /// Where the path under the cursor is declared.
    ///
    /// **The range comes back in the *defining* file's own coordinates**, which
    /// is why this builds a `LineIndex` for it rather than reusing the one for
    /// the file the request came from. Jumping across files is the whole point,
    /// and a byte offset read against the wrong text lands somewhere plausible
    /// and wrong — the worst kind of wrong for a feature whose only job is to
    /// take you somewhere.
    ///
    /// A file the editor has never opened has no cached index, so the text
    /// comes from the database, which is the same text every other query is
    /// answering about.
    fn definition(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;

        let position: lsp_types::Position =
            serde_json::from_value(params.get("position")?.clone()).ok()?;
        let offset = index.offset(position, self.encoding);

        let root = khora_db::source_root(&self.db)?;
        let found = definition::at(&self.db, root, file, offset)?;

        let target_path = found.file.path(&self.db).clone();
        let target_url = Url::from_file_path(&target_path).ok()?;
        let target_index = LineIndex::new(found.file.text(&self.db));

        // Built as JSON rather than `lsp_types::Location`, because that
        // carries the crate's own `Uri` and everything else here — starting
        // with `publishDiagnostics` — already speaks `url::Url`. One
        // conversion at the edge beats two URL types in the same file.
        Some(json!({
            "uri": target_url.as_str(),
            "range": Range {
                start: target_index.position(found.range.start(), self.encoding),
                end: target_index.position(found.range.end(), self.encoding),
            },
        }))
    }

    /// Every span in the file the compiler can classify, as the protocol wants
    /// it: five integers each, positions relative to the token before.
    ///
    /// **Relative encoding is the format, not a compression trick.** A token is
    /// `deltaLine, deltaStart, length, type, modifiers`, where `deltaStart` is
    /// relative to the previous token only when they share a line. Getting that
    /// reset wrong shifts every colour after it by a column, which looks like a
    /// highlighting bug and is an arithmetic one.
    fn semantic_tokens(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let root = khora_db::source_root(&self.db)?;
        let index = LineIndex::new(file.text(&self.db));

        let mut data: Vec<u32> = Vec::new();
        let mut previous: Option<Position> = None;

        for token in semantic::tokens(&self.db, root, file) {
            let start = index.position(token.range.start(), self.encoding);
            let end = index.position(token.range.end(), self.encoding);
            // A token that spans a line has no length the protocol can carry.
            // No name does, so this is a guard rather than a case.
            if end.line != start.line {
                continue;
            }
            let length = end.character.saturating_sub(start.character);
            if length == 0 {
                continue;
            }

            let (delta_line, delta_start) = match previous {
                Some(before) if before.line == start.line => {
                    (0, start.character - before.character)
                }
                Some(before) => (start.line - before.line, start.character),
                None => (start.line, start.character),
            };
            data.extend_from_slice(&[delta_line, delta_start, length, token.kind, token.modifiers]);
            previous = Some(start);
        }

        Some(json!({ "data": data }))
    }

    /// The parameters of the call being typed.
    fn signature_help(&self, params: &Value) -> Option<Value> {
        let (file, offset) = self.locate(params)?;
        let root = khora_db::source_root(&self.db)?;
        let help = signature::at(&self.db, root, file, offset)?;

        let spans: Vec<Value> = help
            .spans()
            .into_iter()
            .map(|(start, end)| json!({ "label": [start, end] }))
            .collect();

        Some(json!({
            "signatures": [{
                "label": help.label(),
                "parameters": spans,
            }],
            "activeSignature": 0,
            // Clamped, because a call with more arguments than parameters is
            // an error the checker reports and not a reason to point at a
            // parameter that does not exist.
            "activeParameter": help.active.min(help.parameters.len().saturating_sub(1)),
        }))
    }

    /// A "Run test" above each `test` block.
    ///
    /// The command is the extension's, not the server's: a language server
    /// cannot run anything, and should not — what it can do is say *where* the
    /// runnable things are and what to call them. `khora test --filter` is
    /// what the extension shells out to, which is the same command a person
    /// would type.
    fn code_lenses(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = LineIndex::new(file.text(&self.db));
        let map = khora_hir::item_map(&self.db, file);

        let out: Vec<Value> = map
            .tests
            .iter()
            .map(|test| {
                let start = index.position(test.range.start(), self.encoding);
                json!({
                    "range": { "start": start, "end": start },
                    "command": {
                        "title": "▶ Run",
                        "command": "khora.runTest",
                        // The directory, so the extension runs in the package
                        // rather than wherever the editor happened to be, and
                        // the name, which is what `--filter` matches.
                        "arguments": [test.name, path.parent().map(|p| p.to_string_lossy().to_string())],
                    },
                })
            })
            .collect();
        Some(Value::Array(out))
    }

    /// The edits offered for the diagnostics under the cursor.
    ///
    /// **The client sends the diagnostics back**, in `context.diagnostics`, so
    /// there is no need to recompute them and no risk of offering a fix for a
    /// diagnostic the editor has already cleared. What this adds is the source
    /// each one covers, which is what lets a replacement keep the part of the
    /// line it is not changing.
    fn code_actions(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;
        let text = file.text(&self.db);

        let reported = params
            .pointer("/context/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut out = Vec::new();
        for diagnostic in reported {
            let Some(range) = range_of(&diagnostic, index, self.encoding) else { continue };
            let message = diagnostic.get("message").and_then(Value::as_str).unwrap_or_default();
            let code = diagnostic.get("code").and_then(Value::as_str);
            let covered = text.get(usize::from(range.start())..usize::from(range.end()))?;

            for fix in fixes::for_diagnostic(message, code, range, covered) {
                out.push(json!({
                    "title": fix.title,
                    "kind": "quickfix",
                    // The diagnostic it answers, so an editor can group the
                    // action with the squiggle it belongs to.
                    "diagnostics": [diagnostic],
                    "edit": { "changes": { url.as_str(): [{
                        "range": Range {
                            start: index.position(fix.range.start(), self.encoding),
                            end: index.position(fix.range.end(), self.encoding),
                        },
                        "newText": fix.replacement,
                    }]}},
                }));
            }
        }
        Some(Value::Array(out))
    }

    /// What each call costs, shown after the call.
    ///
    /// The request carries a range — an editor asks about the part of the file
    /// on screen — and this ignores it and answers for the whole document.
    /// Computing the hints costs one `checked` query, which salsa has already
    /// done for diagnostics, so filtering would be work to save nothing.
    fn inlay_hints(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = LineIndex::new(file.text(&self.db));

        let out: Vec<Value> = hints::in_file(&self.db, file)
            .into_iter()
            .map(|hint| {
                json!({
                    "position": index.position(hint.at, self.encoding),
                    "label": format!("  {}", hint.label),
                    // `Type` is 1: this is a hint about what a call needs, not
                    // about a parameter.
                    "kind": 1,
                    "paddingLeft": true,
                    "tooltip": hint.detail,
                })
            })
            .collect();
        Some(Value::Array(out))
    }

    /// What could be written at the cursor.
    fn completion(&self, params: &Value) -> Option<Value> {
        let (file, offset) = self.locate(params)?;
        let root = khora_db::source_root(&self.db)?;

        let items: Vec<Value> = completion::at(&self.db, root, file, offset)
            .into_iter()
            .map(|candidate| {
                json!({
                    "label": candidate.label,
                    "kind": candidate.kind,
                    "detail": candidate.detail,
                })
            })
            .collect();
        Some(Value::Array(items))
    }

    /// Every mention of the thing under the cursor.
    fn references(&self, params: &Value) -> Option<Value> {
        let (file, offset) = self.locate(params)?;
        let root = khora_db::source_root(&self.db)?;
        let include = params
            .pointer("/context/includeDeclaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let found = references::at(&self.db, root, file, offset, include)?;
        let mut out = Vec::new();
        for (each, ranges) in found.sites {
            let index = LineIndex::new(each.text(&self.db));
            let Ok(url) = Url::from_file_path(each.path(&self.db)) else { continue };
            for range in ranges {
                out.push(json!({
                    "uri": url.as_str(),
                    "range": Range {
                        start: index.position(range.start(), self.encoding),
                        end: index.position(range.end(), self.encoding),
                    },
                }));
            }
        }
        Some(Value::Array(out))
    }

    /// Whether a rename may proceed, and over what.
    fn prepare_rename(&self, params: &Value) -> Result<Value, String> {
        let Some((file, offset)) = self.locate(params) else {
            return Ok(Value::Null);
        };
        let Some(root) = khora_db::source_root(&self.db) else { return Ok(Value::Null) };
        let url = url_of(params).ok_or_else(|| "no document".to_string())?;
        let index = self.lines.get(&url).ok_or_else(|| "that file is not open".to_string())?;

        match references::renameable(&self.db, root, file, offset) {
            references::Renameable::Local { name, ranges } => {
                // The range under the cursor is what the editor pre-fills.
                let here = ranges
                    .iter()
                    .find(|r| r.contains_inclusive(offset))
                    .copied()
                    .unwrap_or_else(|| ranges[0]);
                Ok(json!({
                    "range": Range {
                        start: index.position(here.start(), self.encoding),
                        end: index.position(here.end(), self.encoding),
                    },
                    "placeholder": name,
                }))
            }
            references::Renameable::Refused(why) => Err(why.to_string()),
            references::Renameable::Nothing => Ok(Value::Null),
        }
    }

    /// The edits a rename would make.
    fn rename(&self, params: &Value) -> Result<Value, String> {
        let new_name =
            params.get("newName").and_then(Value::as_str).ok_or("no new name given")?;
        let Some((file, offset)) = self.locate(params) else { return Ok(Value::Null) };
        let Some(root) = khora_db::source_root(&self.db) else { return Ok(Value::Null) };
        let url = url_of(params).ok_or_else(|| "no document".to_string())?;
        let index = self.lines.get(&url).ok_or_else(|| "that file is not open".to_string())?;
        let _ = file;

        match references::renameable(&self.db, root, file, offset) {
            references::Renameable::Local { ranges, .. } => {
                let edits: Vec<Value> = ranges
                    .iter()
                    .map(|range| {
                        json!({
                            "range": Range {
                                start: index.position(range.start(), self.encoding),
                                end: index.position(range.end(), self.encoding),
                            },
                            "newText": new_name,
                        })
                    })
                    .collect();
                Ok(json!({ "changes": { url.as_str(): edits } }))
            }
            references::Renameable::Refused(why) => Err(why.to_string()),
            references::Renameable::Nothing => Ok(Value::Null),
        }
    }

    /// The outline of one file.
    fn document_symbols(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = LineIndex::new(file.text(&self.db));

        let out: Vec<Value> = symbols::in_file(&self.db, file)
            .into_iter()
            .map(|symbol| {
                let range = Range {
                    start: index.position(symbol.range.start(), self.encoding),
                    end: index.position(symbol.range.end(), self.encoding),
                };
                json!({
                    "name": symbol.name,
                    "kind": symbol.kind,
                    "range": range,
                    // The same range for both: `selectionRange` must be inside
                    // `range`, and the name has no range of its own to use.
                    "selectionRange": range,
                })
            })
            .collect();
        Some(Value::Array(out))
    }

    /// Everything in the workspace matching a query.
    fn workspace_symbols(&self, params: &Value) -> Option<Value> {
        let root = khora_db::source_root(&self.db)?;
        let query = params.get("query").and_then(Value::as_str).unwrap_or_default();

        let mut out = Vec::new();
        for (file, symbol) in symbols::in_workspace(&self.db, root, query) {
            let Ok(url) = Url::from_file_path(file.path(&self.db)) else { continue };
            let index = LineIndex::new(file.text(&self.db));
            out.push(json!({
                "name": symbol.name,
                "kind": symbol.kind,
                // The module, so a picker showing three `parse`s says which is
                // which without the reader opening all three.
                "containerName": symbol.module.map(|m| m.to_string()),
                "location": {
                    "uri": url.as_str(),
                    "range": Range {
                        start: index.position(symbol.range.start(), self.encoding),
                        end: index.position(symbol.range.end(), self.encoding),
                    },
                },
            }));
        }
        Some(Value::Array(out))
    }

    /// The file and byte offset a positional request is about.
    ///
    /// Every one of them needs the same three lookups, and doing it in one
    /// place is what keeps a new request from getting the encoding wrong.
    fn locate(&self, params: &Value) -> Option<(SourceFile, text_size::TextSize)> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;
        let position: lsp_types::Position =
            serde_json::from_value(params.get("position")?.clone()).ok()?;
        Some((file, index.offset(position, self.encoding)))
    }

    /// The whole file, formatted, as one edit.
    ///
    /// **One edit over the whole document rather than a minimal diff.** A
    /// minimal diff is what preserves a cursor and a selection, and computing
    /// one honestly needs a tree diff; computing one dishonestly — matching
    /// common prefixes and suffixes — is where formatters put the cursor
    /// somewhere surprising. VS Code already keeps the cursor sensibly across a
    /// full-document edit, so the trade is a cheap correct answer against an
    /// expensive one nobody has asked for yet.
    ///
    /// **A file that does not parse is left exactly as it is.** `khora fmt`
    /// makes the same decision for the same reason: format-on-save runs while
    /// somebody is mid-edit, when a brace is unbalanced more often than not,
    /// and a formatter that rewrites a half-written file is one people turn
    /// off. `None` here means "no edits", which is what the protocol wants —
    /// the errors are already on screen from `publishDiagnostics`.
    ///
    /// Nothing is returned when the text is already canonical either, so an
    /// editor that saves an untouched file records no change and no undo step.
    fn formatting(&self, params: &Value) -> Option<Vec<TextEdit>> {
        let url = url_of(params)?;
        let index = self.lines.get(&url)?;
        let text = index.text();

        let formatted = khora_fmt::format(text).ok()?;
        if formatted == text {
            return Some(Vec::new());
        }

        // The end of the document, in the client's own units. `LineIndex`
        // answers that from the text it was built with, which is the text the
        // client last sent — so the range cannot name a position the client
        // does not have.
        let end = index.position(text_size::TextSize::of(text), self.encoding);
        Some(vec![TextEdit {
            range: Range { start: Position { line: 0, character: 0 }, end },
            new_text: formatted,
        }])
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

/// The byte range a diagnostic covers, back from the client's own units.
///
/// The client sends what the server sent it, so this is the inverse of the
/// conversion in `diagnostics` — and it has to use the same `LineIndex` and
/// the same encoding, or a fix lands a column away from the squiggle it
/// answers.
fn range_of(
    diagnostic: &Value,
    index: &LineIndex,
    encoding: Encoding,
) -> Option<text_size::TextRange> {
    let start: lsp_types::Position =
        serde_json::from_value(diagnostic.pointer("/range/start")?.clone()).ok()?;
    let end: lsp_types::Position =
        serde_json::from_value(diagnostic.pointer("/range/end")?.clone()).ok()?;
    Some(text_size::TextRange::new(index.offset(start, encoding), index.offset(end, encoding)))
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
