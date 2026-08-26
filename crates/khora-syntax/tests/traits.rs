//! `trait` and `impl` syntax.
//!
//! The word is chosen in `docs/design/typeclasses.md` against the behavior it
//! has to predict: `interface` is the more familiar candidate and is structural
//! in both Go and TypeScript, while Khora's resolution is nominal.

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

/// `impl Type { .. }` is the inherent form, so two types with nothing between
/// them parse far enough to give a confusing error unless it is caught.
#[test]
fn an_impl_missing_for_between_two_types_is_reported_specifically() {
    let found = errors("module m;\nimpl Eq Int {\n}\n");
    assert!(
        found.iter().any(|e| e.contains("impl Eq for Int")),
        "expected the correct form to be shown, got {found:?}"
    );
}

/// A type's own methods need no trait at all.
#[test]
fn an_impl_without_for_declares_the_types_own_methods() {
    let out = tree("module m;\nimpl User {\n  fn age(self) -> Int { 1 }\n}\n");
    assert!(out.contains("IMPL_DECL"), "{out}");
    assert!(!out.contains("FOR_KW"), "an inherent impl has no `for`\n{out}");
}

#[test]
fn the_two_impl_forms_are_told_apart() {
    let inherent = parse("module m;\nimpl User {\n}\n").source_file().decls().next();
    let Some(Decl::Impl(i)) = inherent else { panic!("not an impl") };
    assert!(i.is_inherent());
    assert!(i.trait_().is_none(), "an inherent impl names no trait");
    assert!(i.self_type().is_some(), "the only type is the implementing one");

    let for_trait = parse("module m;\nimpl Eq for User {\n}\n").source_file().decls().next();
    let Some(Decl::Impl(t)) = for_trait else { panic!("not an impl") };
    assert!(!t.is_inherent());
    assert!(t.trait_().is_some());
    assert!(t.self_type().is_some());
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
