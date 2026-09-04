//! Assists that rearrange control flow without changing what it decides.
//!
//! **Every one of these is a rewrite of the same program.** An inverted `if`
//! runs the same branch for the same input; a nested pair merged into `&&`
//! tests the same things in the same order and skips the same work. That is
//! the bar, and it is why the negation below is careful rather than
//! convenient: turning `a < b` into `!(a < b)` is correct and turning it into
//! `a > b` is not, because neither says anything about `a == b`.
//!
//! Nothing here needs the type checker. They are tree shapes, which is why
//! there are a lot of them and why they are cheap enough to offer on every
//! keystroke.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every control-flow assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(invert_if(tree, text, selection));
    out.extend(add_else(tree, text, selection));
    out.extend(merge_nested_if(tree, text, selection));
    out.extend(split_and(tree, text, selection));
    out.extend(remove_parens(tree, text, selection));
    out.extend(flip_comparison(tree, text, selection));
    out
}

/// The pieces of an `if`: its condition, its block, and its `else` block.
struct Branches {
    condition: SyntaxNode,
    then_block: SyntaxNode,
    else_block: Option<SyntaxNode>,
}

/// Reads an `IF_EXPR` into its parts.
///
/// The grammar puts them in order — condition, block, then optionally a block
/// or another `if` after `else` — so position is what tells them apart. An
/// `else if` comes back as `else_block: None`, because it is an `IF_EXPR`
/// rather than a block and none of the rewrites here can treat it as one.
fn branches(node: &SyntaxNode) -> Option<Branches> {
    let mut children = node.children();
    let condition = children.next()?;
    let then_block = children.next().filter(|n| n.kind() == SyntaxKind::BLOCK)?;
    let else_block = children.next().filter(|n| n.kind() == SyntaxKind::BLOCK);
    Some(Branches { condition, then_block, else_block })
}

/// **`if a { x } else { y }` becomes `if !a { y } else { x }`.**
///
/// Offered for an `if` with both branches. Reaching for it usually means the
/// interesting case is the one currently second, and reading a function whose
/// early branch is the unusual one is harder than it needs to be.
fn invert_if(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IF_EXPR)?;
    let Branches { condition, then_block, else_block } = branches(&node)?;
    let otherwise = else_block?;

    Some(Assist {
        title: "Invert the `if`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![
            // Written back to front so the earlier edits' offsets stay true
            // when an editor applies them in order.
            Edit { range: otherwise.text_range(), replacement: text_of(text, &then_block) },
            Edit { range: then_block.text_range(), replacement: text_of(text, &otherwise) },
            Edit {
                range: condition.text_range(),
                replacement: negated(&text_of(text, &condition)),
            },
        ],
    })
}

/// **An `if` with no `else` gets one.**
///
/// Small, and the reason it is worth having is that the block has to go in the
/// right place: after the closing brace of the first, which is not where the
/// cursor is when somebody decides they need it.
fn add_else(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IF_EXPR)?;
    let Branches { then_block, else_block, .. } = branches(&node)?;
    if else_block.is_some() {
        return None;
    }
    // An `else if` is an `IF_EXPR` after the `else` keyword rather than a
    // block, and adding a second `else` to it would not parse.
    if node.children_with_tokens().any(|e| e.kind() == SyntaxKind::ELSE_KW) {
        return None;
    }

    let indent = indent_of(text, &node);
    Some(Assist {
        title: "Add an `else` branch".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(then_block.text_range().end()),
            replacement: format!(" else {{\n{indent}  \n{indent}}}"),
        }],
    })
}

/// **`if a { if b { x } }` becomes `if a && b { x }`.**
///
/// Only when neither `if` has an `else` and the outer one holds nothing but
/// the inner: with an `else` on either, or a statement beside the inner `if`,
/// the merge would change which code runs.
fn merge_nested_if(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let outer = covering(tree, selection, SyntaxKind::IF_EXPR)?;
    let Branches { condition: first, then_block, else_block } = branches(&outer)?;
    if else_block.is_some() {
        return None;
    }

    // The outer block must hold exactly one thing, and it must be the `if`.
    let mut inside = then_block.children().filter(|n| n.kind() != SyntaxKind::BLOCK);
    let only = inside.next()?;
    if inside.next().is_some() {
        return None;
    }
    // The `if` itself, or the one an `EXPR_STMT` wraps -- a block holds
    // statements, and a trailing expression is a node of its own.
    let inner = if only.kind() == SyntaxKind::IF_EXPR {
        only.clone()
    } else if only.kind() == SyntaxKind::EXPR_STMT {
        only.children().find(|n| n.kind() == SyntaxKind::IF_EXPR)?
    } else {
        return None;
    };
    let Branches { condition: second, then_block: body, else_block: inner_else } =
        branches(&inner)?;
    if inner_else.is_some() {
        return None;
    }

    let merged = format!("{} && {}", bracketed(&text_of(text, &first)), bracketed(&text_of(text, &second)));
    Some(Assist {
        title: "Merge the nested `if` into `&&`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![
            Edit { range: then_block.text_range(), replacement: text_of(text, &body) },
            Edit { range: first.text_range(), replacement: merged },
        ],
    })
}

/// **`if a && b { x }` becomes `if a { if b { x } }`.**
///
/// The other direction, for when one of the two halves is about to grow an
/// `else` of its own.
fn split_and(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IF_EXPR)?;
    let Branches { condition, then_block, else_block } = branches(&node)?;
    // With an `else`, splitting changes which condition it belongs to.
    if else_block.is_some() {
        return None;
    }
    if condition.kind() != SyntaxKind::BIN_EXPR {
        return None;
    }
    let (left, right) = operands(&condition, SyntaxKind::AMP_AMP)?;

    let indent = indent_of(text, &node);
    let body = text_of(text, &then_block);
    let inner_body = body
        .lines()
        .map(|line| if line.is_empty() { line.to_string() } else { format!("  {line}") })
        .collect::<Vec<_>>()
        .join("\n");
    Some(Assist {
        title: "Split the `&&` into nested `if`s".to_string(),
        kind: "refactor.rewrite",
        edits: vec![
            Edit {
                range: then_block.text_range(),
                replacement: format!(
                    "{{\n{indent}  if {} {}\n{indent}}}",
                    text_of(text, &right),
                    inner_body
                ),
            },
            Edit { range: condition.text_range(), replacement: text_of(text, &left) },
        ],
    })
}

/// **`(x)` becomes `x`, where the parentheses were doing nothing.**
///
/// Only where the parenthesised expression is a whole statement, a whole
/// branch condition or the argument of a call — places where precedence
/// cannot bite. Anywhere inside an operator expression the parentheses may be
/// the only thing holding the meaning together, and this refuses rather than
/// re-deriving the precedence table in an editor.
fn remove_parens(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::PAREN_EXPR)?;
    let parent = node.parent()?;
    let safe = matches!(
        parent.kind(),
        SyntaxKind::EXPR_STMT
            | SyntaxKind::ARG_LIST
            | SyntaxKind::BLOCK
            | SyntaxKind::LET_DECL
            | SyntaxKind::IF_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::RETURN_EXPR
    );
    if !safe {
        return None;
    }
    let inner = node.children().next()?;
    Some(Assist {
        title: "Remove the parentheses".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: node.text_range(), replacement: text_of(text, &inner) }],
    })
}

/// **`a < b` becomes `b > a`.**
///
/// Reading a comparison the other way round is sometimes the way somebody
/// thinks about it, and swapping the operands by hand is where an off-by-one
/// gets written. The operator is turned round with the operands, so the
/// comparison still answers the same question.
fn flip_comparison(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::BIN_EXPR)?;
    let (operator, mirrored) = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find_map(|t| mirror(t.kind()).map(|m| (t, m)))?;
    let mut sides = node.children();
    let left = sides.next()?;
    let right = sides.next()?;

    Some(Assist {
        title: format!("Flip to `{mirrored}`"),
        kind: "refactor.rewrite",
        edits: vec![
            Edit { range: right.text_range(), replacement: text_of(text, &left) },
            Edit { range: operator.text_range(), replacement: mirrored.to_string() },
            Edit { range: left.text_range(), replacement: text_of(text, &right) },
        ],
    })
}

/// The operator that means the same thing with the operands the other way
/// round, for the ones where there is one.
///
/// `==` and `!=` are their own mirrors. `+` and `*` are *not* here even though
/// the arithmetic commutes: the operands are expressions, and swapping them
/// swaps the order two calls happen in.
fn mirror(kind: SyntaxKind) -> Option<&'static str> {
    Some(match kind {
        SyntaxKind::LT => ">",
        SyntaxKind::GT => "<",
        SyntaxKind::LT_EQ => ">=",
        SyntaxKind::GT_EQ => "<=",
        SyntaxKind::EQ_EQ => "==",
        SyntaxKind::BANG_EQ => "!=",
        _ => return None,
    })
}

/// The two operands of a binary expression joined by `operator`.
fn operands(node: &SyntaxNode, operator: SyntaxKind) -> Option<(SyntaxNode, SyntaxNode)> {
    if !node.children_with_tokens().any(|e| e.kind() == operator) {
        return None;
    }
    let mut sides = node.children();
    Some((sides.next()?, sides.next()?))
}

/// A condition with the opposite answer.
///
/// **Not `!(..)` wrapped round everything**, because a negation somebody has
/// to read is worse than the one they wrote. A comparison flips to its
/// opposite comparison, a leading `!` comes off, and anything else is
/// parenthesised and negated — which is the only safe answer for a call or a
/// name.
///
/// `<` becomes `>=` rather than `>`: the third case is what makes it a
/// comparison rather than a coin toss.
fn negated(condition: &str) -> String {
    let trimmed = condition.trim();
    if let Some(rest) = trimmed.strip_prefix('!') {
        // `!(a == b)` back to `a == b`, and `!ready` back to `ready`.
        let rest = rest.trim();
        if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            return inner.trim().to_string();
        }
        return rest.to_string();
    }
    for (from, to) in
        [(" == ", " != "), (" != ", " == "), (" <= ", " > "), (" >= ", " < "), (" < ", " >= "), (" > ", " <= ")]
    {
        // Only where the operator appears once, so `a < b < c` -- which is not
        // a thing Khora parses this way anyway -- cannot be half-rewritten.
        if trimmed.matches(from).count() == 1 {
            return trimmed.replace(from, to);
        }
    }
    format!("!({trimmed})")
}

/// A condition wrapped in parentheses if it needs them to sit beside `&&`.
fn bracketed(condition: &str) -> String {
    let trimmed = condition.trim();
    let needs = [" || ", " && "].iter().any(|op| trimmed.contains(op));
    if needs { format!("({trimmed})") } else { trimmed.to_string() }
}

/// The whitespace in front of the line `node` starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
