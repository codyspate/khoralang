//! `undocumented-export`: a public item nobody described in one line.
//!
//! `khora-doc/tests/std_surface.rs` holds this floor for `std`, as a Rust
//! test, and its reasoning generalises:
//!
//! > An item nobody could be bothered to describe in one line is an item
//! > nobody has decided to promise, and it should not reach 1.0 by default.
//!
//! As a lint it serves every package rather than only the standard library,
//! at whatever level each one chooses. Roadmap 14.26.
//!
//! # Off by default, unlike the rest
//!
//! Rust's `missing_docs` is allow-by-default for the reason that applies here
//! too: a young package with forty undocumented `pub` functions gets forty
//! warnings on the first build, and the response to forty warnings is not to
//! write forty doc comments. It is `[lints] undocumented-export = "warn"` when
//! a package decides its surface is a promise, which is the point at which the
//! lint is telling somebody something they want to know.
//!
//! `std` keeps its Rust test rather than switching this on, because `std` is
//! found beside the compiler and has no manifest to switch anything on in.
//!
//! # Read from the tree, because nothing else carries a doc comment
//!
//! The HIR drops `///` entirely — it is not something a later pass needs — and
//! `khora_doc::Item` has the text but no source range, because it exists to
//! render a page. The CST is lossless and has both, so this walks it.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

/// A public declaration and whether anything documents it.
pub struct Export {
    /// What to point at: the `pub` and the name, not the whole body.
    pub range: TextRange,
    /// What kind of thing it is, for the message.
    pub what: &'static str,
    /// The name, when the declaration has one to read.
    pub name: Option<String>,
}

/// Every `pub` declaration in `tree` that no `///` block precedes.
pub fn undocumented(tree: &SyntaxNode) -> Vec<Export> {
    let mut out = Vec::new();
    for node in tree.descendants() {
        let Some(what) = declaration(node.kind()) else { continue };
        if !is_public(&node) {
            continue;
        }
        if documented(&node) {
            continue;
        }
        let name = name_of(&node);
        // **`main` is not a surface.** Its `pub` is what the language requires
        // of an entry point, not a promise to anybody: nobody imports your
        // `main`, so "describe it or stop exporting it" offers a choice that
        // does not exist. Every fixture in this crate's own suppression tests
        // is a `pub fn main`, which is how obvious this became.
        if name.as_deref() == Some("main") && what == "function" {
            continue;
        }
        out.push(Export { range: heading(&node), what, name });
    }
    out
}

/// What a node is called in a message, if it is a declaration at all.
///
/// An `impl` is not here. It has no name of its own and its *methods* are the
/// surface — those are `FN_DECL`s inside it and are reached as ordinary
/// descendants.
fn declaration(kind: SyntaxKind) -> Option<&'static str> {
    match kind {
        SyntaxKind::FN_DECL => Some("function"),
        SyntaxKind::TYPE_DECL => Some("type"),
        SyntaxKind::TRAIT_DECL => Some("trait"),
        SyntaxKind::EFFECT_DECL => Some("effect"),
        SyntaxKind::CONST_DECL => Some("constant"),
        _ => None,
    }
}

/// Whether the declaration is exported.
fn is_public(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .any(|token| token.kind() == SyntaxKind::PUB_KW)
}

/// Whether a `///` block sits immediately above it.
///
/// Walks back over whitespace and ordinary comments. A blank line does not
/// break the association: `/// text` then a blank then the item still reads as
/// documentation of the item, and treating it otherwise would report something
/// that is plainly documented.
fn documented(node: &SyntaxNode) -> bool {
    let mut before = node.first_token().and_then(|token| token.prev_token());
    while let Some(token) = before {
        match token.kind() {
            SyntaxKind::LINE_COMMENT => {
                if token.text().starts_with("///") {
                    return true;
                }
                // An ordinary `//` note above an item is not documentation,
                // and does not stop a `///` above *it* from being.
            }
            SyntaxKind::WHITESPACE | SyntaxKind::BLOCK_COMMENT => {}
            _ => return false,
        }
        before = token.prev_token();
    }
    false
}

/// The declaration's first line, rather than the whole body.
///
/// A finding whose range covers a two-hundred-line type is a finding that
/// underlines two hundred lines. This stops at the first `{` or `=`, which is
/// where the heading ends in every declaration that has one.
fn heading(node: &SyntaxNode) -> TextRange {
    let start = node.text_range().start();
    for token in node.descendants_with_tokens().filter_map(|it| it.into_token()) {
        if matches!(token.kind(), SyntaxKind::L_BRACE | SyntaxKind::EQ) {
            return TextRange::new(start, token.text_range().start());
        }
    }
    node.text_range()
}

/// The declared name, read as the first identifier after the keyword.
fn name_of(node: &SyntaxNode) -> Option<String> {
    node.descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|token| token.kind() == SyntaxKind::IDENT)
        .map(|token| token.text().to_string())
}
