//! `export extern fn`, as the checker sees it.
//!
//! `docs/design/c-export.md`. Two words the language already has: `extern` is
//! the C boundary, `export` is visible outside. A body tells the directions
//! apart, which is a rule that already existed — errata 5 makes a body
//! optional, so `extern fn` without one is a symbol to find at link time and
//! with one is a symbol to publish.
//!
//! **Reported at the declaration, unlike a foreign import.** An import is
//! checked where the call is generated so that a binding one target lacks is
//! not an error on a target that never calls it. An export is part of the
//! library's published ABI whether or not any Khora code calls it, so a
//! signature C could not call is wrong where it is written and `khora check`
//! should say so.

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
fn a_signature_c_can_call_is_accepted() {
    assert_clean(
        "module m;\nexport extern fn price(units: Int, scale: Int) -> Int { units * scale }\n",
    );
    // `()` as a return is C's `void`, which is fine. As a *parameter* it is
    // not, and `foreign_obstacle` already said so.
    assert_clean("module m;\nexport extern fn tick(n: Int) -> () { }\n");
    assert_clean("module m;\nexport extern fn wide(a: U8, b: I32) -> Bool { true }\n");
}

/// A C symbol is reachable by anything that links the library, so a private one
/// is a contradiction rather than a narrower promise — and somebody who wrote
/// `extern fn` meaning "call out" should be told which of the two this is.
#[test]
fn an_extern_body_must_be_exported() {
    assert_reports(
        "module m;\nextern fn price(u: Int) -> Int { u }\n",
        "cannot be private",
    );
}

#[test]
fn a_type_that_cannot_cross_is_refused_at_the_declaration() {
    assert_reports(
        "module m;\nexport extern fn greet(who: String) -> Int { 1 }\n",
        "its parameter of type `String` cannot cross",
    );
    assert_reports(
        "module m;\nexport extern fn name(n: Int) -> String { \"x\" }\n",
        "its return type `String` cannot cross",
    );
    assert_reports(
        "module m;\nexport extern fn id<A>(a: A) -> A { a }\n",
        "it is generic",
    );
}

/// The advice is the convention `std` already uses between itself and the
/// runtime — `khora_float_text(value, into, capacity) -> Int`. No shared
/// allocator, no free function to forget.
#[test]
fn the_message_says_what_to_do_about_text() {
    assert_reports(
        "module m;\nexport extern fn greet(who: String) -> Int { 1 }\n",
        "take a buffer and a capacity and return the length",
    );
}

/// A declaration with no body is still an import, checked where it is called.
#[test]
fn a_foreign_import_is_left_alone() {
    assert_clean("module m;\nextern fn getenv(name: Ptr) -> Ptr;\n");
    // Including one whose signature could not cross: that is the call site's
    // error to report, on the target that makes the call.
    assert_clean("module m;\nextern fn odd(s: String) -> Int;\n");
}

#[test]
fn an_ordinary_function_is_untouched() {
    assert_clean("module m;\nexport fn plain(s: String) -> String { s }\n");
}
