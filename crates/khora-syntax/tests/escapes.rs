//! Backslash escapes, and what happens to one the language does not know.
//!
//! **The point of this file is the refusal.** An unrecognised escape used to be
//! kept as the two characters it was written with, so `"\u{0}"` — the spelling
//! Rust, JavaScript and Python all use — became six literal characters
//! starting with a backslash. It compiled, it ran, and it produced a string
//! nobody wanted.
//!
//! It cost real time. `std::process` packed its argument buffer with
//! `String::join(parts, "\u{0}")`, so every argument was separated by that
//! six-character run instead of a NUL, and every command failed to start —
//! reporting `NotStarted`, which is exactly what a genuinely missing program
//! reports. The symptom pointed at the wrong file.
//!
//! And `packages/postgres` was already writing `\<newline>` to continue a long
//! message across lines, which silently put a backslash and eleven spaces in
//! the middle of a sentence somebody was meant to read. That one had shipped.

use khora_syntax::parse;

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn accepted(source: &str) {
    let found = errors(source);
    assert!(found.is_empty(), "{source}\n{found:?}");
}

fn refused(source: &str, needle: &str) {
    let found = errors(source);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}\n{source}\n{found:?}"
    );
}

fn program(literal: &str) -> String {
    format!("module t;\nconst s: String = {literal};\n")
}

/// Every escape the language has.
#[test]
fn the_escapes_are_accepted() {
    for literal in [
        r#""\n""#,
        r#""\r""#,
        r#""\t""#,
        r#""\0""#,
        r#""\\""#,
        r#""\"""#,
        r#""\'""#,
        r#""\`""#,
        // A literal dollar, so a template for another tool still fits.
        r#""\$""#,
        r#""\u{41}""#,
        r#""\u{1F600}""#,
        r#""\u{0}""#,
        r#""\u{10FFFF}""#,
        // Together, and next to ordinary text.
        r#""a\tb\u{41}c\\d""#,
    ] {
        accepted(&program(literal));
    }
}

/// **An escape nothing understands is a mistake, and is now said to be one.**
///
/// The message names every escape there is, because somebody who reached for
/// one that does not exist is usually one character away from one that does.
#[test]
fn an_unknown_escape_is_refused() {
    refused(&program(r#""\q""#), "`\\q` is not an escape");
    refused(&program(r#""\e[0m""#), "is not an escape");
    refused(&program(r#""C:\path""#), "is not an escape");

    // And the message says how to write what they probably meant.
    let found = errors(&program(r#""\q""#));
    assert!(found[0].contains(r"write `\\q`"), "{found:?}");
}

/// A `\u` that is not `\u{..}` is its own mistake, and gets its own sentence.
#[test]
fn a_malformed_unicode_escape_says_what_the_shape_is() {
    for bad in [r#""\u41""#, r#""\u{}""#, r#""\u{}x""#, r#""\u{zz}""#, r#""\u{41""#] {
        refused(&program(bad), "written `\\u{..}` around one to six hex digits");
    }
}

/// **A number that is not a character is a third mistake**, and telling it
/// apart from a malformed escape is the difference between "you typed it
/// wrong" and "that value does not exist".
#[test]
fn a_unicode_escape_that_is_not_a_character_is_refused() {
    // Past the last code point.
    refused(&program(r#""\u{110000}""#), "that is not a character");
    // Half of a surrogate pair, which UTF-8 cannot hold.
    refused(&program(r#""\u{D800}""#), "that is not a character");
    refused(&program(r#""\u{DFFF}""#), "that is not a character");
    // The boundaries either side are fine.
    accepted(&program(r#""\u{10FFFF}""#));
    accepted(&program(r#""\u{D7FF}""#));
    accepted(&program(r#""\u{E000}""#));
}

/// **A backslash before a newline continues the line**, which is what
/// `packages/postgres` was already written as and what every neighbouring
/// language means by it.
#[test]
fn a_backslash_before_a_newline_continues_the_line() {
    accepted("module t;\nconst s: String = \"one \\\n    two\";\n");
}

/// The check reaches inside an interpolation, because a hole holds strings
/// too and a bad escape in one is as wrong as anywhere else.
#[test]
fn an_escape_inside_an_interpolation_is_checked() {
    refused(
        "module t;\nconst s: String = \"a ${f(\"\\q\")} b\";\n",
        "is not an escape",
    );
}

/// A backtick literal is the same token, so it gets the same treatment.
#[test]
fn a_backtick_literal_is_checked_too() {
    accepted("module t;\nconst s: String = `a\\tb`;\n");
    refused("module t;\nconst s: String = `a\\qb`;\n", "is not an escape");
}
