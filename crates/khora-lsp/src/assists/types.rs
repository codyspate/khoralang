//! Assists on type, effect and trait declarations, and on the things generated
//! from them.
//!
//! **Two of these write code from a declaration**, which is the kind of assist
//! that saves the most and is wrong most expensively. A handler skeleton and
//! an `impl` block are both a list of names with holes in them, and the list
//! comes from the declaration rather than from a guess — `members.rs` and
//! `handlers.rs` already do this work for completion and for a quick fix, so
//! what is here is the way to ask for it with the cursor rather than by
//! waiting for a diagnostic.
//!
//! The rest are the small ones: a case, a field, a `derive`. They exist for
//! the same reason the match-arm assist does, which is punctuation — a variant
//! case is introduced by `|` and the last field of a record may or may not
//! have a comma, and both are decided by what is already there.

use khora_syntax::{SyntaxKind, SyntaxNode, ast::AstNode, ast::EffectDecl};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every type assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(generate_handler(tree, text, selection));
    out.extend(generate_impl(tree, text, selection));
    out.extend(add_case(tree, text, selection));
    out.extend(add_field(tree, text, selection));
    out.extend(add_derive(tree, text, selection));
    out
}

/// The name a declaration gives, which is its first `NAME` child.
fn declared_name(node: &SyntaxNode, text: &str) -> Option<String> {
    let name = node.children().find(|n| n.kind() == SyntaxKind::NAME)?;
    Some(text_of(text, &name).trim().to_string())
}

/// **A whole handler written from an effect declaration.**
///
/// Every operation, with a closure of the right arity and a hole in it. The
/// list is the declaration's rather than a guess, which is the difference
/// between this and typing it out: an effect that grows an operation makes
/// every hand-written handler fail to compile, and this one is generated from
/// the thing that changed.
///
/// The label is the effect's own name in lower case, because nothing at a
/// declaration says what a use site will call it — `std` installs
/// `LLMService` as `ai` — and a name that has to be changed is better than a
/// row entry that is not there.
fn generate_handler(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::EFFECT_DECL)?;
    let effect = EffectDecl::cast(node.clone())?;
    let name = declared_name(&node, text)?;
    let label = crate::handlers::label_for(&name);
    let written = crate::handlers::skeleton(&effect, &label)?;

    // As a `const`, which is what a handler written once and installed in
    // several places is.
    let indent = indent_of(text, &node);
    Some(Assist {
        title: format!("Write a handler for `{name}`"),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().end()),
            replacement: format!("\n\n{indent}const a_{label} = {};", handler_of(&written)),
        }],
    })
}

/// The handler out of a row entry.
///
/// `handlers::skeleton` answers `label: handler for E { .. }`, because its
/// other caller is putting one in a `with` row. A `const` wants the half after
/// the label, and the split is on the first `: ` -- which is the separator that
/// function writes and never appears before it, since a label is one
/// identifier.
fn handler_of(entry: &str) -> String {
    entry.split_once(": ").map_or_else(|| entry.to_string(), |(_, rest)| rest.trim().to_string())
}

/// **An `impl` block for the type at the cursor.**
///
/// Empty, because what goes in it is the whole question and an editor guessing
/// would be wrong more often than not. What it saves is the header, which has
/// to name the type and its parameters exactly.
fn generate_impl(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::TYPE_DECL)?;
    let name = declared_name(&node, text)?;
    // The type's own parameters, so `impl<A> List<A>` rather than `impl List`.
    let params = node
        .children()
        .find(|n| n.kind() == SyntaxKind::TYPE_PARAMS)
        .map(|n| text_of(text, &n))
        .unwrap_or_default();
    let indent = indent_of(text, &node);
    Some(Assist {
        title: format!("Write an `impl` block for `{name}`"),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().end()),
            replacement: format!("\n\n{indent}impl{params} {name}{params} {{\n{indent}}}"),
        }],
    })
}

/// **A case added to a variant type.**
///
/// The `|` is the reason: a variant type introduces every case with one,
/// including the first, and the last has no trailing anything — so adding a
/// case by hand means looking at what is above it.
fn add_case(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::VARIANT_TYPE)?;
    let last = node.children().filter(|n| n.kind() == SyntaxKind::VARIANT_CASE).last()?;
    let indent = indent_of(text, &node);
    let _ = text;
    Some(Assist {
        title: "Add a case".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(last.text_range().end()),
            replacement: format!("\n{indent}  | Case"),
        }],
    })
}

/// **A field added to a record type.**
///
/// Same argument as the case, with the comma the other way round: a record's
/// fields are separated rather than introduced, so the new one needs a comma
/// in front of it unless the list already ends with one.
fn add_field(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::RECORD_TYPE)?;
    let last = node.children().filter(|n| n.kind() == SyntaxKind::FIELD).last()?;
    let after = &text[usize::from(last.text_range().end())..];
    let comma = if after.trim_start().starts_with(',') { "" } else { "," };
    Some(Assist {
        title: "Add a field".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(last.text_range().end()),
            replacement: format!("{comma} field: Int"),
        }],
    })
}

/// **`derive(..)` added above a type that has none, and the import with it.**
///
/// `Show` is what a record usually wants first, so that is what the skeleton
/// derives and the rest is typed.
///
/// **The import is the half that makes it work.** `derive(Show)` on its own is
/// answered with *needs `Show` in scope; import it from `std::core`* -- which
/// is a correct message about an edit the editor had all the information to
/// finish. `imports::edit` writes it, folding into a `std::core` line that is
/// already there rather than adding a second one, and answers `None` when the
/// name is already imported.
fn add_derive(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::TYPE_DECL)?;
    if node.children().any(|n| n.kind() == SyntaxKind::DERIVE_CLAUSE) {
        return None;
    }
    let indent = indent_of(text, &node);
    let mut edits = vec![Edit {
        range: TextRange::empty(line_start(text, &node)),
        replacement: format!("{indent}derive(Show)\n"),
    }];
    if let Some(bring) = crate::imports::edit(tree, text, "std::core", "Show") {
        edits.push(Edit { range: bring.range, replacement: bring.replacement });
    }
    Some(Assist { title: "Derive `Show`".to_string(), kind: "refactor.rewrite", edits })
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
