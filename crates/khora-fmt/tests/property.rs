//! The two properties from `format.rs`, over generated programs.
//!
//! `format.rs` says why they are the two that matter: *a formatter that loses
//! code is worse than no formatter, and one that is not idempotent turns every
//! save into a diff*. It asserts both against `std` and `examples`, which is a
//! corpus of a few thousand lines that people wrote — and people write the
//! shapes they already know work.
//!
//! These assert the same two against programs nobody wrote. Every bug the
//! formatter has had was a shape that was not in the corpus until something in
//! `std` finally used it: an import that finally needed an alias, a variant
//! that finally had a doc comment, a comparison that finally had a parenthesis
//! on its right. A generator gets to those before `std` does.
//!
//! # Case counts
//!
//! 1,024 cases per property — four times `proptest`'s default — and the file
//! runs in about six tenths of a second. The number is chosen against the
//! clock: a case here formats twice and parses three times, which is the most
//! expensive thing any generative test in this repository does, and six tenths
//! of a second is what a commit can be asked to pay. A soak is
//! `PROPTEST_CASES=400000 cargo test -p khora-fmt --test property --release`,
//! which takes about a minute and is what these were last run at.
//!
//! What the generator does and does not emit is documented on
//! `khora_testgen::program`. The gap that matters most here is that it emits
//! no comments and no blank lines.

use khora_fmt::{format, is_formatted};
use khora_syntax::{LexedStr, SyntaxKind};
use khora_testgen::{program, Entropy};
use proptest::prelude::*;

/// The non-trivia token stream — what must survive formatting.
///
/// The same function as `format.rs`'s, deliberately: if the definition of
/// "the same tokens" drifts between the two files, one of them stops meaning
/// what its name says.
///
/// One token: its kind and its spelling. Both, because the kind alone loses
/// which identifier it was and the text alone loses `1` the integer from `1`
/// the something-else.
type Token = (SyntaxKind, String);

fn tokens(src: &str) -> Vec<Token> {
    let lexed = LexedStr::new(src);
    (0..lexed.len())
        .filter(|i| !lexed.kind(*i).is_trivia())
        .map(|i| (lexed.kind(i), lexed.text(i).to_string()))
        .collect()
}

/// The generated program, and the formatter's output for it.
///
/// Returns `None` for the one case that is not a failure: a program the
/// generator produced that does not parse. That cannot happen — `khora-syntax`
/// asserts it in `generated_programs_parse_cleanly` — and it is checked rather
/// than unwrapped so that a regression in the generator is reported as a
/// generator problem here rather than as a formatter problem.
fn formatted(src: &str) -> String {
    format(src).unwrap_or_else(|e| panic!("generated program does not parse: {e:?}\n{src}"))
}

/// The case count, with `PROPTEST_CASES` still able to override it.
///
/// `ProptestConfig::default()` already reads that variable, so a literal
/// `cases:` field would silently ignore a soak run asking for two hundred
/// thousand. The default here is chosen against the clock — see the header —
/// and setting the variable hands control back.
fn config(cases: u32) -> ProptestConfig {
    let default = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_some() {
        default
    } else {
        ProptestConfig { cases, ..default }
    }
}

proptest! {
    #![proptest_config(config(1024))]

    /// **`format(format(x)) == format(x)`.**
    ///
    /// Idempotence is what makes a formatter usable on save. It is also the
    /// weaker of the two properties, and `format.rs` says so directly:
    /// `formatting_a_documented_variant_twice_is_formatting_it_once` exists
    /// because a bug that produced valid, round-tripping, *wrong* output would
    /// have passed idempotence on its own.
    #[test]
    fn formatting_is_idempotent(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        let text = program(&mut src);

        let once = formatted(&text);
        let twice = formatted(&once);
        prop_assert_eq!(&twice, &once, "not stable under a second pass\ninput:\n{}", text);
    }

    /// **`parse(format(x))` has the same tokens as `parse(x)`.**
    ///
    /// The sequence, not the multiset. `format.rs` compares the multiset over
    /// the corpus because the formatter reorders import lists by design, and
    /// then covers order separately with five hand-written inputs that have no
    /// imports. This does the same, by generating an import list and comparing
    /// what precedes it against what precedes it: everything after the last
    /// `import` is compared in order, and the import declarations themselves
    /// are compared as a multiset.
    ///
    /// A formatter that drops a token is the failure this is for. The alias
    /// bug in `format.rs` was exactly that — four aliases became `{as, as, as,
    /// as}` and the file stopped parsing — and it survived until `std` grew an
    /// import that needed one.
    #[test]
    fn formatting_never_changes_the_tokens(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        let text = program(&mut src);
        let out = formatted(&text);

        let (before_imports, before_rest) = split_at_imports(&tokens(&text));
        let (after_imports, after_rest) = split_at_imports(&tokens(&out));

        prop_assert_eq!(
            before_imports,
            after_imports,
            "an import was lost\ninput:\n{}\noutput:\n{}",
            text,
            out
        );
        prop_assert_eq!(
            before_rest,
            after_rest,
            "the token sequence changed\ninput:\n{}\noutput:\n{}",
            text,
            out
        );
    }

    /// **The output parses, and reports itself formatted.**
    ///
    /// Weaker than the two above and cheap to add beside them. `is_formatted`
    /// is what `khora fmt --check` runs, and a formatter whose own output
    /// fails its own check is what made that flag permanently red on Windows
    /// — the case `format.rs` records at
    /// `a_correctly_formatted_file_with_carriage_returns_needs_no_reformatting`.
    #[test]
    fn output_is_already_formatted(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        let out = formatted(&program(&mut src));
        prop_assert!(
            is_formatted(&out).expect("formatted output parses"),
            "formatter's own output is not formatted:\n{}",
            out
        );
    }
}

/// Splits a token stream into (the tokens of every import declaration, sorted)
/// and (everything else, in order).
///
/// Import lists are reordered and deduplicated by design, so their tokens can
/// only be compared as a multiset. Everything else has to come back in the
/// order it went in.
fn split_at_imports(tokens: &[Token]) -> (Vec<Token>, Vec<Token>) {
    let mut imports = Vec::new();
    let mut rest = Vec::new();
    let mut in_import = false;
    for token in tokens {
        if token.0 == SyntaxKind::IMPORT_KW {
            in_import = true;
        }
        if in_import {
            imports.push(token.clone());
            if token.0 == SyntaxKind::SEMICOLON {
                in_import = false;
            }
        } else {
            rest.push(token.clone());
        }
    }
    imports.sort();
    (imports, rest)
}
