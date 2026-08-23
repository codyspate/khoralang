//! `derive(..)` syntax.
//!
//! Rust's word without Rust's `#[..]`. Khora has no attribute grammar and no
//! second use for one, so the clause stands on its own line above the
//! declaration — which is where a Rust reader already looks. `derive` is
//! contextual, so a program that already calls something `derive` keeps it.

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
fn a_derive_clause_belongs_to_the_type_it_introduces() {
    let out = tree("module m;\nderive(Eq, Ord)\nexport type Point = { x: Int, y: Int };\n");
    assert!(out.contains("DERIVE_CLAUSE"), "{out}");
    assert!(out.contains("DERIVE_KW"), "the word is remapped from IDENT\n{out}");
    // Inside the declaration, not beside it: a reader of the tree asks the
    // type what it derives rather than looking at what came before it.
    let clause = out.find("DERIVE_CLAUSE").expect("a clause");
    let decl = out.find("TYPE_DECL").expect("a declaration");
    assert!(decl < clause, "the clause should be nested in the declaration\n{out}");
}

#[test]
fn the_traits_are_read_back_in_order() {
    assert_eq!(
        derived("module m;\nderive(Eq, Ord, Show, Hash)\ntype P = { x: Int };\n"),
        vec!["Eq", "Ord", "Show", "Hash"]
    );
}

#[test]
fn a_derive_comes_before_export() {
    let out = tree("module m;\nderive(Eq)\nexport type P = { x: Int };\n");
    assert!(out.contains("DERIVE_CLAUSE"), "{out}");
    assert!(out.contains("EXPORT_KW"), "{out}");
}

#[test]
fn one_trait_needs_no_comma_and_a_trailing_one_is_allowed() {
    assert_eq!(derived("module m;\nderive(Eq)\ntype P = { x: Int };\n"), vec!["Eq"]);
    assert_eq!(derived("module m;\nderive(Eq, Ord,)\ntype P = { x: Int };\n"), vec!["Eq", "Ord"]);
}

/// The word means nothing anywhere else, so `derive` stays available as a
/// function, a parameter and a field.
#[test]
fn derive_is_an_ordinary_identifier_everywhere_else() {
    let out = tree(
        "module m;\n\
         type Rules = { derive: Int };\n\
         fn derive(derive: Int) -> Int { derive }\n\
         fn f() -> Int { derive(1) }\n",
    );
    assert!(!out.contains("DERIVE_KW"), "none of these is a keyword\n{out}");
    assert!(!KEYWORDS.contains(&"derive"), "`derive` must not be a hard keyword");
    assert!(CONTEXTUAL_KEYWORDS.contains(&"derive"), "`derive` must be listed as contextual");
}

/// A `derive` in front of anything but a `type` is the mistake worth naming,
/// and naming it here beats a cascade downstream about a call to a function
/// nobody declared.
#[test]
fn a_derive_in_front_of_something_else_is_named() {
    let found = errors("module m;\nderive(Eq)\nfn f() -> Int { 1 }\n");
    assert!(
        found.iter().any(|e| e.contains("introduces a `type` declaration")),
        "got {found:?}"
    );
}

/// Recovery has to reach the next declaration, or one stray `derive` costs the
/// rest of the file.
#[test]
fn recovery_reaches_the_declaration_after_a_stray_derive() {
    let out = parse("module m;\nderive(Eq)\nfn f() -> Int { 1 }\n").debug_tree();
    assert!(out.contains("FN_DECL"), "{out}");
}

#[test]
fn a_derive_with_a_non_name_inside_is_reported() {
    let found = errors("module m;\nderive(1)\ntype P = { x: Int };\n");
    assert!(found.iter().any(|e| e.contains("name of a trait")), "got {found:?}");
}

/// The tree is lossless whatever the input, `derive` included.
#[test]
fn a_broken_derive_still_round_trips() {
    for source in [
        "module m;\nderive\n",
        "module m;\nderive(\n",
        "module m;\nderive(Eq\ntype P = { x: Int };\n",
        "module m;\nderive()\ntype P = { x: Int };\n",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source, "lost text for {source:?}");
    }
}
