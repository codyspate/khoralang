//! Assists on strings, numbers and record literals.
//!
//! **The string one earns its place.** `"a " + name + " b"` and
//! `"a ${name} b"` are the same string, and the second is the one people mean
//! and the first is what gets written when a message grows a piece at a time.
//! Converting by hand means counting quotes, which is where the missing space
//! comes from.
//!
//! `${..}` is part of the literal as far as the lexer is concerned — a string
//! is one token and interpolation is taken apart later — so everything here
//! works on the token's text, and the rules about what may appear inside a
//! hole are the parser's rather than this file's.
//!
//! **There is no "write it as hexadecimal", and there was.** It produced
//! `0xFF`, which Khora does not lex: the language has decimal integer literals
//! and an underscore separator, and no other base. The assist was written, it
//! passed every assertion about the text it emitted, and the check that
//! compiles the result is what caught it. An assist that writes a language
//! feature the language does not have is worse than a missing one, because it
//! looks like the editor knows something.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every literal assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(to_interpolation(tree, text, selection));
    out.extend(add_record_base(tree, text, selection));
    out.extend(underscore_digits(tree, text, selection));
    out
}

/// **`"a " + x + " b"` becomes `"a ${x} b"`.**
///
/// Offered on a `+` chain whose ends are string literals. Every piece that is
/// already a literal keeps its text; every piece that is not becomes a hole.
///
/// Refused when a piece is itself a string containing `${`, because the result
/// would nest a hole inside a hole and this is not the thing to decide that.
///
/// **A `$` that meets a `{` across a join is escaped.** `"$" + "{a}"` prints
/// `${a}`, because neither literal holds a hole; written back as one literal
/// with nothing done, it is `"${a}"`, which prints the value of `a`. So a
/// literal piece that begins with `{` after one that ends in an unescaped `$`
/// writes that `$` as `\$`. A `$` before a hole is safe as it stands: `$${a}`
/// is a dollar and then a hole.
fn to_interpolation(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::BIN_EXPR)?;
    // The outermost `+` chain the cursor is in, so a three-piece message is
    // one assist rather than two nested ones.
    let whole = node
        .ancestors()
        .take_while(|a| a.kind() == SyntaxKind::BIN_EXPR && joins_with_plus(a))
        .last()?;
    if !joins_with_plus(&whole) {
        return None;
    }

    let mut pieces = Vec::new();
    flatten(&whole, &mut pieces);
    // At least one literal, or this is arithmetic and not a message.
    if !pieces.iter().any(|p| p.kind() == SyntaxKind::LITERAL_EXPR) {
        return None;
    }

    let mut built = String::new();
    for piece in &pieces {
        let written = text_of(text, piece);
        let trimmed = written.trim();
        if let Some(inner) = as_string(trimmed) {
            if inner.contains("${") {
                return None;
            }
            if inner.starts_with('{') && ends_in_bare_dollar(&built) {
                built.pop();
                built.push_str("\\$");
            }
            built.push_str(inner);
        } else {
            // A hole holds an expression, and the parser reads to the matching
            // brace, so nothing here has to be parenthesised.
            built.push_str(&format!("${{{trimmed}}}"));
        }
    }

    Some(Assist {
        title: "Write it as one interpolated string".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: whole.text_range(),
            replacement: format!("\"{built}\""),
        }],
    })
}

/// Whether `built` ends in a `$` that no backslash escapes.
///
/// Counted rather than looked at one character back: `\\$` is an escaped
/// backslash and then a bare dollar, and is exactly the case a one-character
/// look would get wrong.
fn ends_in_bare_dollar(built: &str) -> bool {
    let Some(before) = built.strip_suffix('$') else { return false };
    before.chars().rev().take_while(|&c| c == '\\').count() % 2 == 0
}

/// Whether a binary expression's operator is `+`.
fn joins_with_plus(node: &SyntaxNode) -> bool {
    node.children_with_tokens().filter_map(|e| e.into_token()).any(|t| t.kind() == SyntaxKind::PLUS)
}

/// Every operand of a left-nested `+` chain, in written order.
fn flatten(node: &SyntaxNode, into: &mut Vec<SyntaxNode>) {
    if node.kind() == SyntaxKind::BIN_EXPR && joins_with_plus(node) {
        for child in node.children() {
            flatten(&child, into);
        }
    } else {
        into.push(node.clone());
    }
}

/// The contents of a plain string literal, or `None` for anything else.
fn as_string(written: &str) -> Option<&str> {
    written.strip_prefix('"')?.strip_suffix('"')
}

/// **`{ ..old, field: value }` — a record built from another.**
///
/// Offered on a record literal that does not have a base. Writing every field
/// out is what somebody does when they do not know the spread exists, and it
/// is also what breaks the next time a field is added to the type.
fn add_record_base(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::RECORD_EXPR)?;
    if node.children().any(|n| n.kind() == SyntaxKind::RECORD_EXPR_BASE) {
        return None;
    }
    let first = node.children().find(|n| n.kind() == SyntaxKind::RECORD_EXPR_FIELD)?;
    let _ = text;
    Some(Assist {
        title: "Build it from another record with `..`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(first.text_range().start()),
            replacement: "..todo(), ".to_string(),
        }],
    })
}

/// **`1000000` becomes `1_000_000`.**
///
/// The one place a typo is invisible: a number with the wrong number of zeros
/// looks exactly like the right one, and the separator is what makes counting
/// unnecessary.
fn underscore_digits(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LITERAL_EXPR)?;
    let written = text_of(text, &node);
    let digits = written.trim();
    if digits.contains('_') || !digits.chars().all(|c| c.is_ascii_digit()) || digits.len() < 5 {
        return None;
    }
    let grouped: String = digits
        .chars()
        .rev()
        .collect::<Vec<_>>()
        .chunks(3)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .rev()
        .collect();
    Some(Assist {
        title: format!("Group the digits as `{grouped}`"),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: node.text_range(), replacement: grouped }],
    })
}
