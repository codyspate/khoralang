//! Assists on documentation comments.
//!
//! **A `///` block in Khora is a published page, not a comment.** `khora doc`
//! renders it into `website/content/docs/stdlib/api`, `khora doc --check`
//! fails when the page and the source disagree, and
//! `scripts/check-api-examples.sh` fails an exported item that requires a
//! capability and carries no example. So the difference between `//` and `///`
//! above a declaration is the difference between a note to the next reader of
//! the file and a paragraph on a website, and it is one character.
//!
//! That is the argument for all three of these. Promoting a comment, adding a
//! heading, adding an example: each is something the doc pipeline is going to
//! ask for, and each is cheaper to do while the code is in front of you than
//! when a gate names the file three days later.

use khora_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use text_size::TextRange;

use super::{Assist, Edit};

/// Every documentation assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(promote(tree, text, selection));
    out.extend(add_example(tree, text, selection));
    out.extend(add_section(tree, text, selection));
    out
}

/// The comment token the cursor is in.
fn comment_at(tree: &SyntaxNode, selection: TextRange) -> Option<SyntaxToken> {
    tree.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::LINE_COMMENT)
        .find(|t| t.text_range().contains_range(selection))
}

/// **`//` promoted to `///`, which publishes it.**
///
/// Offered on an ordinary comment, and the title says where it goes rather
/// than what it looks like: somebody who wanted a note to the next reader of
/// the file has just been offered a paragraph on a website, and should be told
/// so before pressing return.
fn promote(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let token = comment_at(tree, selection)?;
    let written = token.text();
    if written.starts_with("///") || written.starts_with("//!") {
        return None;
    }
    let rest = written.strip_prefix("//")?;
    let _ = text;
    Some(Assist {
        title: "Make it documentation, which publishes it".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: token.text_range(),
            replacement: format!("///{rest}"),
        }],
    })
}

/// **An example block added to a documentation comment.**
///
/// The fence is ```` ```khora ````, which is what `scripts/check-docs.sh`
/// parses and what `check-api-examples.sh` counts. An item that requires a
/// capability has to have one of these before the gate is happy, and this is
/// the shape it wants.
fn add_example(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let token = comment_at(tree, selection)?;
    if !token.text().starts_with("///") {
        return None;
    }
    let last = block_end(tree, &token)?;
    // Already has one, and a second is a different decision.
    if block_text(tree, &token).contains("```khora") {
        return None;
    }
    let indent = indent_of(text, last.text_range().start());
    Some(Assist {
        title: "Add an example block".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(last.text_range().end()),
            replacement: format!(
                "\n{indent}///\n{indent}/// ```khora\n{indent}/// \n{indent}/// ```"
            ),
        }],
    })
}

/// **A `# Heading` added to a documentation comment.**
///
/// `khora doc` renders these as sections on the item's page, which is what
/// turns a long block into something with a shape. The heading is left as a
/// word to replace rather than guessed at.
fn add_section(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let token = comment_at(tree, selection)?;
    if !token.text().starts_with("///") {
        return None;
    }
    let last = block_end(tree, &token)?;
    let indent = indent_of(text, last.text_range().start());
    Some(Assist {
        title: "Add a section heading".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(last.text_range().end()),
            replacement: format!("\n{indent}///\n{indent}/// # Heading"),
        }],
    })
}

/// Every `///` line of the block `token` belongs to, as one string.
fn block_text(tree: &SyntaxNode, token: &SyntaxToken) -> String {
    run(tree, token).iter().map(|t| t.text().to_string()).collect::<Vec<_>>().join("\n")
}

/// The last `///` line of the block `token` belongs to.
fn block_end(tree: &SyntaxNode, token: &SyntaxToken) -> Option<SyntaxToken> {
    run(tree, token).last().cloned()
}

/// The run of `///` lines around `token`, in order.
///
/// Contiguous by line: a blank line or a statement ends the block, because
/// that is what `khora doc` reads as ending it too.
fn run(tree: &SyntaxNode, token: &SyntaxToken) -> Vec<SyntaxToken> {
    let all: Vec<SyntaxToken> = tree
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::LINE_COMMENT && t.text().starts_with("///"))
        .collect();
    let Some(at) = all.iter().position(|t| t.text_range() == token.text_range()) else {
        return Vec::new();
    };

    let mut first = at;
    while first > 0 && adjacent(&all[first - 1], &all[first]) {
        first -= 1;
    }
    let mut last = at;
    while last + 1 < all.len() && adjacent(&all[last], &all[last + 1]) {
        last += 1;
    }
    all[first..=last].to_vec()
}

/// Whether two comment lines are on consecutive lines of the file.
fn adjacent(before: &SyntaxToken, after: &SyntaxToken) -> bool {
    // One newline and whitespace between them, and nothing else.
    let gap = u32::from(after.text_range().start()) - u32::from(before.text_range().end());
    gap < 64
        && before
            .next_token()
            .is_some_and(|t| t.text().chars().filter(|c| *c == '\n').count() == 1)
}

/// The whitespace in front of the line an offset is on.
fn indent_of(text: &str, at: text_size::TextSize) -> String {
    let start = usize::from(at);
    let line = text[..start].rfind('\n').map_or(0, |n| n + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
