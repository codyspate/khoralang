//! Generators for input that is *lexically* plausible and syntactically
//! arbitrary.
//!
//! Random bytes exercise the lexer's error path and little else: almost every
//! seed is rejected on the first token and the parser never gets past
//! "expected a declaration". Token soup is the useful middle — a stream of
//! real Khora tokens in an order no program would have — because that is what
//! reaches the recovery paths, and recovery is where a recursive-descent
//! parser goes wrong.
//!
//! Nothing here is expected to parse. The only invariant these are generated
//! for is that the parser returns.

use crate::entropy::Entropy;

/// Every token the language has, spelled the way a program spells it.
///
/// Kept flat and literal rather than derived from `SyntaxKind`, so this crate
/// stays free of a dependency on the parser it is used to test. The cost is
/// that a new token has to be added here too; the test that would otherwise
/// catch it is `khora-syntax`'s own `keywords_match_the_lexer`.
const TOKENS: &[&str] = &[
    // Alternative 0 first and simplest: a bare identifier is the token least
    // likely to open anything the parser has to recover from.
    "x", "y", "Foo", "_",
    "0", "1", "1.5", "1.5d", "42_000", "0xFF", "'a'", "'ef", "true", "false",
    "module", "import", "type", "trait", "impl", "fn", "match", "let", "mut",
    "as", "if", "else", "forall", "const", "effect", "with",
    "raises", "raise", "in", "row", "catch", "while", "loop", "break",
    "continue", "return",
    // Contextual: these arrive as identifiers and become keywords in exactly
    // one position each, so a soup that scatters them is a soup that tests the
    // remapping.
    "handler", "for", "context", "test", "bench", "derive",
    ";", ",", ".", "..", ":", "::", "|", "|>", "||>", "->", "=>", "=", "+",
    "-", "*", "/", "%", "!", "&&", "||", "==", "!=", "<=", ">=", "<", ">",
    "(", ")", "{", "}", "[", "]",
    // Trivia, because a comment that is never closed is a token that swallows
    // the file and the parser has to cope with the file ending mid-construct.
    "// c\n", "/* c */", "/* unclosed", "\n", " ",
    // The bytes the lexer has no rule for.
    "@", "#", "$", "~", "\\", "\u{feff}", "\u{0}",
];

/// The two words held back from [`TOKENS`], and why they are held back.
///
/// **Both are known to break the parser**, so a soup containing them fails on
/// roughly one seed in twenty and the failure is always one of these two
/// rather than anything new. Each has a regression test of its own in
/// `crates/khora-syntax/tests/parser_properties.rs`:
///
/// * `pub` not followed by a declaration keyword — the recovery predicate is
///   true *at* `pub`, so recovery consumes nothing, the file loop spins to its
///   step limit and the rest of the file never reaches the tree.
/// * `extern` not followed by `fn` — `fn_decl` asserts on the `fn` it was
///   promised, and an assertion is a panic.
///
/// Move these back into `TOKENS` when both are fixed. Nothing else has to
/// change; [`Vocabulary::Full`] exists to say what "fixed" would mean.
const KNOWN_BAD_TOKENS: &[&str] = &["pub", "extern"];

/// Which vocabulary [`token_soup`] draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vocabulary {
    /// Everything except the two words above. What the ordinary suite uses, so
    /// that a green run means no *new* bug rather than no bug.
    Sound,
    /// Every token, including the two that break the parser today.
    Full,
}

impl Vocabulary {
    fn len(self) -> usize {
        match self {
            Vocabulary::Sound => TOKENS.len(),
            Vocabulary::Full => TOKENS.len() + KNOWN_BAD_TOKENS.len(),
        }
    }

    fn token(self, i: usize) -> &'static str {
        // The held-back words go on the *end*, so that a given seed produces
        // the same soup under `Sound` as under `Full` right up to the point
        // where it draws one of them. A seed that fails under `Full` is
        // therefore one byte away from the soup that passes.
        TOKENS.get(i).copied().unwrap_or_else(|| KNOWN_BAD_TOKENS[i - TOKENS.len()])
    }
}

/// A stream of real tokens in an arbitrary order.
///
/// `max_tokens` bounds the work rather than the shape; 64 is enough that a
/// seed reaches the third or fourth nested construct, which is where recovery
/// starts having to choose between several open blocks.
pub fn token_soup(src: &mut Entropy<'_>, max_tokens: usize, vocab: Vocabulary) -> String {
    let n = src.choice(max_tokens) + 1;
    let mut out = String::new();
    for _ in 0..n {
        // Half the time a token, half the time a string — strings are the
        // sharp part and a uniform draw over `TOKENS` would produce one every
        // hundred tokens.
        if src.chance(96) {
            out.push_str(&interpolated_string(src));
        } else {
            out.push_str(vocab.token(src.choice(vocab.len())));
        }
        out.push(' ');
    }
    out
}

/// The pieces a string literal can be built from, chosen to collide with the
/// lexer's scanner rather than to look like text.
///
/// The scanner tracks three things at once — escaping, how many `${` are open,
/// and whether a quote inside a hole opened a nested string — and a bug in
/// July was exactly a disagreement between two of them: an escaped quote
/// `\"` inside a hole was read as opening a nested string, so the literal ran
/// to the wrong place. Every fragment here is one of those three states being
/// entered or left.
const STRING_PARTS: &[&str] = &[
    "a", " ", "b c", "${", "}", "{", "}", "\"", "\\\"", "\\", "\\\\", "\\n", "\\u{1F600}",
    "\\u{", "$", "${x}", "${f(\"y\")}", "${\"${z}\"}", "${", "`", "\n", "\u{e9}", "\u{1f600}",
    "\u{feff}",
];

/// A string literal, or something that was trying to be one.
///
/// The opening delimiter is drawn separately from the closing one, and the
/// closing one may be absent, because the three ways a literal ends —
/// delimiter, newline, end of file — are three different paths through the
/// scanner and only the first is the one anybody writes.
pub fn interpolated_string(src: &mut Entropy<'_>) -> String {
    // `"` first: it is the ordinary literal, and the one whose scanner stops
    // at a newline.
    let quote = if src.chance(64) { '`' } else { '"' };
    let mut out = String::new();
    out.push(quote);
    let n = src.count(8);
    for _ in 0..n {
        out.push_str(STRING_PARTS[src.choice(STRING_PARTS.len())]);
    }
    // Closed most of the time: an unterminated literal ends the useful part of
    // the seed, since everything after it is inside the literal.
    if !src.chance(48) {
        out.push(quote);
    }
    out
}

/// Nested delimiters, `depth` of them, with a body inside.
///
/// The depth is the caller's to choose and the caller is the one that knows
/// what the stack can take — see [`crate::entropy::DEFAULT_MAX_DEPTH`].
pub fn nested(src: &mut Entropy<'_>, depth: usize) -> String {
    const PAIRS: &[(&str, &str)] = &[("(", ")"), ("[", "]"), ("{", "}"), ("${", "}")];
    let mut open = String::new();
    let mut close = String::new();
    for _ in 0..depth {
        let (l, r) = PAIRS[src.choice(PAIRS.len())];
        open.push_str(l);
        // An unbalanced nest is the interesting one: a parser that recovers by
        // scanning to a closing delimiter has to terminate when there is none.
        if !src.chance(24) {
            close.insert_str(0, r);
        }
    }
    format!("module m;\nfn f() {{ {open}1{close} }}\n")
}
