//! The `///` block above a declaration, for whoever needs to read it.
//!
//! `khora doc` publishes it, and `derive(Decode)` carries a field's into its
//! schema, so that a rendered document and a model's prompt say what the
//! author already wrote. One reader, so the two cannot disagree about where
//! a comment ends.

use crate::{SyntaxElement, SyntaxKind, SyntaxNode};

/// The `///` lines immediately above `node`, in order, with the marker and
/// one following space removed.
///
/// **A blank line ends it.** Two comment blocks separated by one are two
/// different things -- the upper one is almost always about the item above,
/// or about the section -- and running them together produces documentation
/// that starts mid-thought. Anything that is not whitespace or a `///` line
/// ends it too, which is what stops a plain `//` note from being published.
pub fn doc_comment(node: &SyntaxNode) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cursor = preceding(node);
    while let Some(element) = cursor {
        let Some(token) = element.as_token() else { break };
        match token.kind() {
            SyntaxKind::WHITESPACE => {
                if token.text().matches('\n').count() > 1 {
                    break;
                }
            }
            SyntaxKind::LINE_COMMENT => match strip(token.text(), "///") {
                Some(text) => lines.push(text),
                None => break,
            },
            _ => break,
        }
        cursor = element.prev_sibling_or_token();
    }
    lines.reverse();
    lines
}

/// What comes immediately before `node` in the file, looking outward.
///
/// **The comment for a first child is written outside its parent.** The `///`
/// above the first case of a variant type sits between the `=` and the
/// `VARIANT_TYPE` node -- a sibling of the whole list, not of the case -- so
/// asking the case for its previous sibling gets nothing. Climbing while the
/// node starts where its parent does finds it, and stops as soon as there is
/// anything else in between.
pub fn preceding(node: &SyntaxNode) -> Option<SyntaxElement> {
    let mut here = node.clone();
    loop {
        if let Some(found) = here.prev_sibling_or_token() {
            return Some(found);
        }
        let parent = here.parent()?;
        if parent.text_range().start() != here.text_range().start() {
            return None;
        }
        here = parent;
    }
}

/// The text of a comment line after `marker`, or `None` when the line is
/// not one: `////` is a divider somebody drew, not four slashes of
/// documentation.
pub fn strip(text: &str, marker: &str) -> Option<String> {
    let rest = text.trim_end_matches(['\r', '\n']).strip_prefix(marker)?;
    if marker == "///" && rest.starts_with('/') {
        return None;
    }
    Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
}
