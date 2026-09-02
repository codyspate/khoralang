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

/// The return type reaches the lambda as its expectation, so the disagreement
/// is reported where it is: at the body that answers `Int` to a signature
/// that asked for `Bool`, rather than against the whole function.
#[test]
fn a_lambda_of_the_wrong_shape_is_rejected() {
    assert_reports(
        "module m;\nfn f() -> (Int) -> Bool { fn x => x + 1 }\n",
        "this closure's body: expected `Bool`, found `Int`",
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
         pub type Handler = | Of(run: (Int) -> Int);\n\
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

// --- what a lambda knows before its body runs ------------------------------
//
// `expect` works out what an argument has to be before inferring it, and the
// lambda arm used to ignore that and only meet it in the `require` at the end
// — too late for anything inside the body. A `match` destructuring the
// parameter then bound its name against a type that was still a variable,
// which `bind_pattern` cannot take apart, so the binding got `Unknown` and
// every field read off it did too. Silently, because an unsolved owner is the
// one case a field read declines to complain about; the program failed the
// `Unknown` audit at the end instead, pointing at a line with nothing wrong
// with it.
//
// Found by writing the link shortener, which is what it took.

const RECORD: &str = "module m;\n\
                      pub type Link = { code: String, hits: Int };\n\
                      pub type Held = | Nothing | Just(link: Link);\n";

/// The one that failed. A field read on a pattern binding, inside a `match`,
/// inside a lambda whose parameter type comes from the callee's signature.
#[test]
fn a_lambda_knows_its_parameter_before_its_body() {
    assert_clean(&format!(
        "{RECORD}\
         fn apply(v: Held, f: (Held) -> Held) -> Held {{ f(v) }}\n\
         pub fn bump(h: Held) -> Held {{\n\
           apply(h, fn held => match held {{\n\
             Held::Nothing => Held::Nothing,\n\
             Held::Just(found) => Held::Just({{ code: found.code, hits: found.hits + 1 }}),\n\
           }})\n\
         }}\n"
    ));
}

/// The same through a generic parameter, which is how `Shared::update` and
/// every other higher-order function in `std` are declared.
#[test]
fn a_generic_callee_pins_the_parameter_too() {
    assert_clean(&format!(
        "{RECORD}\
         fn apply<A>(v: A, f: (A) -> A) -> A {{ f(v) }}\n\
         pub fn bump(h: Held) -> Held {{\n\
           apply(h, fn held => match held {{\n\
             Held::Nothing => Held::Nothing,\n\
             Held::Just(found) => Held::Just({{ code: found.code, hits: found.hits + 1 }}),\n\
           }})\n\
         }}\n"
    ));
}

/// Reading a field is what exposed it, but the binding was untyped whatever
/// was done with it — this is the shortest version.
#[test]
fn a_pattern_binding_in_a_lambda_has_a_type() {
    assert_clean(&format!(
        "{RECORD}\
         fn count(v: Held, f: (Held) -> Int) -> Int {{ f(v) }}\n\
         pub fn hits(h: Held) -> Int {{\n\
           count(h, fn held => match held {{ Held::Nothing => 0, Held::Just(g) => g.hits }})\n\
         }}\n"
    ));
}

/// The expected type is a hint, not a demand: a lambda that disagrees with it
/// is still reported, and reported against itself rather than swallowed by the
/// early unification.
#[test]
fn a_lambda_that_disagrees_with_the_expected_type_is_still_reported() {
    assert_reports(
        &format!(
            "{RECORD}\
             fn count(v: Held, f: (Held) -> Int) -> Int {{ f(v) }}\n\
             pub fn hits(h: Held) -> Int {{ count(h, fn held => \"not a number\") }}\n"
        ),
        "String",
    );
}

/// Nothing to pin it against is still allowed: a lambda in a `let` with no
/// annotation is solved by how it is used, which is what it always was.
#[test]
fn a_lambda_with_no_expected_type_is_still_inferred_from_use() {
    assert_clean(
        "module m;\n\
         pub fn twice(n: Int) -> Int { let f = fn x => x + 1; f(f(n)) }\n",
    );
}

/// An annotation on a closure parameter is the parameter's type.
///
/// It was dropped in lowering — every parameter got a fresh variable and the
/// written type was never read — so `fn (s: String) => s + "b"` was checked as
/// though `String` had not been said, `+` defaulted the variable to `Int`, and
/// the report was `arithmetic: expected Int, found String` against a line that
/// says `String` on its face.
///
/// A closure in a `let` is the case that cannot recover: there is no call yet
/// to hint from, so the annotation is the only evidence there is.
#[test]
fn an_annotated_closure_parameter_keeps_its_annotation() {
    assert_clean(
        "module m;\n\
         pub fn f() -> String { let g = fn (s: String) => s + \"b\"; g(\"a\") }\n",
    );
    // And it is still *checked*, rather than merely believed.
    assert_reports(
        "module m;\n\
         pub fn f() -> Int { let g = fn (s: Int) => s + \"b\"; g(1) }\n",
        "arithmetic: expected `Int`, found `String`",
    );
    assert_reports(
        "module m;\n\
         fn ap(f: (String) -> String) -> String { f(\"x\") }\n\
         pub fn g() -> String { ap(fn (s: Int) => s + 1) }\n",
        "`String` does not match `Int`",
    );
}

/// `+` on a `String` is concatenation wherever the string arrives from.
///
/// The check ran against the *unzonked* left operand, so it recognised a string
/// only when one was written literally. A `String` reaching `+` as a solved
/// inference variable — which is what a closure parameter is — fell through to
/// the arithmetic path and was reported as `expected Int, found String`.
#[test]
fn concatenation_does_not_depend_on_how_the_string_arrived() {
    assert_clean(
        "module m;\n\
         fn ap(f: (String) -> String) -> String { f(\"x\") }\n\
         pub fn g() -> String { ap(fn s => s + \"b\") }\n",
    );
    assert_clean(
        "module m;\n\
         fn ap<A>(f: (A) -> A, x: A) -> A { f(x) }\n\
         pub fn g() -> String { ap(fn (s: String) => s + \"b\", \"a\") }\n",
    );
    // Nested, because the inner closure's operand comes from the outer one's
    // parameter rather than from its own.
    assert_clean(
        "module m;\n\
         pub fn f() -> String {\n\
           let g = fn (s: String) => { let h = fn (t: String) => t + s; h(\"x\") };\n\
           g(\"y\")\n\
         }\n",
    );
}

/// An unsolved left operand takes its answer from the right rather than
/// defaulting.
///
/// `fn s => s + "b"` has nothing on the left to go on. Defaulting it to `Int`
/// and then reporting the string literal as the mismatch named the wrong
/// operand: there is no `Int + String`, so a `String` on the right settles it.
#[test]
fn an_unsolved_operand_learns_from_the_other_side() {
    assert_clean(
        "module m;\n\
         pub fn f() -> String { let g = fn s => s + \"b\"; g(\"a\") }\n",
    );
    // Arithmetic still defaults the same way it always did.
    assert_clean(
        "module m;\n\
         pub fn f() -> Int { let g = fn s => s + 1; g(2) }\n",
    );
    // And a caller that disagrees with what the body settled is still caught.
    assert_reports(
        "module m;\n\
         pub fn f() -> Int { let g = fn s => s + \"b\"; g(1) }\n",
        "expected `String`, found `Int`",
    );
}
