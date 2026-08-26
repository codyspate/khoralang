//! Generic functions and generic types.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_types::check_file;

fn errors(db: &dyn Db, text: &str) -> Vec<String> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    check_file(db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let db = KhoraDatabase::new();
    let found = errors(&db, text);
    assert!(found.is_empty(), "expected no type errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let db = KhoraDatabase::new();
    let found = errors(&db, text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

const OPTION: &str = "module m;\npub type Option<A> = | Some(value: A) | None;\n";

#[test]
fn the_identity_function_checks() {
    assert_clean("module m;\nfn id<A>(x: A) -> A { x }\n");
}

/// The body of a generic function does not get to decide what its caller's
/// type argument is. Without rigid parameters this would be accepted, and the
/// checker would be unsound.
#[test]
fn a_generic_body_cannot_assume_a_concrete_type() {
    assert_reports(
        "module m;\nfn f<A>(x: A) -> Int { x }\n",
        "the caller chooses",
    );
    assert_reports(
        "module m;\nfn f<A>(x: A) -> A { x + 1 }\n",
        "the caller chooses",
    );
}

#[test]
fn two_type_parameters_stay_distinct() {
    assert_clean("module m;\nfn first<A, B>(a: A, b: B) -> A { a }\n");
    assert_reports("module m;\nfn wrong<A, B>(a: A, b: B) -> A { b }\n", "the caller chooses");
}

/// Each call site is instantiated separately, so two unrelated calls do not
/// constrain one another.
#[test]
fn one_generic_function_serves_several_types() {
    assert_clean(
        "module m;\n\
         fn id<A>(x: A) -> A { x }\n\
         fn use_int() -> Int { id(1) }\n\
         fn use_bool() -> Bool { id(true) }\n",
    );
}

#[test]
fn a_call_result_is_the_instantiated_return_type() {
    assert_reports(
        "module m;\nfn id<A>(x: A) -> A { x }\nfn f() -> Bool { id(1) }\n",
        "returns `Bool`, but its body has type `Int`",
    );
}

#[test]
fn a_generic_type_takes_its_argument_from_the_constructor() {
    assert_clean(&format!("{OPTION}fn f() -> Option<Int> {{ Option::Some(1) }}\n"));
    assert_reports(
        &format!("{OPTION}fn f() -> Option<Int> {{ Option::Some(true) }}\n"),
        "has type `Option<Bool>`",
    );
}

#[test]
fn a_nullary_constructor_is_generic_in_its_parameter() {
    // `None` fits any `Option<A>`, because nothing constrains `A`.
    assert_clean(&format!("{OPTION}fn f() -> Option<Int> {{ Option::None }}\n"));
    assert_clean(&format!("{OPTION}fn g() -> Option<Bool> {{ Option::None }}\n"));
}

#[test]
fn a_pattern_binding_takes_the_instantiated_field_type() {
    assert_clean(&format!(
        "{OPTION}fn f(o: Option<Int>) -> Int {{\n  \
           match o {{\n    Option::Some(v) => v + 1,\n    Option::None => 0,\n  }}\n\
         }}\n"
    ));
    assert_reports(
        &format!(
            "{OPTION}fn f(o: Option<Bool>) -> Int {{\n  \
               match o {{\n    Option::Some(v) => v + 1,\n    Option::None => 0,\n  }}\n\
             }}\n"
        ),
        "expected `Int`, found `Bool`",
    );
}

#[test]
fn a_generic_type_still_checks_exhaustiveness() {
    assert_reports(
        &format!("{OPTION}fn f(o: Option<Int>) -> Int {{\n  match o {{\n    Option::Some(v) => v,\n  }}\n}}\n"),
        "not exhaustive",
    );
}

/// A generic type is not interchangeable with what it contains.
#[test]
fn a_generic_type_is_distinct_from_what_it_contains() {
    assert_clean(&format!("{OPTION}fn f(o: Option<Int>) -> Option<Int> {{ o }}\n"));
    assert_reports(
        &format!("{OPTION}fn g(o: Option<Int>) -> Int {{ o }}\n"),
        "returns `Int`, but its body has type `Option<Int>`",
    );
}

#[test]
fn a_generic_function_over_a_generic_type_checks() {
    assert_clean(&format!(
        "{OPTION}fn unwrap_or<A>(o: Option<A>, fallback: A) -> A {{\n  \
           match o {{\n    Option::Some(v) => v,\n    Option::None => fallback,\n  }}\n\
         }}\n"
    ));
}
