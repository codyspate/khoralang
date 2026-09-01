//! What the lints see, and — mostly — what they decline to say.
//!
//! Over half of these assert *silence*. That is the ratio the crate's own
//! documentation argues for: a warning people learn to ignore is worse than no
//! warning, and the way that starts is one lint being wrong about real code.
//! Each quiet case below is a shape that a plausible implementation reports and
//! that is perfectly fine.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_lint::{
    findings, Finding, DANGLING_EXPRESSION, DISCARDED_RESULT, REFERENCE_CYCLE,
    UNUSED_CAPABILITY,
};

/// What a package sees for this source without changing any level.
///
/// `findings` returns everything, including the lints that default to
/// `allow` -- the level is the consumer's decision, and a manifest can raise
/// one. A test about `unused-capability` does not mean those when it says
/// "what was reported", and every fixture here would otherwise carry an
/// `undocumented-export` for its own scaffolding.
fn lint(db: &dyn Db, text: &str) -> Vec<Finding> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    findings(db, file)
        .iter()
        .filter(|f| khora_lint::default_level(f.lint) != khora_manifest::LintLevel::Allow)
        .cloned()
        .collect()
}

fn names(found: &[Finding]) -> Vec<&str> {
    found.iter().map(|f| f.lint).collect()
}

const EFFECT: &str = "module m;\n\
                      pub effect Clock {\n  \
                      now: () -> Int,\n\
                      }\n";

// --- misplaced main --------------------------------------------------------

/// The same, for a file at a chosen path: this lint asks the layout, not the
/// tree, so the path is the input that matters.
fn lint_at(db: &dyn Db, path: &str, text: &str) -> Vec<Finding> {
    let file = SourceFile::new(db, path.into(), text.to_string());
    findings(db, file)
        .iter()
        .filter(|f| khora_lint::default_level(f.lint) != khora_manifest::LintLevel::Allow)
        .cloned()
        .collect()
}

const PROGRAM: &str = "module m;\nfn print(v: Int);\npub fn main() -> Int { print(1); 0 }\n";

/// A `main` anywhere under `src` but the two places allowed to have one.
///
/// It used to be picked up and run as the program's entry point wherever it
/// sat, so a reader looking for where a package begins had to search every
/// file, and a second one meant whichever the compiler reached first won.
#[test]
fn a_main_outside_an_entry_point_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint_at(&db, "app/src/helpers.kh", PROGRAM);
    assert_eq!(names(&found), vec![khora_lint::MISPLACED_MAIN], "{found:?}");
}

/// `src/main.kh` is the program, and it is the only one.
#[test]
fn an_entry_point_is_not_reported() {
    let db = KhoraDatabase::new();
    assert!(lint_at(&db, "app/src/main.kh", PROGRAM).is_empty(), "src/main.kh is the program");
}

/// **`src/bin/` is where a package's other programs go.**
///
/// For a while it was exempted here, recommended by the lint's own message,
/// and refused by the backend — which compiled every `main` it found into one
/// program. So `khora check` passed on the layout the message suggested and
/// `khora build` then failed with the error the message was trying to help
/// with. The exemption was withdrawn until the layout was real.
///
/// It is real now: `walk` leaves `src/bin` out of the package's own
/// compilation, each file in it is built with the package's modules and not
/// with the other programs, and `khora build` on a package builds all of them.
#[test]
fn a_main_under_src_bin_is_one_of_the_packages_programs() {
    let db = KhoraDatabase::new();
    assert!(
        lint_at(&db, "app/src/bin/tool.kh", PROGRAM).is_empty(),
        "`src/bin` holds the other programs"
    );
}

/// And anywhere else under `src` is still a `main` nobody will run.
///
/// `src/bin/deep/tool.kh` included: one file per program is the rule, because
/// a program that needs several modules is a package, and the shape that makes
/// that clear is the one that does not almost work.
#[test]
fn a_main_deeper_than_src_bin_is_still_reported() {
    let db = KhoraDatabase::new();
    let found = lint_at(&db, "app/src/bin/deep/tool.kh", PROGRAM);
    assert_eq!(names(&found), vec![khora_lint::MISPLACED_MAIN], "{found:?}");
}

/// A loose file has no layout to disagree with.
///
/// `khora run hello.kh` is a script. Telling somebody their one-file program
/// is in the wrong place would be worse than saying nothing, so the lint only
/// speaks where there is a `src` directory to have an opinion about.
#[test]
fn a_script_is_not_reported() {
    let db = KhoraDatabase::new();
    assert!(lint_at(&db, "hello.kh", PROGRAM).is_empty(), "a loose file");
    assert!(lint_at(&db, "scratch/hello.kh", PROGRAM).is_empty(), "and one in a folder");
}

/// A nested `src` is read from the innermost one.
///
/// A workspace is `packages/thing/src/main.kh`, and a checkout of it might sit
/// under a directory of somebody else's called `src`. The last one is the
/// package's.
#[test]
fn the_innermost_src_is_the_one_that_counts() {
    let db = KhoraDatabase::new();
    assert!(
        lint_at(&db, "src/vendor/app/src/main.kh", PROGRAM).is_empty(),
        "the package's own src decides"
    );
}

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
             pub effect Log {{\n  write: (String) -> (),\n}}\n\
             fn f(x: Int) -> Int with {{ clock: Clock, log: Log }} {{ x }}\n"
        ),
    );
    assert_eq!(names(&found), [UNUSED_CAPABILITY, UNUSED_CAPABILITY], "{found:?}");
    let said = found.iter().map(|f| f.message.clone()).collect::<Vec<_>>().join(" ");
    assert!(said.contains("clock") && said.contains("log"), "{found:?}");
}

/// A call to something that needs nothing does not excuse the capability.
///
/// This test used to assert the opposite, and its comment said why: "`g()`
/// cannot need a Clock, but proving that needs the callee's row." The row is
/// recorded now — `BodyTypes::call_rows`, per call site — so the lint no
/// longer gives up the moment a body contains a call, which was almost every
/// real function. Roadmap 14.27.
#[test]
fn a_call_that_needs_nothing_does_not_excuse_a_capability() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             fn g() -> Int {{ 1 }}\n\
             fn f() -> Int with {{ clock: Clock }} {{ g() }}\n"
        ),
    );
    assert_eq!(names(&found), [UNUSED_CAPABILITY], "{found:?}");
}

/// Forwarding is the case the old restriction existed to protect, and it is
/// still protected — now by knowing rather than by declining to look.
#[test]
fn a_call_that_requires_the_capability_is_forwarding_it() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             fn g() -> Int with {{ clock: Clock }} {{ clock.now() }}\n\
             fn f() -> Int with {{ clock: Clock }} {{ g() }}\n"
        ),
    );
    assert!(found.is_empty(), "`g()` requires the clock, so `f` is passing it on: {found:?}");
}

/// And a capability nothing in the chain wants is still reported, even next to
/// one that is forwarded.
#[test]
fn only_the_capability_nobody_needs_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{EFFECT}\n\
             pub effect Log {{\n  write: (String) -> (),\n}}\n\
             fn g() -> Int with {{ clock: Clock }} {{ clock.now() }}\n\
             fn f() -> Int with {{ clock: Clock, log: Log }} {{ g() }}\n"
        ),
    );
    assert_eq!(names(&found), [UNUSED_CAPABILITY], "{found:?}");
    assert!(found[0].message.contains("log"), "the forwarded one is not it: {found:?}");
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

// --- reference cycles ------------------------------------------------------

/// A record that can hold another of its own kind, which is what makes a loop
/// possible at all. `mut`, because an immutable graph is still a DAG.
const NODE: &str = "module m;\npub type Node = { mut next: Node, value: Int }\n";

fn cycles(db: &dyn Db, body: &str) -> Vec<Finding> {
    let text = format!("{NODE}{body}");
    let file = SourceFile::new(db, "a.kh".into(), text.clone());
    // A lint reported on a program that does not type-check says nothing
    // useful, and a fixture that stopped compiling would quietly stop testing.
    let errors: Vec<String> =
        khora_types::diagnostics(db, file).iter().map(|e| e.message.clone()).collect();
    assert!(errors.is_empty(), "the fixture should compile, got {errors:?}\n{text}");
    findings(db, file).iter().filter(|f| f.lint == REFERENCE_CYCLE).cloned().collect()
}

#[test]
fn a_field_pointing_at_its_own_object_is_reported() {
    let db = KhoraDatabase::new();
    let found = cycles(&db, "fn f(a: Node) -> () { a.next = a; }\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains("loop in the heap"), "{:?}", found[0].message);
    // The message has to name what is being closed, or a reader with three
    // assignments on one line cannot tell which.
    assert!(found[0].message.contains("`a.next`"), "{:?}", found[0].message);
}

#[test]
fn two_objects_pointing_at_each_other_are_reported() {
    let db = KhoraDatabase::new();
    let found = cycles(&db, "fn f(a: Node, b: Node) -> () { a.next = b; b.next = a; }\n");
    // Once, on the assignment that closes it. The first is innocent until the
    // second happens, and reporting both would blame a line that is fine.
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_longer_loop_is_still_a_loop() {
    let db = KhoraDatabase::new();
    let found = cycles(
        &db,
        "fn f(a: Node, b: Node, c: Node) -> () { a.next = b; b.next = c; c.next = a; }\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
}

/// Reachability comes from construction as well as assignment: a record built
/// out of `a` reaches `a` before anything is assigned at all.
#[test]
fn a_value_built_from_the_target_is_reported() {
    let db = KhoraDatabase::new();
    let found =
        cycles(&db, "fn f(a: Node) -> () { let b: Node = { next: a, value: 1 }; a.next = b; }\n");
    assert_eq!(found.len(), 1, "{found:?}");
}

// --- and the half that matters more ---------------------------------------

#[test]
fn a_chain_that_does_not_close_is_not_reported() {
    let db = KhoraDatabase::new();
    let found =
        cycles(&db, "fn f(a: Node, b: Node, c: Node) -> () { a.next = b; b.next = c; }\n");
    assert!(found.is_empty(), "a list is not a loop: {found:?}");
}

#[test]
fn building_from_something_else_is_not_reported() {
    let db = KhoraDatabase::new();
    let found =
        cycles(&db, "fn f(a: Node, c: Node) -> () { let b: Node = { next: c, value: 1 }; a.next = b; }\n");
    assert!(found.is_empty(), "{found:?}");
}

/// **A scalar cannot be part of a loop**, and the first version of this pass
/// did not know that. `self.wanted = held`, with `held` an `Int` from
/// `Array::length`, was reported as a cycle in `std/core.kh` — two false
/// positives across twenty-one files, on the first real code it ever saw. A
/// number is copied into a field, not pointed at from one.
#[test]
fn assigning_a_number_is_never_a_cycle() {
    let db = KhoraDatabase::new();
    let text = "module m;\n\
                pub type Counter = { mut seen: Int }\n\
                fn f(c: Counter) -> () { let n = 3; c.seen = n; }\n";
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    let found: Vec<&Finding> =
        findings(&db, file).iter().filter(|f| f.lint == REFERENCE_CYCLE).collect();
    assert!(found.is_empty(), "{found:?}");
}

/// The whole of `std`, `examples` and `bench` is the real test of the quiet
/// half. It is checked by `scripts/baseline.sh` rather than here — a lint that
/// only sees fixtures has not met anything — and this records what that run
/// says so a regression has something to contradict.
#[test]
fn the_corpus_is_quiet() {
    let db = KhoraDatabase::new();
    // A representative shape from `std/core.kh`: a field updated from a length.
    let text = "module m;\n\
                pub type Vector = { mut items: String, mut wanted: Int }\n\
                fn clear(self: Vector) -> () {\n\
                \x20 let held = 4;\n\
                \x20 if held > 0 { self.wanted = held; }\n\
                }\n";
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    let found: Vec<&Finding> =
        findings(&db, file).iter().filter(|f| f.lint == REFERENCE_CYCLE).collect();
    assert!(found.is_empty(), "this is the shape that was reported wrongly: {found:?}");
}

/// **A function's result is not its argument**, and assuming otherwise is how
/// this lint met real code and lost.
///
/// The first Khora written after it landed was `packages/postgres`, whose read
/// loop is `c.pending = advance(c.pending, n)` — a function that builds a new
/// array out of the old one and returns it. Reported twice as a cycle, on a
/// line where nothing points at anything. A constructor holds its arguments;
/// a function is computed *from* them, and almost every call is the second.
#[test]
fn a_call_that_is_not_a_constructor_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = cycles(
        &db,
        "fn shorter(n: Node) -> Node { n }\n\
         fn f(a: Node) -> () { a.next = shorter(a.next); }\n",
    );
    assert!(found.is_empty(), "a call is not a construction: {found:?}");
}

/// And a constructor still is one, or the lint would see nothing at all.
#[test]
fn a_variant_holding_the_target_is_still_reported() {
    let db = KhoraDatabase::new();
    let text = "module m;\n\
                pub type Slot = | Empty | Held(Slot)\n\
                pub type Box = { mut slot: Slot }\n\
                fn f(b: Box, s: Slot) -> () { b.slot = Held(s); }\n";
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    // Not a cycle — `s` does not reach `b` — so this is the quiet direction,
    // and the shape is here to prove a constructor is still walked at all.
    let found: Vec<&Finding> =
        findings(&db, file).iter().filter(|f| f.lint == REFERENCE_CYCLE).collect();
    assert!(found.is_empty(), "{found:?}");
}

// --- a `Result` nobody looked at --------------------------------------------

/// A `Result` and something that produces one.
const RESULTS: &str = "module m;\n\
                       pub type Result<A, E> = | Ok(value: A) | Err(error: E);\n\
                       pub type Oops = | Bad;\n\
                       fn work() -> Result<Int, Oops> { Result::Ok(1) }\n";

/// **The case that bit twice.** A statement that produces a `Result` and does
/// not look at it hides whatever the failure was.
#[test]
fn a_discarded_result_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, &format!("{RESULTS}\nfn f() -> Int {{ work(); 0 }}\n"));
    assert_eq!(names(&found), [DISCARDED_RESULT], "{found:?}");
}

/// **And with `!` on it**, which is the shape that actually shipped. `expr!` is
/// a mark on the effect row and the identity on values, so it does nothing at
/// all about the `Result` — which is exactly why somebody writing it believes
/// the failure is handled.
#[test]
fn a_marked_call_that_still_returns_a_result_is_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{RESULTS}\nfn g() -> Result<Int, Oops> raises Oops {{ work() }}\n\
             fn f() -> Int raises Oops {{ g()!; 0 }}\n"
        ),
    );
    assert_eq!(names(&found), [DISCARDED_RESULT], "{found:?}");
}

/// `let _ =` is how a program says the answer was considered, and it is quiet.
#[test]
fn a_result_bound_to_a_wildcard_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(&db, &format!("{RESULTS}\nfn f() -> Int {{ let _ = work(); 0 }}\n"));
    assert!(found.is_empty(), "{found:?}");
}

/// The tail of a block is the block's value, not a discarded one.
#[test]
fn a_result_that_is_the_blocks_value_is_not_reported() {
    let db = KhoraDatabase::new();
    let found =
        lint(&db, &format!("{RESULTS}\nfn f() -> Result<Int, Oops> {{ work() }}\n"));
    assert!(found.is_empty(), "{found:?}");
}

/// A `match` on it is looking at it, which is the whole point.
#[test]
fn a_matched_result_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!(
            "{RESULTS}\nfn f() -> Int {{\n  \
             match work() {{ Result::Ok(n) => n, Result::Err(_e) => 0 }}\n}}\n"
        ),
    );
    assert!(found.is_empty(), "{found:?}");
}

/// A statement producing anything else is somebody else's business — a
/// `()`-returning call is the ordinary way to do something for its effect.
#[test]
fn a_discarded_unit_is_not_reported() {
    let db = KhoraDatabase::new();
    let found = lint(
        &db,
        &format!("{RESULTS}\nfn side() -> () {{ () }}\nfn f() -> Int {{ side(); 0 }}\n"),
    );
    assert!(found.is_empty(), "{found:?}");
}
