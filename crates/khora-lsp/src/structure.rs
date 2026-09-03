//! What an editor folds, and what it selects when you press expand.
//!
//! Both questions are about the shape of the tree rather than about types, so
//! both are answered from the syntax alone. That is what makes them work in a
//! file that does not compile, which is the file somebody is most often
//! looking at.
//!
//! # Folding
//!
//! A fold is offered for anything with a body worth collapsing: a function, an
//! impl block, a type with cases or fields, a test, a match, and a run of
//! imports. Not for every brace — an `if` inside an expression folded away is a
//! line that now lies about what it does.
//!
//! **The run of imports is the one worth having and the one nothing gives you
//! by accident.** A file's imports are separate declarations, so no node spans
//! them; the run has to be found by walking the declarations and noticing
//! where one kind stops.
//!
//! # Selection
//!
//! Expand-selection is the ancestor chain of the token under the cursor, from
//! the smallest node outward. rowan keeps every byte, so this is a walk rather
//! than anything to be computed: the interesting work is discarding the steps
//! that do not change the selection, which would make the key press do nothing
//! and read as a broken editor.

use khora_db::{Db, SourceFile};
use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::{TextRange, TextSize};

/// A region an editor may collapse.
pub struct Fold {
    pub range: TextRange,
    /// `imports`, `comment`, or `None` for an ordinary region.
    pub kind: Option<&'static str>,
}

/// Every foldable region in a file.
pub fn folds(db: &dyn Db, file: SourceFile) -> Vec<Fold> {
    let tree = khora_db::parse(db, file).syntax();
    let mut out = Vec::new();

    for node in tree.descendants() {
        let foldable = matches!(
            node.kind(),
            SyntaxKind::FN_DECL
                | SyntaxKind::IMPL_DECL
                | SyntaxKind::TRAIT_DECL
                | SyntaxKind::EFFECT_DECL
                | SyntaxKind::CONTEXT_DECL
                | SyntaxKind::TEST_DECL
                | SyntaxKind::BENCH_DECL
                | SyntaxKind::TYPE_DECL
                | SyntaxKind::MATCH_EXPR
        );
        if foldable {
            out.push(Fold { range: node.text_range(), kind: None });
        }
    }

    if let Some(range) = import_run(&tree) {
        out.push(Fold { range, kind: Some("imports") });
    }

    for element in tree.descendants_with_tokens() {
        if let Some(token) = element.as_token() {
            if token.kind() == SyntaxKind::BLOCK_COMMENT {
                out.push(Fold { range: token.text_range(), kind: Some("comment") });
            }
        }
    }

    // A fold that starts and ends on the same line is not a fold, and an
    // editor asked to draw one puts a chevron next to a line that cannot
    // collapse. Filtering here rather than in the caller keeps the rule with
    // the reason.
    let text = file.text(db);
    out.retain(|fold| spans_lines(text, fold.range));
    out.sort_by_key(|fold| (fold.range.start(), fold.range.end()));
    out
}

/// The run of `import` declarations at the top of a file, if there are two.
///
/// One import is not a run: collapsing a single line saves nothing and costs
/// the reader the one thing the line said.
fn import_run(tree: &SyntaxNode) -> Option<TextRange> {
    let mut first: Option<TextRange> = None;
    let mut last: Option<TextRange> = None;
    let mut count = 0;
    for node in tree.children() {
        match node.kind() {
            SyntaxKind::IMPORT_DECL => {
                count += 1;
                first.get_or_insert(node.text_range());
                last = Some(node.text_range());
            }
            SyntaxKind::MODULE_DECL => {}
            // The first thing that is not an import ends the run, so a second
            // group further down the file is not swept in with it.
            _ if count > 0 => break,
            _ => {}
        }
    }
    match (count >= 2, first, last) {
        (true, Some(first), Some(last)) => Some(TextRange::new(first.start(), last.end())),
        _ => None,
    }
}

fn spans_lines(text: &str, range: TextRange) -> bool {
    let start = usize::from(range.start()).min(text.len());
    let end = usize::from(range.end()).min(text.len());
    text[start..end].contains('\n')
}

/// The ranges an editor should step through as it widens a selection.
///
/// Smallest first, each containing the one before it. Steps that do not widen
/// the selection are dropped: a node whose range equals its child's would make
/// the key press appear to do nothing.
pub fn selection_chain(db: &dyn Db, file: SourceFile, offset: TextSize) -> Vec<TextRange> {
    let tree = khora_db::parse(db, file).syntax();
    let token = match tree.token_at_offset(offset) {
        rowan::TokenAtOffset::None => return Vec::new(),
        rowan::TokenAtOffset::Single(token) => token,
        rowan::TokenAtOffset::Between(left, right) => {
            if left.kind() == SyntaxKind::IDENT {
                left
            } else {
                right
            }
        }
    };

    let mut out = vec![token.text_range()];
    for node in token.parent_ancestors() {
        let range = node.text_range();
        if out.last().is_some_and(|seen| *seen == range) {
            continue;
        }
        out.push(range);
    }
    out
}
