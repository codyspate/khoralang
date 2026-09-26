//! Effect clauses: at most one `with` and one `raises`, `with` first.
//!
//! The parser accepted the two clauses in any order and any number of times,
//! and everything after it read only the first of each. So a second clause was
//! dropped without a word: `raises Oops raises Worse` refused `raise
//! Worse::Awful` as not raised, and `with {} with { clock: Clock }` said
//! `clock` was not in scope -- each an error about the line that was right,
//! caused by a line the compiler had silently ignored. One order and one of
//! each is also the only spelling anything in `std` uses.

use khora_syntax::parse;

/// The parse errors for a function declared with `clauses`, and for a
/// function *type* with the same clauses, which share the grammar.
fn errors(clauses: &str) -> (Vec<String>, Vec<String>) {
    let decl = format!("module m;\nfn f() -> Int {clauses} {{ 0 }}\n");
    let ty = format!("module m;\ntype F = () -> Int {clauses};\n");
    let messages = |src: &str| parse(src).errors().iter().map(|e| e.message.clone()).collect();
    (messages(&decl), messages(&ty))
}

#[test]
fn one_of_each_in_order_parses() {
    for clauses in ["", "with { clock: Clock }", "raises Oops", "with { clock: Clock } raises Oops"] {
        let (decl, ty) = errors(clauses);
        assert!(decl.is_empty() && ty.is_empty(), "`{clauses}`: {decl:?} {ty:?}");
    }
}

#[test]
fn raises_before_with_is_refused_and_names_the_order() {
    let (decl, ty) = errors("raises Oops with { clock: Clock }");
    for found in [decl, ty] {
        assert!(
            found.iter().any(|e| e.contains("`with` comes before `raises`")),
            "{found:?}"
        );
    }
}

#[test]
fn a_second_raises_is_refused_and_names_the_union() {
    let (decl, ty) = errors("raises Oops raises Worse");
    for found in [decl, ty] {
        assert!(
            found.iter().any(|e| e.contains("one `raises` clause") && e.contains("raises A + B")),
            "{found:?}"
        );
    }
}

#[test]
fn a_second_with_is_refused_and_names_the_merge() {
    let (decl, ty) = errors("with { a: A } with { b: B }");
    for found in [decl, ty] {
        assert!(
            found.iter().any(|e| e.contains("one `with` clause") && e.contains("with { a: A, b: B }")),
            "{found:?}"
        );
    }
}

/// The refused clause is still parsed as a clause, so nothing after it turns
/// into a cascade of errors about the body.
#[test]
fn a_refused_clause_is_one_error() {
    let (decl, _) = errors("raises Oops raises Worse");
    assert_eq!(decl.len(), 1, "{decl:?}");
}
