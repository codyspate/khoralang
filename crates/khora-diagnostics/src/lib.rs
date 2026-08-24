//! Rendering diagnostics for humans.
//!
//! This is a library rather than a private helper in the CLI for two reasons.
//! The language server needs the same text, and — per decision A7 in
//! `docs/roadmap.md` — diagnostic quality is a product requirement, which means
//! it needs to be a surface that tests can pin. A private `fn` inside a binary
//! is neither.
//!
//! The layout follows rustc's, because it is the format the audience in
//! `docs/vision.md` already reads fluently:
//!
//! ```text
//! error: expected an identifier
//!  --> src/main.kh:2:6
//!   |
//! 2 | type = ;
//!   |      ^
//! ```

use std::fmt::Write;
use std::path::Path;

use khora_hir::HirError;
use khora_syntax::ParseError;

/// How serious a diagnostic is. Parsing and type checking only produce errors
/// today; the linter in phase 8.1 will produce the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One diagnostic, resolved against its source.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// Byte offsets into the source.
    pub start: usize,
    pub end: usize,
}

impl Diagnostic {
    pub fn from_parse_error(err: &ParseError) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            message: err.message.clone(),
            start: usize::from(err.range.start()),
            end: usize::from(err.range.end()),
        }
    }

    /// A diagnostic from a pass after parsing: name resolution, or the type
    /// checker. Both report through `HirError`, so both render identically.
    pub fn from_hir_error(err: &HirError) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            message: err.message.clone(),
            start: usize::from(err.range.start()),
            end: usize::from(err.range.end()),
        }
    }
}

/// A one-based line and column, plus the text of the line itself.
struct Position<'a> {
    line: usize,
    column: usize,
    text: &'a str,
}

/// Columns count characters, not bytes, so the caret lands correctly under
/// non-ASCII source.
fn locate(source: &str, offset: usize) -> Position<'_> {
    let offset = offset.min(source.len());
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[line_start..].find('\n').map_or(source.len(), |i| line_start + i);
    Position {
        line: source[..line_start].bytes().filter(|b| *b == b'\n').count() + 1,
        column: source[line_start..offset].chars().count() + 1,
        text: &source[line_start..line_end],
    }
}

/// Renders one diagnostic, without a trailing newline.
pub fn render(path: &Path, source: &str, diagnostic: &Diagnostic) -> String {
    let pos = locate(source, diagnostic.start);
    let gutter = pos.line.to_string();
    let pad = " ".repeat(gutter.len());

    // A span running past the end of its line is clamped: pointing at a
    // multi-line region with one caret run is worse than pointing at its start.
    let line_end_offset = diagnostic.start + (pos.text.len() - (pos.column - 1).min(pos.text.len()));
    let end = diagnostic.end.min(line_end_offset).max(diagnostic.start);
    let width = source
        .get(diagnostic.start..end)
        .map(|s| s.chars().count())
        .unwrap_or(1)
        .max(1);

    let mut out = String::new();
    let _ = writeln!(out, "{}: {}", diagnostic.severity.label(), diagnostic.message);
    let _ = writeln!(out, "{pad}--> {}:{}:{}", path.display(), pos.line, pos.column);
    let _ = writeln!(out, "{pad} |");
    let _ = writeln!(out, "{gutter} | {}", pos.text);
    let _ = write!(out, "{pad} | {}{}", " ".repeat(pos.column - 1), "^".repeat(width));
    out
}

/// Renders every diagnostic for one file, separated by blank lines.
pub fn render_all(path: &Path, source: &str, diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| render(path, source, d))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Convenience for the common case: parse errors straight from the front end.
pub fn render_parse_errors(path: &Path, source: &str, errors: &[ParseError]) -> String {
    let diagnostics: Vec<_> = errors.iter().map(Diagnostic::from_parse_error).collect();
    render_all(path, source, &diagnostics)
}

/// The same, for errors from name resolution and type checking.
pub fn render_hir_errors(path: &Path, source: &str, errors: &[HirError]) -> String {
    let diagnostics: Vec<_> = errors.iter().map(Diagnostic::from_hir_error).collect();
    render_all(path, source, &diagnostics)
}

/// The same again, at a severity the caller chooses.
///
/// A lint set to `warn` is not an error and must not print as one — a build
/// that prints `error:` and then succeeds teaches people that the word means
/// nothing. Lints are the only thing whose severity is a decision rather than a
/// fact, which is why this is the only entry point that takes one.
pub fn render_hir_errors_as(
    path: &Path,
    source: &str,
    errors: &[HirError],
    severity: Severity,
) -> String {
    let diagnostics: Vec<_> = errors
        .iter()
        .map(|e| Diagnostic { severity, ..Diagnostic::from_hir_error(e) })
        .collect();
    render_all(path, source, &diagnostics)
}
