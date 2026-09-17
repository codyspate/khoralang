//! A lambda whose parameters are written without parentheses.
//!
//! `fn acc, row => acc + row` is what somebody writes in their first hour. The
//! comma is not part of the lambda grammar, so without a rule for it the
//! parser stops at `acc`, fails to find `=>`, and every construct after the
//! comma is read as a fresh declaration — a dozen diagnostics for one missing
//! pair of brackets, none of them naming it.
//!
//! What is pinned here is the count as much as the wording: the cascade is the
//! defect, so a test that only checked for the new message would pass with the
//! other eleven errors still present.

use khora_syntax::parse;

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

/// The source must survive the recovery unchanged; a parser that drops the
/// skipped tokens breaks every editor built on the tree.
fn round_trips(source: &str) {
    assert_eq!(parse(source).syntax().text().to_string(), source, "lost source text");
}

#[test]
fn unparenthesised_parameters_are_one_error_naming_the_brackets() {
    let source = "module t;\nfn f(xs: List<Int>) -> Int {\n  List::fold(xs, 0, fn acc, row => acc + row)\n}\n";
    round_trips(source);
    let errors = errors(source);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("parenthes"), "{errors:?}");
}

/// Three parameters, to show the recovery consumes the whole list rather than
/// one extra name.
#[test]
fn every_extra_parameter_is_absorbed_by_the_one_error() {
    let source = "module t;\nfn f() -> Int {\n  let g = fn a, b, c => a + b + c;\n  g(1, 2, 3)\n}\n";
    round_trips(source);
    let errors = errors(source);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("parenthes"), "{errors:?}");
}

/// The parenthesised form is the one the message recommends, so it has to stay
/// silent — a recovery that fired here would make the advice wrong.
#[test]
fn the_parenthesised_form_is_untouched() {
    let source = "module t;\nfn f() -> Int {\n  let g = fn (a, b) => a + b;\n  g(1, 2)\n}\n";
    round_trips(source);
    assert!(errors(source).is_empty(), "{:?}", errors(source));
}

/// A one-parameter lambda without parentheses is legal Khora and the corpus is
/// full of it. The comma is what the rule keys on, not the missing bracket.
#[test]
fn a_single_bare_parameter_stays_legal() {
    let source = "module t;\nfn f() -> Int {\n  let g = fn a => a + 1;\n  g(1)\n}\n";
    round_trips(source);
    assert!(errors(source).is_empty(), "{:?}", errors(source));
}
