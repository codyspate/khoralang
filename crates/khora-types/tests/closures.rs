//! Closures: inference, function types, and calling a value.

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

#[test]
fn a_lambda_has_a_function_type() {
    assert_clean("module m;\nfn f() -> (Int) -> Int { fn x => x + 1 }\n");
}

#[test]
fn a_lambda_of_the_wrong_shape_is_rejected() {
    assert_reports(
        "module m;\nfn f() -> (Int) -> Bool { fn x => x + 1 }\n",
        "returns `(Int) -> Bool`",
    );
}

/// A parameter with no annotation takes its type from how the lambda is used.
#[test]
fn a_parameter_type_comes_from_the_use_site() {
    assert_clean(
        "module m;\n\
         fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }\n\
         fn g() -> Int { apply(fn n => n * 2, 3) }\n",
    );
    assert_reports(
        "module m;\n\
         fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }\n\
         fn g() -> Int { apply(fn n => n && true, 3) }\n",
        "`Bool`",
    );
}

#[test]
fn calling_a_closure_checks_its_arguments() {
    assert_reports(
        "module m;\nfn f() -> Int { let g = fn x => x + 1; g(true) }\n",
        "this argument",
    );
    assert_reports(
        "module m;\nfn f() -> Int { let g = fn x => x + 1; g(1, 2) }\n",
        "takes 1 argument(s), but 2 were given",
    );
}

/// Reachable only now that functions are values: before, every callee was a
/// path and the question could not come up.
#[test]
fn calling_something_that_is_not_a_function_is_rejected() {
    assert_reports(
        "module m;\nfn f() -> Int { let n = 1; n(2) }\n",
        "`Int` is not a function, so it cannot be called",
    );
}

#[test]
fn a_zero_argument_function_type_is_written_with_empty_parentheses() {
    assert_clean("module m;\nfn f(g: () -> Int) -> Int { g() }\n");
}

#[test]
fn a_function_type_of_several_arguments_is_a_tuple_in_the_syntax() {
    assert_clean(
        "module m;\n\
         fn f(g: (Int, Bool) -> Int) -> Int { g(1, true) }\n",
    );
    assert_reports(
        "module m;\n\
         fn f(g: (Int, Bool) -> Int) -> Int { g(true, 1) }\n",
        "this argument",
    );
}

/// A closure returned from a function outlives the frame it was written in,
/// which is the whole reason it captures by value.
#[test]
fn a_closure_can_be_returned() {
    assert_clean("module m;\nfn make(n: Int) -> (Int) -> Int { fn x => x + n }\n");
}

#[test]
fn a_named_function_is_a_value_of_its_own_type() {
    assert_clean(
        "module m;\n\
         fn double(x: Int) -> Int { x * 2 }\n\
         fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }\n\
         fn g() -> Int { apply(double, 21) }\n",
    );
    assert_reports(
        "module m;\n\
         fn double(x: Int) -> Int { x * 2 }\n\
         fn apply(f: (Bool) -> Int, x: Bool) -> Int { f(x) }\n\
         fn g() -> Int { apply(double, true) }\n",
        "this argument",
    );
}

/// A capture is a copy. Assigning to one would change the closure's copy and
/// nothing else, which is worth refusing rather than silently doing.
#[test]
fn assigning_to_a_capture_is_rejected() {
    assert_reports(
        "module m;\n\
         fn f() -> Int { let mut n = 0; let g = fn x => { n = x; n }; g(1) }\n",
        "captured by value",
    );
}

/// A lambda's own parameter is not a capture, so mutating one is only subject
/// to the ordinary rule about `mut`.
#[test]
fn a_lambda_parameter_follows_the_ordinary_mutability_rule() {
    assert_reports(
        "module m;\nfn f() -> Int { let g = fn x => { x = 1; x }; g(0) }\n",
        "not declared `mut`",
    );
}

#[test]
fn a_closure_can_be_stored_in_an_adt() {
    assert_clean(
        "module m;\n\
         export type Handler = | Of(run: (Int) -> Int);\n\
         fn make() -> Handler { Handler::Of(fn x => x + 1) }\n",
    );
}

// --- a closure that calls itself ------------------------------------------

/// A `let` initializer cannot see its own binding, which is why `let x = x`
/// means the outer `x`. A lambda is the exception, or a recursive closure
/// could not be written at all.
#[test]
fn a_closure_can_call_itself() {
    assert_clean(
        "module m;\n\
         fn f() -> Int {\n\
           let go = fn n => if n == 0 { 0 } else { n + go(n - 1) };\n\
           go(3)\n\
         }\n",
    );
}

/// The result type is a variable the body solves, so it flows out to the use
/// site rather than being assumed.
#[test]
fn a_recursive_closures_result_type_is_inferred() {
    assert_reports(
        "module m;\n\
         fn f() -> Int {\n\
           let go = fn n => if n == 0 { true } else { go(n - 1) };\n\
           go(3) + 1\n\
         }\n",
        "arithmetic: expected `Int`, found `Bool`",
    );
}

#[test]
fn a_recursive_call_is_checked_like_any_other() {
    assert_reports(
        "module m;\n\
         fn f() -> Int { let go = fn n => if n == 0 { 0 } else { go(true) }; go(3) }\n",
        "this argument: expected `Int`, found `Bool`",
    );
    assert_reports(
        "module m;\n\
         fn f() -> Int { let go = fn n => if n == 0 { 0 } else { go(n - 1, 2) }; go(3) }\n",
        "takes 1 argument(s), but 2 were given",
    );
}

/// Only the innermost closure's own name is in scope. Reaching an outer one
/// would capture it, and a closure holding a closure that holds it is a cycle
/// — which reference counting does not collect. See `docs/design/memory.md`.
#[test]
fn an_inner_closure_cannot_name_an_outer_one() {
    assert_reports(
        "module m;\n\
         fn f() -> Int { let outer = fn x => fn y => outer(y); 0 }\n",
        "only a closure's own name is in scope inside it",
    );
}

/// A lambda that is not bound by a `let` has no name to recurse through, so
/// nothing changes for it.
#[test]
fn an_unnamed_lambda_has_no_self_reference() {
    assert_reports(
        "module m;\n\
         fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }\n\
         fn g() -> Int { apply(fn n => nope(n), 1) }\n",
        "cannot find `nope` in this scope",
    );
}
