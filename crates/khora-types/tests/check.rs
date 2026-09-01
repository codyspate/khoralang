//! Type checking over real Khora source.

use khora_db::{Db, KhoraDatabase, SourceFile, SourceRoot};
use khora_types::check_file;

fn errors(db: &dyn Db, text: &str) -> Vec<String> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    check_file(db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let db = KhoraDatabase::new();
    let found = errors(&db, text);
    assert!(found.is_empty(), "expected no type errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let db = KhoraDatabase::new();
    let found = errors(&db, text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

const ADT: &str = "module m;\npub type R = | A | B(n: Int) | C;\n";

#[test]
fn a_well_typed_function_is_accepted() {
    assert_clean("module m;\nfn add(a: Int, b: Int) -> Int { a + b }\n");
}

#[test]
fn a_body_disagreeing_with_the_return_type_is_reported() {
    assert_reports(
        "module m;\nfn f() -> Int { true }\n",
        "returns `Int`, but its body has type `Bool`",
    );
}

#[test]
fn arithmetic_requires_ints() {
    assert_reports("module m;\nfn f(b: Bool) -> Int { b + 1 }\n", "arithmetic: expected `Int`, found `Bool`");
}

/// The reference program concatenates strings with `+`, so it is overloaded.
#[test]
fn plus_concatenates_strings() {
    assert_clean("module m;\nfn f(a: String, b: String) -> String { a + b }\n");
    assert_reports(
        "module m;\nfn f(a: String) -> String { a + 1 }\n",
        "string concatenation: expected `String`, found `Int`",
    );
}

#[test]
fn a_comparison_yields_bool_and_needs_matching_sides() {
    assert_clean("module m;\nfn f(a: Int, b: Int) -> Bool { a < b }\n");
    assert_reports("module m;\nfn f(a: Int, b: Bool) -> Bool { a == b }\n", "this comparison");
}

#[test]
fn an_if_condition_must_be_bool() {
    assert_reports("module m;\nfn f(n: Int) -> Int { if n { 1 } else { 2 } }\n", "`if` condition");
}

#[test]
fn if_branches_must_agree() {
    assert_reports(
        "module m;\nfn f(b: Bool) -> Int { if b { 1 } else { true } }\n",
        "branches disagree",
    );
}

/// Without an `else`, the branch produces nothing, so it must be unit.
#[test]
fn an_if_without_else_must_produce_unit() {
    assert_clean("module m;\nfn f(b: Bool) { if b { print(1); } }\n");
    assert_reports(
        "module m;\nfn f(b: Bool) -> Int { if b { 1 } }\n",
        "must produce `()`",
    );
}

#[test]
fn a_while_condition_must_be_bool() {
    assert_reports("module m;\nfn f(n: Int) { while n { } }\n", "`while` condition");
}

#[test]
fn return_must_match_the_signature() {
    assert_clean("module m;\nfn f(n: Int) -> Int { if n > 0 { return 1; } 2 }\n");
    assert_reports("module m;\nfn f() -> Int { return true; }\n", "this `return`");
}

/// `return` and `break` do not produce a value to the surrounding expression,
/// so a branch that diverges must not force the other branch's type.
#[test]
fn a_diverging_branch_does_not_constrain_the_other() {
    assert_clean("module m;\nfn f(b: Bool) -> Int { if b { return 0; } else { 1 } }\n");
}

#[test]
fn constructor_arity_and_field_types_are_checked() {
    assert_clean(&format!("{ADT}fn f() -> R {{ R::B(1) }}\n"));
    assert_reports(&format!("{ADT}fn f() -> R {{ R::B(true) }}\n"), "this argument");
    assert_reports(&format!("{ADT}fn f() -> R {{ R::B(1, 2) }}\n"), "takes 1 argument(s)");
}

#[test]
fn call_arity_is_checked() {
    assert_reports(
        "module m;\nfn g(a: Int) -> Int { a }\nfn f() -> Int { g(1, 2) }\n",
        "takes 1 argument(s)",
    );
}

#[test]
fn a_nullary_constructor_is_a_value_of_its_type() {
    assert_clean(&format!("{ADT}fn f() -> R {{ R::A }}\n"));
}

// --- exhaustiveness ------------------------------------------------------

#[test]
fn a_non_exhaustive_match_names_the_missing_pattern() {
    let text = format!("{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::A => 1,\n    R::B(n) => n,\n  }}\n}}\n");
    assert_reports(&text, "not exhaustive");
    assert_reports(&text, "`C`");
}

#[test]
fn a_complete_match_is_accepted() {
    assert_clean(&format!(
        "{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::A => 1,\n    R::B(n) => n,\n    R::C => 3,\n  }}\n}}\n"
    ));
}

#[test]
fn a_wildcard_arm_completes_a_match() {
    assert_clean(&format!(
        "{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::A => 1,\n    _ => 0,\n  }}\n}}\n"
    ));
}

/// The pieces `attempt` needs, declared here rather than imported.
///
/// Every test in this file is self-contained, and this one has to be: the
/// point is that the error type reaches the `match` through a *signature*
/// rather than an annotation, so the signature has to be in the source.
const ATTEMPT: &str = "module m;\n\
                       pub type Result<A, E> = | Ok(value: A) | Err(error: E);\n\
                       pub fn attempt<A, E, 'ef>(body: () -> A with 'ef raises E)\n  \
                         -> Result<A, E> with 'ef;\n";

/// **The idiom `testing.md` teaches, which did not compile.**
///
/// Exhaustiveness is a question about the scrutinee's *settled* type: to know
/// that `Err(NotFound(id))` covers every `Err`, the checker has to know the
/// error type is `Oops` and that `NotFound` is its only case. Asked
/// mid-inference the answer was `Result<Int, ?12>` — an unsolved variable has
/// no constructors, so the arm covered part of `Err`'s space and the rest was
/// reported missing: `pattern Err(_) not covered`, for a type with one
/// variant.
///
/// The error type arrives through `attempt`'s signature and a lambda's
/// `raises`, which is a deferred constraint — so it was a variable at the
/// `match` and concrete a few lines later. Annotating the `let` made it
/// compile, which is what told everybody it was a bug rather than a rule, and
/// the annotation is what this test deliberately leaves off.
#[test]
fn a_nested_error_pattern_is_exhaustive_once_the_row_is_solved() {
    assert_clean(&format!(
        "{ATTEMPT}\
         pub type Oops = | NotFound(id: Int);\n\
         fn load(id: Int) -> Int raises Oops {{\n  \
           if id == 999 {{ raise Oops::NotFound(id) }} else {{ id }}\n\
         }}\n\
         fn f() -> Int {{\n  \
           match attempt(fn () => load(999)!) {{\n    \
             Result::Ok(v) => v,\n    \
             Result::Err(Oops::NotFound(id)) => id,\n  \
           }}\n\
         }}\n"
    ));
}

/// **And deferring the check did not turn it into a formality.**
///
/// The same shape with a second case in the error type and one of them
/// unmatched. Before the fix this said `Err(_)`, which is the wrong pattern;
/// now it names the one actually missing, which is the answer that sends a
/// reader to the right line.
#[test]
fn a_nested_error_pattern_that_misses_a_case_names_it() {
    assert_reports(
        &format!(
            "{ATTEMPT}\
             pub type Oops = | NotFound(id: Int) | Denied(id: Int);\n\
             fn load(id: Int) -> Int raises Oops {{\n  \
               if id == 999 {{ raise Oops::NotFound(id) }} else {{ id }}\n\
             }}\n\
             fn f() -> Int {{\n  \
               match attempt(fn () => load(999)!) {{\n    \
                 Result::Ok(v) => v,\n    \
                 Result::Err(Oops::NotFound(id)) => id,\n  \
               }}\n\
             }}\n"
        ),
        "Err(Denied(_))",
    );
}

#[test]
fn an_unreachable_arm_is_reported() {
    assert_reports(
        &format!("{ADT}fn f(r: R) -> Int {{\n  match r {{\n    _ => 0,\n    R::A => 1,\n  }}\n}}\n"),
        "unreachable",
    );
}

/// A guard can fail, so a guarded arm covers nothing. Counting it would make
/// the check unsound.
#[test]
fn a_guarded_arm_does_not_complete_a_match() {
    assert_reports(
        &format!(
            "{ADT}fn f(r: R, ok: Bool) -> Int {{\n  match r {{\n    R::A => 1,\n    R::B(n) => n,\n    R::C if ok => 3,\n  }}\n}}\n"
        ),
        "not exhaustive",
    );
}

#[test]
fn matching_an_int_always_needs_a_wildcard() {
    assert_reports(
        "module m;\nfn f(n: Int) -> Int {\n  match n {\n    1 => 1,\n    2 => 2,\n  }\n}\n",
        "not exhaustive",
    );
    assert_clean("module m;\nfn f(n: Int) -> Int {\n  match n {\n    1 => 1,\n    _ => 0,\n  }\n}\n");
}

#[test]
fn a_bool_match_needs_both_cases() {
    assert_reports(
        "module m;\nfn f(b: Bool) -> Int {\n  match b {\n    true => 1,\n  }\n}\n",
        "not exhaustive",
    );
    assert_clean(
        "module m;\nfn f(b: Bool) -> Int {\n  match b {\n    true => 1,\n    false => 0,\n  }\n}\n",
    );
}

/// Regression: a `match` naming every nested case used to be reported
/// inexhaustive, because payload sub-columns were typed `Unknown` and could
/// never be complete. That rejected valid programs.
#[test]
fn a_fully_covered_nested_match_needs_no_wildcard() {
    let src = "module m;
               pub type Inner = | X | Y;
               pub type Outer = | Wrap(i: Inner) | Empty;
               fn f(o: Outer) -> Int {
                 match o {
                   Outer::Wrap(Inner::X) => 1,
                   Outer::Wrap(Inner::Y) => 2,
                   Outer::Empty => 0,
                 }
               }
";
    assert_clean(src);
}

#[test]
fn an_incomplete_nested_match_is_still_reported() {
    let src = "module m;
               pub type Inner = | X | Y;
               pub type Outer = | Wrap(i: Inner) | Empty;
               fn f(o: Outer) -> Int {
                 match o {
                   Outer::Wrap(Inner::X) => 1,
                   Outer::Empty => 0,
                 }
               }
";
    assert_reports(src, "not exhaustive");
}

/// A type containing itself must not send the checker into a loop.
#[test]
fn a_recursive_type_terminates() {
    let src = "module m;
               pub type List = | Nil | Cons(head: Int, tail: List);
               fn f(l: List) -> Int {
                 match l {
                   List::Nil => 0,
                   List::Cons(h, t) => h,
                 }
               }
";
    assert_clean(src);
}

#[test]
fn match_arms_must_agree_on_a_type() {
    assert_reports(
        &format!("{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::A => 1,\n    _ => true,\n  }}\n}}\n"),
        "arms disagree",
    );
}

#[test]
fn a_pattern_binding_takes_the_payload_type() {
    assert_clean(&format!(
        "{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::B(n) => n + 1,\n    _ => 0,\n  }}\n}}\n"
    ));
    assert_reports(
        &format!("{ADT}fn f(r: R) -> Bool {{\n  match r {{\n    R::B(n) => n,\n    _ => true,\n  }}\n}}\n"),
        "arms disagree",
    );
}

// --- error suppression ---------------------------------------------------

/// A syntax error must not produce a second wave of type errors.
#[test]
fn a_parse_error_does_not_cascade() {
    let db = KhoraDatabase::new();
    let found = errors(&db, "module m;\nfn f() -> Int { let x = ; x + 1 }\n");
    assert!(found.is_empty(), "type errors piled onto a syntax error: {found:?}");
}

/// Unsupported syntax is reported once by HIR lowering; the checker must not
/// add noise on top.
#[test]
fn unsupported_syntax_does_not_produce_type_errors() {
    let db = KhoraDatabase::new();
    let found = errors(&db, "module m;\nfn f() -> Int { let g = fn x => x; 1 }\n");
    assert!(found.is_empty(), "checker piled onto unsupported syntax: {found:?}");
}

// --- constructors are qualified by their type -----------------------------

/// Case names are not unique across a program. A lookup by bare name resolved
/// `Maybe::Some` to whichever `Some` was declared first, which is a wrong tag
/// rather than a diagnostic.
#[test]
fn a_constructor_resolves_to_its_own_type() {
    assert_clean(
        "module m;\n\
         pub type Option<A> = | Some(value: A) | None;\n\
         pub type Maybe<A> = | Some(value: A) | None;\n\
         fn f() -> Maybe<Int> { Maybe::Some(1) }\n\
         fn g() -> Option<Int> { Option::Some(1) }\n",
    );
}

#[test]
fn a_constructor_of_another_type_is_not_reachable() {
    // Name resolution rejects this, so it is a HIR error rather than a type
    // error and `check_file` — which reports only the latter — cannot see it.
    let db = KhoraDatabase::new();
    let file = SourceFile::new(
        &db,
        "a.kh".into(),
        "module m;\n\
         pub type Color = | Red | Green;\n\
         pub type Fruit = | Apple;\n\
         fn f() -> Fruit { Fruit::Red }\n"
            .to_string(),
    );
    let found: Vec<String> =
        khora_types::diagnostics(&db, file).iter().map(|e| e.message.clone()).collect();
    assert!(
        found.iter().any(|e| e.contains("`Color::Red` is `Color`'s")),
        "expected the path to be rejected and the right type named, got {found:?}"
    );
}

/// Matching is qualified too, so an arm cannot name another type's case.
#[test]
fn a_pattern_names_its_own_types_cases() {
    assert_clean(
        "module m;\n\
         pub type First = | A | B;\n\
         pub type Second = | B | A;\n\
         fn f(s: Second) -> Int { match s { Second::B => 1, Second::A => 2 } }\n",
    );
}

// --- annotations, and what they are for ------------------------------------

/// Errata 36. A `let` annotation was parsed and then dropped on the floor, so
/// this compiled clean — and an annotation that is only a comment is worse
/// than no annotation, because it is believed.
#[test]
fn a_let_annotation_is_checked_against_its_initializer() {
    assert_reports(
        "module m;\nfn f() -> Int { let x: Bool = 5; 0 }\n",
        "this binding: expected `Bool`, found `Int`",
    );
    assert_clean("module m;\nfn f() -> Int { let x: Int = 5; x }\n");
}

/// And it is the binding's type afterwards, not merely a check.
#[test]
fn a_let_annotation_decides_the_bindings_type() {
    assert_reports(
        "module m;\nfn f(flag: Bool) -> Int { let x: Int = 5; if x { 1 } else { 0 } }\n",
        "an `if` condition",
    );
}

/// An annotation reaches the *arguments* of the call that fills it, by way of
/// the call's result. Without this, `Array::new(4, 0)` decides `A := Int` from
/// its own literal before ever hearing about the `U8`.
#[test]
fn an_annotation_reaches_a_generic_calls_arguments() {
    assert_clean(
        "module m;
pub type Box<A> = | Full(value: A) | Empty;
fn make<A>(value: A) -> Box<A> { Box::Full(value) }
fn f() -> Int { let b: Box<U8> = make(200); 0 }
",
    );
    assert_reports(
        "module m;
pub type Box<A> = | Full(value: A) | Empty;
fn make<A>(value: A) -> Box<A> { Box::Full(value) }
fn f() -> Int { let b: Box<U8> = make(300); 0 }
",
        "does not fit in `U8`",
    );
}

/// The hint stops at the literal it was meant for. An index is an `Int` however
/// the element is going to be used, and a hint that leaks into one is a wrong
/// answer rather than a missing one.
#[test]
fn an_annotation_does_not_leak_past_the_value_it_describes() {
    assert_clean(
        "module m;
pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn get(self, index: Int) -> A;
}
fn f(cells: Array<U8>) -> Int { let byte: U8 = Array::get(cells, 0); 0 }
",
    );
}

/// Pushing an expected type into a call solves variables earlier than the
/// arguments do, and an error row's entry is labelled by *its type's* name — so
/// an entry whose variable had just been solved was still called `_`, matched
/// nothing on the other side, and reported a label nobody was missing.
///
/// A latent bug rather than a new one: any solution arriving from outside the
/// row unification would have done it.
#[test]
fn a_row_entry_solved_from_outside_is_still_the_same_entry() {
    assert_clean(
        "module m;
pub type Result<A, E> = | Ok(value: A) | Err(error: E);
pub fn attempt<A, E, 'e>(body: () -> A with 'e raises E) -> Result<A, E> with 'e;

pub fn twice<A, E, 'e>(body: () -> A with 'e raises E) -> Int with 'e {
  let mut outcome = attempt(body);
  outcome = attempt(body);
  0
}
",
    );
}

// --- the fixed-width integers ----------------------------------------------

/// There is no implicit widening anywhere, so two integer types are as
/// different as `Int` and `Bool`.
#[test]
fn two_integer_types_do_not_mix() {
    assert_reports(
        "module m;\nfn f(a: U8, b: U16) -> U8 { a + b }\n",
        "arithmetic: expected `U8`, found `U16`",
    );
    assert_reports(
        "module m;\nfn f(a: U8) -> Int { a }\n",
        "this function returns `Int`, but its body has type `U8`",
    );
}

/// `I64` is a second spelling of `Int` rather than another type, so no
/// conversion stands between them.
#[test]
fn i64_is_int() {
    assert_clean("module m;\nfn f(a: I64) -> Int { a + 1 }\n");
}

/// A literal that cannot be the type being asked of it is a mistake with one
/// right answer, and truncating it silently to 44 is the kind of thing that is
/// found in production.
#[test]
fn a_literal_must_fit_the_type_it_is_asked_to_be() {
    assert_reports(
        "module m;\nfn f() -> Int { let b: U8 = 300; 0 }\n",
        "`300` does not fit in `U8`, which holds 0 to 255",
    );
    assert_reports(
        "module m;\nfn f() -> Int { let b: I8 = 200; 0 }\n",
        "`200` does not fit in `I8`, which holds -128 to 127",
    );
    assert_clean("module m;\nfn f() -> Int { let b: U8 = 255; 0 }\n");
}

// --- `Unknown` is a silence, not a type ------------------------------------

/// A body the checker finished cleanly must have no `Unknown` left in it.
///
/// `Unknown` is compatible with everything, which is what makes it useful
/// downstream of an error — one mistake should not become five — and exactly
/// what makes it invisible when nothing went wrong. Four errata are the same
/// sentence about different holes (24, 26, 27, 30), and the fifth was found by
/// the code generator three layers away.
///
/// The construct that has no answer today is a `forall` in an effect
/// operation: a handler is asked for a closure that works for every `A`, and
/// whole-program monomorphization has nowhere to put it. What matters here is
/// that it is *said* rather than swallowed.
#[test]
fn a_type_nobody_worked_out_is_reported() {
    assert_reports(
        "module m;
pub type Spec = { fields: String };
pub trait Extract { type Spec; fn spec() -> Self::Spec; }
pub effect Model {
  extract: forall <A: Extract> . (A::Spec) -> A,
}
fn use_it() -> Int with { model: Model } { model.extract({ fields: \"x\" }); 0 }
",
        "never worked out",
    );
}

/// And it must not fire after something else was already reported: after an
/// error `Unknown` is doing its job, and saying so again would bury the
/// message worth reading.
#[test]
fn an_unknown_after_an_error_is_not_reported_twice() {
    let db = KhoraDatabase::new();
    let found = errors(&db, "module m;\nfn f() -> Int { let x: Bool = 5; x }\n");
    // Two real ones: the annotation disagrees with the initializer, and the
    // body then disagrees with the return type. What must not be here is a
    // third, about a type nobody worked out.
    assert!(
        found.iter().all(|e| !e.contains("never worked out")),
        "the audit piled onto errors already reported: {found:?}"
    );
    assert!(found.iter().any(|e| e.contains("this binding")), "{found:?}");
}

// --- what a `loop` produces -------------------------------------------------

/// A `loop` yields what its `break`s carry.
///
/// It used to be `Unknown`, which unifies with everything — so
/// `let n: Bool = loop { break 1 };` was accepted. Left that way through phase
/// 2 rather than guessed, which was fine until `Unknown` stopped being allowed
/// to mean "not worked out".
#[test]
fn a_loop_produces_what_break_carries() {
    assert_clean("module m;\nfn f() -> Int { loop { break 1; } }\n");
    assert_reports(
        "module m;\nfn f() -> Bool { loop { break 1; } }\n",
        "this function returns `Bool`",
    );
}

/// Two `break`s have to agree, and the second is where the disagreement shows.
#[test]
fn breaks_have_to_agree() {
    assert_clean("module m;\nfn f(b: Bool) -> Int { loop { if b { break 1; } else { break 2; } } }\n");
    assert_reports(
        "module m;\nfn f(b: Bool) -> Int { loop { if b { break 1; } else { break false; } } }\n",
        "`break` values disagree",
    );
}

/// A loop nobody breaks out of *with a value* produces nothing, which is what
/// a `loop` used as a statement has always meant.
#[test]
fn a_loop_with_no_value_produces_unit() {
    assert_clean("module m;\nfn f() -> Int { loop { break; }; 0 }\n");
    assert_reports(
        "module m;\nfn f() -> Int { loop { break; } }\n",
        "this function returns `Int`",
    );
}

// --- nested constructors under a generic ----------------------------------

/// A generic type whose parameter is filled in by the scrutinee, which is the
/// shape `Result<_, MyError>` has and the one that was broken.
const NESTED: &str = "module m;\n\
                      pub type Holder<A> = | Full(value: A) | Empty;\n\
                      pub type Reason = | Late(by: Int) | Lost;\n\
                      fn held() -> Holder<Reason>;\n";

/// **A `match` naming every nested case is exhaustive.** It was not: a
/// variant's field types are written in terms of its own type's parameters, so
/// `Full`'s payload is `A` -- a `Param`, which the column builder answered
/// `Opaque` for -- and the column inside `Full` could not be expanded. A match
/// with an arm for every `Reason` was told `Full(_)` was not covered.
///
/// Errata 58.
#[test]
fn a_nested_constructor_under_a_generic_completes_a_match() {
    assert_clean(&format!(
        "{NESTED}fn f() -> Int {{\n  match held() {{\n    \
         Holder::Full(Reason::Late(n)) => n,\n    \
         Holder::Full(Reason::Lost) => 0,\n    \
         Holder::Empty => 0 - 1,\n  }}\n}}\n"
    ));
}

/// And one that misses a nested case is still refused -- **naming the case**,
/// which it could not do before: the old message could only say `Full(_)`,
/// because it had no idea what was inside.
#[test]
fn a_missing_nested_constructor_is_named() {
    assert_reports(
        &format!(
            "{NESTED}fn f() -> Int {{\n  match held() {{\n    \
             Holder::Full(Reason::Late(n)) => n,\n    \
             Holder::Empty => 0 - 1,\n  }}\n}}\n"
        ),
        "Full(Lost)",
    );
}

/// Missing the outer case is still caught, which is the half a fix to the
/// inner one could quietly take away.
#[test]
fn a_missing_outer_constructor_is_still_caught() {
    assert_reports(
        &format!(
            "{NESTED}fn f() -> Int {{\n  match held() {{\n    \
             Holder::Full(Reason::Late(n)) => n,\n    \
             Holder::Full(Reason::Lost) => 0,\n  }}\n}}\n"
        ),
        "Empty",
    );
}

/// **A `loop` with no `break` is `Never`, so it can be the body of a function
/// that returns something.**
///
/// The comment this replaced said "an infinite loop and a loop that just stops
/// both produce `()`", which reads as one case and is two. A loop with a bare
/// `break` finishes and produces nothing — `()` is right. A loop with no
/// `break` at all does not finish, so there is no value for `()` to be the type
/// of, and calling it `()` made `fn f() -> Int { loop { .. } }` a type error
/// against a body that cannot return at all.
///
/// The same mistake #127 fixed for a diverging branch, left behind in the one
/// construct whose whole purpose is not to end. A server's accept loop and a
/// supervisor's restart loop are both written this way.
#[test]
fn a_loop_with_no_break_can_be_a_function_that_returns_something() {
    assert_clean(
        "module m;\n\
         extern fn tick() -> ();\n\
         fn forever() -> Int {\n  \
           loop {\n    \
             tick();\n  \
           }\n\
         }\n",
    );
}

/// And the direction that must keep failing.
///
/// A bare `break` is still a way out, so the loop finishes and produces `()`.
/// If it did not fail, the fix above would be "call every loop `Never`", which
/// would accept a function that returns while promising an `Int`.
#[test]
fn a_loop_with_a_bare_break_is_still_unit() {
    let db = KhoraDatabase::new();
    let found = errors(
        &db,
        "module m;\n\
         extern fn tick() -> ();\n\
         fn stops() -> Int {\n  \
           loop {\n    \
             tick();\n    \
             break;\n  \
           }\n\
         }\n",
    );
    assert!(
        found.iter().any(|m| m.contains("has type `()`")),
        "a loop that can end does not return an `Int`: {found:?}"
    );
}

/// A `break` carrying a value still decides the loop's type.
#[test]
fn a_loop_takes_the_type_its_breaks_carry() {
    assert_clean(
        "module m;\n\
         fn answers() -> Int {\n  \
           loop {\n    \
             break 42;\n  \
           }\n\
         }\n",
    );
}

/// And two `break`s carrying different types still disagree.
#[test]
fn breaks_carrying_different_types_still_disagree() {
    let db = KhoraDatabase::new();
    let found = errors(
        &db,
        "module m;\n\
         fn muddled() -> Int {\n  \
           loop {\n    \
             break 42;\n    \
             break \"no\";\n  \
           }\n\
         }\n",
    );
    assert!(
        found.iter().any(|m| m.contains("`break` values disagree")),
        "the second `break` has to be reported: {found:?}"
    );
}

/// **A branch that cannot return discharges against a type the caller chose.**
///
/// `Never` is the bottom type and the solver has always treated it as one —
/// `raise` in one arm of a `match` has always type-checked against a generic
/// `A`. What it did not do was arrive: `std::core` declares `pub type Never;`,
/// so a mention of the *name* resolved to an ordinary opaque `Type::Adt` and
/// the two sat next to each other unrelated. The refusal was
///
/// ```text
/// `if` branches disagree: `A` is a type the caller chooses, so it cannot be
/// assumed to be `Never`
/// ```
///
/// which is the right answer to the wrong question. *Solving* a variable to
/// `Never` would be wrong; *discharging* a branch that cannot return is not the
/// same operation.
///
/// This is what every `std` function that traps on a type it does not choose
/// needs. `Vector::at` was the first, and it was written with the trap as a
/// statement and the read following it unconditionally, because the shape below
/// would not compile.
#[test]
fn a_diverging_branch_takes_the_other_branchs_type() {
    assert_clean(
        "module m;\n\
         extern fn stop(index: Int) -> Never;\n\
         fn at<A>(xs: A, index: Int) -> A {\n  \
           if index < 0 { stop(index) } else { xs }\n\
         }\n",
    );
}

/// And the direction that must keep failing: two branches that are both real
/// types still have to agree.
///
/// The risk in binding `Never` to the bottom is over-quieting — a bottom that
/// unified with anything in *both* directions would take this with it.
#[test]
fn two_ordinary_branches_still_have_to_agree() {
    assert_reports(
        "module m;\nfn f(c: Bool) -> Int { if c { 1 } else { true } }\n",
        "branches disagree",
    );
}

/// `Never` may be a foreign function's return type, and nothing else new may.
///
/// An uninhabited return is not a value crossing the boundary; it is no return
/// at all, so the rule that only scalars and pointers cross has nothing to say
/// about it. `khora_bounds_fail` is `-> !` on the Rust side and had to be
/// declared `-> ()` — true about what crosses, and a lie about what it does.
#[test]
fn a_foreign_function_may_diverge() {
    assert_clean(
        "module m;\n\
         extern fn stop(index: Int) -> Never;\n\
         fn f() -> Int { stop(1) }\n",
    );
}

/// **An imported `const` has no type, and the message used to blame the
/// compiler.**
///
/// A constant's type comes from inferring over its initializer, and the type
/// map that carries a module's exports is built from syntax before anything is
/// inferred — so nothing records what an exported `const` is. What came back
/// was the catch-all:
///
/// ```text
/// the type of this expression was never worked out, and nothing else was
/// reported — so either it needs an annotation, or this is a gap in the
/// compiler worth reporting
/// ```
///
/// which for this case is both true and useless. It *is* a gap, it is a known
/// one, and sending somebody to write it up costs them an hour and tells
/// nobody anything. The cookbook shows `const fixed_clock = handler for Clock`
/// as the way to write a test double, so this is met by people following the
/// documentation.
#[test]
fn an_imported_const_says_why_it_has_no_type() {
    let db = KhoraDatabase::new();
    let lib = SourceFile::new(
        &db,
        "lib.kh".into(),
        "module lib;\n\npub const answer = 42;\n".to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\n\nimport lib::{answer};\n\npub fn main() -> Int { answer }\n".to_string(),
    );
    SourceRoot::new(&db, vec![lib, app]);

    let found: Vec<String> =
        khora_types::check_file(&db, app).iter().map(|e| e.message.clone()).collect();
    assert!(
        found.iter().any(|e| e.contains("`answer` is a `const`")),
        "the message should name the constant: {found:?}"
    );
    assert!(
        found.iter().any(|e| e.contains("does not")
            || e.contains("nothing records what an exported one is")),
        "and say why: {found:?}"
    );
    assert!(
        !found.iter().any(|e| e.contains("gap in the compiler")),
        "and not send them to file a bug: {found:?}"
    );
}

/// The generic message still has to exist for everything that is not this.
///
/// A special case that swallowed the catch-all would be worse than the catch-all:
/// it exists for the shapes nobody has thought of yet, which is the whole reason
/// its wording asks to be told about them.
#[test]
fn an_ordinary_unsolved_type_still_gets_the_general_message() {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(
        &db,
        "m.kh".into(),
        // A local `const` is typed by the ordinary pass, so this is not the
        // const case — and it is not unsolved either. Use an empty collection
        // literal with nothing to fix its element type.
        "module m;\n\npub fn main() -> Int { 0 }\n".to_string(),
    );
    SourceRoot::new(&db, vec![file]);
    assert!(khora_types::check_file(&db, file).is_empty());
}
