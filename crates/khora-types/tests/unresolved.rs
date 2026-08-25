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
