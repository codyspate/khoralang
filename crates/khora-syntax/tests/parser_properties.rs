//! Generated input, and the one thing the parser promises about all of it.
//!
//! `lib.rs` says it: *the parser never fails — it always returns a tree
//! covering the whole input, plus a list of diagnostics*. Every example test in
//! this crate checks a tree against an input somebody wrote down. These check
//! the promise against inputs nobody wrote down, because the promise is a
//! statement about **all** input and the bugs it is there to prevent are in
//! the shapes an author would not think to type.
//!
//! Three properties, in the order a failure would matter:
//!
//! 1. `parse` returns. Not a panic, not an unbounded loop, not an overflow.
//! 2. The tree's text is byte-identical to the input. A lossless CST that has
//!    lost a byte is worse than a lossy one, because everything downstream —
//!    the formatter, the LSP's ranges, every diagnostic span — trusts it.
//! 3. The tree's tokens are the lexer's tokens, in order. This is what says
//!    the parser did not silently drop or invent one; `event.rs` asserts it
//!    internally, and this asserts it from outside on input `event.rs` was not
//!    written against.
//!
//! # Case counts
//!
//! 1,024 cases each — four times `proptest`'s default — and the file runs in
//! about six tenths of a second. The number is chosen against the clock rather
//! than against the default, because this has to be cheap enough to sit in the
//! ordinary `cargo test`: a generative test that only runs in a nightly job is
//! one that nobody's failing commit is measured against. Six tenths of a
//! second per commit buys four times the search, and the seeds are cheap here
//! because none of these properties does anything but parse.
//!
//! `PROPTEST_CASES=200000 cargo test -p khora-syntax --test parser_properties`
//! is the soak, and takes under four minutes. Seeds are 0..=256 bytes, which
//! with `khora-testgen`'s fuel budget is a few kilobytes of source at the very
//! top end and a few dozen bytes typically.

use khora_syntax::{parse, LexedStr, SyntaxKind};
use khora_testgen::soup::Vocabulary;
use khora_testgen::{program, soup, Entropy};
use proptest::prelude::*;

/// The three invariants, asserted together because any input worth generating
/// is worth checking all three against.
fn parses_soundly(src: &str) {
    let parse = parse(src);

    assert_eq!(
        parse.syntax().text().to_string(),
        src,
        "the tree does not reproduce the input"
    );

    let lexed = LexedStr::new(src);
    let expected: Vec<(SyntaxKind, &str)> = lexed.iter().collect();
    let actual: Vec<(SyntaxKind, String)> = parse
        .syntax()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .map(|t| (t.kind(), t.text().to_string()))
        .collect();
    assert_eq!(actual.len(), expected.len(), "token count changed");
    for (i, ((ak, at), (ek, et))) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(at.as_str(), *et, "token {i} has different text");
        // The kind may legitimately differ in exactly one way: the parser
        // remaps an `IDENT` to a contextual keyword in the single position
        // where the word is one. Anything else is a token the parser invented.
        if ak != ek {
            assert!(
                ak.is_contextual_keyword() && *ek == SyntaxKind::IDENT,
                "token {i} changed kind from {ek:?} to {ak:?}"
            );
        }
    }
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

    /// Arbitrary bytes, read the way a file on disk is read. The lossy
    /// conversion is not a weakening: `parse` takes a `&str`, so U+FFFD is
    /// what a real caller would hand it for a file that is not UTF-8, and
    /// U+FFFD is itself a byte sequence the lexer has no rule for.
    #[test]
    fn arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        parses_soundly(&String::from_utf8_lossy(&bytes));
    }

    /// Arbitrary text, which reaches the lexer's rules rather than only its
    /// error path — a `char` strategy draws from the whole of Unicode, so this
    /// is where combining marks, astral planes and the byte order mark arrive.
    #[test]
    fn arbitrary_text(text in ".{0,256}") {
        parses_soundly(&text);
    }

    /// Real tokens in an order no program would have. This is the generator
    /// that reaches error recovery, which is the part of a recursive-descent
    /// parser that has no examples written against it.
    #[test]
    fn token_soup(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        parses_soundly(&soup::token_soup(&mut src, 64, Vocabulary::Sound));
    }

    /// String literals, and things that were trying to be one.
    ///
    /// Aimed at `lexer::lex_string`, which tracks escaping, how many `${` are
    /// open and whether a quote inside a hole opened a nested string — three
    /// pieces of state that have already disagreed with each other once, in
    /// `khora-hir`'s copy of the same scan.
    #[test]
    fn interpolated_strings(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        let literal = soup::interpolated_string(&mut src);
        parses_soundly(&literal);
        // And in the position a program would actually put one, where what
        // follows it is code that the literal may have swallowed.
        parses_soundly(&format!("module m;\nfn f() {{ let s = {literal}; s }}\n"));
    }

    /// **Generated programs parse with no diagnostics**, which is
    /// `khora-testgen`'s own contract rather than the parser's.
    ///
    /// It is here and not in that crate because that crate deliberately has no
    /// parser to check itself against. It is the test that keeps the generator
    /// honest: a generator that drifts into emitting something the grammar
    /// does not accept stops testing the formatter and starts testing
    /// `format`'s refusal, and nothing else would notice.
    #[test]
    fn generated_programs_parse_cleanly(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        let text = program(&mut src);
        let parse = parse(&text);
        prop_assert!(
            parse.errors().is_empty(),
            "generated program does not parse: {:?}
{text}",
            parse.errors()
        );
        parses_soundly(&text);
    }

    /// Nested delimiters, balanced and not.
    ///
    /// **Capped at 64, and the cap is the bug below.** Recovery that scans for
    /// a closing delimiter is the loop that has to terminate when there is
    /// none, and 64 is enough nesting to reach it.
    #[test]
    fn nested_delimiters(seed in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut src = Entropy::new(&seed);
        let depth = 1 + (seed.first().copied().unwrap_or(0) as usize % 64);
        parses_soundly(&soup::nested(&mut src, depth));
    }
}

/// **The parser overflows its stack at about 1,550 nested delimiters.**
///
/// `#[ignore]`d because it does not fail, it *aborts*: a stack overflow on
/// Windows is `STATUS_STACK_OVERFLOW` and on Unix a `SIGSEGV`, and neither is
/// a panic `proptest` or the test harness can catch. One case reaching this
/// depth would take the whole run with it and report nothing, which is why
/// every generator above is capped well below it.
///
/// Measured on a 2 MiB thread — the harness default — by bisecting the depth
/// in a subprocess per case:
///
/// ```text
/// (  1554 ok, 1555 overflows      [  1553 ok unclosed
/// {  1572 ok, 1573 overflows      {  1572 ok unclosed
/// [  1697 ok, 1698 overflows      [  1696 ok unclosed
/// g( 1610 ok, 1611 overflows
/// !  3222 ok, 3223 overflows      -  3223 ok, 3224 overflows
/// ```
///
/// Balanced and unbalanced overflow at the same depth, so this is the descent
/// itself and not recovery. `${` nesting inside a string is unaffected — that
/// scan is a loop in the lexer — which is what says the fix belongs in
/// `parser/exprs.rs` and `parser/types.rs` rather than anywhere else.
///
/// Why it matters beyond a pathological file: `khora-syntax` is what the LSP
/// parses with, on every keystroke, over whatever is in the buffer. The parser
/// already has `STEP_LIMIT` for the non-consuming case and the comment on it
/// says why — *keeps a malformed file from hanging the LSP*. This is the same
/// argument for the other unbounded quantity, and it wants the same answer: a
/// depth counter, an "expression nested too deeply" diagnostic, and recovery.
///
/// Run it, in a subprocess of its own, with
/// `cargo test -p khora-syntax --test parser_properties --
///  --ignored --exact deep_nesting_overflows_the_stack`.
#[test]
fn deep_nesting_overflows_the_stack() {
    let src = format!("module m;\nfn f() {{ {}1{} }}\n", "(".repeat(1600), ")".repeat(1600));
    parses_soundly(&src);
}

// --- what the generators found ----------------------------------------------
//
// Four shapes, all reachable by typing at the end of a file, which is the case
// an LSP hits on every keystroke. Two of them panicked the parser, in a
// function documented never to panic on malformed input.
//
// All four are fixed and these tests are the guard. They were written first,
// while the bugs were live, and were `#[ignore]`d with a note saying what each
// did -- so that a run of the suite was green when there was nothing *new*,
// rather than green because nothing was being looked at. When the fixes
// landed, the attributes came off and nothing else changed.
//
// The fifth, `deep_nesting_overflows_the_stack`, is fixed too and runs with
// the rest. It could not be run at all while it was live: a stack overflow
// *aborts* rather than unwinding, so no harness could report it. The parser
// refuses past `DEPTH_LIMIT` now instead of descending.

/// **`pub` at the end of a file loses the rest of the file.**
///
/// Smallest input: `pub`. The tree comes back empty, so
/// `parse(src).syntax().text() != src` and the lossless invariant in `lib.rs`
/// does not hold.
///
/// The mechanism, and it is not a typo anywhere:
/// `decls::declaration`'s `PUB_KW` arm falls through to
/// `err_recover("expected `type`, `trait`, .. after `pub`", at_decl_start)`.
/// `at_decl_start` is true *at* `PUB_KW`, so `err_recover` takes its early
/// return and consumes nothing. `source_file_contents` loops, calls
/// `declaration` again on the same token, and does so until `tick()` runs out
/// of the 10,000 steps `STEP_LIMIT` allows — at which point the loop breaks
/// with every remaining token unconsumed, and `event::build_tree` never emits
/// them.
///
/// So the step limit is doing exactly what its comment says — *keeps a
/// malformed file from hanging the LSP* — and the price is a tree that has
/// lost text. Both halves want fixing: recovery after `pub` should consume the
/// `pub`, and a loop that hits the step limit should still emit the tail of
/// the file rather than dropping it.
///
/// `pub impl` reaches it too, and that one is a real program: `impl` is
/// missing from the `PUB_KW` arm's match. `pub 1`, `pub ;` and `pub }` are the
/// same path.
#[test]
fn pub_at_the_end_of_a_file_is_lost() {
    for src in ["pub", "pub impl X {}", "module m;
pub
"] {
        parses_soundly(src);
    }
}

/// **`extern` not followed by `fn` panics the parser.**
///
/// Smallest input: `extern`. `declaration` dispatches
/// `IDENT if p.at_contextual(EXTERN_KW) => fn_decl(p)`, and `fn_decl` does
/// `p.bump(FN_KW)` — an `assert_eq!` on the current token — without ever
/// having checked that a `fn` follows. `extern x` panics the same way.
///
/// This is a panic in a function whose documentation says *never panics on
/// malformed input*, and `extern` alone on a line is what a file looks like
/// halfway through typing `extern fn`.
#[test]
fn extern_without_a_function_panics() {
    parses_soundly("extern");
}

/// **`pub` before anything but `fn` inside a trait or impl body panics.**
///
/// Smallest input: `trait T { pub type X; }`. Same assertion as above and the
/// same cause: the body loop's `FN_KW | PUB_KW => fn_decl(p)` arm sends `pub`
/// to `fn_decl`, which bumps a `FN_KW` that is not there.
///
/// Not found by the generators — they do not emit `pub` inside a trait body,
/// for this reason — but by reading `decls.rs` beside them, and it is recorded
/// here because it is the same one-line assumption in a second place and a fix
/// to one should be checked against the other.
#[test]
fn pub_before_a_non_function_in_a_trait_body_panics() {
    parses_soundly("trait T { pub type X; }");
}

/// **A `!` at the start of a statement is applied to the statement above it.**
///
/// Smallest input:
///
/// ```text
/// module m;
/// fn f() { if 0 { 0 } else { 1 }
///   !2 }
/// ```
///
/// which reports *this `{` is never closed* against the function's own brace.
///
/// The postfix loop in `parser/exprs.rs` guards `with` with
/// `WITH_KW if !is_block_like(lhs.kind())`, and the comment above it gives the
/// reason: *a block-like expression is a statement*, so `with` after one is a
/// second statement rather than something installed over the first. `BANG` and
/// `CATCH_KW` are in the same loop with no such guard. `catch` is unreachable
/// this way because it cannot begin an expression, but `!` is both a prefix
/// and a postfix operator, so a statement that starts with one is absorbed by
/// the statement before it.
///
/// Everything else is already right: `-2`, `(2)`, `.a`, `[2]`, `*2` and
/// `a |> b()` after the same `if` all parse as a new statement. `!` is the one
/// that does not, which is what makes it an oversight rather than a rule.
///
/// `khora-testgen`'s `block` writes a `;` in this position for this reason,
/// and only in this position, so it keeps generating the no-semicolon rule
/// everywhere else.
#[test]
fn a_try_after_a_block_like_statement_swallows_it() {
    let src = "module m;
fn f() { if 0 { 0 } else { 1 }
  !2 }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}
