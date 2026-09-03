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
//! Diagnostics, hover, completion, formatting, go to definition, to the type,
//! and to the implementations, find references, highlight, rename, symbols,
//! folding, expand-selection, inlay hints, code actions and code lenses. All of them come from things that already existed:
//! `khora_db::parse`, `khora_types::diagnostics`, `khora_lint::findings`, the
//! checker's `BodyTypes`, `khora_fmt`, `khora_hir::resolve_path` and
//! `khora_hir::item_map`.
//!
//! **Rename covers a declaration across the workspace**, and refuses a trait
//! member or a constructor with a reason rather than editing one badly —
//! `references` has the argument for each.

#![deny(missing_docs)]

mod assists;
mod handlers;
mod members;
mod completion;
mod definition;
mod explain;
mod fixes;
mod imports;
mod hints;
mod position;
mod reach;
mod references;
mod semantic;
mod signature;
mod structure;
mod symbols;
mod transport;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use khora_db::{KhoraDatabase, Setter, SourceFile, SourceRoot};
use khora_manifest::LintLevel;
use khora_syntax::ast::{AstNode, FnDecl};
use khora_syntax::SyntaxNode;
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
///
/// **Messages are taken in batches of whatever has already arrived.** The
/// server is still one thread doing one thing at a time; what changes is that
/// it can see the queue before it starts. Typing ten characters used to be ten
/// `didChange` notifications and so ten full type-checks, nine of whose
/// answers were obsolete before they were computed. Now the ten edits are all
/// applied — they must be, each is measured against the last — and the file is
/// checked once, at the end.
///
/// A reader thread is what makes the queue visible. It does nothing but frame
/// messages and hand them over, so the database still has one owner and there
/// is still no locking.
pub fn serve(input: impl BufRead + Send + 'static, output: &mut impl Write) -> Result<()> {
    let (sender, receiver) = std::sync::mpsc::channel::<String>();

    // **Detached, not scoped.** A scoped thread is joined when this function
    // returns, and this one is blocked inside `read`: after `exit` the server
    // has nothing more to do and the client has not closed the pipe yet, so
    // joining means waiting for input that will never come. `khora lsp` hung
    // on exit for exactly that reason, and the gate caught it.
    //
    // The cost of detaching is a thread parked on a read that nobody will
    // answer. In `khora lsp` the process ends immediately afterwards; for any
    // other caller it ends when the stream does.
    std::thread::spawn(move || {
        let mut input = input;
        // The stream ending, or giving us something that is not a message,
        // both mean there is nothing more to read; the main loop stops when
        // the channel closes either way.
        while let Ok(Some(text)) = read_message(&mut input) {
            if sender.send(text).is_err() {
                break;
            }
        }
    });

    let mut server = Server::default();
    while let Ok(text) = receiver.recv() {
        let mut batch = vec![text];
        // Everything else already waiting, without blocking for more.
        while let Ok(more) = receiver.try_recv() {
            batch.push(more);
        }
        for reply in server.handle_batch(&batch)? {
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
    /// The `[fmt]` settings, so that format-on-save and `khora fmt` agree.
    ///
    /// A formatter that gives one answer in the editor and another on the
    /// command line is the worst possible kind: every save fights the last
    /// build, and the diff blames whoever touched the file.
    fmt: khora_fmt::Options,
    /// Set by `exit`, and by `shutdown` followed by a closed stream.
    pub finished: bool,
    /// Apply an edit without reporting on it.
    ///
    /// Set for every edit in a batch but the last, so a run of keystrokes is
    /// spliced in full and checked once.
    quiet: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            db: KhoraDatabase::new(),
            files: HashMap::new(),
            lines: HashMap::new(),
            encoding: Encoding::default(),
            levels: HashMap::new(),
            fmt: khora_fmt::Options::default(),
            finished: false,
            quiet: false,
        }
    }
}

impl Server {
    /// Answers a batch of messages that arrived together.
    ///
    /// Two things are decided here that cannot be decided one message at a
    /// time. A request the batch also cancels is answered with the
    /// protocol's cancellation error rather than computed, which is what
    /// `$/cancelRequest` is for and what a strictly serial loop can never
    /// honour — the cancel always arrives after the work it wanted to stop.
    /// And a run of edits publishes diagnostics once at the end rather than
    /// once each, because every answer but the last is obsolete before it is
    /// written.
    pub fn handle_batch(&mut self, batch: &[String]) -> Result<Vec<Value>> {
        let mut messages = Vec::with_capacity(batch.len());
        for text in batch {
            messages.push(
                serde_json::from_str::<Value>(text).context("a message that is JSON")?,
            );
        }

        let cancelled: Vec<Value> = messages
            .iter()
            .filter(|m| m.get("method").and_then(Value::as_str) == Some("$/cancelRequest"))
            .filter_map(|m| m.pointer("/params/id").cloned())
            .collect();

        // The last edit in the batch is the one whose diagnostics are worth
        // computing; the ones before it are applied and not reported.
        let last_edit = messages.iter().rposition(|m| {
            matches!(
                m.get("method").and_then(Value::as_str),
                Some("textDocument/didChange") | Some("textDocument/didOpen")
            )
        });

        let mut out = Vec::new();
        for (at, message) in messages.iter().enumerate() {
            if let Some(id) = message.get("id") {
                if cancelled.contains(id) {
                    // -32800 is `RequestCancelled`. A client that asked us to
                    // stop still needs an answer, or it waits for ever.
                    out.push(error(id.clone(), -32800, "cancelled by the client"));
                    continue;
                }
            }
            self.quiet = last_edit.is_some_and(|last| at < last);
            out.extend(self.handle(message));
            if self.finished {
                break;
            }
        }
        self.quiet = false;
        Ok(out)
    }

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
            // **The item comes back whole.** A client merges what the server
            // returns over what it sent, and a reply missing `label` or
            // `additionalTextEdits` is a completion that inserts nothing or
            // forgets its import -- so an item this cannot say more about is
            // returned exactly as it arrived.
            ("completionItem/resolve", Some(id)) => {
                let resolved = self.resolve_completion(&params).unwrap_or(params.clone());
                vec![ok(id, resolved)]
            }
            ("textDocument/references", Some(id)) => {
                vec![ok(id, self.references(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/documentHighlight", Some(id)) => {
                vec![ok(id, self.document_highlights(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/typeDefinition", Some(id)) => {
                vec![ok(id, self.type_definition(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/implementation", Some(id)) => {
                vec![ok(id, self.implementations(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/foldingRange", Some(id)) => {
                vec![ok(id, self.folding_ranges(&params).unwrap_or(Value::Null))]
            }
            ("textDocument/selectionRange", Some(id)) => {
                vec![ok(id, self.selection_ranges(&params).unwrap_or(Value::Null))]
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
                match url_of(&params) {
                    // **Cleared, not just forgotten.** A diagnostic is
                    // published against a URI and stays in the client
                    // until something replaces it. Dropping the line
                    // index without saying so left a closed file
                    // squiggled in the Problems panel for the rest of the
                    // session, and nothing would ever take it back.
                    Some(url) => {
                        self.lines.remove(&url);
                        vec![notification(
                            "textDocument/publishDiagnostics",
                            json!({ "uri": url.as_str(), "diagnostics": [] }),
                        )]
                    }
                    None => Vec::new(),
                }
            }
            ("workspace/didChangeWatchedFiles", _) => self.watched_files_changed(&params),
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
                // **Incremental.** The argument for full sync was that the
                // parser is fast and the database backdates a reparse that
                // produces the same tree, so what incremental saves is a few
                // microseconds against a class of desynchronisation bug. The
                // half that argument left out is the wire: full sync sends the
                // *whole file* on every keystroke, so a 4,000-line module
                // costs about 150 KB of JSON per character typed, encoded by
                // the client and parsed here. That is the cost that grows with
                // the file, and it is the one somebody notices.
                //
                // The desynchronisation risk is answered by refusing an edit
                // rather than guessing at one: a range that is inverted, past
                // the end, or not on a character boundary is dropped, and the
                // client's next full-text change puts the document right.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
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
                    // **The documentation is fetched for the one item the
                    // reader looked at, not for all of them.** Completion now
                    // offers every public name in the workspace, and reading
                    // the `///` above each of a thousand declarations to fill a
                    // list where one gets read cost 100ms a keystroke against a
                    // workspace the size of `std`. Doing it on resolve costs a
                    // lookup when an item is highlighted, and nothing at all
                    // for the rest.
                    resolve_provider: Some(true),
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
                // **The kinds, rather than a bare `true`.** A client that
                // knows what a server offers asks for the one menu it is
                // filling -- VS Code's Refactor menu sends `only: [refactor]`
                // -- and a server that says nothing is asked for everything
                // every time. Saying it here is what lets `code_actions` skip
                // the work rather than compute assists and throw them away.
                code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
                    lsp_types::CodeActionOptions {
                        code_action_kinds: Some(vec![
                            lsp_types::CodeActionKind::QUICKFIX,
                            lsp_types::CodeActionKind::REFACTOR_REWRITE,
                            lsp_types::CodeActionKind::REFACTOR_EXTRACT,
                        ]),
                        resolve_provider: Some(false),
                        ..Default::default()
                    },
                )),
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
                document_highlight_provider: Some(OneOf::Left(true)),
                type_definition_provider: Some(lsp_types::TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
                folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::Simple(true)),
                selection_range_provider: Some(lsp_types::SelectionRangeProviderCapability::Simple(true)),
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
        self.fmt = fmt_options(root);
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
        let Some(changes) = params.pointer("/contentChanges").and_then(Value::as_array) else {
            return Vec::new();
        };
        let Ok(parsed) = Url::parse(url) else { return Vec::new() };
        let Ok(path) = parsed.to_file_path() else { return Vec::new() };

        let mut text = match self.files.get(&path) {
            Some(file) => file.text(&self.db).to_string(),
            // A change to a document nobody opened. The only sound reading of
            // an edit against text we do not have is the whole text, and a
            // ranged change against nothing is dropped.
            None => String::new(),
        };

        for change in changes {
            let Some(replacement) = change.get("text").and_then(Value::as_str) else { continue };
            let Some(range) = change.get("range") else {
                // No range is the whole document, which a client may still
                // send under incremental sync and always sends on the first
                // change of a document it has just re-synchronized.
                text = replacement.to_string();
                continue;
            };
            let Ok(range) = serde_json::from_value::<Range>(range.clone()) else { continue };
            // **Rebuilt per change, because each one applies to the document
            // the one before it left.** A client may send several edits in one
            // notification -- a multi-cursor edit is the everyday case -- and
            // their positions are all measured against the text as it stands
            // when that edit is applied.
            let index = LineIndex::new(&text);
            let start = usize::from(index.offset(range.start, self.encoding));
            let end = usize::from(index.offset(range.end, self.encoding));
            if start > end || end > text.len() {
                continue;
            }
            // A splice at a byte that is not a character boundary would panic,
            // and a client that disagrees with us about an offset should cost
            // a wrong document rather than a dead server.
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            text.replace_range(start..end, replacement);
        }

        self.edit(url, text)
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

        if !self.files.contains_key(&path) {
            return Vec::new();
        }
        drop(parsed);
        if self.quiet {
            return Vec::new();
        }
        self.publish_open()
    }

    /// Diagnostics for every open document, not only the one just edited.
    ///
    /// **A build is whole-program, so an edit here is a diagnostic
    /// there.** Deleting a function from `library.kh` breaks `main.kh`,
    /// and publishing only for the file that changed left the other one
    /// showing squiggles from before the edit — or worse, showing none
    /// while the build failed. The editor believed whichever file the
    /// author happened to touch last.
    ///
    /// Every open document, rather than every file in the root: a
    /// diagnostic for a file nobody has open is not displayed anywhere,
    /// and computing it would put the whole workspace on the keystroke
    /// path. Salsa memoizes the ones that did not change, so the cost is
    /// the files that actually moved.
    fn publish_open(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for url in self.lines.keys() {
            let Ok(path) = url.to_file_path() else { continue };
            let Some(file) = self.files.get(&path).copied() else { continue };
            out.push(notification(
                "textDocument/publishDiagnostics",
                json!({
                    "uri": url.as_str(),
                    "diagnostics": self.diagnostics(url.clone(), file),
                }),
            ));
        }
        out
    }

    /// A `.kh` file appeared, changed or went away outside the editor.
    ///
    /// **The client was already sending this and the server was dropping
    /// it.** The extension registers a watcher over `**/*.kh` precisely
    /// because the root is read once at `initialize`, so a file created
    /// by `git checkout`, `khora new`, or another editor never joined it
    /// and every name it defined read as unresolved until somebody
    /// restarted the server.
    ///
    /// A file that is open in the editor is left alone: its buffer is the
    /// truth, and what is on disk is behind whatever has not been saved.
    fn watched_files_changed(&mut self, params: &Value) -> Vec<Value> {
        let Some(changes) = params.pointer("/changes").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut moved = false;
        for change in changes {
            let Some(url) = change.get("uri").and_then(Value::as_str) else { continue };
            let Ok(parsed) = Url::parse(url) else { continue };
            let Ok(path) = parsed.to_file_path() else { continue };
            if self.lines.contains_key(&parsed) {
                continue;
            }
            if !khora_db::selected_for_target(&path, khora_db::host_target()) {
                continue;
            }

            // 1 created, 2 changed, 3 deleted.
            let kind = change.get("type").and_then(Value::as_i64).unwrap_or(2);
            if kind == 3 {
                if self.files.remove(&path).is_some() {
                    moved = true;
                }
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            match self.files.get(&path).copied() {
                Some(file) => {
                    file.set_text(&mut self.db).to(text);
                }
                None => {
                    let file = SourceFile::new(&self.db, path.clone(), text);
                    self.files.insert(path, file);
                    moved = true;
                }
            }
        }

        // Only when the *set* changed: setting the same list back is an
        // input write, and salsa would invalidate every query that read it.
        if moved {
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
        self.publish_open()
    }

    /// What an editor may collapse in this file.
    fn folding_ranges(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;

        let items: Vec<Value> = structure::folds(&self.db, file)
            .into_iter()
            .map(|fold| {
                let range = index.range(fold.range, self.encoding);
                // **The last line of the region, not the line after it.** A
                // node's range ends just past its final character, so a body
                // closed by a brace ends at column 1 of the *next* line and a
                // run of imports ends part-way along its own. Taking the end
                // line unconditionally would fold one line too many in the
                // first case; subtracting one unconditionally drops the second
                // case entirely, because its region is then a single line and
                // an editor will not draw one.
                let end_line = if range.end.character > 0 {
                    range.end.line
                } else {
                    range.end.line.saturating_sub(1)
                };
                let mut item = json!({
                    "startLine": range.start.line,
                    "endLine": end_line,
                });
                if let Some(kind) = fold.kind {
                    item["kind"] = json!(kind);
                }
                item
            })
            .filter(|item| item["endLine"].as_u64() > item["startLine"].as_u64())
            .collect();
        Some(Value::Array(items))
    }

    /// The ranges to step through as a selection widens.
    fn selection_ranges(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;

        let positions = params.get("positions")?.as_array()?.clone();
        let mut out = Vec::new();
        for position in positions {
            let Ok(position) = serde_json::from_value::<Position>(position) else { continue };
            let offset = index.offset(position, self.encoding);
            let chain = structure::selection_chain(&self.db, file, offset);
            // Built from the outside in, because each step is the `parent` of
            // the one before it in the protocol's shape.
            let mut node = Value::Null;
            for range in chain.iter().rev() {
                let mut step = json!({ "range": index.range(*range, self.encoding) });
                if !node.is_null() {
                    step["parent"] = node;
                }
                node = step;
            }
            out.push(node);
        }
        Some(Value::Array(out))
    }

    /// Where the *type* of the thing under the cursor is declared.
    fn type_definition(&self, params: &Value) -> Option<Value> {
        let (file, offset) = self.locate(params)?;
        let root = khora_db::source_root(&self.db)?;
        let found = definition::type_at(&self.db, root, file, offset)?;
        Some(self.as_location(&found))
    }

    /// Every `impl` written for the type or trait under the cursor.
    fn implementations(&self, params: &Value) -> Option<Value> {
        let (file, offset) = self.locate(params)?;
        let root = khora_db::source_root(&self.db)?;
        let found = definition::implementations(&self.db, root, file, offset);
        Some(Value::Array(found.iter().map(|each| self.as_location(each)).collect()))
    }

    /// A definition as the protocol wants it, with the range read against the
    /// file it is in rather than the one that asked.
    fn as_location(&self, found: &definition::Definition) -> Value {
        let index = LineIndex::new(found.file.text(&self.db));
        let uri = Url::from_file_path(found.file.path(&self.db))
            .map(|u| u.to_string())
            .unwrap_or_default();
        json!({ "uri": uri, "range": index.range(found.range, self.encoding) })
    }

    /// Every mention of the thing under the cursor, in this file only.
    ///
    /// The same search `references` runs, narrowed to the document asking
    /// and answered as highlights. An editor asks for this on every cursor
    /// move, so it must not be the cross-file walk.
    fn document_highlights(&self, params: &Value) -> Option<Value> {
        let (file, offset) = self.locate(params)?;
        let root = khora_db::source_root(&self.db)?;
        let found = references::at(&self.db, root, file, offset, true)?;
        let index = LineIndex::new(file.text(&self.db));
        let mut out = Vec::new();
        for (each, ranges) in found.sites {
            if each != file {
                continue;
            }
            for range in ranges {
                out.push(json!({
                    "range": Range {
                        start: index.position(range.start(), self.encoding),
                        end: index.position(range.end(), self.encoding),
                    },
                    "kind": 1,
                }));
            }
        }
        Some(Value::Array(out))
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
            let level = self.levels.get(finding.lint).copied().unwrap_or_else(|| khora_lint::default_level(finding.lint));
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

        // **What a function absorbs, which its signature deliberately does not
        // say.** Khora's rows are transitive, so a lens repeating a signature
        // would be noise -- except where a `with` block or a `catch` stops
        // something reaching it. That is the line worth marking, and nothing
        // else in the editor marks it.
        let mut out = out;
        for found in reach::in_file(&self.db, file) {
            let Some(title) = found.title() else { continue };
            let start = index.position(found.at.start(), self.encoding);
            out.push(json!({
                "range": { "start": start, "end": start },
                "command": { "title": title, "command": "" },
            }));
        }
        Some(Value::Array(out))
    }

    /// The edits offered for the diagnostics under the cursor.
    ///
    /// **The client sends the diagnostics back**, in `context.diagnostics`, so
    /// there is no need to recompute them and no risk of offering a fix for a
    /// diagnostic the editor has already cleared. What this adds is the source
    /// each one covers, which is what lets a replacement keep the part of the
    /// line it is not changing, and the signature of the function it sits in,
    /// for the fixes that put a clause on one.
    fn code_actions(&self, params: &Value) -> Option<Value> {
        let url = url_of(params)?;
        let path = url.to_file_path().ok()?;
        let file = self.files.get(&path).copied()?;
        let index = self.lines.get(&url)?;
        let text = file.text(&self.db);
        let tree = khora_db::parse(&self.db, file).syntax();

        let reported = params
            .pointer("/context/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut out = Vec::new();
        // **What the editor asked for.** A client that wants only quick fixes
        // says so, and answering with a refactoring it filters out costs the
        // work of computing one. An absent `only` means everything.
        let wanted = params
            .pointer("/context/only")
            .and_then(Value::as_array)
            .map(|kinds| {
                kinds.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()
            });
        let asked_for = |kind: &str| {
            wanted.as_ref().is_none_or(|only| {
                only.iter().any(|want| kind == want || kind.starts_with(&format!("{want}.")))
            })
        };

        // Assists come from where the cursor is rather than from a diagnostic,
        // so they are offered whether or not anything is wrong.
        if assists::KINDS.iter().any(|kind| asked_for(kind)) {
            if let Some(selection) = range_of(params, index, self.encoding) {
                for assist in assists::at(&self.db, file, selection) {
                    if !asked_for(assist.kind) {
                        continue;
                    }
                    out.push(json!({
                        "title": assist.title,
                        "kind": assist.kind,
                        "edit": { "changes": { url.as_str(): assist
                            .edits
                            .iter()
                            .map(|edit| json!({
                                "range": Range {
                                    start: index.position(edit.range.start(), self.encoding),
                                    end: index.position(edit.range.end(), self.encoding),
                                },
                                "newText": edit.replacement,
                            }))
                            .collect::<Vec<_>>() }},
                    }));
                }
            }
        }
        if !asked_for("quickfix") {
            return Some(Value::Array(out));
        }

        // **Titles already offered.** Two diagnostics on one line often name
        // the same missing type -- `cannot resolve `List::length`` and
        // ``[a, b, c]` builds a `List`` both do -- and the same action twice
        // in the lightbulb menu reads as a bug in the editor.
        let mut already: Vec<String> = Vec::new();
        for diagnostic in reported {
            let Some(range) = range_of(&diagnostic, index, self.encoding) else { continue };
            let message = diagnostic.get("message").and_then(Value::as_str).unwrap_or_default();
            let code = diagnostic.get("code").and_then(Value::as_str);
            let covered = text.get(usize::from(range.start())..usize::from(range.end()))?;
            let enclosing = enclosing_signature(&tree, range.start());

            // **The import first**, because when a name is not in scope it is
            // almost always the only fix anybody wants, and an editor offers
            // the first action on the keystroke.
            let mut offered = self.import_fixes(file, &tree, text, message);
            offered.extend(self.member_fixes(&tree, text, message, range));
            offered.extend(fixes::for_diagnostic(
                message,
                code,
                range,
                covered,
                text,
                enclosing.as_ref(),
            ));

            for fix in offered {
                if already.iter().any(|seen| seen == &fix.title) {
                    continue;
                }
                already.push(fix.title.clone());
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

    /// The imports that would bring a diagnostic's unresolved names into scope.
    ///
    /// One action per module that exports the name, because an ambiguous name
    /// is the reader's decision and guessing it would be the wrong kind of
    /// help. Nothing is offered when the name is already in scope, so an
    /// unrelated diagnostic that happens to quote a familiar type costs a
    /// lookup and adds no noise.
    fn import_fixes(
        &self,
        file: SourceFile,
        tree: &khora_syntax::SyntaxNode,
        text: &str,
        message: &str,
    ) -> Vec<fixes::Fix> {
        let names = imports::mentioned(message);
        if names.is_empty() {
            return Vec::new();
        }
        let here = khora_hir::module_api(&self.db, file)
            .module
            .as_ref()
            .map(|m| m.to_string())
            .unwrap_or_default();
        let known: Vec<SourceFile> = self.files.values().copied().collect();

        let mut out = Vec::new();
        for name in names {
            for module in imports::providers(&self.db, &known, &name, &here) {
                if let Some(fix) = imports::edit(tree, text, &module, &name) {
                    out.push(fix);
                }
            }
        }
        out
    }

    /// The trait members an impl is missing, written out with their signatures.
    ///
    /// **The signature is the whole value here.** `this impl is missing `cmp``
    /// names the member and nothing else, and finding out what `cmp` takes
    /// means opening the trait in another file, reading it, and typing it back
    /// in with `Self` swapped for the type being implemented. That is a
    /// transcription job, and getting one character of it wrong produces a
    /// second error about the signature not matching.
    ///
    /// Bodies are `todo()`, for the reason the match arms are: a plausible
    /// default would be a wrong answer where the error was a refusal.
    fn member_fixes(
        &self,
        tree: &khora_syntax::SyntaxNode,
        text: &str,
        message: &str,
        range: text_size::TextRange,
    ) -> Vec<fixes::Fix> {
        use khora_syntax::ast::{ImplDecl, TraitDecl};

        let Some((names, trait_name)) = members::missing(message) else { return Vec::new() };
        let Some(imp) = tree
            .descendants()
            .filter_map(ImplDecl::cast)
            .find(|node| node.syntax().text_range().contains_range(range))
        else {
            return Vec::new();
        };
        // What `Self` means inside this impl, which the trait writes and the
        // impl has to spell out.
        let Some(me) = imp.self_type().map(|ty| ty.syntax().text().to_string()) else {
            return Vec::new();
        };

        let Some(root) = khora_db::source_root(&self.db) else { return Vec::new() };
        let graph = khora_hir::module_graph(&self.db, root);
        let Some(declaration) = graph.paths().find_map(|module| {
            let file = graph.file(module)?;
            let item = khora_hir::item_map(&self.db, file).item(&trait_name)?;
            let found = khora_db::parse(&self.db, file)
                .syntax()
                .descendants()
                .filter_map(TraitDecl::cast)
                .find(|node| node.syntax().text_range() == item.range)?;
            Some(found)
        }) else {
            return Vec::new();
        };

        let written: Vec<String> = names
            .iter()
            .filter_map(|name| {
                let member = declaration
                    .functions()
                    .find(|f| f.name().and_then(|n| n.ident()).as_deref() == Some(name.as_str()))?;
                Some(members::body_for(&member.syntax().text().to_string(), &me))
            })
            .collect();
        if written.len() != names.len() {
            // A member whose signature could not be read would be written as a
            // guess, and half an answer here is worse than none: the reader
            // would have to notice which half.
            return Vec::new();
        }

        let Some(fix) = members::insertion(text, imp.syntax().text_range(), &written) else {
            return Vec::new();
        };
        vec![fixes::Fix {
            title: format!(
                "Write the missing member{} from `{trait_name}`",
                if names.len() == 1 { "" } else { "s" }
            ),
            range: fix.0,
            replacement: fix.1,
        }]
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

        let index = LineIndex::new(file.text(&self.db));
        let known: Vec<SourceFile> = self.files.values().copied().collect();

        let items: Vec<Value> = completion::at(&self.db, root, file, offset, &known)
            .into_iter()
            .map(|candidate| {
                let mut item = json!({
                    "label": candidate.label,
                    "kind": candidate.kind,
                    "detail": candidate.detail,
                    // **In scope first.** A local named `rows` must not be
                    // outranked by three hundred names from `std`, and the
                    // client sorts by this rather than by the order sent.
                    "sortText": format!(
                        "{}{}",
                        match (candidate.wanted, candidate.import.is_some()) {
                            // The handler that answers a requirement the code
                            // actually has is not one of several plausible
                            // entries; it is the one.
                            (true, _) => '0',
                            (false, false) => '1',
                            (false, true) => '2',
                        },
                        candidate.label,
                    ),
                });
                // Markdown, and only when there is some: an empty
                // documentation field makes an editor draw a blank panel
                // beside the list, which reads as "this has no
                // documentation" for a name that simply is not a declaration.
                if let Some(docs) = candidate.documentation {
                    item["documentation"] = json!({ "kind": "markdown", "value": docs });
                }
                if let Some(insert) = candidate.insert {
                    // The label is what somebody types to find it; this is
                    // what they wanted written. A whole handler is not a name.
                    item["insertText"] = json!(insert);
                }
                if let Some(module) = candidate.source {
                    // Where it comes from, drawn to the right of the name --
                    // which is the one thing that distinguishes two names that
                    // are otherwise the same word.
                    item["labelDetails"] = json!({ "description": module });
                    // Enough to find the declaration again on resolve, and no
                    // range: the file may be edited between the list being
                    // built and an item in it being highlighted.
                    item["data"] = json!({ "module": module, "name": candidate.label });
                }
                if let Some((range, text)) = candidate.import {
                    item["additionalTextEdits"] = json!([{
                        "range": Range {
                            start: index.position(range.start(), self.encoding),
                            end: index.position(range.end(), self.encoding),
                        },
                        "newText": text,
                    }]);
                }
                item
            })
            .collect();
        Some(Value::Array(items))
    }

    /// The documentation for one completion item, fetched when it is looked at.
    ///
    /// The `data` a candidate carries is the module it comes from and its name,
    /// which is all it takes to find the declaration again -- and deliberately
    /// not a range, since the file may have been edited between the list being
    /// built and an item in it being highlighted.
    fn resolve_completion(&self, item: &Value) -> Option<Value> {
        let module = item.pointer("/data/module").and_then(Value::as_str)?;
        let name = item.pointer("/data/name").and_then(Value::as_str)?;

        let root = khora_db::source_root(&self.db)?;
        let graph = khora_hir::module_graph(&self.db, root);
        let path = graph.paths().find(|p| p.to_string() == module.replace("::", "."))?.clone();
        let file = graph.file(&path)?;
        let declared = khora_hir::item_map(&self.db, file).item(name)?.clone();
        let explained = explain::at(&self.db, file, declared.range)?;

        let mut out = item.clone();
        out["detail"] = json!(explained.signature);
        if let Some(docs) = explained.docs {
            out["documentation"] = json!({ "kind": "markdown", "value": docs });
        }
        Some(out)
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
            references::Renameable::Item { name, sites } => {
                // The occurrence under the cursor, which is the one the editor
                // highlights and pre-fills.
                let here = sites
                    .iter()
                    .find(|(each, _)| *each == file)
                    .and_then(|(_, ranges)| ranges.iter().find(|r| r.contains_inclusive(offset)))
                    .copied();
                match here {
                    Some(here) => Ok(json!({
                        "range": Range {
                            start: index.position(here.start(), self.encoding),
                            end: index.position(here.end(), self.encoding),
                        },
                        "placeholder": name,
                    })),
                    None => Ok(Value::Null),
                }
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
            references::Renameable::Item { sites, .. } => {
                // **Every file that names it, in one edit.** A rename that
                // reached the uses and not the import, or the declaration and
                // not the uses, would leave a repository that does not
                // compile -- which is why this refused to leave a body until
                // the import list and the declaration's own name token were
                // both accounted for.
                let mut changes = serde_json::Map::new();
                for (each, ranges) in sites {
                    let Ok(uri) = Url::from_file_path(each.path(&self.db)) else { continue };
                    let lines = LineIndex::new(each.text(&self.db));
                    let edits: Vec<Value> = ranges
                        .iter()
                        .map(|range| {
                            json!({
                                "range": lines.range(*range, self.encoding),
                                "newText": new_name,
                            })
                        })
                        .collect();
                    changes.insert(uri.to_string(), Value::Array(edits));
                }
                Ok(json!({ "changes": changes }))
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

        let formatted = khora_fmt::format_with(text, &self.fmt).ok()?;
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

        // **The declaration first, and the type only if there is none.**
        // A type answers "what is this" and a signature with its `///`
        // answers "what is it for", which is the question somebody hovering a
        // name they have not used before is actually asking. Falling back to
        // the type keeps every expression hoverable: an arithmetic
        // subexpression names no declaration and its type is the whole of
        // what can be said about it.
        if let Some(explained) = self.explain_at(file, offset) {
            // **The declaration, and what it is here.** A generic signature
            // says `A`; at a call site `A` is something in particular, and
            // which one cannot be read off the declaration. That is the half
            // the old type-only hover had and this one would otherwise have
            // lost.
            let here = best.as_ref().map(|(_, ty)| ty.as_str());
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: explained.markdown_at(here),
                }),
                range: best.map(|(range, _)| index.range(range, self.encoding)),
            });
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

    /// The declaration the cursor names, explained.
    ///
    /// Resolution is `definition::at`'s, so hovering a name and jumping to it
    /// agree about what it refers to. A local binding is deliberately not
    /// explained here: it has no `///` and its signature is its `let`, so the
    /// inferred type below is the better answer for one.
    fn explain_at(&self, file: SourceFile, offset: text_size::TextSize) -> Option<explain::Explained> {
        let root = khora_db::source_root(&self.db)?;
        if definition::local_use_at(&self.db, file, offset).is_some()
            || definition::local_binding_at(&self.db, file, offset).is_some()
        {
            return None;
        }
        let found = definition::at(&self.db, root, file, offset)?;
        explain::at(&self.db, found.file, found.range)
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

/// The `[fmt]` settings for a workspace, or the formatter's own defaults.
///
/// The same reading `khora fmt` does, and it has to stay the same reading:
/// see the `fmt` field.
fn fmt_options(root: &Path) -> khora_fmt::Options {
    let Ok(parsed) = khora_manifest::Manifest::load(&root.join("khora.toml")) else {
        return khora_fmt::Options::default();
    };
    let Some(table) = parsed.manifest.fmt else { return khora_fmt::Options::default() };
    match table.indent_style {
        Some(khora_manifest::IndentStyle::Tab) => khora_fmt::Options::tabs(),
        Some(khora_manifest::IndentStyle::Space) | None => match table.indent_width {
            Some(width) => khora_fmt::Options::spaces(width),
            None => khora_fmt::Options::default(),
        },
    }
}

/// The `[lints]` levels for a workspace, or the defaults.
fn lint_levels(root: &Path) -> HashMap<String, LintLevel> {
    let mut out = HashMap::new();
    let Ok(parsed) = khora_manifest::Manifest::load(&root.join("khora.toml")) else {
        return out;
    };
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

/// The signature of the function containing `offset`.
///
/// **The nearest enclosing declaration, so a closure does not hide it.** A call
/// inside `fn body => ..` still reports against the function the closure is
/// written in, because that is whose row the checker compared -- a closure's is
/// inferred and has nothing to edit.
fn enclosing_signature(tree: &SyntaxNode, offset: text_size::TextSize) -> Option<fixes::Signature> {
    // The token to the right first: a diagnostic points at the start of what it
    // is about, so the right-hand token is the thing itself and the left-hand
    // one is whatever whitespace came before it. Both usually sit under the
    // same declaration; where they do not, the right one is the correct answer.
    let at = tree.token_at_offset(offset);
    let decl = at
        .clone()
        .right_biased()
        .and_then(|t| t.parent_ancestors().find_map(FnDecl::cast))
        .or_else(|| {
            at.left_biased().and_then(|t| t.parent_ancestors().find_map(FnDecl::cast))
        })?;
    // Past the return type, or past the parameters when a signature has none
    // to write it after.
    let clauses_at = decl
        .return_type()
        .map(|ty| ty.syntax().text_range().end())
        .or_else(|| decl.params().map(|p| p.syntax().text_range().end()))?;
    let row = |ty: khora_syntax::ast::Type| fixes::Row {
        range: ty.syntax().text_range(),
        text: ty.syntax().text().to_string(),
    };
    Some(fixes::Signature {
        clauses_at,
        with_row: decl.with_clause().and_then(|c| c.row()).map(row),
        raises_row: decl.raises_clause().and_then(|c| c.row()).map(row),
    })
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
