//! Assists on statements, loops and the two test forms.
//!
//! **The loop pair is the one with an argument behind it.** `while c { .. }`
//! and `loop { if !c { break }; .. }` run the same iterations, and the second
//! is what somebody needs the moment the condition has to be checked in the
//! middle rather than at the top — which is a change nobody makes as a
//! rewrite, they make it as a mistake.
//!
//! The test skeletons are here because of what a Khora test is: a `test`
//! declaration is an item, not a function with an attribute, so there is no
//! way to write half of one and let the compiler say what is missing. Starting
//! it from the function under test also gets the name right, which is the part
//! that is read most and thought about least.

use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

use super::{Assist, Edit, covering, text_of};

/// Every statement assist available at the cursor.
pub fn assists(tree: &SyntaxNode, text: &str, selection: TextRange) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(while_to_loop(tree, text, selection));
    out.extend(generate_test(tree, text, selection));
    out.extend(generate_bench(tree, text, selection));
    out.extend(bind_result(tree, text, selection));
    out
}

/// **`while c { .. }` becomes `loop { if !c { break }; .. }`.**
///
/// The rewrite is worth an assist rather than a moment's typing because of the
/// `break`: written by hand into an existing body it goes in after the first
/// statement about as often as before it, and the two are different programs.
fn while_to_loop(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::WHILE_EXPR)?;
    let mut children = node.children();
    let condition = children.next()?;
    let body = children.next().filter(|n| n.kind() == SyntaxKind::BLOCK)?;

    let indent = indent_of(text, &node);
    let inside = text_of(text, &body);
    let inner = inside
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .map(str::trim)
        .unwrap_or("");
    let separator = if inner.is_empty() { "" } else { "\n" };
    Some(Assist {
        title: "Write the `while` as a `loop`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: node.text_range(),
            replacement: format!(
                "loop {{\n{indent}  if !({}) {{ break }};{separator}{indent}  {inner}\n{indent}}}",
                text_of(text, &condition)
            ),
        }],
    })
}

/// **A `test` block for the function the cursor is on.**
///
/// Named after the function, in the sentence form the rest of the tree uses:
/// a test's name is the claim it makes, and one called `test "go"` says
/// nothing that the function's own name did not.
fn generate_test(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::FN_DECL)?;
    let name = declared_name(&node, text)?;
    let indent = indent_of(text, &node);
    Some(Assist {
        title: format!("Write a test for `{name}`"),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().end()),
            replacement: format!(
                "\n\n{indent}test \"{name} \" {{\n{indent}  assert(todo());\n{indent}}}"
            ),
        }],
    })
}

/// **And a `bench` block**, which is the same shape and a different question.
///
/// Separate rather than one assist with a choice, because a benchmark is not a
/// test that happens to be timed: it wants a body that does the work and
/// nothing else, and putting an `assert` in one is how a measurement ends up
/// measuring the assertion.
fn generate_bench(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::FN_DECL)?;
    let name = declared_name(&node, text)?;
    let indent = indent_of(text, &node);
    Some(Assist {
        title: format!("Write a benchmark for `{name}`"),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().end()),
            replacement: format!("\n\n{indent}bench \"{name}\" {{\n{indent}  todo()\n{indent}}}"),
        }],
    })
}

/// **A statement whose answer is thrown away, given a name.**
///
/// The opposite of inlining, and the thing somebody wants when a call's result
/// turns out to matter after all. Offered on an expression statement that is a
/// call, because that is the one with an answer worth keeping.
fn bind_result(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::EXPR_STMT)?;
    let inner = node.children().next()?;
    if inner.kind() != SyntaxKind::CALL_EXPR && inner.kind() != SyntaxKind::TRY_EXPR {
        return None;
    }
    let _ = text;
    Some(Assist {
        title: "Bind the answer to a name".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(node.text_range().start()),
            replacement: "let answer = ".to_string(),
        }],
    })
}

/// The name a declaration gives.
fn declared_name(node: &SyntaxNode, text: &str) -> Option<String> {
    let name = node.children().find(|n| n.kind() == SyntaxKind::NAME)?;
    Some(text_of(text, &name).trim().to_string())
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
