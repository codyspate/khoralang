//! `khora_syntax::parse` must return, on any input at all.
//!
//! The claim in `khora-syntax/src/lib.rs` is unconditional — *the parser never
//! fails; it always returns a tree covering the whole input* — so the target
//! asserts both halves: that it returns, and that the tree it returns
//! reproduces the input byte for byte.
//!
//! `String::from_utf8_lossy` rather than rejecting non-UTF-8, because that is
//! what a real caller does with a file off disk, and because U+FFFD is itself
//! a byte sequence the lexer has no rule for.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let parse = khora_syntax::parse(&text);
    assert_eq!(
        parse.syntax().text().to_string(),
        text,
        "the tree does not reproduce the input"
    );
});
