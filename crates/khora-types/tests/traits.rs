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

// --- qualified calls --------------------------------------------------------

/// **`Int::show(x)` did not resolve and `Decimal::show(x)` did.**
///
/// The impl search was gated on the map of types the program *declares*, so a
/// trait method could be called type-qualified on a user type and not on a
/// builtin — with the same impl written the same way, three hundred lines
/// apart in the same file. `Int`, `Bool`, `String` and the fixed-width
/// integers have no declaration to be in that map, and a caller has no way to
/// know which side of the line a type falls on.
#[test]
fn a_trait_method_resolves_type_qualified_on_a_builtin() {
    assert_clean(&format!("{SHOW}fn f() -> String {{ Int::show(1) }}\n"));
}

/// And on a declared type, which is the half that always worked and must keep
/// working: the search is by the impl's own head, so both reach it the same
/// way.
#[test]
fn a_trait_method_resolves_type_qualified_on_a_declared_type() {
    assert_clean(
        "module m;\n\
         pub trait Show { fn show(self) -> String; }\n\
         pub type Money = { units: Int };\n\
         impl Show for Money { fn show(self) -> String { \"m\" } }\n\
         fn f(m: Money) -> String { Money::show(m) }\n",
    );
}

/// **A type asked for a function it has not got is a type**, and the message
/// now says so.
///
/// It used to read "`U8` is not a trait with a function named `show`", which
/// answers a question the caller did not ask — and for a builtin it was the
/// only thing they were told.
#[test]
fn a_type_without_the_function_is_not_told_it_is_not_a_trait() {
    let found = errors(&format!("{SHOW}fn f() -> String {{ Int::nope(1) }}\n"));
    assert!(
        found.iter().any(|e| e.contains("`Int` has no function named `nope`")),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|e| e.contains("is not a trait")),
        "the old wording is gone: {found:?}"
    );
}

/// **And a name that is nothing at all is still caught**, one step earlier and
/// with a better message than either wording above.
///
/// Worth pinning because the wording change is on the checker's path, and this
/// says that path is not the one an unknown name takes: resolution refuses it
/// before the checker ever asks what `gone` is.
#[test]
fn a_name_that_is_nothing_at_all_is_refused_by_resolution() {
    let found = errors("module m;\nfn f() -> Int { Nowhere::gone(1) }\n");
    assert!(
        found.iter().any(|e| e.contains("cannot resolve `Nowhere::gone`")),
        "{found:?}"
    );
}

/// **`Ord::cmp(a, b)` from a module that imports `Ord`.**
///
/// The owner of a `::` path had to name a *type*, so `std::core` could write
/// `Eq::eq(head, wanted)` — the trait is declared in that file, and a declared
/// trait is an item there — while a module one file away was told it could not
/// resolve `Ord::cmp`. The same call, refused for being imported rather than
/// declared.
#[test]
fn a_trait_method_resolves_trait_qualified() {
    assert_clean(&format!("{SHOW}fn f() -> String {{ Show::show(1) }}\n"));
}

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

// --- a type's own methods -------------------------------------------------

const USER: &str = "module m;\n\
                    pub type User = | Of(age: Int);\n\
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
         pub type User = | Of(age: Int);\n\
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
         pub type User = | Of(age: Int);\n\
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
         pub type User = | Of(age: Int);\n\
         impl User { fn new(age: Int) -> User { User::Of(age) } }\n\
         fn f() -> User { User::new(3) }\n",
    );
}

/// The same function, checked like any other.
#[test]
fn a_function_reached_by_path_checks_its_arguments() {
    assert_reports(
        "module m;\n\
         pub type User = | Of(age: Int);\n\
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
         pub type User = | Of(age: Int);\n\
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
         pub type User = | Of(age: Int);\n\
         pub trait Show { fn show(self) -> Int; }\n\
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
         pub type User = | Of(age: Int);\n\
         pub trait Show { fn show(self) -> Int; }\n\
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
         pub type Wrapper<A> = | Of(value: A);\n\
         impl<A> Wrapper<A> { fn size(self) -> Int { 1 } }\n\
         fn f(b: Wrapper<Int>) -> Int { b.size() }\n",
    );
}

// --- higher-kinded unification --------------------------------------------

const FUNCTOR: &str = "module m;\n\
                       pub type Option<A> = | Some(value: A) | None;\n\
                       pub type Wrapper<A> = | Of(value: A);\n\
                       pub trait Functor {\n\
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
         pub type Option<A> = | Some(value: A) | None;\n\
         pub type Wrapper<A> = | Of(value: A);\n\
         pub trait Functor {\n\
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
         pub type Option<A> = | Some(value: A) | None;\n\
         pub trait Applicative {\n\
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
                    pub type Option<A> = | Some(value: A) | None;\n\
                    pub type Wrapper<A> = | Of(value: A);\n\
                    pub trait Applicative {\n\
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
         pub type Option<A> = | Some(value: A) | None;\n\
         pub trait Applicative {\n\
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
         pub trait Applicative { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         fn twice<F, A>(x: F<A>, f: (A) -> A) -> F<A> { x.map(f) }\n",
        "add one, as `F: Trait`",
    );
}

// --- associated types -----------------------------------------------------

const ITER: &str = "module m;\n\
                    pub type Step<S, A> = | Yield(state: S, item: A) | Done;\n\
                    pub type Range = | Of(from: Int, to: Int);\n\
                    pub trait Iterator {\n\
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
         pub type Step<S, A> = | Yield(state: S, item: A) | Done;\n\
         pub type List<A> = | Nil | Cons(head: A, tail: List<A>);\n\
         pub trait Iterator {\n\
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
         pub trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         impl Eq for Int { fn eq(self, other: Int) -> Int { 1 } }\n",
        "`eq` returns `Int` here, but `Eq` declares `Bool`",
    );
}

#[test]
fn an_impl_taking_the_wrong_parameter_is_rejected() {
    assert_reports(
        "module m;\n\
         pub trait Eq { fn eq(self, other: Self) -> Bool; }\n\
         impl Eq for Int { fn eq(self, other: Bool) -> Bool { true } }\n",
        "parameter 2 of `eq` is `Bool` here, but `Eq` declares `Int`",
    );
}

#[test]
fn an_impl_with_the_wrong_arity_is_rejected() {
    assert_reports(
        "module m;\n\
         pub trait Eq { fn eq(self, other: Self) -> Bool; }\n\
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
         pub type Step<S, A> = | Yield(state: S, item: A) | Done;\n\
         pub type Range = | Of(from: Int, to: Int);\n\
         pub trait Iterator {\n\
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
         pub type Option<A> = | Some(value: A) | None;\n\
         pub trait Functor { fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>; }\n\
         impl Functor for Option {\n\
           fn map<X, Y>(self: Option<X>, f: (X) -> Y) -> Option<Y> { Option::None }\n\
         }\n",
    );
}

// --- projecting off a type variable (D3) -----------------------------------

const EXTRACT: &str = "module m;\n\
                       pub type Text = | Of(s: String);\n\
                       pub type Num = | Of(n: Int);\n\
                       pub type TextSpec = | Of;\n\
                       pub type NumSpec = | Of;\n\
                       pub trait Extract { type Spec; fn spec() -> Self::Spec; }\n\
                       impl Extract for Text \
                         { type Spec = TextSpec; fn spec() -> TextSpec { TextSpec::Of } }\n\
                       impl Extract for Num \
                         { type Spec = NumSpec; fn spec() -> NumSpec { NumSpec::Of } }\n\
                       pub fn extract<A: Extract>(spec: A::Spec) -> A;\n";

/// The shape D3 was named after. `?A::Spec ~ NumSpec` cannot be solved when it
/// is met — projection is not injective — so it waits for the return type to
/// say what `A` is.
#[test]
fn a_projection_waits_for_its_owner() {
    assert_clean(&format!("{EXTRACT}pub fn f() -> Num {{ extract(Num::spec()) }}\n"));
}

/// A trait function reached through the type that implements it, which is how
/// the spec gets named at all.
#[test]
fn a_trait_function_is_reached_through_the_implementing_type() {
    assert_clean(&format!("{EXTRACT}pub fn f() -> NumSpec {{ Num::spec() }}\n"));
}

/// Nothing settles `A`, and nothing later will.
#[test]
fn a_projection_nothing_settles_is_reported() {
    assert_reports(
        &format!("{EXTRACT}pub fn f() -> Int {{ extract(Num::spec()); 0 }}\n"),
        "nothing here says which type this is projected from",
    );
}

/// The spec is one type's and the result is asked to be another's. Deferring
/// must not turn a real mismatch into a pass.
#[test]
fn a_projection_that_does_not_fit_is_still_reported() {
    assert_reports(
        &format!("{EXTRACT}pub fn f() -> Text {{ extract(Num::spec()) }}\n"),
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
            "{EXTRACT}pub fn f<A: Extract>(spec: A::Spec) -> A \
             {{ extract(NumSpec::Of) }}\n"
        ),
        "NumSpec",
    );
}

// --- more than one bound ---------------------------------------------------
//
// `T: Ord + Show` used to leave `T` with *no* bounds at all. The bound parser
// called the general type parser, to which `+` is the union that writes an
// error row, so both traits came back as one `UNION_TYPE`; `bound_names` only
// understands a path, so it yielded nothing. The result was every method of
// both traits reported missing, under a diagnostic reading "add the bound, as
// `T: Ord`" about a signature that already said exactly that.

const TWO: &str = "module m;\n\
                   pub trait Show { fn show(self) -> String; }\n\
                   pub trait Tag { fn tag(self) -> Int; }\n\
                   impl Show for Int { fn show(self) -> String { \"i\" } }\n\
                   impl Tag for Int { fn tag(self) -> Int { 1 } }\n";

#[test]
fn two_bounds_are_both_kept() {
    assert_clean(&format!(
        "{TWO}pub fn both<T: Show + Tag>(v: T) -> String {{ v.show() }}\n"
    ));
}

/// The second one is not swallowed by the first.
#[test]
fn the_later_bound_is_kept_too() {
    assert_clean(&format!(
        "{TWO}pub fn both<T: Show + Tag>(v: T) -> Int {{ v.tag() }}\n"
    ));
}

/// And a method of a trait that was *not* named is still refused, so the fix
/// did not turn the bound list into "everything".
#[test]
fn a_trait_left_out_of_the_bounds_is_still_missing() {
    assert_reports(
        &format!("{TWO}pub fn one<T: Show>(v: T) -> Int {{ v.tag() }}\n"),
        // The message names the bounds the parameter does have, which is only
        // worth reading now that the list is accurate.
        "no method `tag` on `T`, whose bounds are `Show`",
    );
}

/// Three, to check the loop rather than the pair.
#[test]
fn three_bounds_are_all_kept() {
    assert_clean(
        "module m;\n\
         pub trait A { fn a(self) -> Int; }\n\
         pub trait B { fn b(self) -> Int; }\n\
         pub trait C { fn c(self) -> Int; }\n\
         pub fn all<T: A + B + C>(v: T) -> Int { v.a() + v.b() + v.c() }\n",
    );
}

/// A supertrait list is the same production, so it had the same hole.
#[test]
fn two_supertraits_are_both_kept() {
    assert_clean(&format!(
        "{TWO}\
         pub trait Both: Show + Tag {{ fn both(self) -> Int; }}\n\
         pub fn use_it<T: Both>(v: T) -> Int {{ v.tag() }}\n"
    ));
}

/// A tiny `std::core`: a trait, an impl on a builtin, and a bounded function.
///
/// Named `std::core` because that is what the rule keys on -- a builtin's
/// impls arrive from there without an import, the same way its inherent
/// methods do.
const CORE: &str = "module std::core;

pub trait Ord {
  fn cmp(self, other: Self) -> Int;
}

impl Ord for String {
  fn cmp(self, other: String) -> Int { 0 }
}

pub fn ranked<A: Ord>(left: A, right: A) -> Int { Ord::cmp(left, right) }
";

/// The diagnostics of `user`, with the little `std::core` above beside it.
fn errors_beside_core(user: &str) -> Vec<String> {
    let db = khora_db::KhoraDatabase::new();
    let core = SourceFile::new(&db, "core.kh".into(), CORE.to_string());
    let user = SourceFile::new(&db, "user.kh".into(), user.to_string());
    khora_db::SourceRoot::new(&db, vec![core, user]);
    diagnostics(&db, user).iter().map(|e| e.message.clone()).collect()
}

/// **`String` implements `Ord` whether or not the file imported `Ord`.**
///
/// An impl reaches a module with its trait or with its type, and both routes
/// miss this one: `String` is a builtin, so no `import` line mentions it, and
/// that left importing `Ord` as the only way. A file that used a bounded
/// function on a `String` without naming `Ord` was told
///
///     `String` does not implement `Ord`, which `ranked` requires
///
/// about a type that has implemented it since `std::core` was written.
///
/// Worse than a wrong message: adding the import fixes it, and then
/// `unused-import` reports `Ord` as unused -- satisfying a bound is not a use
/// the lint counts -- so following the compiler's own advice puts the error
/// back. Two people hit that loop independently. Errata 58 made the same
/// argument for a builtin's *inherent* methods; this is the trait half.
#[test]
fn a_builtins_impls_arrive_without_an_import() {
    let found = errors_beside_core(
        "module app;\nimport std::core::{ranked};\nfn f() -> Int { ranked(\"a\", \"b\") }\n",
    );
    assert!(found.is_empty(), "expected no errors, got {found:?}");
}

/// The same, with the trait imported, which always worked and must keep to.
#[test]
fn importing_the_trait_as_well_changes_nothing() {
    let found = errors_beside_core(
        "module app;\nimport std::core::{Ord, ranked};\nfn f() -> Int { ranked(\"a\", \"b\") }\n",
    );
    assert!(found.is_empty(), "expected no errors, got {found:?}");
}

/// **And a type that really has no impl is still caught** -- which is the
/// other half of the same gap, and the one that reached code generation.
///
/// The trait definition travels with the impls now. Without it `check_bounds`
/// skipped the question entirely, because a trait it does not know is one it
/// declines to report on -- so a missing impl went unreported until lowering
/// said "`Ord::cmp` has no body", pointing past the end of the file.
#[test]
fn a_type_with_no_impl_is_still_refused() {
    let found = errors_beside_core(
        "module app;\n\
         import std::core::{ranked};\n\
         pub type Colour = | Red | Green;\n\
         fn f() -> Int { ranked(Colour::Red, Colour::Green) }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("`Colour` does not implement `Ord`")),
        "a missing impl must be caught here, not at lowering: {found:?}"
    );
}

/// **And the message does not show the compiler's own punctuation.**
///
/// A signature key separates the trait from the type with a `#`, and an
/// inherent impl has no trait -- so `Dict::insert` was rendered
/// `#Dict::insert`. The `#` means nothing outside the compiler, and a message
/// that shows it asks somebody to know how declarations are stored in order to
/// read a sentence about their own program.
#[test]
fn a_bound_message_names_the_function_the_way_it_is_written() {
    let found = errors_beside_core(
        "module app;\n\
         import std::core::{ranked};\n\
         pub type Colour = | Red | Green;\n\
         fn f() -> Int { ranked(Colour::Red, Colour::Green) }\n",
    );
    assert!(
        found.iter().all(|e| !e.contains('#')),
        "no message may carry a `#`-prefixed key: {found:?}"
    );
}
