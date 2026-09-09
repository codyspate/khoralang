//! The formatter's two properties, under coverage-guided input.
//!
//! The same assertions as `khora-fmt/tests/property.rs` — a `proptest` run
//! samples seeds uniformly, and this one steers them toward the branches of
//! the formatter that have not been taken yet. They find different things, so
//! both exist.
//!
//! The import comparison is a multiset because import lists are sorted and
//! deduplicated by design; see the `split_at_imports` note in the `proptest`
//! harness for the argument.

#![no_main]

use khora_syntax::{LexedStr, SyntaxKind};
use khora_testgen::{program, Entropy};
use libfuzzer_sys::fuzz_target;

fn tokens(src: &str) -> Vec<(SyntaxKind, String)> {
    let lexed = LexedStr::new(src);
    let mut out: Vec<_> = (0..lexed.len())
        .filter(|i| !lexed.kind(*i).is_trivia())
        .map(|i| (lexed.kind(i), lexed.text(i).to_string()))
        .collect();
    out.sort();
    out
}

fuzz_target!(|data: &[u8]| {
    let mut src = Entropy::new(data);
    let text = program(&mut src);

    let Ok(once) = khora_fmt::format(&text) else {
        panic!("generated program does not parse:\n{text}");
    };
    let twice = khora_fmt::format(&once).expect("formatted output parses");
    assert_eq!(once, twice, "not stable under a second pass:\n{text}");
    assert_eq!(tokens(&text), tokens(&once), "tokens changed:\n{text}");
});
