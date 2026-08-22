//! Effect rows on signatures.
//!
//! `docs/design/effect-runtime.md` decides what these compile to. This is the
//! type-level half: what a function requires of its caller, how it can fail,
//! and the rule that ties the two — a call's row must be *subsumed* by the
//! enclosing function's, never merely equal to it.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

const SERVICES: &str = "module m;\n\
                        export type Ledger;\n\
                        export type Ai;\n\
                        export type Report;\n\
                        export type DbError;\n\
                        export type ModelError;\n\
                        fn analyze(id: Int) -> Report with { ledger: Ledger };\n\
                        fn classify(r: Report) -> Int with { ai: Ai };\n\
                        fn risky(id: Int) -> Int raises DbError;\n";

// --- capabilities ----------------------------------------------------------

#[test]
fn requiring_exactly_what_is_called_is_accepted() {
    assert_clean(&format!(
        "{SERVICES}export fn f(id: Int) -> Report with {{ ledger: Ledger }} {{ analyze(id) }}\n"
    ));
}

/// Subsumption, not equality: a caller with more than the callee names is
/// fine, and this is the case the whole system rests on.
#[test]
fn requiring_more_than_is_called_is_accepted() {
    assert_clean(&format!(
        "{SERVICES}export fn f(id: Int) -> Report with {{ ledger: Ledger, ai: Ai }} \
         {{ analyze(id) }}\n"
    ));
}

/// Phase 4's exit criterion: the diagnostic names the absent label and the
/// function that required it.
#[test]
fn a_capability_the_caller_lacks_is_rejected_by_name() {
    assert_reports(
        &format!(
            "{SERVICES}export fn f(id: Int) -> Int with {{ ledger: Ledger }} \
             {{ classify(analyze(id)) }}\n"
        ),
        "`classify` needs `ai: Ai`, which this function does not require",
    );
}

/// No clause at all means the closed empty row, so nothing is required and
/// nothing may be called that requires anything.
#[test]
fn a_function_with_no_clause_requires_nothing() {
    assert_reports(
        &format!("{SERVICES}export fn f(id: Int) -> Report {{ analyze(id) }}\n"),
        "`analyze` needs `ledger: Ledger`",
    );
    assert_clean(&format!("{SERVICES}export fn f(id: Int) -> Int {{ id + 1 }}\n"));
}

/// Order is not part of a row's identity.
#[test]
fn a_row_is_the_same_written_either_way() {
    assert_clean(&format!(
        "{SERVICES}\
         fn both(id: Int) -> Int with {{ ledger: Ledger, ai: Ai }};\n\
         export fn f(id: Int) -> Int with {{ ai: Ai, ledger: Ledger }} {{ both(id) }}\n"
    ));
}

// --- failures --------------------------------------------------------------

#[test]
fn a_raise_the_caller_does_not_declare_is_rejected() {
    assert_reports(
        &format!("{SERVICES}export fn f(id: Int) -> Int {{ risky(id)! }}\n"),
        "`risky` needs `DbError`, which this function does not raise",
    );
}

#[test]
fn declaring_the_raise_accepts_it() {
    assert_clean(&format!(
        "{SERVICES}export fn f(id: Int) -> Int raises DbError {{ risky(id)! }}\n"
    ));
}

/// An error row widens the same way a capability row does.
#[test]
fn a_wider_error_row_accepts_a_narrower_call() {
    assert_clean(&format!(
        "{SERVICES}export fn f(id: Int) -> Int raises DbError + ModelError {{ risky(id)! }}\n"
    ));
}

/// The two clauses are separate rows: declaring a raise does not supply a
/// capability, and vice versa.
#[test]
fn the_two_rows_do_not_satisfy_each_other() {
    assert_reports(
        &format!("{SERVICES}export fn f(id: Int) -> Int raises DbError {{ classify(analyze(id)) }}\n"),
        "needs `ledger: Ledger`",
    );
}

// --- polymorphism ----------------------------------------------------------

/// `'e` stands for whatever else the caller has. A function that names it
/// passes its caller's capabilities through.
#[test]
fn a_row_variable_passes_the_rest_through() {
    assert_clean(&format!(
        "{SERVICES}\
         export fn wrap<'e>(id: Int) -> Report with {{ 'e | ledger: Ledger }} {{ analyze(id) }}\n\
         export fn caller(id: Int) -> Report with {{ ledger: Ledger, ai: Ai }} {{ wrap(id) }}\n"
    ));
}

#[test]
fn a_row_variable_does_not_conjure_a_capability() {
    assert_reports(
        &format!(
            "{SERVICES}\
             export fn wrap<'e>(id: Int) -> Report with {{ 'e | ledger: Ledger }} \
             {{ analyze(id) }}\n\
             export fn thin(id: Int) -> Report {{ wrap(id) }}\n"
        ),
        "`wrap` needs `ledger: Ledger`",
    );
}

/// A capability's type has to match, not just its label.
#[test]
fn one_label_cannot_carry_two_types() {
    assert_reports(
        &format!(
            "{SERVICES}export fn f(id: Int) -> Report with {{ ledger: Ai }} {{ analyze(id) }}\n"
        ),
        "Ai",
    );
}

// --- catch -----------------------------------------------------------------

const ERRORS: &str = "module m;
                      export type DbError = | Timeout | Refused;
                      export type ModelError = | RateLimited(ms: Int) | TooLong;
                      fn fetch(id: Int) -> Int raises DbError + ModelError;
                      fn only_db(id: Int) -> Int raises DbError;
";

/// Naming every error type takes the row to empty, so the function needs no
/// `raises` clause at all. This is the whole point of the feature.
#[test]
fn a_total_catch_empties_the_row() {
    assert_clean(&format!(
        "{ERRORS}         export fn safe(id: Int) -> Int {{ only_db(id)! catch {{          DbError::Timeout => 0, DbError::Refused => 1, }} }}
"
    ));
}

/// What the arms did not name is still the enclosing function's problem.
#[test]
fn an_unnamed_error_type_stays_in_the_row() {
    assert_clean(&format!(
        "{ERRORS}         export fn half(id: Int) -> Int raises DbError {{ fetch(id)! catch {{          ModelError::RateLimited(ms) => ms, ModelError::TooLong => 0, }} }}
"
    ));
}

#[test]
fn what_a_catch_leaves_behind_still_has_to_be_declared() {
    assert_reports(
        &format!(
            "{ERRORS}             export fn half(id: Int) -> Int {{ fetch(id)! catch {{              ModelError::RateLimited(ms) => ms, ModelError::TooLong => 0, }} }}
"
        ),
        "`fetch` needs `DbError`",
    );
}

/// Naming a type commits to all of it: a `catch` that handles one variant and
/// not the other would have to both subtract the type and leave it in.
#[test]
fn naming_an_error_type_means_handling_all_of_it() {
    assert_reports(
        &format!(
            "{ERRORS}             export fn safe(id: Int) -> Int {{ only_db(id)! catch {{              DbError::Timeout => 0, }} }}
"
        ),
        "Refused",
    );
}

/// The arms have to say which errors they handle, and a wildcard cannot: the
/// subtraction is by type name and `_` names nothing.
#[test]
fn a_catch_arm_has_to_name_a_constructor() {
    assert_reports(
        &format!("{ERRORS}export fn safe(id: Int) -> Int {{ only_db(id)! catch {{ _ => 0, }} }}
"),
        "name an error constructor",
    );
}

/// Catching something the operand cannot raise is dead code, and much more
/// likely a typo in the type name.
#[test]
fn catching_an_error_nothing_raises_is_an_error() {
    assert_reports(
        &format!(
            "{ERRORS}             export fn safe(id: Int) -> Int {{ only_db(id)! catch {{              DbError::Timeout => 0, DbError::Refused => 1,              ModelError::TooLong => 2, ModelError::RateLimited(ms) => ms, }} }}
"
        ),
        "nothing in this expression raises `ModelError`",
    );
}

/// An arm stands in for the value the operand would have produced, so it has
/// to be that type.
#[test]
fn a_catch_arm_produces_what_the_operand_would_have() {
    assert_reports(
        &format!(
            "{ERRORS}             export fn safe(id: Int) -> Int {{ only_db(id)! catch {{              DbError::Timeout => \"nope\", DbError::Refused => 1, }} }}
"
        ),
        "catch arms disagree",
    );
}

/// A `catch` does not excuse the mark. Control still leaves the operand, and
/// `!` is where the reader is owed a sign of it.
#[test]
fn a_catch_still_needs_the_mark() {
    assert_reports(
        &format!(
            "{ERRORS}             export fn safe(id: Int) -> Int {{ only_db(id) catch {{              DbError::Timeout => 0, DbError::Refused => 1, }} }}
"
        ),
        "needs `!`",
    );
}

// --- rows on function types ------------------------------------------------

const HANDLERS: &str = "module m;\n\
                        export type Db;\n\
                        export type Ai;\n\
                        export type Req = | Of;\n\
                        export type Res = | Of;\n\
                        export type Oops = | Bad;\n\
                        export fn mount<'r>(handler: Req -> Res with 'r) -> Int;\n\
                        export fn mount_db(handler: Req -> Res with { db: Db }) -> Int;\n\
                        export fn plain(r: Req) -> Res { Res::Of }\n\
                        export fn served(r: Req) -> Res with { db: Db } { Res::Of }\n\
                        export fn fallible(r: Req) -> Res raises Oops { Res::Of }\n";

/// The point of the whole feature. Naming `served` does not charge its
/// requirement to whoever wrote the name — the requirement is part of its
/// type, and travels with the value to whoever eventually calls it.
#[test]
fn a_function_that_needs_a_capability_can_be_passed_as_a_value() {
    assert_clean(&format!("{HANDLERS}export fn go() -> Int {{ mount(served) }}\n"));
}

/// The same, with the row written out rather than a variable.
#[test]
fn an_explicit_row_on_a_parameter_accepts_a_matching_function() {
    assert_clean(&format!("{HANDLERS}export fn go() -> Int {{ mount_db(served) }}\n"));
}

/// A row variable absorbs the empty row too, so a plain function fits where a
/// row-polymorphic one is wanted.
#[test]
fn a_row_variable_accepts_a_function_that_needs_nothing() {
    assert_clean(&format!("{HANDLERS}export fn go() -> Int {{ mount(plain) }}\n"));
}

/// `with { db: Db }` is a demand on the *argument*, not a wildcard. This is
/// what the fix for bare `'r` in type position was hiding: an unread row
/// variable became `Unknown`, and `Unknown` accepts everything.
#[test]
fn an_explicit_row_on_a_parameter_rejects_a_function_needing_something_else() {
    assert_reports(
        &format!(
            "{HANDLERS}\
             export fn other(r: Req) -> Res with {{ ai: Ai }} {{ Res::Of }}\n\
             export fn go() -> Int {{ mount_db(other) }}\n"
        ),
        "ai: Ai",
    );
}

/// The error row travels the same way the requirement row does.
#[test]
fn a_functions_error_row_travels_with_it() {
    assert_reports(
        &format!("{HANDLERS}export fn go() -> Int {{ mount(fallible) }}\n"),
        "Oops",
    );
}

/// Calling through a binding charges the caller, exactly as calling by name
/// does — the rows are in the type, so where the name came from is irrelevant.
#[test]
fn calling_through_a_binding_still_charges_the_caller() {
    assert_reports(
        &format!(
            "{HANDLERS}export fn go(r: Req) -> Res {{ let f = served; f(r) }}\n"
        ),
        "db: Db",
    );
}

#[test]
fn calling_through_a_binding_is_accepted_when_declared() {
    assert_clean(&format!(
        "{HANDLERS}export fn go(r: Req) -> Res with {{ db: Db }} {{ let f = served; f(r) }}\n"
    ));
}

// --- named contexts --------------------------------------------------------

const TWO_SERVICES: &str = "module m;\n\
                            export type Ledger;\n\
                            export type Ai;\n\
                            export effect Log { note: (Int) -> Int, }\n\
                            export effect Clock { now: () -> Int, }\n\
                            export fn stamped(n: Int) -> Int \
                              with { log: Log, clock: Clock } { log.note(clock.now() + n) }\n";

/// `with Mock { .. }` is `with { <Mock's bindings> } { .. }`. It used to be a
/// no-op: the block had no record literal, so nothing was installed and
/// nothing was discharged.
#[test]
fn a_named_context_installs_its_bindings() {
    assert_clean(&format!(
        "{TWO_SERVICES}\
         export context Mock {{\n\
           log: handler for Log {{ note: fn n => n }},\n\
           clock: handler for Clock {{ now: fn () => 7 }},\n\
         }}\n\
         export fn go() -> Int {{ with Mock {{ stamped(1) }} }}\n"
    ));
}

/// Half a context is still half: what it does not bind is still required.
#[test]
fn a_named_context_discharges_only_what_it_binds() {
    assert_reports(
        &format!(
            "{TWO_SERVICES}\
             export context Half {{\n\
               log: handler for Log {{ note: fn n => n }},\n\
             }}\n\
             export fn go() -> Int {{ with Half {{ stamped(1) }} }}\n"
        ),
        "clock: Clock",
    );
}

#[test]
fn a_context_that_does_not_exist_is_reported() {
    assert_reports(
        &format!("{TWO_SERVICES}export fn go() -> Int {{ with Nope {{ 1 }} }}\n"),
        "cannot find a `context` named `Nope`",
    );
}
