//! Labeled arguments, `f(a, verbose: true)`, as the parser sees them.
//!
//! A label is `IDENT ":"` at the start of an argument. No expression starts
//! that way -- `::` is its own token and a record literal starts with `{` --
//! so the rule takes nothing that parsed before. These pin the shapes next to
//! it that must keep their old meaning: `f(x = 2)` is still an assignment,
//! `f({ a: 1 })` is still a record, and `f(A::b)` is still a path.

use khora_syntax::parse;

fn tree(src: &str) -> String {
    let parsed = parse(src);
    assert_eq!(parsed.syntax().text().to_string(), src, "lost source text");
    parsed.debug_tree()
}

fn clean(src: &str) -> String {
    let parsed = parse(src);
    assert!(parsed.errors().is_empty(), "{:?}\n{}", parsed.errors(), parsed.debug_tree());
    tree(src)
}

#[test]
fn a_labeled_argument_is_its_own_node() {
    let dump = clean("module m;\nfn f() { g(a, verbose: true) }\n");
    assert_eq!(dump.matches("LABELED_ARG").count(), 1, "{dump}");
    // The label is a `NAME` inside it, and the value is an ordinary expression.
    let at = dump.find("LABELED_ARG").unwrap();
    let inside = &dump[at..];
    assert!(inside.contains("NAME"), "{dump}");
    assert!(inside.contains("LITERAL_EXPR"), "{dump}");
}

#[test]
fn every_argument_may_be_labeled_and_they_mix_freely() {
    let dump = clean("module m;\nfn f() { g(a: 1, 2, c: 3) }\n");
    assert_eq!(dump.matches("LABELED_ARG").count(), 2, "{dump}");
}

#[test]
fn a_label_in_a_method_call_and_a_pipe() {
    clean("module m;\nfn f() { x.reply(\"ok\", keep: false) }\n");
    clean("module m;\nfn f() { x |> reply(\"ok\", keep: false) }\n");
    clean("module m;\nfn f() { 5 |> three(1, _, flag: true) }\n");
}

/// **`=` is not the spelling, because it already means something.**
/// `f(x = 2, true)` is an assignment passed as an argument, and stays one.
#[test]
fn an_equals_sign_is_still_an_assignment() {
    let dump = clean("module m;\nfn f() { g(x = 2, true) }\n");
    assert!(dump.contains("ASSIGN_EXPR"), "{dump}");
    assert!(!dump.contains("LABELED_ARG"), "{dump}");
}

#[test]
fn a_record_argument_is_still_a_record() {
    let dump = clean("module m;\nfn f() { g({ a: 1 }) }\n");
    assert!(dump.contains("RECORD_EXPR"), "{dump}");
    assert!(!dump.contains("LABELED_ARG"), "{dump}");
}

#[test]
fn a_path_argument_is_still_a_path() {
    let dump = clean("module m;\nfn f() { g(Color::Red) }\n");
    assert!(!dump.contains("LABELED_ARG"), "{dump}");
}

/// `_` names nothing, so it cannot be a label. Said in one error at the `_`,
/// rather than the `expected )` cascade the generic path gives.
#[test]
fn an_underscore_is_not_a_label() {
    let src = "module m;\nfn f() { g(_: 1, 2) }\n";
    let parsed = parse(src);
    assert_eq!(parsed.syntax().text().to_string(), src, "lost source text");
    let messages: Vec<String> = parsed.errors().iter().map(|e| e.message.clone()).collect();
    assert_eq!(messages.len(), 1, "one error, not a cascade: {messages:?}");
    assert!(messages[0].contains("`_` is not a label"), "{messages:?}");
}

#[test]
fn a_label_with_no_value_is_an_error_and_keeps_its_position() {
    let src = "module m;\nfn f() { g(a, verbose:) }\n";
    let parsed = parse(src);
    assert_eq!(parsed.syntax().text().to_string(), src, "lost source text");
    assert!(
        parsed.errors().iter().any(|e| e.message.contains("expected an argument after the label")),
        "{:?}",
        parsed.errors()
    );
}
