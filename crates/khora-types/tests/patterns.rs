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
