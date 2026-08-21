//! Tuple types.
//!
//! Tuples parsed and lowered long before they were typed, so every use of one
//! silently type checked as `Unknown` — which accepts anything. These tests pin
//! the width and the component types.

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

#[test]
fn a_tuple_literal_has_the_type_of_its_parts() {
    assert_clean("module m;\nfn f() -> (Int, Bool) { (1, true) }\n");
}

#[test]
fn a_component_of_the_wrong_type_is_rejected() {
    assert_reports(
        "module m;\nfn f() -> (Int, Bool) { (1, 2) }\n",
        "returns `(Int, Bool)`, but its body has type `(Int, Int)`",
    );
}

#[test]
fn a_tuple_of_the_wrong_width_is_rejected() {
    assert_reports(
        "module m;\nfn f() -> (Int, Bool) { (1, true, 3) }\n",
        "returns `(Int, Bool)`, but its body has type `(Int, Bool, Int)`",
    );
}

#[test]
fn tuples_nest() {
    assert_clean("module m;\nfn f() -> (Int, (Bool, String)) { (1, (true, \"a\")) }\n");
    assert_reports(
        "module m;\nfn f() -> (Int, (Bool, String)) { (1, (true, 2)) }\n",
        "`String` does not match `Int`",
    );
}

/// `()` is `Unit`, not a zero-width tuple: one spelling of "no information".
#[test]
fn the_empty_tuple_is_unit() {
    assert_clean("module m;\nfn f() -> () { () }\n");
}

#[test]
fn a_tuple_flows_through_a_parameter() {
    assert_clean("module m;\nfn f(p: (Int, Bool)) -> (Int, Bool) { p }\n");
    assert_reports(
        "module m;\nfn f(p: (Int, Bool)) -> (Bool, Int) { p }\n",
        "returns `(Bool, Int)`, but its body has type `(Int, Bool)`",
    );
}

#[test]
fn a_tuple_can_carry_a_type_parameter() {
    assert_clean(
        "module m;\n\
         fn pair<A>(x: A) -> (A, A) { (x, x) }\n\
         fn f() -> (Int, Int) { pair(1) }\n",
    );
    assert_reports(
        "module m;\n\
         fn pair<A>(x: A) -> (A, A) { (x, x) }\n\
         fn f() -> (Int, Bool) { pair(1) }\n",
        "returns `(Int, Bool)`",
    );
}

/// Destructuring binds each name at its own component's type, not `Unknown`.
#[test]
fn destructuring_binds_each_component_at_its_own_type() {
    assert_clean(
        "module m;\n\
         fn f(p: (Int, Bool)) -> Int {\n\
           let (n, flag) = p;\n\
           if flag { n } else { 0 }\n\
         }\n",
    );
    assert_reports(
        "module m;\n\
         fn f(p: (Int, Bool)) -> Int {\n\
           let (n, flag) = p;\n\
           if n { 1 } else { 0 }\n\
         }\n",
        "`Bool`",
    );
}

/// Tuples in a type argument are what make a tensor shape checkable.
#[test]
fn a_tuple_in_a_type_argument_is_compared_componentwise() {
    let shapes = "module m;\n\
                  pub type Tensor<S>;\n\
                  fn f(t: Tensor<(2, 3)>) -> Tensor<(2, 3)> { t }\n";
    assert_clean(shapes);
    assert_reports(
        "module m;\n\
         pub type Tensor<S>;\n\
         fn f(t: Tensor<(2, 3)>) -> Tensor<(2, 4)> { t }\n",
        "returns `Tensor<(2, 4)>`, but its body has type `Tensor<(2, 3)>`",
    );
}
