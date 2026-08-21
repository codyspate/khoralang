//! `trait` and `impl` syntax.
//!
//! The spelling is Rust's, decided in `docs/design/typeclasses.md`: the concept
//! is Rust's trait, so it gets Rust's word rather than Haskell's `class`.

use khora_syntax::ast::Decl;
use khora_syntax::parse;

fn tree(source: &str) -> String {
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source, "did not round-trip");
    assert!(parsed.errors().is_empty(), "{source}\n{:?}", parsed.errors());
    parsed.debug_tree()
}

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn decls(source: &str) -> Vec<Decl> {
    parse(source).source_file().decls().collect()
}

#[test]
fn a_trait_declares_functions() {
    let out = tree("module m;\npub trait Eq {\n  fn eq(self, other: Self) -> Bool;\n}\n");
    assert!(out.contains("TRAIT_DECL"), "{out}");
    assert!(out.contains("TRAIT_KW"), "{out}");
    assert!(out.contains("FN_DECL"), "{out}");
}

#[test]
fn a_supertrait_is_a_bound_on_the_trait() {
    let out = tree("module m;\ntrait Ord: Eq {\n  fn cmp(self, other: Self) -> Int;\n}\n");
    assert!(out.contains("TYPE_BOUNDS"), "{out}");
}

#[test]
fn an_impl_names_the_trait_then_the_type() {
    let out = tree("module m;\nimpl Eq for Int {\n  fn eq(self, o: Int) -> Bool { true }\n}\n");
    assert!(out.contains("IMPL_DECL"), "{out}");
    assert!(out.contains("FOR_KW"), "{out}");
}

/// The order is `impl Trait for Type`, so omitting `for` has to say which half
/// is missing rather than reporting a stray type.
#[test]
fn an_impl_without_for_is_reported_specifically() {
    let found = errors("module m;\nimpl Eq Int {\n}\n");
    assert!(
        found.iter().any(|e| e.contains("impl Eq for Int")),
        "expected the correct form to be shown, got {found:?}"
    );
}

#[test]
fn a_trait_declares_associated_types() {
    let out = tree("module m;\ntrait Iterator {\n  type Item;\n  fn next(self) -> Int;\n}\n");
    assert!(out.contains("ASSOC_TYPE_DECL"), "{out}");
}

#[test]
fn an_impl_assigns_an_associated_type() {
    let out = tree("module m;\nimpl Iterator for Range {\n  type Item = Int;\n}\n");
    assert!(out.contains("ASSOC_TYPE_DECL"), "{out}");
}

/// Higher kinds need no notation of their own: `Self` simply takes arguments.
#[test]
fn a_higher_kinded_trait_applies_self() {
    let out = tree(
        "module m;\ntrait Functor {\n  fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;\n}\n",
    );
    assert!(out.contains("TRAIT_DECL"), "{out}");
    assert!(out.contains("TYPE_ARGS"), "expected `Self<A>` to parse as an application\n{out}");
}

#[test]
fn bounds_are_plus_separated() {
    let out = tree("module m;\nfn f<T: Eq + Ord + Show>(x: T) -> T { x }\n");
    assert!(out.contains("TYPE_BOUNDS"), "{out}");
    assert_eq!(out.matches("TYPE_BOUNDS").count(), 1, "one node whatever the count\n{out}");
}

#[test]
fn a_single_bound_uses_the_same_node_as_several() {
    let out = tree("module m;\nfn f<T: Eq>(x: T) -> T { x }\n");
    assert!(out.contains("TYPE_BOUNDS"), "{out}");
}

/// A const parameter's `:` introduces the type of a value, not a bound.
#[test]
fn a_const_parameter_is_not_bounded() {
    let out = tree("module m;\nfn f<const N: Int>(x: Int) -> Int { x }\n");
    assert!(!out.contains("TYPE_BOUNDS"), "`const N: Int` is not a bound\n{out}");
}

#[test]
fn traits_and_impls_are_declarations() {
    let found = decls("module m;\ntrait A {\n}\nimpl A for Int {\n}\n");
    assert!(matches!(found.first(), Some(Decl::Trait(_))), "{found:?}");
    assert!(matches!(found.get(1), Some(Decl::Impl(_))), "{found:?}");
}

/// `trait` and `impl` are hard keywords, so a program cannot use them as names.
#[test]
fn trait_and_impl_are_reserved() {
    assert!(!errors("module m;\nfn f() -> Int { let trait = 1; trait }\n").is_empty());
    assert!(!errors("module m;\nfn f() -> Int { let impl = 1; impl }\n").is_empty());
}

#[test]
fn a_trait_body_recovers_from_a_stray_item() {
    // The `let` is not allowed here, but the `fn` after it must still parse.
    let found = errors("module m;\ntrait A {\n  let x = 1;\n  fn f(self) -> Int;\n}\n");
    assert!(!found.is_empty(), "expected a diagnostic");
    let out = parse("module m;\ntrait A {\n  let x = 1;\n  fn f(self) -> Int;\n}\n").debug_tree();
    assert!(out.contains("FN_DECL"), "recovery should reach the `fn`\n{out}");
}
