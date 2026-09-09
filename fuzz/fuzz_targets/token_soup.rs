//! The same invariant as `parse`, reached through `khora-testgen`'s token
//! soup rather than through raw bytes.
//!
//! Raw bytes almost never get past the first token, so they exercise the
//! lexer's error path and nothing else. The soup turns the fuzzer's bytes into
//! real Khora tokens in an order no program would have, which is what reaches
//! error recovery — and recovery is where a recursive-descent parser goes
//! wrong.
//!
//! [`Vocabulary::Full`] on purpose, unlike the `proptest` harness in
//! `khora-syntax/tests/parser_properties.rs`: this target is expected to find
//! the two known bugs in the first few seconds, and will keep finding them
//! until they are fixed. Switch to `Vocabulary::Sound` to look for anything
//! else in the meantime.

#![no_main]

use khora_testgen::soup::{token_soup, Vocabulary};
use khora_testgen::Entropy;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut src = Entropy::new(data);
    let text = token_soup(&mut src, 64, Vocabulary::Full);
    let parse = khora_syntax::parse(&text);
    assert_eq!(
        parse.syntax().text().to_string(),
        text,
        "the tree does not reproduce the input"
    );
});
