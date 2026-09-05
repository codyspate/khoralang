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

/// **A `with` clause naming a bare type is diagnosed as the clause it is, not
/// as a capability nobody supplied.**
///
/// `row_of_syntax` shares its fallback arm between `with` and `raises`, and
/// that arm labels an entry after its own type -- right for `raises DbError`,
/// wrong for `with Ledger`, which comes out as an entry called `Ledger` of type
/// `Ledger` that no handler can be given the name to satisfy. The old message
/// was ``needs `Ledger: Ledger``` and sent the reader looking for a capability
/// instead of at the clause they had written. Found while spiking `Stream`,
/// where `with Self::Effects` is the shape somebody reaches for first --
/// `docs/design/effect-survey.md` 3.4.
#[test]
fn a_with_clause_naming_a_type_says_so() {
    // The malformed clause has to be on the *callee*: that is what puts the
    // unusable entry in the demand the call site is measured against. A caller
    // that writes one declares a capability nobody can supply, which is also
    // wrong and is not what this reports -- see the note below.
    assert_reports(
        &format!(
            "{SERVICES}fn broken(id: Int) -> Int with m::Ledger;\n\
             pub fn f(id: Int) -> Int {{ broken(id) }}\n"
        ),
        "which is a type rather than a capability",
    );
    // **`with Ledger` is not the same mistake and is not reported.** A bare
    // *name* comes out as an entry labelled `Ledger` of type `Ledger`, and
    // `with { Ledger: handler }` supplies exactly that -- unconventional, since
    // capabilities are lowercase by habit, but writable and therefore not
    // broken. Only a label nobody could write is.
    assert_clean(&format!(
        "{SERVICES}fn named() -> Int with Ledger;\n\
         pub fn f(id: Int) -> Int with {{ Ledger: Ledger }} {{ named() }}\n"
    ));
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

/// A name after `with` may be a `context` or a handler value, so a name that
/// is neither says both rather than only the one it happened to look for
/// first.
#[test]
fn a_context_that_does_not_exist_is_reported() {
    assert_reports(
        &format!("{TWO_SERVICES}pub fn go() -> Int {{ with Nope {{ 1 }} }}\n"),
        "`with` takes a handler value or the name of a `context`",
    );
}

// --- installing a capability by its type -----------------------------------

/// Two functions naming one capability type differently, which is the whole
/// problem. `docs/design/capability-installation.md`.
const TWO_LABELS: &str = "module m;\n\
                          pub type Ledger;\n\
                          pub type Clock;\n\
                          fn a_ledger() -> Ledger;\n\
                          fn a_clock() -> Clock;\n\
                          fn transfer() -> Int with { ledger: Ledger };\n\
                          fn reconcile() -> Int with { books: Ledger };\n\
                          fn stamp() -> Int with { clock: Clock };\n";

/// One value, two labels, neither of them the name it is bound under.
#[test]
fn one_value_supplies_every_label_of_its_type() {
    assert_clean(&format!(
        "{TWO_LABELS}\
         const Books = a_ledger();\n\
         pub fn go() -> Int {{ with Books {{ transfer() + reconcile() }} }}\n"
    ));
}

/// And only labels of *its* type: a second capability is still missing.
#[test]
fn a_value_supplies_only_its_own_type() {
    assert_reports(
        &format!(
            "{TWO_LABELS}\
             const Books = a_ledger();\n\
             pub fn go() -> Int {{ with Books {{ transfer() + stamp() }} }}\n"
        ),
        "clock: Clock",
    );
}

/// Comma-separated, one per capability.
#[test]
fn several_values_may_be_installed_at_once() {
    assert_clean(&format!(
        "{TWO_LABELS}\
         const Books = a_ledger();\n\
         const Tick = a_clock();\n\
         pub fn go() -> Int {{ with Books, Tick {{ reconcile() + stamp() }} }}\n"
    ));
}

/// Nested installs each supply their own type.
#[test]
fn installs_by_type_nest() {
    assert_clean(&format!(
        "{TWO_LABELS}\
         const Books = a_ledger();\n\
         const Tick = a_clock();\n\
         pub fn go() -> Int {{ with Books {{ with Tick {{ reconcile() + stamp() }} }} }}\n"
    ));
}

/// The postfix form, which is the same installation on an expression.
#[test]
fn a_value_may_be_installed_postfix() {
    assert_clean(&format!(
        "{TWO_LABELS}\
         const Books = a_ledger();\n\
         pub fn go() -> Int {{ reconcile() with Books }}\n"
    ));
}

/// **The right name and the wrong type is an error.** It was not: the label
/// was matched and the type never looked at, so `with { ledger: a_clock() }`
/// satisfied `ledger: Ledger`, compiled clean, and dispatched `ledger.note(5)`
/// to `Clock::now`. Errata 54.
#[test]
fn a_label_of_the_wrong_type_is_refused() {
    assert_reports(
        &format!(
            "{TWO_LABELS}\
             pub fn go() -> Int {{ with {{ ledger: a_clock() }} {{ transfer() }} }}\n"
        ),
        "`ledger` here is `Clock`",
    );
}

/// And the right name with the right type still passes, which is the half a
/// careless fix takes away.
#[test]
fn a_label_of_the_right_type_is_accepted() {
    assert_clean(&format!(
        "{TWO_LABELS}\
         pub fn go() -> Int {{ with {{ ledger: a_ledger() }} {{ transfer() }} }}\n"
    ));
}

/// A capability that arrives from the function's *own* `with` clause is still
/// charged to the signature rather than quietly discharged -- the mistake the
/// first version of the type check made, which told `unused-capability` that
/// every pass-through function used nothing.
#[test]
fn a_capability_from_the_signature_is_still_required() {
    assert_reports(
        &format!("{TWO_LABELS}pub fn go() -> Int {{ transfer() }}\n"),
        "ledger: Ledger",
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

// --- a `with` used as a value ----------------------------------------------

/// The shape these are about: something that installs a capability over a
/// generic body and hands the result back inside a wrapper.
const INSTALLING: &str = "module m;\n\
                          pub type Counter;\n\
                          pub type Oops;\n\
                          fn counter() -> Counter;\n\
                          fn ok<T>(value: T) -> T;\n";

/// **A postfix `with` in argument position types as its body.** It did not.
///
/// `expr with { .. }` lowers to a block whose statements bind the capabilities
/// and whose *tail* is `expr`. The hint describing the block's value was being
/// consumed by the first statement instead of the tail, so the capability's
/// type was unified with whatever the caller was waiting on.
///
/// Only visible when the hint is an unsolved variable -- as it is for a call
/// argument. Against a concrete hint the unification simply fails and is
/// discarded, which is why the same expression checked clean as a function's
/// tail and failed one layer in. Errata 53.
#[test]
fn an_installed_value_has_the_type_of_its_body() {
    assert_clean(&format!(
        "{INSTALLING}\
         fn body() -> Int with {{ counter: Counter }};\n\
         pub fn go() -> Int {{ ok(body() with {{ counter: counter() }}) }}\n"
    ));
}

/// The same, generic, which is `postgres::pool::held`. This reported that the
/// caller's own type parameter "cannot be assumed to be `Counter`".
#[test]
fn an_installed_value_keeps_a_caller_chosen_type() {
    assert_clean(&format!(
        "{INSTALLING}\
         pub fn hold<A>(body: () -> A with {{ counter: Counter }}) -> A {{\n\
         \x20 ok(body() with {{ counter: counter() }})\n\
         }}\n"
    ));
}

/// The other half, and the one a careless fix takes away: the hint still
/// reaches the **tail**.
///
/// An integer literal is `Int` unless something is asking for a narrower one,
/// so `take({ 5 })` only checks if the argument's `U8` hint reaches the
/// literal through the block. Deleting the restore in `infer_block` fails
/// this, which is what makes it a guard rather than decoration.
#[test]
fn the_tail_of_a_block_sees_the_hint() {
    assert_clean(
        "module m;\n\
         fn take(b: U8) -> Int;\n\
         pub fn go() -> Int { take({ 5 }) }\n",
    );
}

/// And it still reaches the tail with a statement in front of it -- the case
/// the bug broke, on the ordinary blocks a `with` is lowered to.
#[test]
fn the_tail_sees_the_hint_past_a_statement() {
    assert_clean(
        "module m;\n\
         fn take(b: U8) -> Int;\n\
         pub fn go() -> Int { take({ let _n = 1; 5 }) }\n",
    );
}

// --- a row as a type argument ----------------------------------------------

/// A type that takes a row, which is what `Fiber<A, 'er>` is.
const CARRIER: &str = "module m;
pub type Boom = | Bang;
pub type Other = | Nope;

pub type Slot<A, 'er>;
impl<A, 'er> Slot<A, 'er> {
  pub fn of(body: () -> A raises 'er) -> Slot<A, 'er>;
  pub fn take(self) -> A raises 'er;
}

fn safe() -> Int { 7 }
fn risky() -> Int raises Boom { raise Boom::Bang }
";

/// **The row survives on the type**, so a carrier built from an infallible
/// thunk is one whose `take` needs no `!`.
///
/// This is the property `Fiber<A, 'er>` exists for: an erased handle would
/// have demanded a `raises` clause from every caller, including one whose
/// fiber provably cannot fail.
#[test]
fn a_carrier_of_an_infallible_thunk_takes_without_a_mark() {
    assert_clean(&format!(
        "{CARRIER}\
         pub fn go() -> Int {{ Slot::take(Slot::of(fn () => safe())) }}\n"
    ));
}

/// And a carrier built from a fallible one raises **by name**, which is the
/// other half: the caller can `catch` the case rather than wildcarding it.
#[test]
fn a_carrier_of_a_fallible_thunk_raises_what_it_carried() {
    assert_clean(&format!(
        "{CARRIER}\
         pub fn go() -> Int {{ Slot::take(Slot::of(fn () => risky()!))! catch {{ Boom::Bang => 0 }} }}\n"
    ));
    assert_reports(
        &format!(
            "{CARRIER}\
             pub fn go() -> Int {{ Slot::take(Slot::of(fn () => risky()!)) }}\n"
        ),
        "needs `Boom`",
    );
}

/// **A written row in a type-argument position is checked.** Errata 59: it fell
/// through the type converter to `Unknown`, and `Unknown` agrees with
/// everything — so an annotation naming a row was decoration, and
/// `Fibers::adopt`'s `Fiber<(), {}>` promise held only by convention.
#[test]
fn an_empty_row_argument_refuses_a_carrier_that_can_fail() {
    assert_clean(&format!(
        "{CARRIER}\
         pub fn keep(_s: Slot<Int, {{}}>) -> () {{ }}\n\
         pub fn go() -> () {{ keep(Slot::of(fn () => safe())) }}\n"
    ));
    assert_reports(
        &format!(
            "{CARRIER}\
             pub fn keep(_s: Slot<Int, {{}}>) -> () {{ }}\n\
             pub fn go() -> () {{ keep(Slot::of(fn () => risky()!)) }}\n"
        ),
        "Boom",
    );
}

/// The same, between two rows that are both non-empty and different. Neither
/// is a subset of the other, so neither annotation should take the other.
#[test]
fn one_row_argument_does_not_pass_for_another() {
    assert_reports(
        &format!(
            "{CARRIER}\
             pub fn keep(_s: Slot<Int, Other>) -> () {{ }}\n\
             pub fn go() -> () {{ keep(Slot::of(fn () => risky()!)) }}\n"
        ),
        "Boom",
    );
}

// --- an operation that quantifies over a row -------------------------------

/// An effect whose operation names a row the effect itself does not declare.
const CARRIER_EFFECT: &str = "module m;
pub type Boom = | Bang;
pub type Other = | Nope;

pub type Slot<A, 'er>;
impl<A, 'er> Slot<A, 'er> {
  pub fn of(body: () -> A raises 'er) -> Slot<A, 'er>;
}

pub effect Crew {
  adopt: (Slot<(), 'er>) -> (),
}

fn quiet() -> () { }
fn risky() -> () raises Boom { raise Boom::Bang }
fn other() -> () raises Other { raise Other::Nope }
";

/// **An operation may be generic in a row**, and each call chooses its own.
///
/// This is what lets `Nursery::adopt` take the fiber you forked instead of a
/// separate handle type. An operation cannot be generic in a *type* — a
/// handler's fields are closures, and a closure is one piece of code — but a
/// row costs nothing to quantify: a capability crosses as evidence and an
/// error as a tag, so the closure is the same code whatever the row is. A type
/// parameter decides a layout and has to be monomorphized; a row does not.
///
/// Three adoptions, three different rows, one handler.
#[test]
fn an_operation_may_quantify_over_a_row() {
    assert_clean(&format!(
        "{CARRIER_EFFECT}\
         pub fn go() -> () {{\n\
           with {{ crew: handler for Crew {{ adopt: fn _s => () }} }} {{\n\
             crew.adopt(Slot::of(fn () => quiet()));\n\
             crew.adopt(Slot::of(fn () => risky()!));\n\
             crew.adopt(Slot::of(fn () => other()!));\n\
           }}\n\
         }}\n"
    ));
}

/// **And the handler may not look at the row it is generic in**, which is what
/// keeps the quantifier honest.
///
/// The instantiation happens at the *call*; the handler stays rigid. So an
/// operation is row-generic exactly when its handler does not care what the
/// row is — `adopt` waits for a child and never inspects how it can fail — and
/// a handler that tries to use `'er` is refused the way any function assuming
/// something about a caller's type parameter is refused.
#[test]
fn a_handler_may_not_use_the_row_it_is_generic_in() {
    assert_reports(
        "module m;
         pub effect Weird {
           run: (() -> () raises 'er) -> (),
         }
         pub fn go() -> () {
           with { w: handler for Weird { run: fn f => f()! } } { }
         }
        ",
        "is a type the caller chooses",
    );
}

/// A row the effect *does* declare is still the effect's, and is fixed once
/// for the whole handler rather than per call.
#[test]
fn a_row_the_effect_declares_is_not_requantified() {
    assert_reports(
        &format!(
            "{CARRIER_EFFECT}\
             pub fn keep(_s: Slot<(), {{Other}}>) -> () {{ }}\n\
             pub fn go() -> () {{ keep(Slot::of(fn () => risky()!)) }}\n"
        ),
        "Boom",
    );
}

/// **A type written where a row belongs is told which it is.**
///
/// `Fiber<(), Oops>` and `Fiber<(), { Oops }>` print almost the same, and the
/// answer — one pair of braces — appears nowhere in `std` or the reference,
/// both of which only ever show a row *variable*, which needs none. It cost
/// somebody a quarter of an hour, and the declaration
/// `Shared<Option<Fiber<(), NotifyError>>>` had type-checked happily on the
/// way: a field type nothing can inhabit, accepted in silence.
#[test]
fn a_type_where_a_row_belongs_is_told_to_use_braces() {
    let found = errors(
        "module m;\n\
         pub type Oops = | Bad;\n\
         pub type Holder<A, 'er> = { value: A };\n\
         fn f(h: Holder<Int, { Oops: Oops }>) -> Holder<Int, Oops> { h }\n",
    );
    assert!(
        found.iter().any(|e| e.contains("is a type and a row belongs here")),
        "the mismatch must say which is which: {found:?}"
    );
    assert!(
        found.iter().any(|e| e.contains("write it `{ Oops: Oops }`")),
        "and show the spelling: {found:?}"
    );
}

/// And the spelling it shows has to be the one that works.
///
/// **This test is the reason the hint was wrong for so long.** The one above
/// asserted the message said `{ Oops }`, so the message said `{ Oops }`, and
/// nothing anywhere checked that `{ Oops }` meant what the sentence claimed.
/// It does not: a row's entries are labelled, so `{ Oops }` parses as a row
/// whose *tail* is `Oops` and prints back as `{ | Oops }` -- following the
/// advice produced the same error again, word for word, about a different
/// type. A hint nobody can act on is worse than none, because it costs the
/// reader the time to try it.
#[test]
fn the_spelling_the_hint_shows_is_accepted() {
    let found = errors(
        "module m;\n\
         pub type Oops = | Bad;\n\
         pub type Holder<A, 'er> = { value: A };\n\
         fn f(h: Holder<Int, { Oops: Oops }>) -> Holder<Int, { Oops: Oops }> { h }\n",
    );
    assert!(found.is_empty(), "the hint's own spelling must type-check: {found:?}");
}
/// A callee taking one error type, handed a body that raises two, says why.
///
/// **`` `B` is not accounted for here`` is true and is a riddle.** `attempt` is
/// the callee this is for: `attempt<A, E, 'ef>` takes a single `E`, so a body
/// raising `HttpError + ChildFailed` cannot go through the documented way to
/// turn a failure into a value — and the message named a type without saying
/// what about it was the problem or what to do instead.
///
/// The limit is real. `Result<A, E>` needs one `E` and Khora has no anonymous
/// sum to name "either of these two", so `catch` — which matches per type and
/// never names the union — is the answer rather than a workaround.
#[test]
fn one_error_type_handed_two_says_to_use_catch() {
    assert_reports(
        "module a;\n\
         type A = Int;\n\
         type B = Int;\n\
         fn one<X, E, 'ef>(body: () -> X with 'ef raises E) -> X with 'ef raises E { body()! }\n\
         fn both(n: Int) -> Int raises A + B {\n\
           if n == 0 { let a: A = A(1); raise a } else { let b: B = B(2); raise b }\n\
         }\n\
         pub fn go() -> Int raises A + B { one(fn () => both(5)!)! }\n",
        "handle them with `catch` instead",
    );
}

/// A row of one is what it is for, and says nothing extra.
#[test]
fn one_error_type_handed_one_is_left_alone() {
    assert_clean(
        "module a;\n\
         type A = Int;\n\
         fn one<X, E, 'ef>(body: () -> X with 'ef raises E) -> X with 'ef raises E { body()! }\n\
         fn just(n: Int) -> Int raises A { let a: A = A(1); raise a }\n\
         pub fn go() -> Int raises A { one(fn () => just(5)!)! }\n",
    );
}

/// And a capability row short a label reaches the same arm and must not get it.
///
/// The note distinguishes the two by what the *expected* side wants: one error
/// type is a closed row of one, and a capability row is a row of its own.
#[test]
fn a_missing_capability_does_not_mention_catch() {
    let found = errors(
        "module a;\n\
         pub effect Clock { now: () -> Int, }\n\
         pub effect Log { note: (Int) -> (), }\n\
         fn needs() -> Int with { clock: Clock, log: Log } { clock.now() }\n\
         pub fn go() -> Int with { clock: Clock } { needs() }\n",
    );
    assert!(!found.is_empty(), "the missing capability is still reported");
    assert!(
        !found.iter().any(|e| e.contains("`catch`")),
        "a capability is not a failure: {found:?}"
    );
}
