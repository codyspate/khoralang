//! The lints 14.22–14.27 added.
//!
//! A lint earns its keep by being right about real code, so most of these are
//! about what it must *not* say. A lint that reports live code gets switched
//! off in a manifest, and then it is worth nothing; one that misses a case
//! annoys nobody. Where there is a choice, these pin the quiet direction.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_lint::{Finding, UNREACHABLE_CODE, UNUSED_BINDING};

fn findings(source: &str) -> Vec<Finding> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "t.kh".into(), source.to_string());
    SourceRoot::new(&db, vec![file]);
    khora_lint::findings(&db, file).clone()
}

/// Only the findings for one lint, so an unrelated one does not fail a test
/// about this one.
fn of(source: &str, lint: &str) -> Vec<Finding> {
    findings(source).into_iter().filter(|f| f.lint == lint).collect()
}

fn body(inner: &str) -> String {
    format!("module t;\n\npub fn main() -> Int {{\n{inner}\n}}\n")
}

// ---------------------------------------------------------------------------
// 14.24 unreachable-code

#[test]
fn a_statement_after_a_return_is_reported() {
    let source = "module t;\n\nfn early(n: Int) -> Int {\n  return n;\n  let a = 1;\n  a\n}\n\
                  \npub fn main() -> Int {\n  early(3)\n}\n";
    let found = of(source, UNREACHABLE_CODE);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains("return"), "{:?}", found[0]);
}

#[test]
fn one_finding_per_block_and_not_one_per_dead_line() {
    // Three lines after a `return` are one mistake. Three warnings about it is
    // output people learn to scroll past.
    let source = "module t;\n\nfn early(n: Int) -> Int {\n  return n;\n  let a = 1;\n  \
                  let b = 2;\n  let c = 3;\n  a\n}\n\
                  \npub fn main() -> Int {\n  early(3)\n}\n";
    assert_eq!(of(source, UNREACHABLE_CODE).len(), 1);
}

#[test]
fn a_return_at_the_end_is_not_reported() {
    let source = "module t;\n\nfn fine(n: Int) -> Int {\n  return n;\n}\n\
                  \npub fn main() -> Int {\n  fine(3)\n}\n";
    assert!(of(source, UNREACHABLE_CODE).is_empty());
}

#[test]
fn a_return_inside_a_branch_does_not_kill_what_follows_the_branch() {
    // The case that would make this lint worthless if it were wrong: an early
    // return under an `if` leaves the *branch*, not the block around it.
    let source = "module t;\n\nfn guard(n: Int) -> Int {\n  if n < 0 {\n    return 0;\n  }\n  \
                  n + 1\n}\n\npub fn main() -> Int {\n  guard(3)\n}\n";
    assert!(of(source, UNREACHABLE_CODE).is_empty(), "{:?}", of(source, UNREACHABLE_CODE));
}

#[test]
fn a_break_ends_a_block_too() {
    let source = "module t;\n\npub fn main() -> Int {\n  let mut n = 0;\n  \
                  while n < 3 {\n    break;\n    n = n + 1;\n  }\n  n\n}\n";
    assert_eq!(of(source, UNREACHABLE_CODE).len(), 1, "{:?}", of(source, UNREACHABLE_CODE));
}

// ---------------------------------------------------------------------------
// 14.23 unused-binding

#[test]
fn a_local_nothing_reads_is_reported() {
    let found = of(&body("  let a = 1;\n  0"), UNUSED_BINDING);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains('a'), "{:?}", found[0]);
}

#[test]
fn a_local_something_reads_is_not() {
    assert!(of(&body("  let a = 1;\n  a"), UNUSED_BINDING).is_empty());
}

#[test]
fn a_leading_underscore_is_the_escape() {
    // A parameter often cannot be used and still wants a name, because the
    // name is what tells the next reader what the argument is.
    assert!(of(&body("  let _a = 1;\n  0"), UNUSED_BINDING).is_empty());
}

#[test]
fn an_unused_parameter_is_reported() {
    let source = "module t;\n\nfn takes(used: Int, spare: Int) -> Int {\n  used\n}\n\
                  \npub fn main() -> Int {\n  takes(1, 2)\n}\n";
    let found = of(source, UNUSED_BINDING);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains("spare"), "{:?}", found[0]);
}

#[test]
fn a_name_bound_by_a_pattern_and_ignored_is_reported() {
    // The most common shape by far: `Option::Some(v) => true`. Thirty-two of
    // these were in `std/core.kh` when this lint first ran.
    let source = "module t;\n\ntype Wrap = | Full(Int) | Empty;\n\n\
                  fn has(w: Wrap) -> Bool {\n  match w { Wrap::Full(v) => true, \
                  Wrap::Empty => false }\n}\n\
                  \npub fn main() -> Int {\n  if has(Wrap::Empty) { 1 } else { 0 }\n}\n";
    let found = of(source, UNUSED_BINDING);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains('v'), "{:?}", found[0]);
}

#[test]
fn a_wildcard_binds_nothing_and_is_never_reported() {
    let source = "module t;\n\ntype Wrap = | Full(Int) | Empty;\n\n\
                  fn has(w: Wrap) -> Bool {\n  match w { Wrap::Full(_) => true, \
                  Wrap::Empty => false }\n}\n\
                  \npub fn main() -> Int {\n  if has(Wrap::Empty) { 1 } else { 0 }\n}\n";
    assert!(of(source, UNUSED_BINDING).is_empty());
}

#[test]
fn assigning_to_a_binding_counts_as_using_it() {
    // A miss rather than a false report, and deliberate: "assigned and never
    // read" is a different mistake, and catching it here would mean reporting
    // it with this lint's message.
    assert!(of(&body("  let mut a = 1;\n  a = 2;\n  0"), UNUSED_BINDING).is_empty());
}
