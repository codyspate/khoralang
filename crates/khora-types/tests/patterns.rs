//! A constructor pattern checked against the value it is matched on.
//!
//! **`T::C(..)` is a claim that the value is a `T`, and nothing used to test
//! it.** `match 3 { Maybe::Some(v) => v, _ => 0 }` checked clean and then
//! panicked the code generator. Where both types were heap objects --
//! `Maybe<Big>` matched with `Either::Ok(v)` -- it checked clean, built, and
//! read `v` out of the wrong constructor's layout: an answer with nothing to
//! say it was wrong. The same held at every depth, in `match`, `catch` and
//! `let` alike, because all three bind through one walk and the walk read a
//! field's declared type without asking whose field it was.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

/// Declarations every case uses. `Big` is a record with pointer fields, so a
/// `Maybe<Big>` and an `Either<..>` are both heap objects: the pair whose
/// mix-up the code generator could not notice.
const TYPES: &str = "module m;\n\
    pub type Big = { a: String, b: String };\n\
    pub type Maybe<A> = | Some(v: A) | None;\n\
    pub type Either<A, B> = | Ok(v: A) | Err(e: B);\n\
    pub type Gx<A> = | X(s: String, v: A) | Y(n: Int);\n\
    pub type One = | W(n: Int);\n\
    pub type Point = { x: Int, y: Int };\n\
    fn fi() -> Int raises Gx<Int> { raise Gx::X(\"i\", 3) }\n\
    fn fb() -> Int raises Gx<Maybe<Big>> { raise Gx::X(\"b\", Maybe::None) }\n";

const WRONG: &str = "this pattern is a";

#[test]
fn a_constructor_pattern_over_an_int_is_refused() {
    assert_reports(
        &format!("{TYPES}fn f() -> Int {{ match 3 {{ Maybe::Some(v) => v, _ => 0 }} }}\n"),
        "this pattern is a `Maybe` case, and the value here is a `Int`",
    );
}

#[test]
fn a_constructor_pattern_of_another_boxed_type_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn f(o: Maybe<Big>) -> Int {{ \
             match o {{ Maybe::Some(Either::Ok(v)) => 1, _ => 0 }} }}\n"
        ),
        "this pattern is a `Either` case, and the value here is a `Big`",
    );
}

#[test]
fn the_outer_pattern_of_a_match_is_checked_too() {
    assert_reports(
        &format!(
            "{TYPES}fn f(o: Maybe<Int>) -> Int {{ \
             match o {{ Either::Ok(v) => v, _ => 0 }} }}\n"
        ),
        "this pattern is a `Either` case, and the value here is a `Maybe<Int>`",
    );
}

#[test]
fn a_nested_pattern_in_a_catch_arm_is_checked() {
    assert_reports(
        &format!(
            "{TYPES}fn f() -> Int {{ fi()! catch {{ Gx::X(s, Maybe::Some(v)) => v, _ => 0 }} }}\n"
        ),
        "this pattern is a `Maybe` case, and the value here is a `Int`",
    );
}

#[test]
fn a_boxed_nested_pattern_in_a_catch_arm_is_checked() {
    assert_reports(
        &format!(
            "{TYPES}fn f() -> Int {{ \
             fb()! catch {{ Gx::X(s, Maybe::Some(Either::Ok(v))) => 1, _ => 0 }} }}\n"
        ),
        "this pattern is a `Either` case, and the value here is a `Big`",
    );
}

#[test]
fn a_let_pattern_is_checked() {
    assert_reports(
        &format!("{TYPES}fn f() -> Int {{ let One::W(n) = 3; n }}\n"),
        "this pattern is a `One` case, and the value here is a `Int`",
    );
}

#[test]
fn a_record_pattern_is_checked() {
    assert_reports(
        &format!("{TYPES}fn f() -> Int {{ match 3 {{ Point {{ x }} => x, _ => 0 }} }}\n"),
        "this pattern is a `Point` case, and the value here is a `Int`",
    );
}

#[test]
fn a_constructor_with_no_payload_is_checked() {
    assert_reports(
        &format!("{TYPES}fn f() -> Int {{ match 3 {{ Maybe::None => 1, _ => 0 }} }}\n"),
        "this pattern is a `Maybe` case, and the value here is a `Int`",
    );
}

#[test]
fn a_pattern_inside_a_tuple_pattern_is_checked() {
    assert_reports(
        &format!(
            "{TYPES}fn f() -> Int {{ match (1, 2) {{ (Maybe::Some(a), b) => b, _ => 0 }} }}\n"
        ),
        "this pattern is a `Maybe` case, and the value here is a `Int`",
    );
}

/// A type parameter is the caller's to choose, so no constructor pattern can
/// assume which type it is.
#[test]
fn a_constructor_pattern_over_a_type_parameter_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn f<T>(x: T) -> Int {{ match x {{ Maybe::Some(_) => 1, _ => 0 }} }}\n"
        ),
        "this pattern is a `Maybe` case, and the value here is a `T`",
    );
}

/// **The refusal must not reach the correct programs**: the same pattern at
/// the right type, at every depth, with the arguments inferred.
#[test]
fn a_pattern_of_the_right_type_is_accepted_at_every_depth() {
    assert_clean(&format!(
        "{TYPES}fn f(o: Maybe<Either<Int, String>>) -> Int {{ \
         match o {{ Maybe::Some(Either::Ok(v)) => v, Maybe::Some(Either::Err(_)) => 1, \
         Maybe::None => 0 }} }}\n\
         fn g() -> Int {{ fi()! catch {{ Gx::X(s, v) => v, Gx::Y(n) => n }} }}\n\
         fn h(p: Point) -> Int {{ let Point {{ x, y }} = p; x + y }}\n\
         fn k(w: One) -> Int {{ let One::W(n) = w; n }}\n"
    ));
}

/// A value whose type is still being inferred takes the pattern's type, which
/// is how a lambda's parameter learns it is a `Maybe` from the arms matching
/// on it.
#[test]
fn a_constructor_pattern_settles_a_value_still_being_inferred() {
    assert_clean(&format!(
        "{TYPES}fn f() -> Int {{ \
         let g = fn (o) => match o {{ Maybe::Some(v) => v + 1, Maybe::None => 0 }}; \
         g(Maybe::Some(1)) }}\n"
    ));
}

/// Exhaustiveness still sees through a pattern of the right type: a missing
/// case is still reported, and the wrong-type refusal does not replace it.
#[test]
fn coverage_is_still_checked() {
    let found = errors(&format!(
        "{TYPES}fn f(o: Maybe<Int>) -> Int {{ match o {{ Maybe::Some(v) => v }} }}\n"
    ));
    assert!(found.iter().any(|e| e.contains("not exhaustive")), "{found:?}");
    assert!(!found.iter().any(|e| e.contains(WRONG)), "{found:?}");
}

/// **A literal pattern must fit the type of the value it is matched
/// against**, as `let b: U8 = 256;` must. A code generator that compares at
/// the value's width reads `256` on a `U8` as its low byte, so `0` took the
/// `256` arm with exit status 0. Every row of the review's `trunc.kh` and
/// `trunc_int.kh`, every base, inside a tuple, a constructor payload and a
/// record pattern.
#[test]
fn a_literal_pattern_that_does_not_fit_its_value_is_refused() {
    for (ty, arm, says) in [
        ("U8", "0x100", "`0x100` does not fit in `U8`, which holds 0 to 255"),
        ("U8", "256", "`256` does not fit in `U8`, which holds 0 to 255"),
        ("U8", "0b1_0000_0000", "`0b1_0000_0000` does not fit in `U8`"),
        ("U8", "0o400", "`0o400` does not fit in `U8`"),
        ("I8", "0x80", "`0x80` does not fit in `I8`, which holds -128 to 127"),
        ("I8", "128", "`128` does not fit in `I8`"),
        ("U16", "65537", "`65537` does not fit in `U16`"),
        ("I16", "0xFFFF", "`0xFFFF` does not fit in `I16`"),
        ("U32", "0x1_0000_0005", "`0x1_0000_0005` does not fit in `U32`"),
        ("I32", "0x8000_0000", "`0x8000_0000` does not fit in `I32`"),
        ("U64", "0x1_0000_0000_0000_0000", "`0x1_0000_0000_0000_0000` does not fit in `U64`"),
        ("Int", "18446744073709551615", "`18446744073709551615` does not fit in an `Int`"),
        ("Int", "0x8000_0000_0000_0000", "`0x8000_0000_0000_0000` does not fit in an `Int`"),
    ] {
        assert_reports(
            &format!(
                "module m;\nimport std::core::{{U8, I8, U16, I16, U32, I32, U64}};\n\
                 fn f(x: {ty}) -> Int {{ match x {{ {arm} => 1, _ => 0 }} }}\n"
            ),
            says,
        );
    }
}

/// The same refusal below the top of the pattern: in a tuple, a constructor
/// payload and a record field, each against the component's own type.
#[test]
fn a_nested_literal_pattern_that_does_not_fit_is_refused() {
    let decls = "module m;\nimport std::core::{U8, I8};\n\
                 pub type Msg = | Pair(a: U8, b: I8) | Other;\n\
                 pub type Px = { tag: U8, n: Int };\n";
    for (body, says) in [
        (
            "fn f(x: (U8, I8)) -> Int { match x { (0x1FF, 0) => 1, _ => 0 } }",
            "`0x1FF` does not fit in `U8`",
        ),
        (
            "fn f(x: (U8, I8)) -> Int { match x { (0, 0xFF) => 1, _ => 0 } }",
            "`0xFF` does not fit in `I8`",
        ),
        (
            "fn f(m: Msg) -> Int { match m { Msg::Pair(0xFE, 0x100) => 1, _ => 0 } }",
            "`0x100` does not fit in `I8`",
        ),
        (
            "fn f(p: Px) -> Int { match p { Px { tag: 300, n } => n, _ => 0 } }",
            "`300` does not fit in `U8`",
        ),
    ] {
        assert_reports(&format!("{decls}{body}\n"), says);
    }
}

/// And a literal that fits is not refused, at the top, nested, at each
/// type's edges and in each base.
#[test]
fn a_literal_pattern_that_fits_is_accepted() {
    assert_clean(
        "module m;\nimport std::core::{U8, I8, U16, I16, U64};\n\
         pub type Msg = | Pair(a: U8, b: I8) | Other;\n\
         fn a(x: U8) -> Int { match x { 0xFF => 1, 0 => 2, 0b1010 => 3, 0o17 => 4, _ => 0 } }\n\
         fn b(x: I8) -> Int { match x { 0x7F => 1, 126 => 2, _ => 0 } }\n\
         fn c(x: U16) -> Int { match x { 0xFFFF => 1, _ => 0 } }\n\
         fn d(x: U64) -> Int { match x { 0xFFFF_FFFF_FFFF_FFFF => 1, 18446744073709551614 => 2, _ => 0 } }\n\
         fn e(x: Int) -> Int { match x { 0x7FFF_FFFF_FFFF_FFFF => 1, 9223372036854775806 => 2, _ => 0 } }\n\
         fn g(m: Msg) -> Int { match m { Msg::Pair(0xFE, 0x7F) => 1, _ => 0 } }\n\
         fn h(x: (U8, I16)) -> Int { match x { (0xFF, 0x7FFF) => 1, _ => 0 } }\n",
    );
}

/// **A literal pattern is checked against the type the value settles to**,
/// not the one it has when the `match` is read. A lambda's parameter is a
/// variable there: the first version of this check skipped it, and
/// `fn x => match x { 0x100 => .. }` applied to a `U8` reached the code
/// generator's assertion at `khora build` instead of `khora check`.
#[test]
fn a_literal_pattern_is_checked_against_the_settled_type() {
    for (body, says) in [
        (
            "fn f() -> Int { let g = fn x => match x { 0x100 => 1, _ => 0 }; let z: U8 = 0; g(z) }",
            "`0x100` does not fit in `U8`",
        ),
        (
            "fn f() -> Int { let g = fn x => match x { 18446744073709551615 => 1, _ => 0 }; g(-1) }",
            "`18446744073709551615` does not fit in an `Int`",
        ),
        (
            "fn f() -> Int { let g = fn p => match p { (0, 0x80) => 1, _ => 0 }; let b: I8 = 0; g((1, b)) }",
            "`0x80` does not fit in `I8`",
        ),
    ] {
        assert_reports(&format!("module m;\nimport std::core::{{U8, I8}};\n{body}\n"), says);
    }
}
