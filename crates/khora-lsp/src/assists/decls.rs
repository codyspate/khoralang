//! Assists on declarations: what is exported, what is documented, and what a
//! signature says about itself.
//!
//! **`pub` is the one with teeth.** Adding it is how a module grows a public
//! surface, and a public surface is the thing a library cannot take back
//! without a version somebody has to read about. So the assist that adds it
//! says so in its title rather than presenting it as tidying, and the one that
//! removes it is offered freely: shrinking what you promise is always safe.
//!
//! The doc-comment skeleton is here because of what `khora doc` does with it.
//! A `///` block is not a comment that happens to be above a declaration — it
//! is the page, and `check-api-examples.sh` will ask an exported item that
//! requires a capability for an example. Starting the block with the headings
//! it will be judged on is cheaper than being told later.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering};

/// Every declaration assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(export(tree, text, selection));
    out.extend(unexport(tree, text, selection));
    out.extend(document(tree, text, selection));
    out.extend(add_raises(tree, text, selection));
    out.extend(add_with(tree, text, selection));
    out
}

/// Declaration kinds a reader can make public.
const DECLARATIONS: [SyntaxKind; 6] = [
    SyntaxKind::FN_DECL,
    SyntaxKind::TYPE_DECL,
    SyntaxKind::CONST_DECL,
    SyntaxKind::EFFECT_DECL,
    SyntaxKind::TRAIT_DECL,
    SyntaxKind::LET_DECL,
];

/// The declaration the cursor is in, of any kind that can be exported.
fn declaration(tree: &SyntaxNode, selection: TextRange) -> Option<SyntaxNode> {
    DECLARATIONS
        .iter()
        .filter_map(|kind| covering(tree, selection, *kind))
        .min_by_key(|node| node.text_range().len())
}

/// Whether a declaration already says `pub`.
fn is_public(node: &SyntaxNode) -> bool {
    node.children_with_tokens().any(|e| e.kind() == SyntaxKind::PUB_KW)
}

/// **`pub` added, and the title says what that costs.**
///
/// Everything a module exports is something it has promised, and a promise is
/// the part of a library that cannot be taken back quietly. The wording is the
/// assist's whole contribution beyond four characters.
fn export(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = declaration(tree, selection)?;
    if is_public(&node) {
        return None;
    }
    let _ = text;
    Some(Assist {
        title: "Export it with `pub`, which is a promise".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().start()),
            replacement: "pub ".to_string(),
        }],
    })
}

/// **And `pub` taken off**, which is always safe: shrinking what you promise
/// breaks nothing outside, and the compiler names anything inside that was
/// relying on it.
fn unexport(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = declaration(tree, selection)?;
    let keyword = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::PUB_KW)?;
    let _ = text;
    Some(Assist {
        title: "Stop exporting it".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::new(
                keyword.text_range().start(),
                keyword.text_range().end() + text_size::TextSize::from(1),
            ),
            replacement: String::new(),
        }],
    })
}

/// **A `///` block started above a declaration that has none.**
///
/// The first line is the summary `khora doc` renders under the heading, so it
/// is left empty for somebody to write rather than filled with the item's own
/// name said twice.
fn document(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = declaration(tree, selection)?;
    // A block already there is the author saying it, however short.
    if documented(text, &node) {
        return None;
    }
    let indent = indent_of(text, &node);
    Some(Assist {
        title: "Start a documentation comment".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(line_start(text, &node)),
            replacement: format!("{indent}/// \n"),
        }],
    })
}

/// Whether the line above a declaration is a `///` comment.
fn documented(text: &str, node: &SyntaxNode) -> bool {
    let start = usize::from(line_start(text, node));
    let before = &text[..start];
    before.trim_end().lines().next_back().is_some_and(|line| line.trim_start().starts_with("///"))
}

/// **A `raises` clause added to a signature that has none.**
///
/// The type is left as a hole rather than guessed at: which failure a function
/// raises is a decision, and the compiler's message when the hole is wrong is
/// better than an editor's guess when it is nearly right.
fn add_raises(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::FN_DECL)?;
    if node.children().any(|n| n.kind() == SyntaxKind::RAISES_CLAUSE) {
        return None;
    }
    let at = clause_position(&node)?;
    let _ = text;
    Some(Assist {
        title: "Add a `raises` clause".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: TextRange::empty(at), replacement: " raises ".to_string() }],
    })
}

/// **A `with` clause added to a signature that has none.**
///
/// Same argument, and the same hole. `fixes.rs` fills this in from a
/// diagnostic when the compiler already knows which capability is missing;
/// this is the one for writing the signature first.
fn add_with(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::FN_DECL)?;
    if node.children().any(|n| n.kind() == SyntaxKind::WITH_CLAUSE) {
        return None;
    }
    let at = clause_position(&node)?;
    let _ = text;
    Some(Assist {
        title: "Add a `with` clause".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: TextRange::empty(at), replacement: " with {  }".to_string() }],
    })
}

/// Where an effect clause goes: after the return type and any clause already
/// there, and before the body.
///
/// Read off the tree rather than found in the text, because a return type may
/// contain a brace — a record type — and looking for the body's `{` finds that
/// one instead.
fn clause_position(node: &SyntaxNode) -> Option<text_size::TextSize> {
    let body = node.children().find(|n| n.kind() == SyntaxKind::BLOCK)?;
    let before = node
        .children()
        .filter(|n| n.text_range().end() <= body.text_range().start())
        .map(|n| n.text_range().end())
        .max()?;
    Some(before)
}

/// The offset the declaration's own line starts at.
fn line_start(text: &str, node: &SyntaxNode) -> text_size::TextSize {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    (line as u32).into()
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
