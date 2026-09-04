//! Assists on `match`, and on the two shapes it trades places with.
//!
//! **A `match` and an `if` are the same decision written two ways**, and which
//! one reads better depends on how many answers there are and whether the
//! reader cares what they are called. Two arms over a `Bool` is an `if` that
//! has been made to look like a case analysis; five `if`s in a chain is a case
//! analysis pretending it is a sequence of unrelated questions. Both
//! conversions are here, and neither changes what runs.
//!
//! The one that needs the compiler is filling in the arms, and that already
//! exists as a quick fix on the *diagnostic*. What is here is the version
//! somebody asks for before the diagnostic appears: turning a `_` arm into the
//! cases it was standing in for, which is how a wildcard stops hiding the
//! variant somebody added last week.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every match assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(match_to_if(tree, text, selection));
    out.extend(if_to_match(tree, text, selection));
    out.extend(add_arm(tree, text, selection));
    out.extend(add_guard(tree, text, selection));
    out.extend(block_arm(tree, text, selection));
    out
}

/// The pattern, guard and body of one arm.
struct Arm {
    pattern: SyntaxNode,
    body: SyntaxNode,
    has_guard: bool,
}

fn arm(node: &SyntaxNode) -> Option<Arm> {
    let pattern = node.children().next()?;
    let has_guard = node.children().any(|n| n.kind() == SyntaxKind::MATCH_GUARD);
    let body = node.children().last()?;
    if body.text_range() == pattern.text_range() {
        return None;
    }
    Some(Arm { pattern, body, has_guard })
}

/// Every arm of a `match`, in order.
fn arms(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children().filter(|n| n.kind() == SyntaxKind::MATCH_ARM).collect()
}

/// **A two-arm `match` on `true`/`false` becomes an `if`.**
///
/// Refused for anything else: a `match` over two of five constructors is not
/// an `if`, it is a `match` somebody has not finished, and rewriting it as one
/// would throw away the exhaustiveness the compiler is about to check.
///
/// A guard is refused too, because `if` has nowhere to put it.
fn match_to_if(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::MATCH_EXPR)?;
    let scrutinee = node.children().next()?;
    let cases = arms(&node);
    if cases.len() != 2 {
        return None;
    }
    let first = arm(&cases[0])?;
    let second = arm(&cases[1])?;
    if first.has_guard || second.has_guard {
        return None;
    }

    // Which way round the two are written decides which branch is which.
    let (yes, no) = match text_of(text, &first.pattern).trim() {
        "true" => (&first, &second),
        "false" => (&second, &first),
        _ => return None,
    };
    let other = text_of(text, &no.pattern);
    let other = other.trim();
    if other != "true" && other != "false" && other != "_" {
        return None;
    }

    Some(Assist {
        title: "Write the `match` as an `if`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!(
                "if {} {{ {} }} else {{ {} }}",
                text_of(text, &scrutinee),
                text_of(text, &yes.body).trim(),
                text_of(text, &no.body).trim()
            ),
        }],
    })
}

/// **And an `if` with both branches becomes a `match`**, which is where it
/// wants to go when the condition is about to become three cases.
fn if_to_match(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IF_EXPR)?;
    let mut children = node.children();
    let condition = children.next()?;
    let then_block = children.next().filter(|n| n.kind() == SyntaxKind::BLOCK)?;
    let else_block = children.next().filter(|n| n.kind() == SyntaxKind::BLOCK)?;

    let indent = indent_of(text, &node);
    Some(Assist {
        title: "Write the `if` as a `match`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!(
                "match {} {{\n{indent}  true => {},\n{indent}  false => {},\n{indent}}}",
                text_of(text, &condition),
                inner(text, &then_block),
                inner(text, &else_block)
            ),
        }],
    })
}

/// **An arm added below the one the cursor is on.**
///
/// Small, and it exists because the comma is the thing people get wrong: the
/// last arm of a `match` may or may not have one depending on how it was
/// written, and adding an arm after it means fixing that first.
fn add_arm(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::MATCH_ARM)?;
    let indent = indent_of(text, &node);
    let end = node.text_range().end();
    // A trailing comma may already be there, and two would not parse.
    let after = &text[usize::from(end)..];
    let comma = if after.trim_start().starts_with(',') { "" } else { "," };
    Some(Assist {
        title: "Add an arm below".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(end),
            replacement: format!("{comma}\n{indent}_ => todo()"),
        }],
    })
}

/// **A guard added to the arm the cursor is on.**
///
/// `Case(n) if n > 0 =>`. Worth an assist because the guard goes between the
/// pattern and the arrow, which is the one place somebody who has written a
/// `match` in another language does not look.
fn add_guard(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::MATCH_ARM)?;
    let parts = arm(&node)?;
    if parts.has_guard {
        return None;
    }
    let _ = text;
    Some(Assist {
        title: "Add a guard to this arm".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(parts.pattern.text_range().end()),
            replacement: " if todo()".to_string(),
        }],
    })
}

/// A block's contents without its braces, for putting on an arm.
fn inner(text: &str, block: &SyntaxNode) -> String {
    let whole = text_of(text, block);
    let trimmed = whole.trim();
    let stripped = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .map(str::trim)
        .unwrap_or(trimmed);
    // A block holding several statements has to stay a block on the arm, or
    // the commas between arms would be read as part of it.
    if stripped.contains(';') { whole.trim().to_string() } else { stripped.to_string() }
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// **An arm's body given a block.**
///
/// `Case => value` becomes `Case => { value }`, which is where the second
/// statement goes. The same argument as the lambda one, and the same mistake
/// it prevents: braces added by hand to an arm are added around the comma
/// about as often as inside it.
fn block_arm(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::MATCH_ARM)?;
    let parts = arm(&node)?;
    if matches!(parts.body.kind(), SyntaxKind::BLOCK | SyntaxKind::BLOCK_EXPR) {
        return None;
    }
    Some(Assist {
        title: "Give the arm a block body".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: parts.body.text_range(),
            replacement: format!("{{ {} }}", text_of(text, &parts.body)),
        }],
    })
}
