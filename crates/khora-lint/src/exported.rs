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
/// **The same rule `khora_doc::doc_of` uses, and that is the requirement.**
/// A blank line ends the block, and so does an ordinary `//` note. This lint
/// was written more leniently at first — skipping both — and the two answers
/// disagreeing is a bug rather than a matter of taste: `khora check` would
/// call an item documented while `khora doc` published nothing for it, which
/// is the worst of both, since the page is what a reader actually gets.
///
/// So a `///` block has to be the thing immediately above the declaration.
/// Anything between detaches it, in both tools, the same way.
fn documented(node: &SyntaxNode) -> bool {
    let mut before = node.first_token().and_then(|token| token.prev_token());
    while let Some(token) = before {
        match token.kind() {
            // `////` is a divider somebody drew, not documentation, which is
            // the one place `strip` is subtler than a prefix test.
            SyntaxKind::LINE_COMMENT => {
                return token.text().starts_with("///") && !token.text().starts_with("////")
            }
            SyntaxKind::WHITESPACE => {
                if token.text().matches('\n').count() > 1 {
                    return false;
                }
            }
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

/// A function whose constructor name disagrees with its shape.
pub struct Misnamed {
    /// The heading, to point at.
    pub range: TextRange,
    /// What to say about it.
    pub message: String,
}

/// Functions named `new`, `empty`, `root` or `of*` whose arity contradicts
/// what the name claims.
///
/// `docs/design/naming.md` is the rule and the message points there. Only the
/// two consequences a machine can check are checked -- whether a thing *grows*
/// decides `new` against `empty` and no compiler can see it.
///
/// **A function that did not choose one of these names is never reported.**
/// Calling something `make` or `create` opts out entirely, which is what lets
/// this default to `warn` without having an opinion about anybody's package.
pub fn misnamed_constructors(tree: &SyntaxNode) -> Vec<Misnamed> {
    let mut out = Vec::new();
    for node in tree.descendants() {
        if node.kind() != SyntaxKind::FN_DECL {
            continue;
        }
        let Some(name) = name_of(&node) else { continue };
        let takes = parameters(&node);

        // `of` is a conversion, so it takes what it converts from.
        let converts = name == "of" || name.starts_with("of_");
        if converts && takes == 0 {
            out.push(Misnamed {
                range: heading(&node),
                message: format!(
                    "`{name}` names a conversion and takes nothing to convert. `docs/design/naming.md`"
                ),
            });
            continue;
        }

        // `new`, `empty` and `root` name a thing there is one obvious version
        // of, so an argument means the name describes something else.
        if matches!(name.as_str(), "new" | "empty" | "root") && takes > 0 {
            out.push(Misnamed {
                range: heading(&node),
                message: format!(
                    "`{name}` names an empty or outermost one and takes {takes} argument(s). `docs/design/naming.md`"
                ),
            });
        }
    }
    out
}

/// How many parameters a declaration takes, not counting `self`.
fn parameters(node: &SyntaxNode) -> usize {
    let Some(list) = node.children().find(|c| c.kind() == SyntaxKind::PARAM_LIST) else {
        return 0;
    };
    list.children()
        .filter(|c| c.kind() == SyntaxKind::PARAM)
        .filter(|param| {
            // `self` is an ordinary `PARAM` in the tree, and a receiver is not
            // an argument for this purpose: `Method::of(text)` and a method
            // `of(self, text)` are the same claim about the same name.
            param.text().to_string().trim() != "self"
        })
        .count()
}

/// Every function declaration with this name, by the range of its name.
///
/// `descendants` rather than `children`, because that is where the
/// declarations are -- the same walk `misnamed_constructors` does, and the
/// reason this returned nothing on its first run.
///
/// The name's range rather than the declaration's, so the caret lands on the
/// word the reader has to change.
pub(crate) fn functions_named(tree: &SyntaxNode, wanted: &str) -> Vec<TextRange> {
    let mut out = Vec::new();
    for node in tree.descendants() {
        if node.kind() != SyntaxKind::FN_DECL {
            continue;
        }
        let named = node
            .descendants_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|token| token.kind() == SyntaxKind::IDENT);
        if let Some(token) = named {
            if token.text() == wanted {
                out.push(token.text_range());
            }
        }
    }
    out
}
