//! `0.01d`, as the lexer sees it.
//!
//! `docs/design/numbers.md` §"Decimal". The language's **only** literal
//! suffix, and a real addition rather than a convention already in use: every
//! other numeric type is constructed, as `I32::of(0)` and `U64::wrapping(-1)`.
//! It earns the exception because without it there is no way to write an exact
//! decimal *constant* — `Decimal::of("0.01")` parses at run time and returns a
//! `Result`, and going through a `Float` throws away the exactness the type
//! exists for.
//!
//! `numbers.md` leaves three notes for whoever implements it. All three are
//! tests here.

use khora_syntax::parse;

fn tree(source: &str) -> String {
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source, "did not round-trip");
    assert!(parsed.errors().is_empty(), "{source}\n{:?}", parsed.errors());
    parsed.debug_tree()
}

fn decimals(source: &str) -> usize {
    tree(source).matches("DECIMAL_LIT@").count()
}

#[test]
fn a_fraction_with_the_suffix_is_one_token() {
    assert_eq!(decimals("module t;\nfn f() -> Int { let a = 0.01d; 0 }\n"), 1);
}

/// A whole amount of money is the common case, so `1.00d` is not mandatory.
#[test]
fn a_whole_number_may_carry_the_suffix() {
    assert_eq!(decimals("module t;\nfn f() -> Int { let a = 1d; 0 }\n"), 1);
}

/// **The first note.** The suffix goes after any exponent, so this is one
/// token rather than a float followed by an identifier.
#[test]
fn the_suffix_goes_after_the_exponent() {
    assert_eq!(decimals("module t;\nfn f() -> Int { let a = 1.5e3d; 0 }\n"), 1);
    assert_eq!(decimals("module t;\nfn f() -> Int { let a = 1.5e-3d; 0 }\n"), 1);
}

/// **The second note.** Nothing may lex between the digits and the `d`.
#[test]
fn a_space_before_the_suffix_is_not_a_decimal() {
    let dumped = tree("module t;\nfn f(d: Int) -> Int { let a = 1 * d; 0 }\n");
    assert!(!dumped.contains("DECIMAL_LIT@"), "{dumped}");
    assert!(dumped.contains("INT_LIT@"), "{dumped}");
}

/// Underscores separate here as they do in every other numeral.
#[test]
fn underscores_separate() {
    assert_eq!(decimals("module t;\nfn f() -> Int { let a = 1_000.25d; 0 }\n"), 1);
}

// --- what did not change ----------------------------------------------------

/// An ordinary float is still an ordinary float. `0.01` staying a `Float` is
/// the decision the suffix exists to make possible.
#[test]
fn a_bare_fraction_is_still_a_float() {
    let dumped = tree("module t;\nfn f() -> Float { 0.01 }\n");
    assert!(dumped.contains("FLOAT_LIT@"), "{dumped}");
    assert!(!dumped.contains("DECIMAL_LIT@"), "{dumped}");
}

#[test]
fn an_integer_is_still_an_integer() {
    let dumped = tree("module t;\nfn f() -> Int { 1 }\n");
    assert!(dumped.contains("INT_LIT@"), "{dumped}");
    assert!(!dumped.contains("DECIMAL_LIT@"), "{dumped}");
}

/// An identifier beginning with `d` is not a suffix looking for a number, and
/// a name that merely ends in one is untouched.
#[test]
fn identifiers_are_unaffected() {
    let dumped = tree("module t;\nfn f() -> Int { let dozen = 1; let id = dozen; id }\n");
    assert!(!dumped.contains("DECIMAL_LIT@"), "{dumped}");
}

/// A field access on an integer is not a decimal, which is the shape the
/// float regex could have swallowed.
#[test]
fn a_method_call_on_a_number_is_not_a_decimal() {
    let dumped = tree("module t;\nfn f() -> String { 1.to_string() }\n");
    assert!(!dumped.contains("DECIMAL_LIT@"), "{dumped}");
}
