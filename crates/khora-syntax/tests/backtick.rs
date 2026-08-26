//! Backtick string literals, as the lexer sees them.
//!
//! `docs/design/multiline-strings.md`, D17. A backtick literal is the **same
//! token** as a quoted one — `STRING_LIT` either way — so the parser, the type
//! checker and the backend never learn there were two spellings. What differs
//! is where it ends: a quoted literal stops at a newline, because a `"` that
//! reaches one is almost always a typo; a backtick that reaches one is doing
//! its job.

use khora_syntax::parse;

fn tree(source: &str) -> String {
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source, "did not round-trip");
    assert!(parsed.errors().is_empty(), "{source}\n{:?}", parsed.errors());
    parsed.debug_tree()
}

/// One token, spanning the lines, and the same kind an ordinary string gets.
#[test]
fn a_backtick_literal_crosses_lines_as_one_token() {
    let dumped = tree("module t;\nconst s: String = `one\ntwo\nthree`;\n");
    assert_eq!(dumped.matches("STRING_LIT@").count(), 1, "{dumped}");
    // The dump escapes a newline, so the token reads back with a literal
    // backslash-n rather than the break itself.
    assert!(dumped.contains(r"`one\ntwo\nthree`"), "{dumped}");
}

/// The reason a quoted literal stops at a newline is unchanged: a `"` that
/// reaches one has almost certainly lost its partner.
#[test]
fn a_quoted_literal_still_stops_at_a_newline() {
    let parsed = parse("module t;\nconst s: String = \"one\ntwo\";\n");
    assert!(!parsed.errors().is_empty(), "an unterminated quote should still be an error");
}

#[test]
fn an_escaped_backtick_does_not_end_the_literal() {
    let dumped = tree("module t;\nconst s: String = `a \\` b`;\n");
    assert_eq!(dumped.matches("STRING_LIT@").count(), 1, "{dumped}");
    // The dump escapes the backslash too, so it reads back doubled.
    // One token is the whole claim: had the escape not held, the literal would
    // have ended at the middle backtick and the rest would be a second one or
    // an error. `tree` has already checked the text round-trips exactly, so
    // there is nothing further to assert about the content — and asserting it
    // here would mean escaping a backslash through a Rust literal, a debug
    // dump and a shell, which is three chances to test the wrong string.
}

/// A hole may hold a quoted string, and the backtick inside it must not be
/// mistaken for the end — the same rule `lex_string` follows for `"`.
#[test]
fn a_hole_may_contain_a_quoted_string() {
    let dumped = tree("module t;\nconst s: String = `a ${f(\"b\")} c`;\n");
    assert_eq!(dumped.matches("STRING_LIT@").count(), 1, "{dumped}");
}

#[test]
fn a_hole_may_span_lines_too() {
    let dumped = tree("module t;\nconst s: String = `a ${\n  f(1)\n} b`;\n");
    assert_eq!(dumped.matches("STRING_LIT@").count(), 1, "{dumped}");
}

/// Two literals on one line do not run together, which is the first thing a
/// greedy scanner gets wrong.
#[test]
fn two_backtick_literals_on_one_line_stay_apart() {
    let dumped = tree("module t;\nfn f() -> Int { g(`a`, `b`); 0 }\n");
    assert_eq!(dumped.matches("STRING_LIT@").count(), 2, "{dumped}");
}

/// An empty one is a string, not the start of the next thing.
#[test]
fn an_empty_backtick_literal_is_a_literal() {
    let dumped = tree("module t;\nconst s: String = ``;\n");
    assert_eq!(dumped.matches("STRING_LIT@").count(), 1, "{dumped}");
}

/// And nothing else in the grammar wanted a backtick, so no existing program
/// changes meaning.
#[test]
fn a_backtick_was_not_previously_a_token() {
    let parsed = parse("module t;\nfn f() -> Int { `\n");
    // Unterminated, so it swallows the rest — but it lexes as a literal rather
    // than as the `LEX_ERROR` a stray byte used to produce.
    assert!(parsed.debug_tree().contains("STRING_LIT@"), "{}", parsed.debug_tree());
}
