//! Assists on import declarations.
//!
//! **Import lists rot in a particular way**: two lines importing the same
//! module because two people added a name a week apart, an alphabetical order
//! that stopped being alphabetical, a name left behind by the code that used
//! it. None of it is hard to fix and all of it is fiddly enough that nobody
//! does it while thinking about something else, which is when it happens.
//!
//! Every edit here rewrites one declaration or removes one, so nothing has to
//! reason about what an import *means* — only about the text of the list.

use khora_syntax::{SyntaxKind, SyntaxNode, ast::AstNode, ast::ImportDecl};
use text_size::TextRange;

use super::{Assist, Edit, covering};

/// Every import assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(sort_names(tree, text, selection));
    out.extend(merge_duplicates(tree, text, selection));
    out.extend(split_list(tree, text, selection));
    out
}

/// The module a declaration imports from, and the names it brings.
fn parts(node: &SyntaxNode) -> Option<(String, Vec<String>)> {
    let decl = ImportDecl::cast(node.clone())?;
    if decl.is_glob() {
        return None;
    }
    let path = decl.path().map(|p| p.text_path())?;
    let names: Vec<String> = decl.items().filter_map(|i| i.name()?.ident()).collect();
    if names.is_empty() {
        return None;
    }
    Some((path, names))
}

/// One import line, written the way this file writes them.
fn written(module: &str, names: &[String]) -> String {
    format!("import {module}::{{{}}};", names.join(", "))
}

/// **The names in one import put in order.**
///
/// Offered only when they are not already, so it is silent on a tidy file
/// rather than offering an edit that changes nothing.
fn sort_names(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IMPORT_DECL)?;
    let (module, names) = parts(&node)?;
    let mut sorted = names.clone();
    sorted.sort();
    if sorted == names {
        return None;
    }
    let _ = text;
    Some(Assist {
        title: "Sort the imported names".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: written(&module, &sorted),
        }],
    })
}

/// **Two imports of one module folded into one.**
///
/// The second line goes and the first grows, so the names keep the order they
/// were written in and a reader's eye does not have to move. Sorting is a
/// separate assist, deliberately: merging and reordering at once makes a diff
/// nobody can read.
fn merge_duplicates(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IMPORT_DECL)?;
    let (module, mut names) = parts(&node)?;

    let others: Vec<SyntaxNode> = tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::IMPORT_DECL)
        .filter(|n| n.text_range() != node.text_range())
        .filter(|n| parts(n).is_some_and(|(path, _)| path == module))
        .collect();
    if others.is_empty() {
        return None;
    }

    let mut edits = Vec::new();
    for other in &others {
        let (_, more) = parts(other)?;
        for name in more {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        edits.push(Edit { range: whole_line(text, other), replacement: String::new() });
    }
    edits.push(Edit { range: node.text_range(), replacement: written(&module, &names) });

    Some(Assist {
        title: format!("Merge the imports of `{module}`"),
        kind: "refactor.rewrite",
        edits,
    })
}

/// **One import per name**, for when a list has grown past reading.
///
/// The other direction from merging, and worth having for the same reason a
/// version-control diff is: one name per line means adding one touches one
/// line, and two branches adding two names do not conflict.
fn split_list(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IMPORT_DECL)?;
    let (module, names) = parts(&node)?;
    if names.len() < 2 {
        return None;
    }
    let indent = indent_of(text, &node);
    let lines: Vec<String> =
        names.iter().map(|name| written(&module, std::slice::from_ref(name))).collect();
    Some(Assist {
        title: "Split into one import per name".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: lines.join(&format!("\n{indent}")),
        }],
    })
}

/// A node's range extended to the whole lines it sits on, so removing it does
/// not leave a blank line behind.
fn whole_line(text: &str, node: &SyntaxNode) -> TextRange {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let end = usize::from(node.text_range().end());
    let to = text[end..].find('\n').map_or(text.len(), |at| end + at + 1);
    TextRange::new((line as u32).into(), (to as u32).into())
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
