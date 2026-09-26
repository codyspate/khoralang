//! A `raise` of a value whose type is still being inferred.
//!
//! **Such a raise was charged to no row.** `raise e`, with `e` a lambda
//! parameter nothing had typed yet when the `catch` beside it was checked,
//! pushed no demand: the `catch` did not see it, the closure's row did not
//! carry it, and the error escaped a function the checker had called
//! infallible -- exit 130 and no message, and later a trap naming a compiler
//! bug. The raise is charged once the type is known, under the rules a typed
//! raise meets; a type that is never worked out is refused.

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

const TYPES: &str = "module m;\n\
    pub type Gx<A> = | X(s: String, v: A) | Y(n: Int);\n\
    pub type Nf = { p: String };\n\
    pub type Dn = { q: Int };\n\
    fn fs() -> Int raises Gx<String> { raise Gx::X(\"s\", \"str\") }\n\
    fn fi() -> Int raises Gx<Int> { raise Gx::X(\"i\", 3) }\n\
    fn nf() -> Int raises Nf { raise { p: \"x\" } }\n";

/// The non-generic form: `e` turns out a `Dn`, which the `catch` does not
/// name, so the closure raises `Dn` and `work`, which declares nothing, is
/// refused for calling it.
#[test]
fn an_untyped_raise_the_catch_does_not_name_reaches_the_enclosing_row() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ nf()! }}) catch {{ Nf {{ p }} => 1 }}; \
             let d: Dn = {{ q: 4 }}; k(d) }}\n"
        ),
        "Dn",
    );
}

/// The generic form: `e` turns out a `Gx<Int>`, and the arm was bound at
/// the `Gx<String>` `fs` raises -- the refusal a typed raise gets there.
#[test]
fn an_untyped_raise_at_another_instantiation_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ fs()! }}) catch {{ \
             Gx::X(s, v) => 1, Gx::Y(n) => n }}; \
             k(Gx::X(\"a\", 3)) }}\n"
        ),
        "raises two",
    );
}

/// The same program declared: the closure's row now carries `Dn`, so a
/// `work` that says it raises `Dn` is accepted. Passes with the fix disabled
/// too (a `!` on a call that cannot fail is not refused), so it pins only
/// that nothing new is refused; the test below is the one that goes red.
#[test]
fn an_untyped_raise_is_carried_by_the_closures_row() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Dn {{ let b = true; \
         let k = fn (e) => (if b {{ raise e }} else {{ nf()! }}) catch {{ Nf {{ p }} => 1 }}; \
         let d: Dn = {{ q: 4 }}; k(d)! }}\n"
    ));
}

/// The closure can now fail, so calling it has to wear the mark.
#[test]
fn a_closure_carrying_an_untyped_raise_needs_the_mark() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int raises Dn {{ let b = true; \
             let k = fn (e) => (if b {{ raise e }} else {{ nf()! }}) catch {{ Nf {{ p }} => 1 }}; \
             let d: Dn = {{ q: 4 }}; k(d) }}\n"
        ),
        "needs `!`",
    );
}

/// The instantiation the `catch` was built for is handled, and nothing
/// escapes: the correct program stays correct.
#[test]
fn an_untyped_raise_the_catch_names_is_handled() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int {{ let b = true; \
         let k = fn (e) => (if b {{ raise e }} else {{ fi()! }}) catch {{ \
         Gx::X(s, v) => v * 10, Gx::Y(n) => n }}; \
         k(Gx::X(\"a\", 3)) }}\n"
    ));
}

#[test]
fn an_untyped_raise_under_a_wildcard_is_handled() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int {{ let b = true; \
         let k = fn (e) => (if b {{ raise e }} else {{ nf()! }}) catch {{ _ => 1 }}; \
         let d: Dn = {{ q: 4 }}; k(d) }}\n"
    ));
}

/// A value nothing ever types is refused, not charged to nothing.
#[test]
fn a_raised_value_never_worked_out_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int {{ let k = fn (e) => {{ raise e }}; 0 }}\n"
        ),
        "the type of this raised value was never worked out",
    );
}

/// Alone in its operand, the raise still leaves the `catch` with nothing it
/// knows is raised when the arms are checked; this stays refused.
#[test]
fn an_untyped_raise_alone_in_a_catch_is_still_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int {{ \
             let k = fn (e) => (raise e) catch {{ Gx::X(s, v) => 1, Gx::Y(n) => n }}; \
             k(Gx::X(\"a\", 3)) }}\n"
        ),
        "nothing in this expression raises `Gx`",
    );
}
