//! The compiler's half of the permission contract, against the shared table.
//!
//! `tests/agreement/permission_cases.rs` holds the cases; this file runs them
//! against `khora_manifest`, and `crates/khora-codegen-llvm/tests/agreement.rs`
//! runs the same list against `std/permissions.kh` by compiling and executing
//! a Khora program. Neither file has a case list of its own, so neither can
//! cover a rule the other does not.
//!
//! `tests/permissions.rs` next door is about the manifest's own parsing and
//! about what these functions mean; this file is only about the two
//! implementations agreeing.

// `allow(dead_code)`, and it is the shared file's own doing: the Khora side
// needs the source-generating helpers and this side needs none of them. Two
// consumers of one module each use a different part of it, and neither is
// wrong for not using the other's.
#[path = "agreement/permission_cases.rs"]
#[allow(dead_code)]
mod permission_cases;

use permission_cases::{CASES, rust_answer};

/// **Every case in the shared table gets the answer the table records.**
///
/// The Khora side asserts the same list, so a row this test disagrees with is
/// a divergence between the two matchers and a row they *both* disagree with
/// is a deliberate change somebody has to write down here.
#[test]
fn the_compilers_matcher_answers_the_shared_table() {
    let mut wrong = Vec::new();
    for (at, case) in CASES.iter().enumerate() {
        let said = rust_answer(case);
        if said != case.granted {
            wrong.push(format!(
                "  case {at}: {:?} {:?} against {:?}\n    expected {}, khora_manifest said {}\n    {}",
                case.kind, case.subject, case.grants, case.granted, said, case.why
            ));
        }
    }
    assert!(wrong.is_empty(), "khora_manifest disagrees with the shared table:\n{}", wrong.join("\n"));
}

/// A table nobody filled in would pass every assertion in this file.
///
/// Cheap, and it is the failure mode of a data-driven gate: the cases are the
/// gate, so an empty list -- a bad merge, a `cfg` that excluded them -- is a
/// green run that checks nothing. The counts are floors rather than exact, so
/// adding a case never means editing this test.
#[test]
fn the_shared_table_covers_all_three_matchers() {
    let count = |kind| CASES.iter().filter(|c| c.kind == kind).count();
    assert!(count(permission_cases::Kind::Path) >= 15, "the path cases are the ones that diverged");
    assert!(count(permission_cases::Kind::Name) >= 5, "names");
    assert!(count(permission_cases::Kind::Host) >= 5, "hosts");
    assert!(
        CASES.iter().any(|c| !c.granted),
        "a table of nothing but grants would pass against a matcher that always says yes"
    );
    assert!(CASES.iter().any(|c| c.granted), "and the other way round");
}
