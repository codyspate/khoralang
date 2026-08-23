//! The trailing `impl` clause on a type declaration.
//!
//! ```khora
//! export type Point = { x: Int, y: Int } impl Eq, Ord, Show;
//! ```
//!
//! `impl` rather than a word of its own: it already means "here are
//! implementations for this type", so the clause costs no keyword at all — not
//! even a contextual one — and reads as the short form of what it expands
//! into. Trailing, because Khora already puts a declaration's clauses after it
//! and because the name is what a reader is looking for.
//!
//! It was `derive(Eq, Ord)` on the line above, which is Rust's spelling. It
//! went because it was attribute-shaped in a language with no attributes.

use khora_syntax::ast::Decl;
use khora_syntax::{parse, CONTEXTUAL_KEYWORDS, KEYWORDS};

fn tree(source: &str) -> String {
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source, "did not round-trip");
    assert!(parsed.errors().is_empty(), "{source}\n{:?}", parsed.errors());
    parsed.debug_tree()
}

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn derived(source: &str) -> Vec<String> {
    parse(source)
        .source_file()
        .decls()
        .filter_map(|d| match d {
            Decl::Type(t) => t.derive_clause(),
            _ => None,
        })
        .flat_map(|c| c.traits().filter_map(|t| t.ident()).collect::<Vec<_>>())
        .collect()
}

#[test]
fn the_clause_belongs_to_the_type_it_follows() {
    let out = tree("module m;\nexport type Point = { x: Int, y: Int } impl Eq, Ord;\n");
    assert!(out.contains("DERIVE_CLAUSE"), "{out}");
    // Inside the declaration, not beside it: a reader of the tree asks the type
    // what it implements rather than looking at what came after it.
    let clause = out.find("DERIVE_CLAUSE").expect("a clause");
    let decl = out.find("TYPE_DECL").expect("a declaration");
    assert!(decl < clause, "the clause should be nested in the declaration\n{out}");
}

#[test]
fn the_traits_are_read_back_in_order() {
    assert_eq!(
        derived("module m;\ntype P = { x: Int } impl Eq, Ord, Show, Hash;\n"),
        vec!["Eq", "Ord", "Show", "Hash"]
    );
}

/// A variant type's clause comes after the last case, where the `;` was.
#[test]
fn a_variant_type_takes_the_clause_too() {
    assert_eq!(
        derived("module m;\ntype Shape =\n  | Circle(r: Int)\n  | Square(s: Int)\n  impl Eq, Show;\n"),
        vec!["Eq", "Show"]
    );
}

/// An opaque type has no body for the clause to follow, and still takes one.
#[test]
fn an_opaque_type_can_carry_a_clause() {
    assert_eq!(derived("module m;\nexport type Handle impl Eq;\n"), vec!["Eq"]);
}

#[test]
fn one_trait_needs_no_comma_and_a_trailing_one_is_allowed() {
    assert_eq!(derived("module m;\ntype P = { x: Int } impl Eq;\n"), vec!["Eq"]);
    assert_eq!(derived("module m;\ntype P = { x: Int } impl Eq, Ord,;\n"), vec!["Eq", "Ord"]);
}

/// The clause reuses a keyword the language already had, so nothing was added
/// to either list. `derive` in particular went back to being an ordinary word.
#[test]
fn the_clause_costs_no_keyword() {
    let out = tree(
        "module m;\n\
         type Rules = { derive: Int };\n\
         fn derive(derive: Int) -> Int { derive }\n\
         fn f() -> Int { derive(1) }\n",
    );
    assert!(!out.contains("DERIVE_KW"), "there is no such token any more\n{out}");
    assert!(!KEYWORDS.contains(&"derive"), "`derive` must not be a keyword");
    assert!(!CONTEXTUAL_KEYWORDS.contains(&"derive"), "`derive` must not be contextual either");
    assert!(KEYWORDS.contains(&"impl"), "`impl` is the keyword the clause spends");
}

/// A trait declaration's `impl` is not this one, and an ordinary `impl` block
/// still parses beside a type that carries a clause.
#[test]
fn a_clause_does_not_swallow_the_impl_block_after_it() {
    let out = tree(
        "module m;\n\
         type P = { x: Int } impl Eq;\n\
         impl P { fn get(self) -> Int { self.x } }\n",
    );
    assert!(out.contains("DERIVE_CLAUSE"), "{out}");
    assert!(out.contains("IMPL_DECL"), "the block after it is still a block\n{out}");
}

#[test]
fn a_clause_with_a_non_name_inside_is_reported() {
    let found = errors("module m;\ntype P = { x: Int } impl 1;\n");
    assert!(found.iter().any(|e| e.contains("name of a trait")), "got {found:?}");
}

/// The tree is lossless whatever the input.
#[test]
fn a_broken_clause_still_round_trips() {
    for source in [
        "module m;\ntype P = { x: Int } impl\n",
        "module m;\ntype P = { x: Int } impl ,;\n",
        "module m;\ntype P = { x: Int } impl Eq\n",
        "module m;\ntype P impl Eq Ord;\n",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source, "lost text for {source:?}");
    }
}
