//! Type checking the element forms of a list literal.
//!
//! **What these guard: the first `[if c { x }]` anybody writes.** It is the
//! Khora `if`, so it is what a reader tries first, and it is a one-element
//! list holding the value of an `if` with no `else` -- a type error unless `x`
//! is `()`. The error is right; what it must also do is name the spelling
//! the reader was reaching for.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_types::check_file;

const LIST: &str = "module m;
pub type List<A> = | Nil | Cons(A, List<A>);
impl<A> List<A> {
  fn reverse_onto(self, acc: List<A>) -> List<A> { acc }
}
";

fn errors(db: &dyn Db, text: &str) -> Vec<String> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    check_file(db, file).iter().map(|e| e.message.clone()).collect()
}

fn of(f: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    errors(&db, &format!("{LIST}{f}\n"))
}

const HINT: &str = "is written `if c => x`";

#[test]
fn a_block_if_with_a_value_in_a_literal_names_the_element_form() {
    let found = of("fn f(c: Bool) { let l = [if c { 1 }]; }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("an `if` without `else` must produce `()`"), "{found:?}");
    assert!(found[0].contains(HINT), "{found:?}");
}

/// The same `if` beside element forms, which take the other lowering.
#[test]
fn the_hint_is_given_beside_element_forms_too() {
    let found = of("fn f(c: Bool) { let l = [(), if c { 1 }, if c => ()]; }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains(HINT), "{found:?}");
}

/// Outside a literal the element form means nothing, so the hint would send
/// the reader somewhere wrong.
#[test]
fn outside_a_literal_there_is_no_hint() {
    let found = of("fn f(c: Bool) -> Int { let x = if c { 1 }; 0 }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(!found[0].contains(HINT), "{found:?}");
}

/// `[if c { log() }]` is a well-typed one-element `List<()>`, and stays one.
#[test]
fn a_unit_block_if_in_a_literal_is_accepted() {
    let found = of("fn log() -> () { () }\nfn f(c: Bool) -> List<()> { [if c { log() }] }");
    assert!(found.is_empty(), "{found:?}");
}

/// Every form checks, and the literal has one element type.
#[test]
fn the_element_forms_type_check() {
    let found = of("fn f(c: Bool, xs: List<Int>) -> List<Int> { [0, if c => 1 else 2, ..xs] }");
    assert!(found.is_empty(), "{found:?}");
}

/// An element of the wrong type is refused, at the element.
#[test]
fn an_element_of_another_type_is_refused() {
    let found = of("fn f(c: Bool) -> List<Int> { [0, if c => \"s\"] }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`String`"), "{found:?}");
}

/// The design's diagnostics table for the rows the checker and the lowering
/// say: each message whole, once, at the source it points to. (The parser's
/// rows are snapshots in `khora-diagnostics`.)
#[test]
fn each_checker_diagnostic_is_said_once_at_its_source() {
    let std_like = "module m;
pub type List<A> = | Nil | Cons(A, List<A>);
impl<A> List<A> {
  fn reverse_onto(self, acc: List<A>) -> List<A> { acc }
}
pub type Step<S, A> = | Yield(S, A) | Done;
pub trait Iterator {
  type Item;
  fn next(self) -> Step<Self, Self::Item>;
}
impl<A> Iterator for List<A> {
  type Item = A;
  fn next(self) -> Step<List<A>, A> {
    match self { List::Nil => Step::Done, List::Cons(h, t) => Step::Yield(t, h) }
  }
}
";
    let rows: [(&str, &str, &str); 6] = [
        (
            "fn f(rows: List<Int>) -> List<String> { [for r in rows => \"x\", 4] }",
            "this argument: expected `String`, found `Int`",
            "4",
        ),
        ("fn f() -> List<Int> { [0, ..5] }", "this argument: expected `List<Int>`, found `Int`", "5"),
        (
            "fn f() -> List<Int> { [for r in 5 => r] }",
            "`Int` does not implement `Iterator`, which is where `next` comes from",
            "for r in 5 => r",
        ),
        (
            "fn f(rows: List<Int>) -> List<Int> { [for r in rows => if r > 1 => break] }",
            "`break`: a `for` inside `[..]` makes elements and cannot be left early; \
             filter with `if` or use a `for` statement",
            "break",
        ),
        (
            "fn f(c: Bool) { let l = [if c { 1 }]; }",
            "an `if` without `else` must produce `()`: expected `()`, found `Int`; inside \
             `[..]`, an element that is only sometimes there is written `if c => x`",
            "if c { 1 }",
        ),
        (
            "fn f(rows: List<Int>) -> Int { let l = [for r in rows => r]; 0 }",
            "",
            "",
        ),
    ];
    for (f, message, at) in rows {
        let text = format!("{std_like}{f}\n");
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "a.kh".into(), text.clone());
        // What `khora check` prints: lowering's errors and the checker's.
        let found = khora_types::diagnostics(&db, file);
        if message.is_empty() {
            assert!(found.is_empty(), "{f}: {:?}", found.iter().map(|e| &e.message).collect::<Vec<_>>());
            continue;
        }
        assert_eq!(found.len(), 1, "{f}: {:?}", found.iter().map(|e| &e.message).collect::<Vec<_>>());
        assert_eq!(found[0].message, message, "{f}");
        let range = found[0].range;
        assert_eq!(&text[usize::from(range.start())..usize::from(range.end())], at, "{f}");
    }

    // Without `Step` and `Iterator` in scope: what the `for` statement says.
    let said = |f: &str| {
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "a.kh".into(), format!("{LIST}{f}\n"));
        khora_types::diagnostics(&db, file).iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    };
    let element = said("fn f(xs: List<Int>) -> List<Int> { [for x in xs => x] }");
    let statement = said("fn f(xs: List<Int>) -> Int { for x in xs { }; 0 }");
    assert_eq!(element, statement);
    assert!(
        element[0] == "`for` needs `Step` and `Iterator` in scope; import them from `std::core`",
        "{element:?}"
    );
}

/// A spread operand has to be a list of the same element type.
#[test]
fn a_spread_of_something_other_than_a_list_is_refused() {
    let found = of("fn f() -> List<Int> { [0, ..5, 1] }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("expected `List<Int>`, found `Int`"), "{found:?}");
}
