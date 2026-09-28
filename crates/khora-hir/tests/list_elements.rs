//! Lowering the element forms of a list literal.
//!
//! **What these guard: a `break` that leaves a loop nobody wrote.** A `for`
//! inside `[..]` expands to the same loop a `for` statement does, so without a
//! refusal `break` inside one would quietly end the literal early and keep the
//! elements made so far. That is a list shorter than the reader can see from
//! the source.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_hir::body::{bodies, Body};

const LIST: &str = "module m;
pub type List<A> = | Nil | Cons(A, List<A>);
pub type Step<S, A> = | Yield(S, A) | Done;
pub trait Iterator { fn next(self) -> Int; }
";

fn lower(db: &dyn Db, text: &str) -> Vec<(String, Body)> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    bodies(db, file).clone()
}

/// Every error from lowering the function `f` in `LIST` plus `f`.
fn errors_of(f: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let all = lower(&db, &format!("{LIST}{f}\n"));
    let (_, body) = all.iter().find(|(name, _)| name.ends_with('f')).expect("a function `f`");
    body.errors.iter().map(|e| e.message.clone()).collect()
}

const REFUSED: &str = "a `for` inside `[..]` makes elements and cannot be left early";

#[test]
fn break_in_a_for_element_is_refused_with_its_own_message() {
    let found = errors_of("fn f(xs: List<Int>) -> List<Int> { [for x in xs => if x > 1 => break] }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains(REFUSED), "{found:?}");
}

#[test]
fn continue_in_a_for_element_is_refused_with_its_own_message() {
    let found = errors_of("fn f(xs: List<Int>) -> List<Int> { [for x in xs => if x > 1 => continue] }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains(REFUSED), "{found:?}");
}

/// Inside a loop somebody did write, the element is still not that loop, so
/// the `break` is still refused rather than leaving the outer loop.
#[test]
fn break_in_a_for_element_inside_a_loop_is_still_refused() {
    let found = errors_of(
        "fn f(xs: List<Int>) -> Int { loop { let l = [for x in xs => if x > 1 => break]; }; 0 }",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains(REFUSED), "{found:?}");
}

/// A loop written inside the element's value is a loop, and its `break` is
/// its own.
#[test]
fn a_loop_inside_a_for_element_may_break() {
    let found = errors_of("fn f(xs: List<Int>) -> List<Int> { [for x in xs => loop { break x }] }");
    assert!(found.is_empty(), "{found:?}");
}

/// A lambda is its own function: a `break` in one is outside any loop, and
/// says the general thing, not the element's.
#[test]
fn a_lambda_inside_a_for_element_does_not_inherit_the_refusal() {
    let found = errors_of("fn f(xs: List<Int>) -> List<() -> ()> { [for x in xs => fn () => break] }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`break` outside a loop"), "{found:?}");
}

/// A `List` with a `Nil` and no `Cons` is somebody else's type. It is
/// reported, not a panic in the compiler.
#[test]
fn a_list_type_without_cons_is_reported() {
    let db = KhoraDatabase::new();
    let all = lower(&db, "module m;\npub type List = | Nil | Other;\nfn f(c: Bool) { [if c => 1]; }\n");
    let (_, body) = all.iter().find(|(name, _)| name.ends_with('f')).expect("f");
    let found: Vec<&str> = body.errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("builds a `List`"), "{found:?}");
}
