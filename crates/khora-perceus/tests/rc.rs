//! Where reference counting lands.
//!
//! These assert the *shape* of the plan rather than exact counts: phase 6 will
//! remove pairs that cancel, and a test pinning "exactly two dups" would fail
//! for a good reason. What must not change is which values are counted at all,
//! and that every owned reference is released.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_perceus::{is_boxed, rc_plans, RcPlan};
use khora_types::Type;

fn plan(db: &dyn Db, text: &str, function: &str) -> RcPlan {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    rc_plans(db, file)
        .iter()
        .find(|(name, _)| name == function)
        .map(|(_, plan)| plan.clone())
        .unwrap_or_else(|| panic!("no function `{function}`"))
}

const ADT: &str = "module m;\nexport type R = | A | B(n: Int);\n";

#[test]
fn machine_words_are_not_counted() {
    assert!(!is_boxed(&Type::Int));
    assert!(!is_boxed(&Type::Bool));
    assert!(!is_boxed(&Type::Unit));

    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(a: Int) -> Int { let b = a; b }\n", "f");
    assert!(p.boxed.is_empty(), "an Int should not be reference counted: {p:?}");
    assert!(p.dups.is_empty(), "no dups for machine words");
}

#[test]
fn strings_and_adts_are_counted() {
    assert!(is_boxed(&Type::Str));
    assert!(is_boxed(&Type::adt("R")));
}

#[test]
fn a_boxed_parameter_is_owned_and_released() {
    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(s: String) -> String { s }\n", "f");

    assert_eq!(p.boxed.len(), 1, "the parameter should be counted: {p:?}");
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert_eq!(released.len(), 1, "an owned parameter must be dropped: {p:?}");
}

/// Reading a counted local produces a value that outlives the read, so it
/// needs its own reference.
#[test]
fn reading_a_boxed_local_dups() {
    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(s: String) -> String { let t = s; t }\n", "f");
    assert!(!p.dups.is_empty(), "reading a boxed local should dup: {p:?}");
}

#[test]
fn a_boxed_let_is_released_by_its_block() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!("{ADT}fn f() -> Int {{\n  let r = R::B(1);\n  0\n}}\n"),
        "f",
    );

    assert_eq!(p.boxed.len(), 1, "the ADT local should be counted: {p:?}");
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert_eq!(released.len(), 1, "the block must release what it declared: {p:?}");
}

/// Every counted local has to be released exactly once somewhere, or the
/// runtime's live counter will not return to zero.
#[test]
fn every_counted_local_is_released_once() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!(
            "{ADT}fn f(s: String) -> Int {{\n  let a = R::B(1);\n  let b = R::A;\n  let c = s;\n  0\n}}\n"
        ),
        "f",
    );

    let mut released: Vec<_> = p.drops.values().flatten().copied().collect();
    let before = released.len();
    released.sort();
    released.dedup();
    assert_eq!(released.len(), before, "a local was released twice: {p:?}");

    for local in &p.boxed {
        assert!(released.contains(local), "local {local:?} is counted but never released: {p:?}");
    }
}

/// A binding from a match arm borrows out of the scrutinee, which the arm does
/// not own. Dropping it would free a value the scrutinee still holds.
#[test]
fn match_arm_bindings_are_not_released_by_the_arm() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!(
            "{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::B(n) => n,\n    R::A => 0,\n  }}\n}}\n"
        ),
        "f",
    );

    // `r` itself is owned by the function and released; the arm binding is not.
    let released: Vec<_> = p.drops.values().flatten().copied().collect();
    assert_eq!(released.len(), 1, "only the parameter should be released: {p:?}");
}

#[test]
fn nested_blocks_release_what_they_declared() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!("{ADT}fn f() -> Int {{\n  let outer = R::A;\n  {{\n    let inner = R::A;\n    0\n  }}\n}}\n"),
        "f",
    );

    assert_eq!(p.boxed.len(), 2, "both locals should be counted: {p:?}");
    assert_eq!(p.drops.len(), 2, "each block should release its own: {p:?}");
    for locals in p.drops.values() {
        assert_eq!(locals.len(), 1, "a block released someone else's local: {p:?}");
    }
}

#[test]
fn a_function_with_nothing_boxed_needs_no_plan() {
    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(a: Int, b: Int) -> Int { a + b }\n", "f");
    assert!(p.boxed.is_empty() && p.dups.is_empty() && p.drops.is_empty(), "{p:?}");
}
