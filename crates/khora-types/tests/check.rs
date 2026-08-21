//! Type checking over real Khora source.

use khora_db::{Db, KhoraDatabase, SourceFile};
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

const ADT: &str = "module m;\nexport type R = | A | B(n: Int) | C;\n";

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
               export type Inner = | X | Y;
               export type Outer = | Wrap(i: Inner) | Empty;
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
               export type Inner = | X | Y;
               export type Outer = | Wrap(i: Inner) | Empty;
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
               export type List = | Nil | Cons(head: Int, tail: List);
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
         export type Option<A> = | Some(value: A) | None;\n\
         export type Maybe<A> = | Some(value: A) | None;\n\
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
         export type Color = | Red | Green;\n\
         export type Fruit = | Apple;\n\
         fn f() -> Fruit { Fruit::Red }\n"
            .to_string(),
    );
    let found: Vec<String> =
        khora_types::diagnostics(&db, file).iter().map(|e| e.message.clone()).collect();
    assert!(
        found.iter().any(|e| e.contains("cannot resolve `Fruit::Red`")),
        "expected the path to be rejected, got {found:?}"
    );
}

/// Matching is qualified too, so an arm cannot name another type's case.
#[test]
fn a_pattern_names_its_own_types_cases() {
    assert_clean(
        "module m;\n\
         export type First = | A | B;\n\
         export type Second = | B | A;\n\
         fn f(s: Second) -> Int { match s { Second::B => 1, Second::A => 2 } }\n",
    );
}
