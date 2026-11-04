//! Integer literals in hexadecimal, binary and octal, and characters the lexer
//! cannot read.
//!
//! **What these guard: constants nobody can review.** A hand-written SHA-256
//! had to spell its round constants in decimal, where `1116352408` and
//! `0x428a2f98` are the same number and only one of them can be checked
//! against the standard by eye. `0x`, `0b` and `0o` are integer literals with
//! the typing a decimal one has.
//!
//! **And a wrong underline is worse than no underline.** Before the lexer
//! matched a based literal as one token, `0xFF` lexed as `0` and the
//! identifier `xFF`, and the parser reported whatever those broke next -- for
//! `{ 0xFF }`, *this `{` is never closed*, pointing at a brace on the line
//! above. So a malformed one (`0b12`, `0x`) is still one token, and the error
//! is at it.

use khora_syntax::number::{int_literal_value, IntLiteralError};
use khora_syntax::parse;

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn wrapped(body: &str) -> String {
    format!("module m;\n\nfn go() -> Int {{\n  {body}\n}}\n")
}

/// Every spelling that is a number: both cases of each prefix and of the
/// digits, `_` anywhere after the prefix, and the decimal forms beside them.
#[test]
fn every_base_and_case_parses() {
    for written in [
        "0xFF", "0xff", "0XFF", "0xAbCd", "0b1010", "0B1010", "0o17", "0O17", "0xFFFF_FFFF",
        "0b1111_0000", "0x_FF", "0xFF_", "0o7_7_7",
    ] {
        let found = errors(&wrapped(written));
        assert!(found.is_empty(), "`{written}` should parse: {found:?}");
        let dumped = parse(&wrapped(written)).debug_tree();
        assert!(dumped.contains("INT_LIT@"), "`{written}` is an integer literal: {dumped}");
    }
}

/// The value each one names, in its own base.
#[test]
fn each_literal_names_the_value_written() {
    for (written, value) in [
        ("0xFF", 255u128),
        ("0xff", 255),
        ("0XfF", 255),
        ("0b1010", 10),
        ("0B1010", 10),
        ("0o17", 15),
        ("0O17", 15),
        ("0xFFFF_FFFF", 4_294_967_295),
        ("0x8000_0000_0000_0000", 9_223_372_036_854_775_808),
        ("0xFFFF_FFFF_FFFF_FFFF", 18_446_744_073_709_551_615),
        ("0x_1", 1),
        ("255", 255),
        ("1_000_000", 1_000_000),
        ("0", 0),
    ] {
        assert_eq!(int_literal_value(written), Ok(value), "`{written}`");
    }
}

/// **The suffix applies only to decimal numerals.** `d` is a hexadecimal
/// digit, so `0x1d` is twenty-nine and not a `Decimal` one; the numbers
/// design note asked for exactly this whenever hexadecimal arrived.
#[test]
fn a_d_in_a_hexadecimal_literal_is_a_digit() {
    let dumped = parse(&wrapped("0x1d")).debug_tree();
    assert!(dumped.contains("INT_LIT@"), "{dumped}");
    assert!(!dumped.contains("DECIMAL_LIT@"), "{dumped}");
    assert_eq!(int_literal_value("0x1d"), Ok(29));
}

/// A digit the base does not have is refused at the literal, and the message
/// names the digit and the base, with the article the base's name takes.
#[test]
fn a_digit_the_base_does_not_have_is_refused() {
    for (written, digit, phrase) in [
        ("0b12", '2', "is not a binary number: `2` is not a binary digit"),
        ("0o8", '8', "is not an octal number: `8` is not an octal digit"),
        ("0xG", 'G', "is not a hexadecimal number: `G` is not a hexadecimal digit"),
        ("0b1d", 'd', "is not a binary number: `d` is not a binary digit"),
    ] {
        let found = errors(&wrapped(written));
        assert!(found.iter().any(|e| e.contains(phrase)), "`{written}`: {found:?}");
        assert_eq!(int_literal_value(written), Err(IntLiteralError::Digit(digit)), "`{written}`");
    }
}

/// A prefix with no digits after it is not zero.
#[test]
fn a_prefix_with_no_digits_is_refused() {
    for written in ["0x", "0x_", "0b", "0o__"] {
        let found = errors(&wrapped(written));
        assert!(found.iter().any(|e| e.contains("no digits")), "`{written}`: {found:?}");
        assert_eq!(int_literal_value(written), Err(IntLiteralError::Empty), "`{written}`");
    }
}

/// A value too wide for any integer is the type checker's to report, against
/// the type it was meant to be; the parser leaves it alone.
#[test]
fn too_wide_is_not_a_syntax_error() {
    let written = "0x1_0000_0000_0000_0000_0000_0000_0000_0000";
    assert!(errors(&wrapped(written)).is_empty());
    assert_eq!(int_literal_value(written), Err(IntLiteralError::TooLarge));
}

/// **The underline is on the literal**, which is the regression the
/// one-token lexing exists for: the error must not be about a brace.
#[test]
fn a_malformed_literal_does_not_blame_a_brace() {
    for written in ["0b12", "0x"] {
        let found = errors(&wrapped(written));
        assert!(!found.iter().any(|e| e.contains("never closed")), "`{written}`: {found:?}");
    }
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
/// neither is anything else ordinary -- a pattern greedy enough to catch `0xG`
/// is one that could catch these.
#[test]
fn ordinary_numbers_are_left_alone() {
    for written in ["0", "10", "1_000_000", "0.5", "1.5e3", "9223372036854775807", "0d", "0.5d"] {
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

/// A const-generic argument reads the same value in any base.
#[test]
fn a_type_argument_may_be_written_in_any_base() {
    use khora_syntax::ast::{AstNode, LiteralType};
    let parsed = parse("module m;\n\nfn go(a: Array<Int, 0x10>) -> Int { 0 }\n");
    assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());
    let value = parsed.syntax().descendants().find_map(LiteralType::cast).and_then(|l| l.value());
    assert_eq!(value, Some(16));
}

/// **The signed reader takes the `-` the front end writes.** Source never
/// spells a negative literal -- `-5` is a negation -- but a negative
/// `Decimal` literal desugars to integer halves whose high half is `-1`, and
/// a reader that refused the sign refused every negative `Decimal` literal.
#[test]
fn the_signed_reader_takes_a_written_minus() {
    use khora_syntax::number::int_literal_signed;
    assert_eq!(int_literal_signed("-1"), Ok(-1));
    assert_eq!(int_literal_signed("-0x80"), Ok(-128));
    assert_eq!(int_literal_signed("255"), Ok(255));
    assert_eq!(int_literal_signed("-170141183460469231731687303715884105728"), Ok(i128::MIN));
    assert_eq!(int_literal_signed("170141183460469231731687303715884105728"), Err(IntLiteralError::TooLarge));
    assert_eq!(int_literal_signed("-0b2"), Err(IntLiteralError::Digit('2')));
}
