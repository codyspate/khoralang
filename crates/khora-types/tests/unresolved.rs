//! A type name that names nothing is reported where it is written.
//!
//! Unresolved *value* names have always been reported — `cannot find x in this
//! scope`. Type names were not: `named_type` turned a name nothing declares
//! into a nominal type distinct from everything else, and the comment there
//! said the mistake was "already an error where the name was resolved", which
//! was true for values and not for types. `fn f(x: Wibble) -> Int { 1 }`
//! type-checked clean.
//!
//! The consequence is mild-looking, which is how it survived. The invented
//! type does not unify with anything, so genuine mismatches were still caught —
//! reported as ``this function returns `Wibble`, but its body has type `Int` ``,
//! which reads as two real types disagreeing and sends the reader hunting for
//! the wrong thing. A typo in a signature is the ordinary way to meet it.
//!
//! Half of this file is the cases that must stay *quiet*. A diagnostic that
//! has never existed before earns its place by never being wrong.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_names_it(text: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains("cannot find type `Wibble`")),
        "expected the unresolved name to be reported, got {found:?}\n{text}"
    );
}

fn assert_quiet(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

#[test]
fn a_signature_that_names_nothing_is_reported() {
    assert_names_it("module m;\nfn f(x: Wibble) -> Int { 1 }\n");
    assert_names_it("module m;\nfn f() -> Wibble { 1 }\n");
}

#[test]
fn so_is_one_inside_a_body() {
    assert_names_it("module m;\nfn f() -> Int { let x: Wibble = 1; 2 }\n");
    assert_names_it("module m;\nfn f() -> Int { let g = fn (s: Wibble) => 1; g(2) }\n");
}

#[test]
fn so_is_one_in_a_declaration() {
    assert_names_it("module m;\ntype P = { a: Wibble }\n");
    assert_names_it("module m;\ntype P =\n  | Q(v: Wibble)\n  | R;\n");
    assert_names_it("module m;\ntype P = Wibble;\n");
}

/// Nested inside a type argument, where the outer name is fine.
#[test]
fn so_is_one_buried_in_a_type_argument() {
    assert_names_it("module m;\ntype B<A> = { a: A }\nfn f(x: B<Wibble>) -> Int { 1 }\n");
}

/// A bound is a type mention too, and this is the case that used to surface as
/// `no method hi on A, whose bounds are Wibble` — blaming the method for the
/// bound's problem, and reading as though `Wibble` were a real trait lacking a
/// method.
#[test]
fn a_bound_naming_no_trait_is_reported_as_the_bound() {
    assert_names_it("module m;\nfn f<A: Wibble>(x: A) -> A { x }\n");
}

// --- and everything that must stay quiet ----------------------------------

#[test]
fn the_builtins_are_not_reported() {
    assert_quiet(
        "module m;\n\
         fn f(a: Int, b: Bool, c: String, d: Float, e: U8, g: I16, h: ()) -> Int { 1 }\n",
    );
}

#[test]
fn a_type_parameter_is_in_scope_in_its_own_declaration() {
    assert_quiet("module m;\nfn f<A>(x: A) -> A { x }\n");
    assert_quiet("module m;\ntype B<A> = { a: A }\nimpl<A> B<A> { fn get(self) -> A { self.a } }\n");
    assert_quiet("module m;\ntype B<A> = { a: A }\nfn f(x: B<B<Int>>) -> Int { 1 }\n");
}

/// `Self` inside a trait, and `Self::Item` as a projection. The projection is
/// passed over rather than resolved — see the note in `unresolved.rs` on why
/// qualified names are left alone.
#[test]
fn self_and_its_projections_are_in_scope() {
    assert_quiet("module m;\ntrait Same { fn eqq(self, o: Self) -> Bool; }\n");
    assert_quiet("module m;\ntrait It { type Item; fn one(self) -> Self::Item; }\n");
}

#[test]
fn a_locally_declared_name_resolves() {
    assert_quiet("module m;\ntype P = { a: Int }\nfn f(x: P) -> Int { x.a }\n");
    assert_quiet("module m;\ntrait G { fn hi(self) -> Int; }\nfn f<A: G>(x: A) -> Int { x.hi() }\n");
    assert_quiet("module m;\ntype P =\n  | Q(Int, Bool)\n  | R;\n");
}

#[test]
fn structural_types_carry_no_name_to_resolve() {
    assert_quiet("module m;\nfn f(g: (Int) -> Bool) -> Bool { g(1) }\n");
    assert_quiet("module m;\nfn f(g: (Int, Bool)) -> Int { 1 }\n");
}

// --- and a name the *desugaring* needs ------------------------------------

/// A `for` loop with no `Step` in scope says so, at check time.
///
/// **The message existed and nobody printed it.** `Resolution::Unsupported`
/// carries the text and only the *backend* read it, so `khora check` reported
/// "`Int` has no method `next`" — pointing at a method call the desugaring
/// wrote and the programmer did not. That is exactly the "unresolved-name
/// error pointing at code nobody wrote" the message was written to replace,
/// and it was the one anybody actually saw.
///
/// The follow-on about `next` is still reported after it. That is the
/// established policy rather than an oversight: `reporting.rs` puts lowering
/// errors first because a name that did not resolve makes the type error
/// following it noise.
#[test]
fn a_for_loop_without_step_says_what_to_import() {
    // Both names, because the expansion needs both: `Step` for the `match` and
    // `Iterator` for the `next` it calls. Errata 58.
    const LOOP: &str = "module m;
fn f() -> Int { let mut t = 0; for x in 1..3 { t = t + x }; t }
";
    let found = errors(LOOP);
    assert!(
        found.iter().any(|e| e.contains("`for` needs `Step` and `Iterator` in scope")),
        "expected the import to be named, got {found:?}"
    );
    // Once for the pair. `Yield` and `Done` both missing is one mistake, and
    // saying it twice about one loop is worse than saying it once.
    let said = found.iter().filter(|e| e.contains("`for` needs `Step` and `Iterator`")).count();
    assert_eq!(said, 1, "said once, got {found:?}");
}
/// Parentheses around a type mean grouping, and used to mean "anything".
///
/// **`fn f(xs: List<(Int)>)` accepted a `List<String>`.** `type_of_syntax` has
/// an arm per type form and `_ => Type::Unknown` under them, and `Unknown`
/// unifies with everything — so a reader adding parentheses to clarify a type
/// switched off the checking of that position instead. Errata 60 named this
/// shape twice about two other matches; this is the third.
#[test]
fn parentheses_around_a_type_are_still_that_type() {
    let found = errors(
        "module m;\n\
         pub type List<A> = | Nil | Cons(head: A, tail: List<A>);\n\
         fn takes(xs: List<(Int)>) -> Int { 0 }\n\
         pub fn go() -> Int { let words: List<String> = List::Nil; takes(words) }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("`Int` does not match `String`")),
        "the parenthesised argument is still checked: {found:?}"
    );
}

/// A `+` union written where a type goes is reported rather than ignored.
///
/// `+` builds a row, which is what `raises` and `with` take. In a type
/// argument there is nothing for it to mean, and it used to mean "anything" —
/// `Result<Int, A + B>` accepted a `Result<Int, C>` for a `C` in neither arm.
#[test]
fn a_union_written_as_a_type_argument_is_refused() {
    let found = errors(
        "module m;\n\
         pub type Result<A, E> = | Ok(value: A) | Err(error: E);\n\
         type A = Int;\n\
         type B = Int;\n\
         fn hold(r: Result<Int, A + B>) -> Int { 0 }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("builds a `raises` or `with` row")),
        "the union is reported: {found:?}"
    );
}

/// And a `raises` row is exactly where it does belong.
#[test]
fn a_union_in_a_raises_clause_is_left_alone() {
    let found = errors(
        "module m;\n\
         type A = Int;\n\
         type B = Int;\n\
         fn fine() -> Int raises A + B { 1 }\n",
    );
    assert!(found.is_empty(), "a row where a row belongs: {found:?}");
}

/// An inline variant in a type position is reported, names and all.
///
/// The worst of the four, because nothing was checked at all: `Red` and `Blue`
/// are undeclared and the unresolved-name walk never saw them, since the whole
/// type had already become `Unknown`.
#[test]
fn a_variant_spelled_out_as_a_type_is_refused() {
    let found = errors(
        "module m;\n\
         fn colour(x: | Red | Blue) -> Int { 0 }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("declared with `type Name = | A | B`")),
        "the inline variant is reported: {found:?}"
    );
}

/// And a `type` declaration is exactly where one does belong.
#[test]
fn a_variant_in_a_declaration_is_left_alone() {
    let found = errors(
        "module m;\n\
         pub type Colour = | Red | Blue;\n\
         fn colour(c: Colour) -> Int { 0 }\n",
    );
    assert!(found.is_empty(), "a variant where a variant belongs: {found:?}");
}
