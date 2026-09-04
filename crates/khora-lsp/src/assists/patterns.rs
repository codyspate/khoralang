//! Assists on patterns, and the one that needs the checker to be worth having.
//!
//! **Expanding a `_` arm is the reason this module exists.** A wildcard is how
//! a `match` stops being exhaustive without stopping compiling: the day a
//! variant is added, every `match` that names its cases fails and every
//! `match` ending in `_` quietly sends the new one down the default path. That
//! is a real bug with no diagnostic, and the only thing an editor can do about
//! it is make writing the cases out cheaper than leaving the wildcard.
//!
//! So this asks the checker what the scrutinee is, asks the item map what
//! cases that type has, and writes the ones the `match` does not already
//! mention. It refuses where it cannot answer either question, rather than
//! guessing from the arms that are there.

use khora_db::{Db, SourceFile};
use khora_syntax::{SyntaxKind, SyntaxNode};
use khora_types::Type;
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every pattern assist available at the cursor.
pub fn assists(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(expand_wildcard(db, file, tree, text, selection));
    out.extend(name_a_wildcard(tree, text, selection));
    out
}

/// **A `_` arm replaced by the cases it was standing in for.**
///
/// The type comes from the checker and the cases from the item map, so a
/// `match` on a three-case type gets three arms whatever the wildcard was
/// hiding. Cases the `match` already names are left alone — this fills in the
/// rest rather than rewriting what somebody wrote.
///
/// Refused when the scrutinee's type is not a variant type, or when the
/// checker did not finish with it: an arm written from a guess is worse than
/// a wildcard, because it looks like it was checked.
fn expand_wildcard(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Option<Assist> {
    let arm = covering(tree, selection, SyntaxKind::MATCH_ARM)?;
    let pattern = arm.children().next()?;
    if pattern.kind() != SyntaxKind::WILDCARD_PAT {
        return None;
    }
    let whole = arm.ancestors().find(|a| a.kind() == SyntaxKind::MATCH_EXPR)?;
    let scrutinee = whole.children().next()?;

    let name = adt_named(db, file, scrutinee.text_range())?;
    let map = khora_hir::item_map(db, file);
    let cases: Vec<String> = map.variants_of(&name).map(|v| v.name.clone()).collect();
    if cases.is_empty() {
        return None;
    }

    // What the other arms already say, so this adds rather than duplicates.
    let written = text_of(text, &whole);
    let missing: Vec<&String> = cases.iter().filter(|case| !mentions(&written, case)).collect();
    if missing.is_empty() {
        return None;
    }

    // The qualification the arms already use. A bare constructor name is a
    // *binding* in a pattern, so writing `Green =>` where the file writes
    // `Colour::Green =>` produces an arm that matches everything and a
    // program that compiles and is wrong -- the trap `fixes.rs` records.
    let qualifier =
        if written.contains(&format!("{name}::")) { format!("{name}::") } else { String::new() };

    let indent = indent_of(text, &arm);
    let body = text_of(text, &arm.children().last()?);
    let arms: Vec<String> =
        missing.iter().map(|case| format!("{qualifier}{case} => {body}")).collect();

    Some(Assist {
        title: format!("Write out the {} case(s) the `_` covers", missing.len()),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: arm.text_range(),
            replacement: arms.join(&format!(",\n{indent}")),
        }],
    })
}

/// Whether a `match`'s text already names a case.
///
/// Word-boundaried, so `Red` does not count as mentioned by `Reddish`.
fn mentions(written: &str, case: &str) -> bool {
    written.match_indices(case).any(|(at, _)| {
        let before = written[..at].chars().next_back();
        let after = written[at + case.len()..].chars().next();
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        edge(before) && edge(after)
    })
}

/// The variant type an expression has, by name.
fn adt_named(db: &dyn Db, file: SourceFile, at: TextRange) -> Option<String> {
    let checked = khora_types::checked(db, file);
    for (owner, body) in khora_hir::body::bodies(db, file) {
        let Some(id) = body.exprs().map(|(id, _)| id).find(|id| body.range(*id) == at) else {
            continue;
        };
        let types = checked.bodies.iter().find(|(n, _)| n == owner).map(|(_, t)| t)?;
        let Type::Adt { name, .. } = types.of(id) else { return None };
        return Some(name.clone());
    }
    None
}

/// **A `_` pattern given a name**, for an arm that turned out to want the
/// value it was throwing away.
fn name_a_wildcard(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::WILDCARD_PAT)?;
    let _ = text;
    Some(Assist {
        title: "Name what the `_` is ignoring".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: node.text_range(), replacement: "value".to_string() }],
    })
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
