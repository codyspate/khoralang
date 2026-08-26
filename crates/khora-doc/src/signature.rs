//! Turning a declaration back into the line a reader wants to see.
//!
//! Three shapes, because three things are wanted:
//!
//! - A **function** is its signature and never its body, collapsed to one line.
//!   A wrapped signature is a formatting decision about a source file and
//!   carries nothing here; a reference wants one line to scan.
//! - A **type**, an **effect** or a **constant** is printed as written. It has
//!   no body to remove, and its shape *is* the documentation -- a variant type
//!   spread over eight lines is easier to read than the same thing on one.
//!   `khora fmt` has already made that text canonical.
//! - A **trait** or an **impl** is its header only. What is inside gets its own
//!   entry, so repeating it here would print every method twice.
//!
//! Comments are removed from all three. A `///` inside a record type belongs to
//! the field it precedes and is emitted there; leaving it in the type's own
//! block would print it twice.

use khora_syntax::ast::{self, AstNode};
use khora_syntax::{SyntaxKind, SyntaxNode};

/// A function's signature: everything up to the body.
pub(crate) fn function(f: &ast::FnDecl) -> String {
    let end = f.body().map(|b| b.syntax().text_range().start());
    let mut out = String::new();
    for token in tokens(f.syntax()) {
        if end.is_some_and(|at| token.text_range().start() >= at) {
            break;
        }
        push(&mut out, token.kind(), token.text());
    }
    // A trait's `fn length(self) -> Int;` has no body and keeps its semicolon,
    // which reads as punctuation rather than as part of the signature.
    out.trim_end_matches([' ', ';']).to_string()
}

/// The declaration as written, with its comments taken out.
pub(crate) fn declaration(node: &SyntaxNode) -> String {
    let mut out = String::new();
    let mut pending_blank = false;
    for line in node.text().to_string().lines() {
        let stripped = without_comment(line);
        if stripped.trim().is_empty() {
            // A line that was nothing but a comment leaves a hole. Remember it
            // rather than printing it, so a blank line between two fields
            // survives but one left behind by a removed comment does not.
            pending_blank = !line.trim().is_empty() || !out.is_empty();
            continue;
        }
        if pending_blank && !out.is_empty() && line.trim().is_empty() {
            out.push('\n');
        }
        pending_blank = false;
        out.push_str(stripped.trim_end());
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// A trait's or an impl's first line: everything before the `{`.
pub(crate) fn header(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for token in tokens(node) {
        if token.kind() == SyntaxKind::L_BRACE {
            break;
        }
        push(&mut out, token.kind(), token.text());
    }
    out.trim().to_string()
}

/// A variant case or a record field, on one line.
pub(crate) fn one_line(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for token in tokens(node) {
        push(&mut out, token.kind(), token.text());
    }
    out.trim().trim_end_matches(',').trim_end().to_string()
}

/// A type as it is written, for naming an impl.
pub(crate) fn type_text(ty: &ast::Type) -> String {
    one_line(ty.syntax())
}

/// Every token under `node`, comments and all, in source order.
fn tokens(node: &SyntaxNode) -> impl Iterator<Item = khora_syntax::SyntaxToken> {
    node.descendants_with_tokens().filter_map(|it| it.into_token())
}

/// Appends one token, deciding whether a space goes in front of it.
///
/// Written as a table of what may *not* have a space before it rather than as a
/// grammar, because it is reassembling a line and not parsing one: `(`, `,` and
/// `<` are the cases, and everything else reads correctly with a space.
fn push(out: &mut String, kind: SyntaxKind, text: &str) {
    use SyntaxKind::*;
    if matches!(kind, WHITESPACE | LINE_COMMENT | BLOCK_COMMENT) {
        return;
    }

    // A trailing comma is a formatting decision about a wrapped signature --
    // `port: Int,` on its own line -- and it has no meaning once the line is
    // one line again.
    if matches!(kind, R_PAREN | R_BRACE | R_BRACK | GT) && out.ends_with(',') {
        out.pop();
    }

    let tight = matches!(
        kind,
        R_PAREN | COMMA | COLON | COLON_COLON | SEMICOLON | DOT | LT | GT | L_BRACK | R_BRACK
    ) || (kind == L_PAREN && ends_with_name(out))
        || out.ends_with(['(', '<', '['])
        || out.ends_with("::")
        || out.ends_with('.')
        || out.is_empty();
    if !tight {
        out.push(' ');
    }
    out.push_str(text);
}

/// Whether the text so far ends in something a `(` should hug.
///
/// `fn scaled(` and `Cons(` hug; `-> (Int, Int)` does not, and neither does the
/// `(` after a `,`. The test is the previous character rather than the previous
/// token because `out` is the only thing this function can see, and a name, a
/// `>` closing type arguments and a `)` closing a group all end one.
fn ends_with_name(out: &str) -> bool {
    out.chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '>')
}

/// One line with any `//` comment removed.
///
/// Cutting at the first `//` outside a string literal. A `//` inside one is
/// rare in a type declaration and getting it wrong would corrupt a default
/// value, so the quotes are counted rather than assumed away.
fn without_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if in_string => at += 1,
            b'"' => in_string = !in_string,
            b'/' if !in_string && bytes.get(at + 1) == Some(&b'/') => return &line[..at],
            _ => {}
        }
        at += 1;
    }
    line
}
