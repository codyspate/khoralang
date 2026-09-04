//! Assists on calls, pipelines and lambdas.
//!
//! **The pipeline pair is the interesting one.** `f(g(x))` and `x |> g |> f`
//! are the same call in the order they happen and the reverse of the order
//! they are written, and which one reads better is not a matter of taste — it
//! depends on whether the reader cares more about the data or the operations.
//! A four-deep nest is unreadable and a four-stage pipeline is a list; a
//! two-deep nest is fine and a two-stage pipeline is ceremony.
//!
//! Converting by hand means turning the expression inside out, which is the
//! kind of edit that loses an argument.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every call assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(to_pipeline(tree, text, selection));
    out.extend(from_pipeline(tree, text, selection));
    out.extend(name_the_lambda_parameter(tree, text, selection));
    out.extend(block_body(tree, text, selection));
    out
}

/// **`f(x)` becomes `x |> f`.**
///
/// Offered for a call of one argument, because that is what the pipe means: it
/// puts what is on its left in the first position. A call with two arguments
/// would need the rest to stay behind, which is a different and less obvious
/// rewrite.
fn to_pipeline(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::CALL_EXPR)?;
    let callee = node.children().next()?;
    let args = node.children().find(|n| n.kind() == SyntaxKind::ARG_LIST)?;
    let mut given = args.children();
    let only = given.next()?;
    if given.next().is_some() {
        return None;
    }
    // A lambda argument reads worse piped than nested, and is usually the
    // *second* half of a pipeline anyway.
    if only.kind() == SyntaxKind::LAMBDA_EXPR {
        return None;
    }
    Some(Assist {
        title: "Write it as a pipeline".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!("{} |> {}", text_of(text, &only), text_of(text, &callee)),
        }],
    })
}

/// **And `x |> f` back to `f(x)`**, for a pipeline that turned out to be one
/// stage long.
fn from_pipeline(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::PIPE_EXPR)?;
    let mut sides = node.children();
    let subject = sides.next()?;
    let function = sides.next()?;
    // `x |> f(a)` puts `x` first and keeps `a`, so the call form has two
    // arguments and rebuilding it means splicing rather than wrapping.
    if function.kind() == SyntaxKind::CALL_EXPR {
        return None;
    }
    Some(Assist {
        title: "Write the pipeline as a call".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!("{}({})", text_of(text, &function), text_of(text, &subject)),
        }],
    })
}

/// **`fn _ => ..` gets its parameter named.**
///
/// An underscore says the parameter is not used, and a lambda that has grown
/// a use for it needs a name before it can mention it. Offered only for the
/// underscore form, because renaming a parameter somebody named is what rename
/// is for.
fn name_the_lambda_parameter(
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LAMBDA_EXPR)?;
    let list = node.children().find(|n| n.kind() == SyntaxKind::PARAM_LIST)?;
    let param = list.children().find(|n| n.kind() == SyntaxKind::PARAM)?;
    if text_of(text, &param).trim() != "_" {
        return None;
    }
    Some(Assist {
        title: "Name the parameter".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: param.text_range(), replacement: "value".to_string() }],
    })
}

/// **A lambda whose body is one expression gets a block.**
///
/// `fn x => f(x)` becomes `fn x => { f(x) }`, which is where the second
/// statement goes. Offered because the braces are what somebody adds
/// immediately after deciding the lambda needs to do two things, and adding
/// them by hand is where the `=>` gets deleted.
fn block_body(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LAMBDA_EXPR)?;
    let body = node.children().last()?;
    if matches!(body.kind(), SyntaxKind::BLOCK | SyntaxKind::BLOCK_EXPR) {
        return None;
    }
    // A lambda with no `=>` yet is being typed, and adding braces to a body
    // that is really the parameter list would make a mess of it.
    if !node.children_with_tokens().any(|e| e.kind() == SyntaxKind::FAT_ARROW) {
        return None;
    }
    Some(Assist {
        title: "Give the lambda a block body".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: body.text_range(),
            replacement: format!("{{ {} }}", text_of(text, &body)),
        }],
    })
}
