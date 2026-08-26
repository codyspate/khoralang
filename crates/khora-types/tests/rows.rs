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
                        pub type Ledger;\n\
                        pub type Ai;\n\
                        pub type Report;\n\
                        pub type DbError;\n\
                        pub type ModelError;\n\
                        fn analyze(id: Int) -> Report with { ledger: Ledger };\n\
                        fn classify(r: Report) -> Int with { ai: Ai };\n\
                        fn risky(id: Int) -> Int raises DbError;\n";

// --- capabilities ----------------------------------------------------------

#[test]
fn requiring_exactly_what_is_called_is_accepted() {
    assert_clean(&format!(
        "{SERVICES}pub fn f(id: Int) -> Report with {{ ledger: Ledger }} {{ analyze(id) }}\n"
    ));
}

/// Subsumption, not equality: a caller with more than the callee names is
/// fine, and this is the case the whole system rests on.
#[test]
fn requiring_more_than_is_called_is_accepted() {
    assert_clean(&format!(
        "{SERVICES}pub fn f(id: Int) -> Report with {{ ledger: Ledger, ai: Ai }} \
         {{ analyze(id) }}\n"
    ));
}

/// Phase 4's exit criterion: the diagnostic names the absent label and the
/// function that required it.
#[test]
fn a_capability_the_caller_lacks_is_rejected_by_name() {
    assert_reports(
        &format!(
            "{SERVICES}pub fn f(id: Int) -> Int with {{ ledger: Ledger }} \
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
        &format!("{SERVICES}pub fn f(id: Int) -> Report {{ analyze(id) }}\n"),
        "`analyze` needs `ledger: Ledger`",
    );
    assert_clean(&format!("{SERVICES}pub fn f(id: Int) -> Int {{ id + 1 }}\n"));
}

/// Order is not part of a row's identity.
#[test]
fn a_row_is_the_same_written_either_way() {
    assert_clean(&format!(
        "{SERVICES}\
         fn both(id: Int) -> Int with {{ ledger: Ledger, ai: Ai }};\n\
         pub fn f(id: Int) -> Int with {{ ai: Ai, ledger: Ledger }} {{ both(id) }}\n"
    ));
}

// --- failures --------------------------------------------------------------

#[test]
fn a_raise_the_caller_does_not_declare_is_rejected() {
    assert_reports(
        &format!("{SERVICES}pub fn f(id: Int) -> Int {{ risky(id)! }}\n"),
        "`risky` needs `DbError`, which this function does not raise",
    );
}

#[test]
fn declaring_the_raise_accepts_it() {
    assert_clean(&format!(
        "{SERVICES}pub fn f(id: Int) -> Int raises DbError {{ risky(id)! }}\n"
    ));
}

/// An error row widens the same way a capability row does.
#[test]
fn a_wider_error_row_accepts_a_narrower_call() {
    assert_clean(&format!(
        "{SERVICES}pub fn f(id: Int) -> Int raises DbError + ModelError {{ risky(id)! }}\n"
    ));
}

/// The two clauses are separate rows: declaring a raise does not supply a
/// capability, and vice versa.
#[test]
fn the_two_rows_do_not_satisfy_each_other() {
    assert_reports(
        &format!("{SERVICES}pub fn f(id: Int) -> Int raises DbError {{ classify(analyze(id)) }}\n"),
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
         pub fn wrap<'e>(id: Int) -> Report with {{ 'e | ledger: Ledger }} {{ analyze(id) }}\n\
         pub fn caller(id: Int) -> Report with {{ ledger: Ledger, ai: Ai }} {{ wrap(id) }}\n"
    ));
}

#[test]
fn a_row_variable_does_not_conjure_a_capability() {
    assert_reports(
        &format!(
            "{SERVICES}\
             pub fn wrap<'e>(id: Int) -> Report with {{ 'e | ledger: Ledger }} \
             {{ analyze(id) }}\n\
             pub fn thin(id: Int) -> Report {{ wrap(id) }}\n"
        ),
        "`wrap` needs `ledger: Ledger`",
    );
}

/// A capability's type has to match, not just its label.
#[test]
fn one_label_cannot_carry_two_types() {
    assert_reports(
        &format!(
            "{SERVICES}pub fn f(id: Int) -> Report with {{ ledger: Ai }} {{ analyze(id) }}\n"
        ),
        "Ai",
    );
}

// --- catch -----------------------------------------------------------------

const ERRORS: &str = "module m;
                      pub type DbError = | Timeout | Refused;
                      pub type ModelError = | RateLimited(ms: Int) | TooLong;
                      fn fetch(id: Int) -> Int raises DbError + ModelError;
                      fn only_db(id: Int) -> Int raises DbError;
";

/// Naming every error type takes the row to empty, so the function needs no
/// `raises` clause at all. This is the whole point of the feature.
#[test]
fn a_total_catch_empties_the_row() {
    assert_clean(&format!(
        "{ERRORS}         pub fn safe(id: Int) -> Int {{ only_db(id)! catch {{          DbError::Timeout => 0, DbError::Refused => 1, }} }}
"
    ));
}

/// What the arms did not name is still the enclosing function's problem.
#[test]
fn an_unnamed_error_type_stays_in_the_row() {
    assert_clean(&format!(
        "{ERRORS}         pub fn half(id: Int) -> Int raises DbError {{ fetch(id)! catch {{          ModelError::RateLimited(ms) => ms, ModelError::TooLong => 0, }} }}
"
    ));
}

#[test]
fn what_a_catch_leaves_behind_still_has_to_be_declared() {
    assert_reports(
        &format!(
            "{ERRORS}             pub fn half(id: Int) -> Int {{ fetch(id)! catch {{              ModelError::RateLimited(ms) => ms, ModelError::TooLong => 0, }} }}
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
            "{ERRORS}             pub fn safe(id: Int) -> Int {{ only_db(id)! catch {{              DbError::Timeout => 0, }} }}
"
        ),
        "Refused",
    );
}

/// `_` handles everything the operand can raise, so the function that wrote it
/// cannot fail.
#[test]
fn a_wildcard_catch_arm_handles_the_whole_row() {
    assert_clean(&format!(
        "{ERRORS}pub fn safe(id: Int) -> Int {{ fetch(id)! catch {{ _ => 0, }} }}
"
    ));
}

/// The point of it: a row nobody can name because the *caller* chooses it.
///
/// A supervisor takes work whose failures are a type parameter and has to be
/// able to recover from them anyway — no constructor exists to write down, so
/// without a wildcard there is nothing that can be written at all.
#[test]
fn a_wildcard_catch_arm_handles_a_row_variable() {
    assert_clean(
        "module m;
         pub fn supervise<'e>(work: () -> Int raises 'e) -> Int {
           work()! catch { _ => 0, }
         }
",
    );
}

/// Handling a type by name and the rest with `_` is one `catch`, and after it
/// nothing is left.
#[test]
fn named_arms_and_a_wildcard_compose() {
    assert_clean(&format!(
        "{ERRORS}pub fn safe(id: Int) -> Int {{ fetch(id)! catch {{
           DbError::Timeout => 0, DbError::Refused => 1, _ => 2, }} }}
"
    ));
}

/// A pattern that is neither a constructor nor `_` still has nothing to say
/// about which errors it handles.
#[test]
fn a_catch_arm_has_to_name_a_constructor() {
    assert_reports(
        &format!("{ERRORS}pub fn safe(id: Int) -> Int {{ only_db(id)! catch {{ 3 => 0, }} }}
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
            "{ERRORS}             pub fn safe(id: Int) -> Int {{ only_db(id)! catch {{              DbError::Timeout => 0, DbError::Refused => 1,              ModelError::TooLong => 2, ModelError::RateLimited(ms) => ms, }} }}
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
            "{ERRORS}             pub fn safe(id: Int) -> Int {{ only_db(id)! catch {{              DbError::Timeout => \"nope\", DbError::Refused => 1, }} }}
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
            "{ERRORS}             pub fn safe(id: Int) -> Int {{ only_db(id) catch {{              DbError::Timeout => 0, DbError::Refused => 1, }} }}
"
        ),
        "needs `!`",
    );
}

// --- rows on function types ------------------------------------------------

const HANDLERS: &str = "module m;\n\
                        pub type Db;\n\
                        pub type Ai;\n\
                        pub type Req = | Of;\n\
                        pub type Res = | Of;\n\
                        pub type Oops = | Bad;\n\
                        pub fn mount<'r>(handler: Req -> Res with 'r) -> Int;\n\
                        pub fn mount_db(handler: Req -> Res with { db: Db }) -> Int;\n\
                        pub fn plain(r: Req) -> Res { Res::Of }\n\
                        pub fn served(r: Req) -> Res with { db: Db } { Res::Of }\n\
                        pub fn fallible(r: Req) -> Res raises Oops { Res::Of }\n";

/// The point of the whole feature. Naming `served` does not charge its
/// requirement to whoever wrote the name — the requirement is part of its
/// type, and travels with the value to whoever eventually calls it.
#[test]
fn a_function_that_needs_a_capability_can_be_passed_as_a_value() {
    assert_clean(&format!("{HANDLERS}pub fn go() -> Int {{ mount(served) }}\n"));
}

/// The same, with the row written out rather than a variable.
#[test]
fn an_explicit_row_on_a_parameter_accepts_a_matching_function() {
    assert_clean(&format!("{HANDLERS}pub fn go() -> Int {{ mount_db(served) }}\n"));
}

/// A row variable absorbs the empty row too, so a plain function fits where a
/// row-polymorphic one is wanted.
#[test]
fn a_row_variable_accepts_a_function_that_needs_nothing() {
    assert_clean(&format!("{HANDLERS}pub fn go() -> Int {{ mount(plain) }}\n"));
}

/// `with { db: Db }` is a demand on the *argument*, not a wildcard. This is
/// what the fix for bare `'r` in type position was hiding: an unread row
/// variable became `Unknown`, and `Unknown` accepts everything.
#[test]
fn an_explicit_row_on_a_parameter_rejects_a_function_needing_something_else() {
    assert_reports(
        &format!(
            "{HANDLERS}\
             pub fn other(r: Req) -> Res with {{ ai: Ai }} {{ Res::Of }}\n\
             pub fn go() -> Int {{ mount_db(other) }}\n"
        ),
        "ai: Ai",
    );
}

/// The error row travels the same way the requirement row does.
#[test]
fn a_functions_error_row_travels_with_it() {
    assert_reports(
        &format!("{HANDLERS}pub fn go() -> Int {{ mount(fallible) }}\n"),
        "Oops",
    );
}

/// Calling through a binding charges the caller, exactly as calling by name
/// does — the rows are in the type, so where the name came from is irrelevant.
#[test]
fn calling_through_a_binding_still_charges_the_caller() {
    assert_reports(
        &format!(
            "{HANDLERS}pub fn go(r: Req) -> Res {{ let f = served; f(r) }}\n"
        ),
        "db: Db",
    );
}

#[test]
fn calling_through_a_binding_is_accepted_when_declared() {
    assert_clean(&format!(
        "{HANDLERS}pub fn go(r: Req) -> Res with {{ db: Db }} {{ let f = served; f(r) }}\n"
    ));
}

// --- named contexts --------------------------------------------------------

const TWO_SERVICES: &str = "module m;\n\
                            pub type Ledger;\n\
                            pub type Ai;\n\
                            pub effect Log { note: (Int) -> Int, }\n\
                            pub effect Clock { now: () -> Int, }\n\
                            pub fn stamped(n: Int) -> Int \
                              with { log: Log, clock: Clock } { log.note(clock.now() + n) }\n";

/// `with Mock { .. }` is `with { <Mock's bindings> } { .. }`. It used to be a
/// no-op: the block had no record literal, so nothing was installed and
/// nothing was discharged.
#[test]
fn a_named_context_installs_its_bindings() {
    assert_clean(&format!(
        "{TWO_SERVICES}\
         pub context Mock {{\n\
           log: handler for Log {{ note: fn n => n }},\n\
           clock: handler for Clock {{ now: fn () => 7 }},\n\
         }}\n\
         pub fn go() -> Int {{ with Mock {{ stamped(1) }} }}\n"
    ));
}

/// Half a context is still half: what it does not bind is still required.
#[test]
fn a_named_context_discharges_only_what_it_binds() {
    assert_reports(
        &format!(
            "{TWO_SERVICES}\
             pub context Half {{\n\
               log: handler for Log {{ note: fn n => n }},\n\
             }}\n\
             pub fn go() -> Int {{ with Half {{ stamped(1) }} }}\n"
        ),
        "clock: Clock",
    );
}

#[test]
fn a_context_that_does_not_exist_is_reported() {
    assert_reports(
        &format!("{TWO_SERVICES}pub fn go() -> Int {{ with Nope {{ 1 }} }}\n"),
        "cannot find a `context` named `Nope`",
    );
}

// --- what a lambda raises is a lower bound ---------------------------------

const FALLIBLE: &str = "module m;
pub type Oops = | Bad;
pub type Other = | Worse;
pub fn run<A>(body: () -> A raises Oops) -> A raises Oops { body()! }
";

/// A stub that cannot fail satisfies an interface that allows failure.
///
/// Raising *fewer* things is always safe, so a body's error row is a lower
/// bound rather than an exact answer. Before this, every test double had to
/// raise on a branch it never took, which is a tax on exactly the code an
/// effect system is supposed to make easy.
#[test]
fn a_body_that_cannot_fail_is_accepted_where_failure_is_allowed() {
    assert_clean(&format!("{FALLIBLE}fn f() -> Int raises Oops {{ run(fn () => 1)! }}\n"));
}

/// And one that fails in the way expected is still accepted, which is the case
/// that always worked.
#[test]
fn a_body_that_fails_the_expected_way_is_accepted() {
    assert_clean(&format!(
        "{FALLIBLE}fn f() -> Int raises Oops {{ run(fn () => raise Oops::Bad)! }}\n"
    ));
}

/// A lower bound is a bound. Raising something the interface did not mention
/// is still refused — the widening only ever goes one way.
#[test]
fn a_body_that_fails_an_unexpected_way_is_refused() {
    assert_reports(
        &format!("{FALLIBLE}fn f() -> Int raises Oops {{ run(fn () => raise Other::Worse)! }}\n"),
        "Other",
    );
}

/// And a body that *can* fail is still refused where nothing may.
#[test]
fn a_body_that_can_fail_is_refused_where_nothing_may() {
    assert_reports(
        "module m;
pub type Oops = | Bad;
pub fn run<A>(body: () -> A) -> A { body() }
fn f() -> Int { run(fn () => raise Oops::Bad) }
",
        "Oops",
    );
}

/// The widening is not a licence to lose the mark: a call that really can
/// leave still needs its `!`.
#[test]
fn widening_does_not_excuse_the_mark() {
    assert_reports(
        &format!("{FALLIBLE}fn f() -> Int raises Oops {{ run(fn () => 1) }}\n"),
        "needs `!`",
    );
}

/// A lambda nothing ever asked to be wider is exactly as fallible as its body,
/// which is what keeps `!` meaningful for the ordinary case.
#[test]
fn an_unconstrained_lambda_raises_only_what_its_body_does() {
    // No `!` anywhere, and none needed: neither the lambda nor the call fails.
    assert_clean("module m;\nfn f() -> Int { let g = fn n => n + 1; g(3) }\n");
    // A recursive one is the case that made the tail visible: it asks for the
    // row it is in the middle of inferring.
    assert_clean(
        "module m;
fn f() -> Int { let go = fn n => if n == 0 { 0 } else { n + go(n - 1) }; go(3) }
",
    );
}

// --- what a closure requires -----------------------------------------------
//
// `docs/design/capability-passing.md`. A lambda resolves a capability
// lexically if it can and requires it if it cannot, which is what lets a
// library install one for the duration of a callback.

const CALLBACK: &str = "module m;
                        pub type Ledger;
                        pub type Ai;
                        fn report(id: Int) -> Int with { ledger: Ledger };
                        fn install(body: (Int) -> Int with { ledger: Ledger }) -> Int;
                        fn apply(body: (Int) -> Int) -> Int;
";

/// The one that was refused. `f` and `fn x => f(x)` are the same function, so a
/// callback that needs a capability can be written either way.
#[test]
fn a_lambda_requires_what_it_cannot_resolve() {
    assert_clean(&format!("{CALLBACK}pub fn go() -> Int {{ install(fn id => report(id)) }}\n"));
}

/// And the named function it expands from, which always worked.
#[test]
fn a_named_function_can_be_the_same_callback() {
    assert_clean(&format!("{CALLBACK}pub fn go() -> Int {{ install(report) }}\n"));
}

/// The requirement is the *closure's*, not the enclosing function's. `go`
/// neither has a ledger nor needs one — `install` is what supplies it — and
/// charging it here is what the old error did.
#[test]
fn the_enclosing_function_is_not_charged_for_it() {
    let found = errors(&format!(
        "{CALLBACK}pub fn go() -> Int {{ install(fn id => report(id)) }}\n"
    ));
    assert!(
        !found.iter().any(|e| e.contains("does not require")),
        "the closure asked for it, not `go`: {found:?}"
    );
}

/// A capability that *is* in scope is captured, and the row stays empty — so a
/// callback that logs can still be handed to something requiring nothing. This
/// is what keeps every higher-order function in the library from having to be
/// polymorphic in its callback's requirements.
#[test]
fn a_capability_in_scope_is_captured_not_required() {
    assert_clean(&format!(
        "{CALLBACK}\
         pub fn go() -> Int with {{ ledger: Ledger }} {{ apply(fn id => report(id)) }}\n"
    ));
}

/// Requiring is not conjuring. A closure handed to something that supplies
/// nothing still cannot call what needs a ledger.
#[test]
fn a_callback_cannot_require_what_nobody_supplies() {
    assert_reports(
        &format!("{CALLBACK}pub fn go() -> Int {{ apply(fn id => report(id)) }}\n"),
        "required here but not provided",
    );
}

/// The row is exactly what the body could not reach: a closure needing two
/// capabilities, handed one, still says so about the other.
#[test]
fn a_closure_requires_only_what_it_could_not_reach() {
    assert_reports(
        &format!(
            "{CALLBACK}\
             fn both(id: Int) -> Int with {{ ledger: Ledger, ai: Ai }};\n\
             pub fn go() -> Int {{ install(fn id => both(id)) }}\n"
        ),
        "ai: Ai",
    );
}
