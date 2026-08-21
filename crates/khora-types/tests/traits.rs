//! Traits: coherence, kinds, bounds, and method resolution.
//!
//! The rules under test are stated in `docs/design/typeclasses.md`.

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

const SHOW: &str = "module m;\n\
                    pub trait Show { fn show(self) -> String; }\n\
                    impl Show for Int { fn show(self) -> String { \"i\" } }\n";

// --- resolution -----------------------------------------------------------

#[test]
fn a_method_resolves_through_the_impl() {
    assert_clean(&format!("{SHOW}fn f() -> String {{ 1.show() }}\n"));
}

#[test]
fn a_method_resolves_through_a_bound() {
    assert_clean(&format!("{SHOW}fn f<T: Show>(x: T) -> String {{ x.show() }}\n"));
}

#[test]
fn a_type_without_the_impl_is_rejected() {
    assert_reports(
        &format!("{SHOW}fn f() -> String {{ true.show() }}\n"),
        "`Bool` does not implement `Show`",
    );
}

/// The message has to distinguish "you need a bound" from "no such method",
/// because the fixes are different.
#[test]
fn an_unbounded_parameter_is_told_to_add_a_bound() {
    assert_reports(
        &format!("{SHOW}fn f<T>(x: T) -> String {{ x.show() }}\n"),
        "add one, as `T: Trait`",
    );
}

#[test]
fn a_bound_that_does_not_cover_the_method_names_the_bounds_it_has() {
    assert_reports(
        "module m;\n\
         pub trait Show { fn show(self) -> String; }\n\
         pub trait Size { fn size(self) -> Int; }\n\
         fn f<T: Size>(x: T) -> String { x.show() }\n",
        "no method `show` on `T`, whose bounds are `Size`",
    );
}

#[test]
fn a_supertrait_bound_provides_its_parents_methods() {
    assert_clean(
        "module m;\n\
         pub trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         pub trait Ord: Eq { fn cmp(self, other: Self) -> Int; }\n\
         fn f<T: Ord>(a: T, b: T) -> Bool { a.eq(b) }\n",
    );
}

#[test]
fn a_method_call_checks_its_arguments() {
    assert_reports(
        "module m;\n\
         pub trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         impl Eq for Int { fn eq(self, other: Int) -> Bool { true } }\n\
         fn f() -> Bool { 1.eq(true) }\n",
        "this argument",
    );
}

#[test]
fn the_return_type_of_a_method_is_its_traits() {
    assert_reports(
        &format!("{SHOW}fn f() -> Int {{ 1.show() }}\n"),
        "returns `Int`, but its body has type `String`",
    );
}

// --- bounds at call sites -------------------------------------------------

#[test]
fn an_unsatisfied_bound_names_the_type_the_trait_and_the_function() {
    let found = errors(&format!(
        "{SHOW}fn need<T: Show>(x: T) -> String {{ x.show() }}\n\
         fn f() -> String {{ need(true) }}\n"
    ));
    let message = found.join(" ");
    assert!(
        message.contains("`Bool`") && message.contains("`Show`") && message.contains("`need`"),
        "the diagnostic should name all three, got {found:?}"
    );
}

/// A bound is satisfied inside a generic function when the enclosing signature
/// promised it — this is what makes generic code compose at all.
#[test]
fn a_bound_is_discharged_by_the_enclosing_signature() {
    assert_clean(&format!(
        "{SHOW}fn inner<T: Show>(x: T) -> String {{ x.show() }}\n\
         fn outer<U: Show>(x: U) -> String {{ inner(x) }}\n"
    ));
}

#[test]
fn a_bound_the_caller_does_not_have_is_rejected() {
    assert_reports(
        &format!(
            "{SHOW}fn inner<T: Show>(x: T) -> String {{ x.show() }}\n\
             fn outer<U>(x: U) -> String {{ inner(x) }}\n"
        ),
        "`U` does not implement `Show`",
    );
}

#[test]
fn a_supertrait_discharges_a_bound_on_its_parent() {
    assert_clean(
        "module m;\n\
         pub trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         pub trait Ord: Eq { fn cmp(self, other: Self) -> Int; }\n\
         fn needs_eq<T: Eq>(a: T, b: T) -> Bool { a.eq(b) }\n\
         fn has_ord<T: Ord>(a: T, b: T) -> Bool { needs_eq(a, b) }\n",
    );
}

// --- coherence ------------------------------------------------------------

#[test]
fn two_impls_of_one_trait_for_one_type_are_rejected() {
    assert_reports(
        &format!("{SHOW}impl Show for Int {{ fn show(self) -> String {{ \"j\" }} }}\n"),
        "already implemented for `Int`",
    );
}

#[test]
fn one_type_may_implement_several_traits() {
    assert_clean(
        "module m;\n\
         pub trait Show { fn show(self) -> String; }\n\
         pub trait Size { fn size(self) -> Int; }\n\
         impl Show for Int { fn show(self) -> String { \"i\" } }\n\
         impl Size for Int { fn size(self) -> Int { 8 } }\n",
    );
}

#[test]
fn an_impl_of_an_unknown_trait_is_rejected() {
    assert_reports("module m;\nimpl Nope for Int { }\n", "`Nope` is not a trait in scope");
}

#[test]
fn an_impl_missing_a_function_is_rejected() {
    assert_reports(
        "module m;\n\
         pub trait Show { fn show(self) -> String; }\n\
         impl Show for Int { }\n",
        "missing `show` from `Show`",
    );
}

#[test]
fn an_impl_with_a_function_the_trait_never_declared_is_rejected() {
    assert_reports(
        "module m;\n\
         pub trait Show { fn show(self) -> String; }\n\
         impl Show for Int { fn show(self) -> String { \"i\" } fn extra(self) -> Int { 1 } }\n",
        "`Show` has no function named `extra`",
    );
}

/// A default body is what lets an impl leave a function out.
#[test]
fn a_default_body_makes_a_function_optional() {
    assert_clean(
        "module m;\n\
         pub trait Show {\n\
           fn show(self) -> String;\n\
           fn label(self) -> String { \"thing\" }\n\
         }\n\
         impl Show for Int { fn show(self) -> String { \"i\" } }\n",
    );
}

// --- associated types -----------------------------------------------------

#[test]
fn an_impl_must_supply_every_associated_type() {
    assert_reports(
        "module m;\n\
         pub trait Iterator { type Item; fn next(self) -> Int; }\n\
         impl Iterator for Int { fn next(self) -> Int { 1 } }\n",
        "missing the associated type `Item`",
    );
}

#[test]
fn an_associated_type_the_trait_never_declared_is_rejected() {
    assert_reports(
        "module m;\n\
         pub trait Show { fn show(self) -> String; }\n\
         impl Show for Int { type Item = Int; fn show(self) -> String { \"i\" } }\n",
        "`Show` has no associated type named `Item`",
    );
}

// --- kinds ----------------------------------------------------------------

/// The whole kind system a reader sees: `Functor` writes `Self<A>`, so `Int`
/// cannot implement it, and nobody had to write `* -> *`.
#[test]
fn a_higher_kinded_trait_rejects_a_plain_type() {
    assert_reports(
        "module m;\n\
         pub trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Int { }\n",
        "kind `* -> *`",
    );
}

#[test]
fn a_plain_trait_rejects_a_constructor() {
    assert_reports(
        "module m;\n\
         pub type Option<A> = | Some(value: A) | None;\n\
         pub trait Show { fn show(self) -> String; }\n\
         impl Show for Option { }\n",
        "kind `*`",
    );
}

/// The fix for the common mistake is spelled out, because the difference
/// between `Option` and `Option<A>` is exactly what the reader got wrong.
#[test]
fn applying_a_constructor_too_far_suggests_the_bare_name() {
    assert_reports(
        "module m;\n\
         pub type Option<A> = | Some(value: A) | None;\n\
         pub trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl<A> Functor for Option<A> { }\n",
        "write `impl Functor for Option`",
    );
}

#[test]
fn a_higher_kinded_trait_accepts_a_constructor() {
    let found = errors(
        "module m;\n\
         pub type Option<A> = | Some(value: A) | None;\n\
         pub trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Option {\n\
           fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> { Option::None }\n\
         }\n",
    );
    assert!(!found.iter().any(|e| e.contains("kind")), "no kind error expected, got {found:?}");
}

/// Const parameters make a different kind, so a trait cannot be implemented for
/// a shape-indexed type as though its arguments were types.
#[test]
fn a_const_parameter_gives_a_different_kind() {
    assert_reports(
        "module m;\n\
         pub type Vector<const N: Int>;\n\
         pub trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Vector { }\n",
        "kind",
    );
}
