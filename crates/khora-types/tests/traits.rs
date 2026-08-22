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
                    export trait Show { fn show(self) -> String; }\n\
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
         export trait Show { fn show(self) -> String; }\n\
         export trait Size { fn size(self) -> Int; }\n\
         fn f<T: Size>(x: T) -> String { x.show() }\n",
        "no method `show` on `T`, whose bounds are `Size`",
    );
}

#[test]
fn a_supertrait_bound_provides_its_parents_methods() {
    assert_clean(
        "module m;\n\
         export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         export trait Ord: Eq { fn cmp(self, other: Self) -> Int; }\n\
         fn f<T: Ord>(a: T, b: T) -> Bool { a.eq(b) }\n",
    );
}

#[test]
fn a_method_call_checks_its_arguments() {
    assert_reports(
        "module m;\n\
         export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
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
         export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         export trait Ord: Eq { fn cmp(self, other: Self) -> Int; }\n\
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
         export trait Show { fn show(self) -> String; }\n\
         export trait Size { fn size(self) -> Int; }\n\
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
         export trait Show { fn show(self) -> String; }\n\
         impl Show for Int { }\n",
        "missing `show` from `Show`",
    );
}

#[test]
fn an_impl_with_a_function_the_trait_never_declared_is_rejected() {
    assert_reports(
        "module m;\n\
         export trait Show { fn show(self) -> String; }\n\
         impl Show for Int { fn show(self) -> String { \"i\" } fn extra(self) -> Int { 1 } }\n",
        "`Show` has no function named `extra`",
    );
}

/// A default body is what lets an impl leave a function out.
#[test]
fn a_default_body_makes_a_function_optional() {
    assert_clean(
        "module m;\n\
         export trait Show {\n\
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
         export trait Iterator { type Item; fn next(self) -> Int; }\n\
         impl Iterator for Int { fn next(self) -> Int { 1 } }\n",
        "missing the associated type `Item`",
    );
}

#[test]
fn an_associated_type_the_trait_never_declared_is_rejected() {
    assert_reports(
        "module m;\n\
         export trait Show { fn show(self) -> String; }\n\
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
         export trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Int { }\n",
        "kind `* -> *`",
    );
}

#[test]
fn a_plain_trait_rejects_a_constructor() {
    assert_reports(
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         export trait Show { fn show(self) -> String; }\n\
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
         export type Option<A> = | Some(value: A) | None;\n\
         export trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl<A> Functor for Option<A> { }\n",
        "write `impl Functor for Option`",
    );
}

#[test]
fn a_higher_kinded_trait_accepts_a_constructor() {
    let found = errors(
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         export trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
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
         export type Vector<const N: Int>;\n\
         export trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Vector { }\n",
        "kind",
    );
}

// --- a type's own methods -------------------------------------------------

const USER: &str = "module m;\n\
                    export type User = | Of(age: Int);\n\
                    impl User {\n\
                      fn age(self) -> Int { match self { User::Of(a) => a } }\n\
                    }\n";

/// The point of the whole feature: a method with no trait anywhere.
#[test]
fn a_type_can_have_a_method_without_a_trait() {
    assert_clean(&format!("{USER}fn f(u: User) -> Int {{ u.age() }}\n"));
}

#[test]
fn an_inherent_method_checks_its_arguments_and_result() {
    assert_reports(
        &format!("{USER}fn f(u: User) -> Bool {{ u.age() }}\n"),
        "returns `Bool`, but its body has type `Int`",
    );
    assert_reports(
        &format!("{USER}fn f(u: User) -> Int {{ u.age(1) }}\n"),
        "takes 0 argument(s) after the receiver, but 1 were given",
    );
}

#[test]
fn a_method_the_type_does_not_have_is_still_reported() {
    assert_reports(
        &format!("{USER}fn f(u: User) -> Int {{ u.nope() }}\n"),
        "no method",
    );
}

/// Declaring the same name twice for one type is an error wherever the two
/// blocks are, because a call could not say which it meant.
#[test]
fn one_type_cannot_declare_a_method_name_twice() {
    assert_reports(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         impl User { fn age(self) -> Int { 1 } }\n\
         impl User { fn age(self) -> Int { 2 } }\n",
        "`User` already has a method named `age`",
    );
}

/// Splitting a type's methods across blocks is ordinary and allowed.
#[test]
fn a_type_may_have_several_impl_blocks() {
    assert_clean(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         impl User { fn age(self) -> Int { 1 } }\n\
         impl User { fn next(self) -> Int { 2 } }\n",
    );
}

/// A constructor function. It has no receiver to be reached through, so the
/// only way to call it is by naming the type — which is also the shape every
/// `Type::new()` in the reference application uses.
#[test]
fn a_type_s_own_function_is_reached_by_path() {
    assert_clean(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         impl User { fn new(age: Int) -> User { User::Of(age) } }\n\
         fn f() -> User { User::new(3) }\n",
    );
}

/// The same function, checked like any other.
#[test]
fn a_function_reached_by_path_checks_its_arguments() {
    assert_reports(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         impl User { fn new(age: Int) -> User { User::Of(age) } }\n\
         fn f() -> User { User::new(true) }\n",
        "Bool",
    );
}

/// Naming a type and then something it does not have is an error about that,
/// not about traits — `User` is not one.
#[test]
fn a_function_the_type_does_not_have_is_reported_by_name() {
    assert_reports(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         impl User { fn new(age: Int) -> User { User::Of(age) } }\n\
         fn f() -> User { User::make(3) }\n",
        "`make`",
    );
}

/// A type's own method wins over a trait's. Adding a trait to a program must
/// not silently change what an existing call does.
#[test]
fn an_inherent_method_shadows_a_trait_method_of_the_same_name() {
    assert_clean(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         export trait Show { fn show(self) -> Int; }\n\
         impl Show for User { fn show(self) -> Int { 1 } }\n\
         impl User { fn show(self) -> Int { 2 } }\n\
         fn f(u: User) -> Int { u.show() }\n",
    );
}

/// A type can have its own methods and implement traits at the same time.
#[test]
fn inherent_methods_and_trait_impls_coexist() {
    assert_clean(
        "module m;\n\
         export type User = | Of(age: Int);\n\
         export trait Show { fn show(self) -> Int; }\n\
         impl Show for User { fn show(self) -> Int { 1 } }\n\
         impl User { fn age(self) -> Int { 2 } }\n\
         fn f(u: User) -> Int { u.show() + u.age() }\n",
    );
}

/// An inherent impl over a constructor learns its parameter from the receiver,
/// the same way a parameterised trait impl does.
#[test]
fn a_parameterised_inherent_impl_is_allowed() {
    assert_clean(
        "module m;\n\
         export type Wrapper<A> = | Of(value: A);\n\
         impl<A> Wrapper<A> { fn size(self) -> Int { 1 } }\n\
         fn f(b: Wrapper<Int>) -> Int { b.size() }\n",
    );
}

// --- higher-kinded unification --------------------------------------------

const FUNCTOR: &str = "module m;\n\
                       export type Option<A> = | Some(value: A) | None;\n\
                       export type Wrapper<A> = | Of(value: A);\n\
                       export trait Functor {\n\
                         fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;\n\
                       }\n\
                       impl Functor for Option {\n\
                         fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> {\n\
                           match self {\n\
                             Option::Some(v) => Option::Some(f(v)),\n\
                             Option::None => Option::None,\n\
                           }\n\
                         }\n\
                       }\n";

/// Calling a higher-kinded method has to solve `Self<A>` against `Option<Int>`,
/// deciding `Self := Option` and `A := Int` separately. Getting this wrong the
/// obvious way binds `Self := Option<Int>` and nothing type checks afterwards.
#[test]
fn a_higher_kinded_method_solves_the_constructor_and_the_argument() {
    assert_clean(&format!(
        "{FUNCTOR}fn f(o: Option<Int>) -> Option<Bool> {{ o.map(fn x => x == 1) }}\n"
    ));
}

#[test]
fn the_result_keeps_the_receivers_constructor() {
    assert_reports(
        &format!("{FUNCTOR}fn f(o: Option<Int>) -> Wrapper<Bool> {{ o.map(fn x => x == 1) }}\n"),
        "returns `Wrapper<Bool>`, but its body has type `Option<Bool>`",
    );
}

#[test]
fn the_result_element_type_comes_from_the_function() {
    assert_reports(
        &format!("{FUNCTOR}fn f(o: Option<Int>) -> Option<Int> {{ o.map(fn x => x == 1) }}\n"),
        "`Int` does not match `Bool`",
    );
}

/// The element type flows the other way too: the lambda's parameter is decided
/// by the receiver, not annotated.
#[test]
fn the_lambda_parameter_is_decided_by_the_receiver() {
    assert_reports(
        &format!("{FUNCTOR}fn f(o: Option<Int>) -> Option<Bool> {{ o.map(fn x => x && true) }}\n"),
        "`Bool`",
    );
}

/// A trait whose `Self` is applied cannot be reached from a type that is not a
/// constructor, and the error should say so rather than failing later.
#[test]
fn a_higher_kinded_method_is_not_available_on_a_plain_type() {
    assert_reports(
        &format!("{FUNCTOR}fn f(n: Int) -> Int {{ n.map(fn x => x) }}\n"),
        "does not implement `Functor`",
    );
}

/// A second constructor implementing the same trait keeps its own identity.
#[test]
fn two_constructors_may_implement_one_higher_kinded_trait() {
    assert_clean(
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         export type Wrapper<A> = | Of(value: A);\n\
         export trait Functor {\n\
           fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;\n\
         }\n\
         impl Functor for Option {\n\
           fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> { Option::None }\n\
         }\n\
         impl Functor for Wrapper {\n\
           fn map<A, B>(self: Wrapper<A>, f: (A) -> B) -> Wrapper<B> {\n\
             match self { Wrapper::Of(v) => Wrapper::Of(f(v)) }\n\
           }\n\
         }\n\
         fn a(o: Option<Int>) -> Option<Bool> { o.map(fn x => x == 1) }\n\
         fn b(x: Wrapper<Int>) -> Wrapper<Bool> { x.map(fn y => y == 1) }\n",
    );
}

/// An `Applicative` whose receiver is `Self<(A) -> B>` — a constructor applied
/// to a function type — is the shape `traverse` needs.
#[test]
fn a_constructor_applied_to_a_function_type_unifies() {
    assert_clean(
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         export trait Applicative {\n\
           fn ap<A, B>(self: Self<(A) -> B>, value: Self<A>) -> Self<B>;\n\
         }\n\
         impl Applicative for Option {\n\
           fn ap<A, B>(self: Option<(A) -> B>, value: Option<A>) -> Option<B> {\n\
             Option::None\n\
           }\n\
         }\n\
         fn f(g: Option<(Int) -> Bool>, v: Option<Int>) -> Option<Bool> { g.ap(v) }\n",
    );
}

// --- trait functions with no receiver -------------------------------------

const PURE: &str = "module m;\n\
                    export type Option<A> = | Some(value: A) | None;\n\
                    export type Wrapper<A> = | Of(value: A);\n\
                    export trait Applicative {\n\
                      fn pure<A>(value: A) -> Self<A>;\n\
                    }\n\
                    impl Applicative for Option {\n\
                      fn pure<A>(value: A) -> Option<A> { Option::Some(value) }\n\
                    }\n\
                    impl Applicative for Wrapper {\n\
                      fn pure<A>(value: A) -> Wrapper<A> { Wrapper::Of(value) }\n\
                    }\n";

/// `Applicative::pure(x)` has no receiver to select an impl from, so the
/// expected type decides — the same way `Default::default()` reads elsewhere.
#[test]
fn a_trait_function_without_a_receiver_is_chosen_by_the_expected_type() {
    assert_clean(&format!("{PURE}fn f() -> Option<Int> {{ Applicative::pure(1) }}\n"));
    assert_clean(&format!("{PURE}fn f() -> Wrapper<Int> {{ Applicative::pure(1) }}\n"));
}

#[test]
fn a_trait_function_still_checks_its_argument() {
    assert_reports(
        &format!("{PURE}fn f() -> Option<Bool> {{ Applicative::pure(1) }}\n"),
        "returns `Option<Bool>`",
    );
}

/// Through a bounded parameter the caller chooses, not the expected type.
#[test]
fn a_trait_function_reached_through_a_bound_uses_that_parameter() {
    assert_clean(&format!(
        "{PURE}fn wrap<F: Applicative, A>(value: A) -> F<A> {{ F::pure(value) }}\n"
    ));
}

#[test]
fn a_parameter_without_the_bound_cannot_reach_the_function() {
    assert_reports(
        &format!("{PURE}fn wrap<F, A>(value: A) -> F<A> {{ F::pure(value) }}\n"),
        "is not a trait with a function named `pure`",
    );
}

#[test]
fn a_function_the_trait_does_not_have_is_reported() {
    assert_reports(
        &format!("{PURE}fn f() -> Option<Int> {{ Applicative::nope(1) }}\n"),
        "`Applicative` is not a trait with a function named `nope`",
    );
}

/// A method on a value of type `F<B>`, where `F` is a bounded parameter, has
/// only the methods `F`'s bounds promise. This is what makes the body of a
/// generic `traverse` typecheck at all.
#[test]
fn a_method_on_a_bounded_application_resolves_through_the_bound() {
    assert_clean(
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         export trait Applicative {\n\
           fn pure<A>(value: A) -> Self<A>;\n\
           fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;\n\
         }\n\
         impl Applicative for Option {\n\
           fn pure<A>(value: A) -> Option<A> { Option::Some(value) }\n\
           fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> { Option::None }\n\
         }\n\
         fn twice<F: Applicative, A>(x: F<A>, f: (A) -> A) -> F<A> { x.map(f).map(f) }\n",
    );
}

#[test]
fn an_unbounded_application_has_no_methods() {
    assert_reports(
        "module m;\n\
         export trait Applicative { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         fn twice<F, A>(x: F<A>, f: (A) -> A) -> F<A> { x.map(f) }\n",
        "add one, as `F: Trait`",
    );
}

// --- associated types -----------------------------------------------------

const ITER: &str = "module m;\n\
                    export type Step<S, A> = | Yield(state: S, item: A) | Done;\n\
                    export type Range = | Of(from: Int, to: Int);\n\
                    export trait Iterator {\n\
                      type Item;\n\
                      fn next(self) -> Step<Self, Self::Item>;\n\
                    }\n\
                    impl Iterator for Range {\n\
                      type Item = Int;\n\
                      fn next(self) -> Step<Range, Int> { Step::Done }\n\
                    }\n";

/// The projection has to normalize, or every use of `Self::Item` is an opaque
/// name that unifies with nothing and reports itself in diagnostics.
#[test]
fn an_associated_type_projects_to_the_impls_binding() {
    assert_clean(&format!(
        "{ITER}fn first(r: Range) -> Int {{\n\
           match r.next() {{ Step::Yield(rest, item) => item, Step::Done => 0 }}\n\
         }}\n"
    ));
}

#[test]
fn a_projection_is_the_bound_type_not_a_fresh_one() {
    assert_reports(
        &format!(
            "{ITER}fn first(r: Range) -> Bool {{\n\
               match r.next() {{ Step::Yield(rest, item) => item, Step::Done => true }}\n\
             }}\n"
        ),
        "expected `Int`, found `Bool`",
    );
}

/// An impl's parameters have to be read off the receiver first, or
/// `List<Int>::Item` projects to a rigid `A` instead of to `Int`.
#[test]
fn a_projection_through_a_parameterised_impl_substitutes_first() {
    assert_clean(
        "module m;\n\
         export type Step<S, A> = | Yield(state: S, item: A) | Done;\n\
         export type List<A> = | Nil | Cons(head: A, tail: List<A>);\n\
         export trait Iterator {\n\
           type Item;\n\
           fn next(self) -> Step<Self, Self::Item>;\n\
         }\n\
         impl<A> Iterator for List<A> {\n\
           type Item = A;\n\
           fn next(self) -> Step<List<A>, A> { Step::Done }\n\
         }\n\
         fn first(l: List<Int>) -> Int {\n\
           match l.next() { Step::Yield(rest, item) => item, Step::Done => 0 }\n\
         }\n",
    );
}

/// Inside a generic function the owner is still a parameter, so the projection
/// stays rigid: the body cannot assume what the caller's `Item` will be.
#[test]
fn an_unresolved_projection_is_rigid() {
    assert_reports(
        &format!(
            "{ITER}fn first<I: Iterator>(it: I) -> Int {{\n\
               match it.next() {{ Step::Yield(rest, item) => item, Step::Done => 0 }}\n\
             }}\n"
        ),
        "I::Item",
    );
}

// --- an impl must match what its trait promised ---------------------------

/// Without this an impl could promise `Bool` and return `Int`, and the
/// mismatch would surface as invalid LLVM IR blamed on the compiler.
#[test]
fn an_impl_returning_the_wrong_type_is_rejected() {
    assert_reports(
        "module m;\n\
         export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         impl Eq for Int { fn eq(self, other: Int) -> Int { 1 } }\n",
        "`eq` returns `Int` here, but `Eq` declares `Bool`",
    );
}

#[test]
fn an_impl_taking_the_wrong_parameter_is_rejected() {
    assert_reports(
        "module m;\n\
         export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         impl Eq for Int { fn eq(self, other: Bool) -> Bool { true } }\n",
        "parameter 2 of `eq` is `Bool` here, but `Eq` declares `Int`",
    );
}

#[test]
fn an_impl_with_the_wrong_arity_is_rejected() {
    assert_reports(
        "module m;\n\
         export trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         impl Eq for Int { fn eq(self) -> Bool { true } }\n",
        "`eq` takes 2 parameter(s) in `Eq`, but this impl declares 1",
    );
}

/// The check compares through the associated type, so an impl whose method
/// disagrees with its own `type Item` is caught.
#[test]
fn an_impl_disagreeing_with_its_own_associated_type_is_rejected() {
    assert_reports(
        "module m;\n\
         export type Step<S, A> = | Yield(state: S, item: A) | Done;\n\
         export type Range = | Of(from: Int, to: Int);\n\
         export trait Iterator {\n\
           type Item;\n\
           fn next(self) -> Step<Self, Self::Item>;\n\
         }\n\
         impl Iterator for Range {\n\
           type Item = Int;\n\
           fn next(self) -> Step<Range, Bool> { Step::Done }\n\
         }\n",
        "`next` returns `Step<Range, Bool>` here, but `Iterator` declares `Step<Range, Int>`",
    );
}

/// Renaming a method's own parameters is allowed: `fn map<X, Y>` implements
/// `fn map<A, B>`. Comparing by name rather than by position would reject it.
#[test]
fn an_impl_may_rename_the_methods_own_parameters() {
    assert_clean(
        "module m;\n\
         export type Option<A> = | Some(value: A) | None;\n\
         export trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Option {\n\
           fn map<X, Y>(self: Option<X>, f: (X) -> Y) -> Option<Y> { Option::None }\n\
         }\n",
    );
}

// --- projecting off a type variable (D3) -----------------------------------

const EXTRACT: &str = "module m;\n\
                       export type Text = | Of(s: String);\n\
                       export type Num = | Of(n: Int);\n\
                       export type TextSpec = | Of;\n\
                       export type NumSpec = | Of;\n\
                       export trait Extract { type Spec; fn spec() -> Self::Spec; }\n\
                       impl Extract for Text \
                         { type Spec = TextSpec; fn spec() -> TextSpec { TextSpec::Of } }\n\
                       impl Extract for Num \
                         { type Spec = NumSpec; fn spec() -> NumSpec { NumSpec::Of } }\n\
                       export fn extract<A: Extract>(spec: A::Spec) -> A;\n";

/// The shape D3 was named after. `?A::Spec ~ NumSpec` cannot be solved when it
/// is met — projection is not injective — so it waits for the return type to
/// say what `A` is.
#[test]
fn a_projection_waits_for_its_owner() {
    assert_clean(&format!("{EXTRACT}export fn f() -> Num {{ extract(Num::spec()) }}\n"));
}

/// A trait function reached through the type that implements it, which is how
/// the spec gets named at all.
#[test]
fn a_trait_function_is_reached_through_the_implementing_type() {
    assert_clean(&format!("{EXTRACT}export fn f() -> NumSpec {{ Num::spec() }}\n"));
}

/// Nothing settles `A`, and nothing later will.
#[test]
fn a_projection_nothing_settles_is_reported() {
    assert_reports(
        &format!("{EXTRACT}export fn f() -> Int {{ extract(Num::spec()); 0 }}\n"),
        "nothing here says which type this is projected from",
    );
}

/// The spec is one type's and the result is asked to be another's. Deferring
/// must not turn a real mismatch into a pass.
#[test]
fn a_projection_that_does_not_fit_is_still_reported() {
    assert_reports(
        &format!("{EXTRACT}export fn f() -> Text {{ extract(Num::spec()) }}\n"),
        "expected `TextSpec`, found `NumSpec`",
    );
}

/// A rigid owner is the case that stays an error: inside a generic body
/// `A::Spec` is opaque, and assuming what it becomes is the body deciding its
/// caller's type.
#[test]
fn a_projection_off_a_rigid_parameter_is_still_rigid() {
    assert_reports(
        &format!(
            "{EXTRACT}export fn f<A: Extract>(spec: A::Spec) -> A \
             {{ extract(NumSpec::Of) }}\n"
        ),
        "NumSpec",
    );
}
