//! What the lints see, and — mostly — what they decline to say.
//!
//! Over half of these assert *silence*. That is the ratio the crate's own
//! documentation argues for: a warning people learn to ignore is worse than no
//! warning, and the way that starts is one lint being wrong about real code.
//! Each quiet case below is a shape that a plausible implementation reports and
//! that is perfectly fine.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_lint::{findings, Finding, DANGLING_EXPRESSION, UNUSED_CAPABILITY};

fn lint(db: &dyn Db, text: &str) -> Vec<Finding> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    findings(db, file).clone()
}

fn names(found: &[Finding]) -> Vec<&str> {
    found.iter().map(|f| f.lint).collect()
}

const EFFECT: &str = "module m;\n\
                      export effect Clock {\n  \
                      now: () -> Int,\n\
                      }\n";

// --- unused capability -----------------------------------------------------

#[test]
fn a_capability_nothing_touches_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!("{EFFECT}\nfn f() -> Int with {{ clock: Clock }} {{ 1 }}\n"),
    );
    assert_eq!(names(&found), [UNUSED_CAPABILITY], "{found:?}");
    assert!(found[0].message.contains("clock"), "{found:?}");
}

#[test]
fn a_capability_the_body_reads_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!("{EFFECT}\nfn f() -> Int with {{ clock: Clock }} {{ clock.now() }}\n"),
    );
    assert!(found.is_empty(), "{found:?}");
}

/// The case that makes this lint hard, and the one a reads-only implementation
/// gets wrong: `g` forwards its capability to `f` without ever naming it. There
/// is no `Expr::Local` for `clock` anywhere in `g`.
#[test]
fn a_forwarded_capability_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             fn f() -> Int with {{ clock: Clock }} {{ clock.now() }}\n\
             fn g() -> Int with {{ clock: Clock }} {{ f() }}\n"
        ),
    );
    assert!(found.is_empty(), "a pass-through function is not an unused capability: {found:?}");
}

/// Two capabilities, neither read, and the body cannot call anything. Both
/// are reported and each names itself.
#[test]
fn each_unused_capability_names_itself() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             export effect Log {{\n  write: (String) -> (),\n}}\n\
             fn f(x: Int) -> Int with {{ clock: Clock, log: Log }} {{ x }}\n"
        ),
    );
    assert_eq!(names(&found), [UNUSED_CAPABILITY, UNUSED_CAPABILITY], "{found:?}");
    let said = found.iter().map(|f| f.message.clone()).collect::<Vec<_>>().join(" ");
    assert!(said.contains("clock") && said.contains("log"), "{found:?}");
}

/// The conservative half, and the reason for it: a call may be forwarding the
/// capability, and nothing available here says whether it is. Reporting on a
/// guess is how a lint gets ignored.
#[test]
fn a_body_that_calls_anything_is_left_alone() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             fn g() -> Int {{ 1 }}\n\
             fn f() -> Int with {{ clock: Clock }} {{ g() }}\n"
        ),
    );
    assert!(
        found.is_empty(),
        "`g()` cannot need a Clock, but proving that needs the callee's row: {found:?}"
    );
}

#[test]
fn a_function_with_no_capabilities_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f() -> Int { 1 }\n");
    assert!(found.is_empty(), "{found:?}");
}

// --- dangling expression ---------------------------------------------------

#[test]
fn a_statement_that_computes_and_discards_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f(x: Int) -> Int { x + 1; x }\n");
    assert_eq!(names(&found), [DANGLING_EXPRESSION], "{found:?}");
}

#[test]
fn a_bare_local_as_a_statement_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f(x: Int) -> Int { x; 1 }\n");
    assert_eq!(names(&found), [DANGLING_EXPRESSION], "{found:?}");
}

/// The tail is the block's value, not a discarded statement.
#[test]
fn the_tail_expression_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f(x: Int) -> Int { x + 1 }\n");
    assert!(found.is_empty(), "{found:?}");
}

/// A call's result being ignored is often exactly right, and deciding which
/// ones are not needs to know whether the callee does anything. Out of scope
/// on purpose — the crate documentation says why.
#[test]
fn a_call_whose_result_is_ignored_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        "module m;\nfn g() -> Int { 1 }\nfn f() -> Int { g(); 2 }\n",
    );
    assert!(found.is_empty(), "a call may well do something: {found:?}");
}

#[test]
fn an_assignment_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f() -> Int { let mut x = 1; x = 2; x }\n");
    assert!(found.is_empty(), "{found:?}");
}

/// `let` binds the value, so nothing is discarded.
#[test]
fn a_let_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f(x: Int) -> Int { let y = x + 1; y }\n");
    assert!(found.is_empty(), "{found:?}");
}

/// An arithmetic statement containing a call might do something, so the whole
/// statement is left alone rather than reported for its inert half.
#[test]
fn arithmetic_containing_a_call_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        "module m;\nfn g() -> Int { 1 }\nfn f(x: Int) -> Int { x + g(); 2 }\n",
    );
    assert!(found.is_empty(), "{found:?}");
}

/// A lint on top of an error is noise: the reader has a real problem to fix
/// and a second opinion about the same line does not help.
#[test]
fn an_unresolved_name_is_not_also_linted() {
    let db = KhoraDatabase::new();
    let found = lint(&db, "module m;\nfn f() -> Int { nonexistent; 1 }\n");
    assert!(found.is_empty(), "{found:?}");
}

// --- reporting -------------------------------------------------------------

/// A reader goes through a file from the top, so a list in pass order makes
/// them jump around.
#[test]
fn findings_come_out_in_source_order() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             fn a(x: Int) -> Int with {{ clock: Clock }} {{ x + 1; x }}\n\
             fn b(x: Int) -> Int {{ x + 2; x }}\n"
        ),
    );
    assert!(found.len() >= 3, "expected all three: {found:?}");
    let starts: Vec<u32> = found.iter().map(|f| f.range.start().into()).collect();
    let mut sorted = starts.clone();
    sorted.sort();
    assert_eq!(starts, sorted, "{found:?}");
}
