//! The type of a `catch` whose operand never finishes.
//!
//! **A `catch`'s type is the join of its operand's and its arms', and a
//! `Never` operand contributes nothing to it.** Typed from the operand alone,
//! `(raise F::W) catch { F::W => 7 }` was a `Never`: a `let` of it was
//! refused, and in any other value position -- an interpolation, a tail, an
//! argument -- code generation believed there was no value, kept no slot for
//! the arm's, and the program read 0.
//!
//! A `Never` fits wherever any type is wanted, so the observable here is the
//! opposite one: the `catch` used where a `String` is wanted must be refused
//! as the `Int` its arms produce. Accepting it is the old typing.

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

/// Asserts `catch` is typed `Int`: bound where a `String` is wanted, it is
/// refused, and the refusal names the `Int`.
fn assert_is_int(catch: &str) {
    let text = format!("{TYPES}fn f() -> Int {{ let s: String = {catch}; 0 }}\n");
    let found = errors(&text);
    assert!(
        found.iter().any(|e| e.contains("`String`") && e.contains("`Int`")),
        "expected the `catch` to be an `Int`, got {found:?}\n{text}"
    );
}

const TYPES: &str = "module m;\n\
    pub type F = | Z(n: Int) | W;\n\
    pub type E = | X | Y;\n";

#[test]
fn a_catch_over_a_bare_raise_is_its_arms_type() {
    assert_is_int("(raise F::W) catch { F::W => 7, F::Z(k) => k }");
}

#[test]
fn a_catch_over_a_raising_block_is_its_arms_type() {
    assert_is_int("({ let x = 1; raise F::Z(x) }) catch { F::W => 7, F::Z(k) => k }");
}

#[test]
fn a_nested_catch_over_a_raise_is_its_arms_type() {
    assert_is_int(
        "(raise E::X) catch { E::X => (raise F::W) catch { F::W => 7, F::Z(k) => k }, E::Y => 1 }",
    );
}

/// The outer operand is `Never` because *its* arms diverge.
#[test]
fn a_catch_whose_operand_is_a_diverging_catch_is_its_arms_type() {
    assert_is_int("((raise F::W) catch { F::W => raise E::X, F::Z(_) => raise E::Y }) catch { E::X => 1, E::Y => 2 }");
}

/// Accepted with the fix and without it: the `let` is untyped, and `Never`
/// fits anywhere, so this pins only that nothing new is refused. The LLVM
/// tests pin the value it yields.
#[test]
fn a_let_of_a_catch_over_a_raise_is_accepted() {
    assert_clean(&format!(
        "{TYPES}fn f() -> Int {{ let v = (raise F::W) catch {{ F::W => 7, F::Z(k) => k }}; v + 1 }}\n"
    ));
}

/// Every arm diverging leaves nothing to join, so the whole `catch` does not
/// finish, and fits an `Int` tail as `Never` does. Passes either way too.
#[test]
fn a_catch_whose_arms_all_diverge_is_still_never() {
    assert_clean(&format!(
        "{TYPES}fn f() -> Int raises E {{ (raise F::W) catch {{ F::W => raise E::X, F::Z(_) => return 3 }} }}\n"
    ));
}
