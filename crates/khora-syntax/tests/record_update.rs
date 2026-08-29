//! `{ ..base, field: value }` at the grammar.
//!
//! The token is new: `..` did not exist before this, because nothing in the
//! language spelled two dots. Logos takes the longest match, so `.` is
//! unaffected — but that is the kind of claim that should be checked rather
//! than asserted, which is most of what this file does.

use khora_syntax::parse;

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn accepted(source: &str) {
    let found = errors(source);
    assert!(found.is_empty(), "{source}\n{found:?}");
}

fn program(body: &str) -> String {
    format!("module t;\npub type P = {{ x: Int, y: Int }};\nfn f(p: P) -> P {{ {body} }}\n")
}

/// The shapes a record update comes in.
#[test]
fn a_record_update_parses() {
    accepted(&program("{ ..p }"));
    accepted(&program("{ ..p, x: 1 }"));
    accepted(&program("{ ..p, x: 1, y: 2 }"));
    // A trailing comma, as every other list in the language takes.
    accepted(&program("{ ..p, x: 1, }"));
    // An arbitrary expression as the base, not only a name.
    accepted(&program("{ ..f(p), x: 1 }"));
    accepted(&program("{ ..{ x: 1, y: 2 }, x: 3 }"));
}

/// **`..` did not take `.` with it.** Field access, method calls and the
/// pipeline operator all still lex as they did.
#[test]
fn a_single_dot_is_unaffected() {
    accepted(&program("{ ..p, x: p.x + 1 }"));
    accepted(&program("{ ..p, x: p.x, y: p.y }"));
    accepted(
        "module t;\n\
         pub type P = { x: Int, y: Int };\n\
         fn g(n: Int) -> Int { n }\n\
         fn f(p: P) -> Int { p.x |> g }\n",
    );
}

/// A base with nothing after the dots is a mistake, and is said to be one at
/// the dots rather than as a cascade about the brace never closing.
#[test]
fn a_base_with_no_expression_is_refused() {
    let found = errors(&program("{ .., x: 1 }"));
    assert!(
        found.iter().any(|e| e.contains("expected the record to take the rest of the fields from")),
        "{found:?}"
    );
}

/// An ordinary record literal still parses, which is the rule the new leader
/// had to be added in front of without disturbing.
#[test]
fn an_ordinary_record_literal_still_parses() {
    accepted(&program("{ x: 1, y: 2 }"));
    accepted("module t;\npub type E = {};\nfn f() -> E { {} }\n");
}
