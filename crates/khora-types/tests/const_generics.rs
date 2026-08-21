//! Const generics: dimensions checked at compile time.
//!
//! `docs/roadmap.md` names this as phase 3's exit criterion — a `matmul` whose
//! shared dimension does not agree must be a compile error, not a runtime
//! assertion.

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

/// Shapes are part of the type, so the compiler can see a bad product.
const MATRIX: &str = "module m;\n\
                      pub type Matrix<const R: Int, const C: Int>;\n\
                      fn matmul<const M: Int, const K: Int, const N: Int>(\n\
                        a: Matrix<M, K>, b: Matrix<K, N>\n\
                      ) -> Matrix<M, N>;\n";

#[test]
fn a_matching_shared_dimension_is_accepted() {
    assert_clean(&format!(
        "{MATRIX}fn f(a: Matrix<2, 3>, b: Matrix<3, 4>) -> Matrix<2, 4> {{ matmul(a, b) }}\n"
    ));
}

/// The exit criterion: the error names both dimensions.
#[test]
fn a_mismatched_shared_dimension_is_a_compile_error() {
    let source = format!(
        "{MATRIX}fn f(a: Matrix<2, 3>, b: Matrix<4, 5>) -> Matrix<2, 5> {{ matmul(a, b) }}\n"
    );
    assert_reports(&source, "this argument");

    let db = KhoraDatabase::new();
    let found = errors(&db, &source);
    let message = found.join(" ");
    assert!(
        message.contains('3') && message.contains('4'),
        "the diagnostic should name both dimensions, got {found:?}"
    );
}

#[test]
fn the_result_shape_follows_from_the_arguments() {
    assert_reports(
        &format!(
            "{MATRIX}fn f(a: Matrix<2, 3>, b: Matrix<3, 4>) -> Matrix<9, 9> {{ matmul(a, b) }}\n"
        ),
        "returns `Matrix<9, 9>`",
    );
}

#[test]
fn two_shapes_of_the_same_type_are_distinct() {
    assert_reports(
        "module m;\n\
         pub type Vector<const N: Int>;\n\
         fn f(v: Vector<3>) -> Vector<4> { v }\n",
        "returns `Vector<4>`, but its body has type `Vector<3>`",
    );
}

#[test]
fn a_const_parameter_is_rigid_inside_the_body() {
    // `N` is chosen by the caller, so the body cannot assume it is 3.
    assert_reports(
        "module m;\n\
         pub type Vector<const N: Int>;\n\
         fn to_three<const N: Int>(v: Vector<N>) -> Vector<3> { v }\n",
        "the caller chooses",
    );
}

#[test]
fn a_const_generic_function_serves_several_shapes() {
    assert_clean(
        "module m;\n\
         pub type Vector<const N: Int>;\n\
         fn id<const N: Int>(v: Vector<N>) -> Vector<N> { v }\n\
         fn a(v: Vector<3>) -> Vector<3> { id(v) }\n\
         fn b(v: Vector<7>) -> Vector<7> { id(v) }\n",
    );
}

#[test]
fn a_shape_and_a_type_argument_coexist() {
    assert_clean(
        "module m;\n\
         pub type Buffer<const N: Int, T>;\n\
         fn first<const N: Int, T>(b: Buffer<N, T>) -> Buffer<N, T> { b }\n\
         fn f(b: Buffer<8, Int>) -> Buffer<8, Int> { first(b) }\n",
    );
    assert_reports(
        "module m;\n\
         pub type Buffer<const N: Int, T>;\n\
         fn f(b: Buffer<8, Int>) -> Buffer<8, Bool> { b }\n",
        "returns `Buffer<8, Bool>`",
    );
}

/// A conflict buried inside a type argument must lead with the types the
/// programmer wrote. "expected `3`, found `4`" alone leaves them hunting for
/// where either number came from.
#[test]
fn a_nested_mismatch_names_the_whole_type_first() {
    assert_reports(
        &format!(
            "{MATRIX}fn f(a: Matrix<2, 3>, b: Matrix<4, 5>) -> Matrix<2, 5> {{ matmul(a, b) }}
"
        ),
        "expected `Matrix<3, _>`, found `Matrix<4, 5>`; dimension `3` does not match `4`",
    );
}

/// An argument the checker has not pinned down reads as `_`, which is what a
/// Rust or TypeScript developer already understands. `?7` is compiler trivia.
#[test]
fn an_unsolved_argument_reads_as_an_underscore() {
    let db = KhoraDatabase::new();
    let found = errors(
        &db,
        "module m;
         pub type Vector<const N: Int>;
         fn id<const N: Int>(v: Vector<N>) -> Vector<N> { v }
         fn f(v: Vector<3>) -> Int { id(v) }
",
    );
    let message = found.join(" ");
    assert!(!message.contains('?'), "internal variable numbering leaked: {found:?}");
}
