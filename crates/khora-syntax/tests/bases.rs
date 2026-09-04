//! Numeric bases Khora does not have, and characters it cannot read.
//!
//! **A wrong underline is worse than no underline.** `0xFF` used to be lexed
//! as the integer `0` followed by the identifier `xFF`, which the grammar then
//! broke on somewhere else entirely — the message was *this `{` is never
//! closed*, naming a brace on the line above. Every unlexable character had
//! the same shape: `1 @ 2` reported the same thing.
//!
//! The parser was not lying. By the time it gave up, the block really was
//! unclosed. But an editor draws its underline where the message points, and
//! it pointed at a line with nothing wrong on it — so a person reading the
//! screen was sent to look at a brace they had typed correctly.
//!
//! These pin the messages that replaced it, and one that has to keep working:
//! the decimal literals and the `_` separator that are the only numeric
//! spellings there are.

use khora_syntax::parse;

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn wrapped(body: &str) -> String {
    format!("module m;\n\nfn go() -> Int {{\n  {body}\n}}\n")
}

/// The message names the base, so a reader learns the rule rather than the
/// symptom.
#[test]
fn hexadecimal_is_named_as_hexadecimal() {
    let found = errors(&wrapped("0xFF"));
    assert!(
        found.iter().any(|e| e.contains("hexadecimal") && e.contains("no hexadecimal literals")),
        "{found:?}"
    );
}

#[test]
fn binary_is_named_as_binary() {
    let found = errors(&wrapped("0b1010"));
    assert!(found.iter().any(|e| e.contains("no binary literals")), "{found:?}");
}

#[test]
fn octal_is_named_as_octal() {
    let found = errors(&wrapped("0o17"));
    assert!(found.iter().any(|e| e.contains("no octal literals")), "{found:?}");
}

/// **And it says what to write instead.** A message that only refuses leaves
/// somebody guessing whether the answer is `16r FF` or a function call.
#[test]
fn the_message_says_what_khora_does_have() {
    let found = errors(&wrapped("0xFF"));
    assert!(
        found.iter().any(|e| e.contains("decimal") && e.contains('_')),
        "the separator is the other half of the answer: {found:?}"
    );
}

/// **The old message is gone**, which is the actual regression to guard: the
/// error must not be about a brace.
#[test]
fn a_based_literal_no_longer_blames_a_brace() {
    let found = errors(&wrapped("0xFF"));
    assert!(
        !found.iter().any(|e| e.contains("never closed")),
        "the underline belongs on the literal: {found:?}"
    );
}

/// A character the lexer cannot read is named, at itself.
#[test]
fn an_unreadable_character_is_named() {
    let found = errors(&wrapped("1 @ 2"));
    assert!(found.iter().any(|e| e.contains('@')), "{found:?}");
    assert!(
        !found.first().is_some_and(|first| first.contains("never closed")),
        "the first thing said should be about the character: {found:?}"
    );
}

/// **The half that has to keep working.** `0` is not a based literal, and
/// neither is anything else ordinary — a pattern greedy enough to catch `0xFF`
/// is one that could catch these.
#[test]
fn ordinary_numbers_are_left_alone() {
    for written in ["0", "10", "1_000_000", "0.5", "1.5e3", "9223372036854775807"] {
        let found = errors(&wrapped(written));
        assert!(found.is_empty(), "`{written}` should lex: {found:?}");
    }
}

/// A name beginning with a digit is not a thing, but `x0b` is an ordinary
/// identifier and must not be read as a base.
#[test]
fn an_identifier_that_contains_a_base_prefix_is_still_an_identifier() {
    let source = "module m;\n\nfn go() -> Int {\n  let x0b1 = 1;\n  x0b1\n}\n";
    assert!(errors(source).is_empty(), "{:?}", errors(source));
}
