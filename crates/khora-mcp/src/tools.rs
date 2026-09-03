//! The tools, and what each is for.
//!
//! Ordered by how much they matter. [`CHECK`] is the one that does the work;
//! the rest exist so that the first thing an agent writes is close enough to be
//! worth checking.

use anyhow::{bail, Result};
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use serde_json::{json, Value};

use crate::surface::{public_items, Entry};

pub const CHECK: &str = "khora_check";
const SEARCH: &str = "khora_std_search";
const GRAMMAR: &str = "khora_grammar";
const DOCS: &str = "khora_design_doc";
const FORMAT: &str = "khora_format";

/// Sent in the `initialize` reply.
///
/// Says the thing an agent cannot work out for itself: that it does not know
/// this language, that its instincts come from Rust and will be subtly wrong,
/// and that there is a compiler here to ask.
pub const ORIENTATION: &str = "\
Khora is a statically-typed pure-functional systems language. You have almost \
certainly never seen it: it is not in your training data, and code that looks \
right by analogy with Rust or TypeScript will usually be wrong in ways you \
cannot detect by reading it.

Use `khora_check` constantly. It runs the real compiler over a snippet and \
returns real diagnostics, so you can find out whether something works instead \
of guessing. Check before you answer, not after.

Four things have no close analogue elsewhere, and are where guesses fail:

  - A function that needs a capability declares it: `with { rng: Random }`. \
Callers supply it and it propagates; forgetting it is a type error, not a \
runtime one.
  - A function that can fail declares how: `raises NotFound`. `!` propagates a \
failure, `catch` handles it.
  - Effects are direct-style. There is no async, no await, no monad, no \
`Promise`. A function that requires a capability is called like any other.
  - Anything crossing into a fiber must implement `Share`, which is restricted \
to the module declaring the type.

`khora_std_search` is how you find what exists rather than inventing it. \
`khora_design_doc` explains why something is the way it is, which matters when \
the obvious approach is refused.";

/// What `tools/list` answers.
pub fn describe() -> Vec<Value> {
    vec![
        json!({
            "name": CHECK,
            "description": "Type-check Khora source and return the compiler's diagnostics. \
                            The standard library is in scope. Use this to verify anything \
                            before you rely on it — it is the only way to know whether Khora \
                            code is correct, since you have no training data for this \
                            language. A snippet needs a `module` declaration at the top.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Khora source, starting with `module something;`."
                    }
                },
                "required": ["source"]
            }
        }),
        json!({
            "name": SEARCH,
            "description": "Search the standard library's public items by substring, in the \
                            name or the documentation. Returns each one's module, its \
                            declaration as written, and its doc comment. Use it to find what \
                            exists instead of inventing a name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "A substring, case-insensitive." },
                    "limit": { "type": "integer", "description": "Default 20." }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": GRAMMAR,
            "description": "The complete grammar, as EBNF. Use it when you are unsure whether \
                            something is syntax Khora has, rather than guessing from another \
                            language.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": DOCS,
            "description": "Read a design note, or list them all with no argument. These say \
                            why the language is the way it is, which is what you need when \
                            the compiler refuses something that looks reasonable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Such as `effects` or `memory`." }
                }
            }
        }),
        json!({
            "name": FORMAT,
            "description": "Format Khora source canonically. Also a syntax check: source that \
                            does not parse cannot be formatted. Pass `path` when the source \
                            belongs to a package, so its `[fmt]` settings are used rather than \
                            the defaults.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string" },
                    "path": {
                        "type": "string",
                        "description": "A file or directory inside the package this source \
                                        belongs to. The nearest `khora.toml` above it decides \
                                        indentation."
                    }
                },
                "required": ["source"]
            }
        }),
    ]
}

/// Dispatches a `tools/call`.
pub fn run(name: &str, arguments: &Value, surface: &[Entry]) -> Result<String> {
    match name {
        CHECK => check(text_argument(arguments, "source")?),
        SEARCH => Ok(search(
            text_argument(arguments, "query")?,
            arguments.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize,
            surface,
        )),
        GRAMMAR => read_repo_file("docs/grammar.ebnf"),
        DOCS => match arguments.get("name").and_then(Value::as_str) {
            Some(want) => design_doc(want),
            None => Ok(list_design_docs()),
        },
        FORMAT => format(
            text_argument(arguments, "source")?,
            arguments.get("path").and_then(Value::as_str),
        ),
        other => bail!("`{other}` is not a tool this server has"),
    }
}

fn text_argument<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    match arguments.get(key).and_then(Value::as_str) {
        Some(text) => Ok(text),
        None => bail!("`{key}` is required and must be a string"),
    }
}

/// The tool that matters.
///
/// Compiles the snippet together with the whole standard library, which is what
/// makes the answer worth having: a diagnostic about `Random` is the real one,
/// from the real `std`, not from a description of it.
fn check(source: &str) -> Result<String> {
    let db = KhoraDatabase::new();
    let mut files = std_files(&db);
    let snippet = SourceFile::new(&db, std::path::PathBuf::from("snippet.kh"), source.to_string());
    files.push(snippet);
    SourceRoot::new(&db, files);

    let parse = khora_db::parse(&db, snippet);
    if !parse.errors().is_empty() {
        let mut out = String::from("This does not parse.\n\n");
        for error in parse.errors() {
            out.push_str(&format!("  {}: {}\n", at(source, error.range.start().into()), error.message));
        }
        return Ok(out);
    }

    let diagnostics = khora_types::diagnostics(&db, snippet);
    if diagnostics.is_empty() {
        return Ok("No errors. This compiles.".to_string());
    }

    let mut out = format!("{} error(s).\n\n", diagnostics.len());
    for error in diagnostics {
        out.push_str(&format!("  {}: {}\n", at(source, error.range.start().into()), error.message));
    }
    Ok(out)
}

/// `line:column`, one-based, for a byte offset.
fn at(source: &str, offset: usize) -> String {
    let offset = offset.min(source.len());
    let line = source[..offset].bytes().filter(|b| *b == b'\n').count() + 1;
    let start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let column = source[start..offset].chars().count() + 1;
    format!("{line}:{column}")
}

/// Formats source the way the package it belongs to would.
///
/// **The settings have to come from somewhere, and a string has no package.**
/// This formatted with the defaults while `khora fmt` and the language server
/// both read `[fmt]` from the manifest, so an agent editing a four-space
/// project produced two-space output that the project's own `--check` then
/// rejected. `path` is how a caller says which package the source is from;
/// without one the defaults are still the only honest answer.
fn format(source: &str, path: Option<&str>) -> Result<String> {
    let options = path.map(std::path::Path::new).map(fmt_options).unwrap_or_default();
    match khora_fmt::format_with(source, &options) {
        Ok(text) => Ok(text),
        Err(errors) => {
            let mut out = String::from("This does not parse, so it cannot be formatted.\n\n");
            for error in errors {
                out.push_str(&format!(
                    "  {}: {}\n",
                    at(source, error.range.start().into()),
                    error.message
                ));
            }
            Ok(out)
        }
    }
}

/// The `[fmt]` settings of the package nearest `start`, or the defaults.
fn fmt_options(start: &std::path::Path) -> khora_fmt::Options {
    let mut here = Some(if start.is_dir() { start } else { start.parent().unwrap_or(start) });
    while let Some(dir) = here {
        let manifest = dir.join("khora.toml");
        if manifest.is_file() {
            let Ok(parsed) = khora_manifest::Manifest::load(&manifest) else { break };
            let Some(table) = parsed.manifest.fmt else { break };
            return match table.indent_style {
                Some(khora_manifest::IndentStyle::Tab) => khora_fmt::Options::tabs(),
                Some(khora_manifest::IndentStyle::Space) | None => match table.indent_width {
                    Some(width) => khora_fmt::Options::spaces(width),
                    None => khora_fmt::Options::default(),
                },
            };
        }
        here = dir.parent();
    }
    khora_fmt::Options::default()
}

/// Substring search over names and documentation.
///
/// Public because `khora std search` answers out of it too. The wording is
/// written for whoever reads the result, and both readers want the same thing:
/// what exists, spelled the way the compiler spells it.
pub fn search(query: &str, limit: usize, surface: &[Entry]) -> String {
    let needle = query.to_lowercase();
    let mut hits: Vec<&Entry> = surface
        .iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&needle) || e.doc.to_lowercase().contains(&needle)
        })
        .collect();

    // A name match is what was asked for; a documentation match is a guess.
    hits.sort_by_key(|e| (!e.name.to_lowercase().contains(&needle), e.module.clone(), e.name.clone()));

    if hits.is_empty() {
        return format!(
            "Nothing in `std` matches `{query}`.\n\nIt may not exist — Khora's standard \
             library is small. Do not assume a function exists because another language has it."
        );
    }

    let total = hits.len();
    let mut out = String::new();
    for entry in hits.iter().take(limit) {
        out.push_str(&format!("--- {}::{} ({})\n", entry.module, entry.name, entry.kind));
        if !entry.doc.is_empty() {
            for line in entry.doc.lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
        out.push_str(&format!("\n{}\n\n", entry.signature));
    }
    if total > limit {
        out.push_str(&format!("... and {} more; narrow the query.\n", total - limit));
    }
    out
}

fn list_design_docs() -> String {
    let dir = repo().join("docs").join("design");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "md").then(|| path.file_stem()?.to_str().map(str::to_string))?
        })
        .collect();
    names.sort();
    format!(
        "Design notes. Ask for one by name.\n\n{}\n\nAlso `errata`, which records what \
         turned out to be wrong, and `positioning`, which says what the language is for.\n",
        names.iter().map(|n| format!("  {n}")).collect::<Vec<_>>().join("\n")
    )
}

fn design_doc(name: &str) -> Result<String> {
    // A name with a separator in it would reach outside `docs/`, and this
    // server answers whatever an agent asks.
    if name.contains(['/', '\\', '.']) {
        bail!("`{name}` is not a document name");
    }
    for candidate in [
        format!("docs/design/{name}.md"),
        format!("docs/{name}.md"),
    ] {
        if let Ok(text) = read_repo_file(&candidate) {
            return Ok(text);
        }
    }
    bail!("no design note called `{name}`. Ask with no argument to list them")
}

/// Where the compiler's own tree is.
///
/// Found the same way `std` is: beside the compiler. An installed toolchain
/// ships its documentation next to its standard library.
fn repo() -> std::path::PathBuf {
    if let Some(std_dir) = khora_db::standard_library() {
        if let Some(parent) = std_dir.parent() {
            return parent.to_path_buf();
        }
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read_repo_file(relative: &str) -> Result<String> {
    let path = repo().join(relative);
    std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))
}

/// Every `.kh` file of the standard library, for this target.
fn std_files(db: &KhoraDatabase) -> Vec<SourceFile> {
    let Some(root) = khora_db::standard_library() else { return Vec::new() };
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&here) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out
}

/// The standard library's public surface, read once at start-up.
pub fn read_std_surface() -> Vec<Entry> {
    let db = KhoraDatabase::new();
    let files = std_files(&db);
    SourceRoot::new(&db, files.clone());
    files.iter().flat_map(|f| public_items(&db, *f)).collect()
}
