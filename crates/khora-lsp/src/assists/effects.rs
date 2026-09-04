//! Assists about capabilities and failures — the two rows a Khora signature
//! carries and no other language's editor has to think about.
//!
//! **These are the ones worth having, and the reason is that the rows are the
//! part people get stuck on.** A reader who knows the language still has to
//! remember whether `catch` is postfix, what a binding arm covers, which of
//! `attempt` and `!` is the one that yields a value, and where a `with` block
//! goes relative to the statement it answers for. None of that is hard and all
//! of it is a trip to the reference.
//!
//! Each one is a wrapping rather than a rearrangement, so the code inside is
//! untouched and the edit is one insertion and one closing.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every effect assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(wrap_in_catch(tree, selection));
    out.extend(wrap_in_attempt(tree, text, selection));
    out.extend(unwrap_attempt(tree, text, selection));
    out.extend(wrap_in_scoped(tree, text, selection));
    out.extend(postfix_with_to_block(tree, text, selection));
    out.extend(block_with_to_postfix(tree, text, selection));
    out
}

/// **`foo()!` gets a `catch`, with the arm that covers everything.**
///
/// A single binding arm handles every failure the operand can raise, which is
/// what the failures reference calls the form to reach for when the answer
/// does not depend on which variant arrived. It is also the arm somebody
/// writing this by hand has to look up, because the alternative — one arm per
/// constructor, exhaustive — is what `catch` usually wants.
///
/// Offered on a `!`, because that is the expression that has something to
/// catch. On a call with no `!` there is nothing leaving.
fn wrap_in_catch(tree: &SyntaxNode, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::TRY_EXPR)?;
    // Already caught: `foo()! catch { .. }` is a `CATCH_EXPR` around this.
    if node.parent().is_some_and(|p| p.kind() == SyntaxKind::CATCH_EXPR) {
        return None;
    }
    Some(Assist {
        title: "Handle the failure with `catch`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().end()),
            replacement: " catch { failure => todo() }".to_string(),
        }],
    })
}

/// **`foo()!` becomes `attempt(fn () => foo()!)`, which is a value.**
///
/// The two ways out of a failure, and which one somebody wants depends on
/// whether the caller is going to branch on it now. `!` sends it up; `attempt`
/// turns it into a `Result` to be matched on here.
fn wrap_in_attempt(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::TRY_EXPR)?;
    if node.parent().is_some_and(|p| p.kind() == SyntaxKind::CATCH_EXPR) {
        return None;
    }
    // Already inside one: the argument of `attempt(fn () => ..)`.
    if inside_call_to(&node, "attempt") {
        return None;
    }
    Some(Assist {
        title: "Turn the failure into a `Result` with `attempt`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!("attempt(fn () => {})", text_of(text, &node)),
        }],
    })
}

/// **The way back: `attempt(fn () => x)` becomes `x`.**
///
/// For when the `Result` was being matched on only to raise again, which is
/// the shape `!` exists for.
fn unwrap_attempt(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::CALL_EXPR)?;
    if callee_name(&node)? != "attempt" {
        return None;
    }
    let args = node.children().find(|n| n.kind() == SyntaxKind::ARG_LIST)?;
    let lambda = args.children().find(|n| n.kind() == SyntaxKind::LAMBDA_EXPR)?;
    // `attempt` takes a thunk, so a lambda that declares a parameter is not
    // the one this is looking at. The list is asked for its *contents*: a
    // `fn ()` still has a `PARAM_LIST` node, an empty one, so checking for the
    // list itself rejects every thunk -- which is the whole population.
    let takes_arguments = lambda
        .children()
        .filter(|n| n.kind() == SyntaxKind::PARAM_LIST)
        .any(|list| list.children().any(|p| p.kind() == SyntaxKind::PARAM));
    if takes_arguments {
        return None;
    }
    let body = lambda.children().last()?;
    Some(Assist {
        title: "Raise it instead of returning a `Result`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: node.text_range(), replacement: text_of(text, &body) }],
    })
}

/// **A statement wrapped in `scoped`, which is where releases run.**
///
/// `acquire` registers a release with the enclosing region, and the region is
/// what `scoped` opens. Written the other way round — an `acquire` with no
/// `scoped` above it — the code does not compile, and the message names a
/// capability rather than the word somebody needs to type.
fn wrap_in_scoped(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::CALL_EXPR)?;
    if callee_name(&node)? != "acquire" {
        return None;
    }
    // Already inside one.
    if inside_call_to(&node, "scoped") {
        return None;
    }
    let statement = statement_of(&node)?;
    let indent = indent_of(text, &statement);
    let body = text_of(text, &statement);
    Some(Assist {
        title: "Open a `scoped` region for the release".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: statement.text_range(),
            replacement: format!("scoped(fn () => {{\n{indent}  {body}\n{indent}}})!"),
        }],
    })
}

/// **`x with { log: quiet() }` becomes `with { log: quiet() } { x }`.**
///
/// The two spellings install the same handlers, and which reads better depends
/// on how much is inside. The postfix form is right for one expression and
/// unreadable for five statements, which is exactly when somebody wants this.
fn postfix_with_to_block(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::WITH_EXPR)?;
    let mut children = node.children();
    let subject = children.next()?;
    let row = children.next()?;
    let indent = indent_of(text, &node);
    Some(Assist {
        title: "Write the `with` as a block".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!(
                "with {} {{\n{indent}  {}\n{indent}}}",
                text_of(text, &row),
                text_of(text, &subject)
            ),
        }],
    })
}

/// **And back**, for a block that turned out to hold one expression.
fn block_with_to_postfix(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::WITH_BLOCK)?;
    let mut children = node.children();
    let row = children.next()?;
    let block = children.next().filter(|n| n.kind() == SyntaxKind::BLOCK)?;
    // One expression and nothing else, or the postfix form would have to hold
    // a block anyway and nothing is gained.
    let mut inside = block.children();
    let only = inside.next()?;
    if inside.next().is_some() {
        return None;
    }
    let subject = if only.kind() == SyntaxKind::EXPR_STMT {
        only.children().next()?
    } else {
        only
    };
    Some(Assist {
        title: "Write the `with` after the expression".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!("{} with {}", text_of(text, &subject), text_of(text, &row)),
        }],
    })
}

/// The name a call is calling, when it is a plain path.
fn callee_name(call: &SyntaxNode) -> Option<String> {
    let callee = call.children().next()?;
    if callee.kind() != SyntaxKind::PATH_EXPR {
        return None;
    }
    Some(callee.text().to_string())
}

/// Whether the node is an argument of a call to `name`, however deep.
fn inside_call_to(node: &SyntaxNode, name: &str) -> bool {
    node.ancestors()
        .filter(|a| a.kind() == SyntaxKind::CALL_EXPR)
        .any(|call| callee_name(&call).as_deref() == Some(name))
}

/// The statement the node sits in.
fn statement_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    let mut here = node.clone();
    loop {
        let parent = here.parent()?;
        if parent.kind() == SyntaxKind::BLOCK {
            return Some(here);
        }
        here = parent;
    }
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
