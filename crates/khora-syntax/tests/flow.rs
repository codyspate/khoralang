//! The flow operator, `||>`, as the lexer and the parser see it.
//!
//! `docs/design/flow-operator.md`. `||> a |> b` is a unary anonymous function
//! whose argument starts the pipeline, and it is sugar: by the time anything
//! semantic runs it is an ordinary lambda. What is pinned here is the shape of
//! the tree and the two rules a reader has to know — the operator is greedy
//! over the pipes that follow it, and it changes nothing about `|>`.

use khora_syntax::parse;

fn tree(source: &str) -> String {
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source, "did not round-trip");
    assert!(parsed.errors().is_empty(), "{source}\n{:?}", parsed.errors());
    parsed.debug_tree()
}

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

/// How many `|>` tokens a dump contains.
///
/// Counted by subtraction because `PIPE_PIPE_GT@` *contains* `PIPE_GT@`, so
/// the naive count is one too many for every flow -- which is a trap worth a
/// helper rather than three chances to fall into it.
fn pipes(dumped: &str) -> usize {
    dumped.matches("PIPE_GT@").count() - dumped.matches("PIPE_PIPE_GT@").count()
}

/// One token, not `||` followed by `>`. Nothing legitimate is taken away by
/// preferring the longer one: `a || > b` has no valid parse.
#[test]
fn the_operator_lexes_as_one_token() {
    let dumped = tree("module t;\nfn f() -> Int { let g = ||> inc; 0 }\n");
    assert!(dumped.contains("PIPE_PIPE_GT@"), "{dumped}");
    assert!(!dumped.contains("PIPE_PIPE@"), "not `||` and then `>`: {dumped}");
}

/// And logical-or still lexes as itself, which is the thing the spelling puts
/// at risk.
#[test]
fn logical_or_is_untouched() {
    let dumped = tree("module t;\nfn f(a: Bool, b: Bool) -> Bool { a || b }\n");
    assert!(dumped.contains("PIPE_PIPE@"), "{dumped}");
    assert!(!dumped.contains("PIPE_PIPE_GT@"), "{dumped}");
}

#[test]
fn one_stage_is_a_flow_with_one_child() {
    let dumped = tree("module t;\nfn f() -> Int { let g = ||> inc; 0 }\n");
    assert!(dumped.contains("FLOW_EXPR@"), "{dumped}");
}

/// **The greedy rule.** Every `|>` after the first stage belongs to the flow.
/// An operator that stopped at the first stage would need parentheses at every
/// use, which is most of what it exists to remove.
#[test]
fn the_operator_takes_every_pipe_that_follows_it() {
    let dumped = tree("module t;\nfn f() -> Int { let g = ||> a |> b |> c; 0 }\n");
    let flow = dumped.split("FLOW_EXPR@").nth(1).expect("a flow");
    // Three stages, and the two pipes between them, all inside the one flow
    // rather than a pipe expression wrapped around it.
    assert_eq!(pipes(flow), 2, "{dumped}");
    assert!(!dumped.contains("PIPE_EXPR@"), "the pipes belong to the flow: {dumped}");
}

/// A stage keeps everything that binds tighter than a pipe, which is the
/// existing precedence and not a second one.
#[test]
fn a_stage_takes_what_binds_tighter_than_a_pipe() {
    let dumped = tree("module t;\nfn f() -> Int { let g = ||> add(1) |> double; 0 }\n");
    assert!(dumped.contains("CALL_EXPR@"), "{dumped}");
    let flow = dumped.split("FLOW_EXPR@").nth(1).expect("a flow");
    assert_eq!(pipes(flow), 1, "{dumped}");
}

/// A comma ends a flow, so it can be one argument among several.
#[test]
fn a_flow_is_one_argument_among_several() {
    let dumped = tree("module t;\nfn f() -> Int { apply(||> inc |> double, 1) }\n");
    assert!(dumped.contains("FLOW_EXPR@"), "{dumped}");
    let flow = dumped.split("FLOW_EXPR@").nth(1).expect("a flow");
    assert_eq!(pipes(flow), 1, "the comma ends it: {dumped}");
}

/// Parentheses are how the *function* gets piped somewhere, which is the case
/// the greedy rule gives up.
#[test]
fn parentheses_let_the_function_itself_be_piped() {
    let dumped = tree("module t;\nfn f() -> Int { (||> inc) |> apply }\n");
    assert!(dumped.contains("FLOW_EXPR@"), "{dumped}");
    assert!(dumped.contains("PIPE_EXPR@"), "the outer pipe is outside the flow: {dumped}");
}

// --- what ordinary pipes still do -------------------------------------------

/// The regression that matters. Nothing about `|>` changed.
#[test]
fn an_ordinary_pipeline_is_unchanged() {
    let dumped = tree("module t;\nfn f(v: Int) -> Int { v |> inc |> double }\n");
    assert!(!dumped.contains("FLOW_EXPR@"), "{dumped}");
    assert_eq!(dumped.matches("PIPE_EXPR@").count(), 2, "{dumped}");
}

#[test]
fn a_pipe_still_binds_looser_than_arithmetic() {
    let dumped = tree("module t;\nfn f(v: Int) -> Int { v |> add(1 + 2) }\n");
    assert!(dumped.contains("BIN_EXPR@"), "{dumped}");
}

// --- diagnostics -------------------------------------------------------------

/// Named for what was written. The lambda it becomes is an implementation
/// detail and a reader cannot act on it.
#[test]
fn a_flow_with_no_stage_says_so() {
    let found = errors("module t;\nfn f() -> Int { let g = ||>; 0 }\n");
    assert!(
        found.iter().any(|e| e.contains("first stage") && e.contains("||>")),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|e| e.contains("lambda")),
        "the desugaring should not leak into the message: {found:?}"
    );
}

#[test]
fn a_trailing_pipe_says_a_stage_is_missing() {
    let found = errors("module t;\nfn f() -> Int { let g = ||> inc |>; 0 }\n");
    assert!(found.iter().any(|e| e.contains("stage")), "{found:?}");
}
