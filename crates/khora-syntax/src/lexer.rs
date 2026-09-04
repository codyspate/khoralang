//! `logos`-backed tokenizer.
//!
//! The lexer is lossless: whitespace and comments are emitted as ordinary
//! tokens so the parser can build a CST whose text is byte-identical to the
//! input. Unrecognized bytes become [`SyntaxKind::LEX_ERROR`] tokens rather
//! than aborting the lex, which keeps the LSP useful inside broken files.

use logos::{Lexer, Logos};
use text_size::{TextRange, TextSize};

use crate::kind::SyntaxKind;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    // U+FEFF is in there because a file saved by a Windows editor starts with
    // one. It is ZERO WIDTH NO-BREAK SPACE, so whitespace is what it is —
    // and without this the lexer reports "expected a declaration" against a
    // `module` line that is perfectly good and looks it, since a byte order
    // mark does not print. Anywhere rather than only at the start, because a
    // file concatenated from two others has one in the middle.
    #[regex(r"[ \t\r\n\x0c\u{feff}]+")]
    Whitespace,
    #[regex(r"//[^\r\n]*")]
    LineComment,
    #[token("/*", lex_block_comment)]
    BlockComment,

    // **Before the float and the int**, because `logos` prefers the longer
    // match and these are longer by exactly the suffix. `1d` must not lex as
    // `1` followed by an identifier `d`.
    //
    // The suffix goes after any exponent, so `1.5e3d` is one token. Nothing
    // may lex between the digits and the `d`, so `1 d` is not a decimal --
    // which falls out of these being single regexes rather than a token pair.
    #[regex(r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9]+)?d")]
    #[regex(r"[0-9][0-9_]*d")]
    DecimalLit,

    #[regex(r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9]+)?")]
    FloatLit,
    #[regex(r"[0-9][0-9_]*")]
    IntLit,

    /// `0xFF`, `0b1010`, `0o17` — a base Khora does not have.
    ///
    /// **Matched so that it can be refused by name.** Khora has decimal
    /// integer literals and `_` as a separator, and nothing else. Without this
    /// pattern the `0` lexes as an integer and `xFF` as an identifier, and the
    /// parser reports whatever those two break next: for `{ 0xFF }` that is
    /// *this `{` is never closed*, pointing at a brace on the line above. An
    /// editor draws that underline in the wrong place, which is worse than
    /// drawing none.
    ///
    /// The digits are matched loosely — `[0-9A-Fa-f_]` for every base — so
    /// that `0b12` is one token to complain about rather than a token and a
    /// half. Nothing here is deciding whether the digits are valid for the
    /// base; the answer is the same either way.
    #[regex(r"0[xXbBoO][0-9A-Fa-f_]+")]
    BasedLit,
    #[token("\"", lex_string)]
    // A backtick literal is the *same token*: a `String` either way, so the
    // parser, the type checker and the backend never learn there were two
    // spellings. What differs is where it ends -- see `lex_backtick`.
    #[token("`", lex_backtick)]
    StringLit,

    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,

    /// `'a'`, and never `'ef`.
    ///
    /// **Exactly one character between the quotes, and that is what keeps a
    /// row variable a row variable.** Both start with `'`, so the two patterns
    /// compete at every apostrophe in the file and logos gives it to whichever
    /// matches more: `'a'` is three characters and `'a` is two, so a char
    /// literal wins; `'ef` has no closing quote, nothing matches three, and
    /// the row variable wins.
    ///
    /// **The `*` version of this regex is a bug**, and an easy one to write.
    /// `'(..)*'` over `<'a, 'b>` matches from the first apostrophe to the
    /// second — six characters, longer than `'a` — and a generic list becomes
    /// a character literal containing a comma. One character, not any number.
    ///
    /// The unicode escape is spelled out rather than left to `\\.` so that
    /// `'\u{1F600}'` is one token; every other escape is two characters and
    /// `\\.` covers them.
    #[regex(r"'([^'\\\n]|\\u\{[0-9A-Fa-f]{1,6}\}|\\.)'")]
    CharLit,

    #[regex(r"'[A-Za-z_][A-Za-z0-9_]*")]
    RowVar,

    #[token(";")]
    Semicolon,
    #[token(",")]
    Comma,
    /// `..`, which only a record update uses: `{ ..old, field: value }`.
    ///
    /// Longest match, so `.` is unaffected — logos prefers this where both
    /// apply, and nothing else in the language spells two dots.
    #[token("..")]
    DotDot,
    #[token(".")]
    Dot,
    #[token("::")]
    ColonColon,
    #[token(":")]
    Colon,
    // Before `||`, so the longer one wins. `a || > b` has no valid parse, so
     // nothing legitimate is taken away by preferring it.
    #[token("||>")]
    PipePipeGt,
    #[token("|>")]
    PipeGt,
    #[token("||")]
    PipePipe,
    #[token("|")]
    Pipe,
    #[token("->")]
    ThinArrow,
    #[token("=>")]
    FatArrow,
    #[token("==")]
    EqEq,
    #[token("=")]
    Eq,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("!=")]
    BangEq,
    #[token("!")]
    Bang,
    #[token("&&")]
    AmpAmp,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBrack,
    #[token("]")]
    RBrack,
}

/// Consumes a `/* ... */` comment, honouring nesting.
fn lex_block_comment(lex: &mut Lexer<Tok>) -> bool {
    let rest = lex.remainder();
    let bytes = rest.as_bytes();
    let mut depth = 1usize;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'/', b'*') => {
                depth += 1;
                i += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    lex.bump(i);
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    // Unterminated: swallow the rest of the file so we still produce one token.
    lex.bump(rest.len());
    true
}

/// Consumes a double-quoted string literal with backslash escapes. An
/// unterminated literal ends at the newline so recovery stays line-local.
///
/// **`${..}` is part of the literal, quotes and all.** A string stays one token
/// — interpolation is taken apart in `khora-hir`, not here — so the only thing
/// this has to know is that a `"` inside a hole opens a nested string rather
/// than ending the literal. Without that, `"${f("x")}"` ends at the third quote
/// and the rest of the line lexes as code, which surfaces as `expected )`
/// several tokens later and points at nothing to do with strings.
///
/// Braces are counted so a hole may contain them. A newline still ends the
/// literal whatever the depth: an unclosed `${` is a mistake, and swallowing
/// the rest of the file for it would turn one bad line into a bad file.
fn lex_string(lex: &mut Lexer<Tok>) -> bool {
    let rest = lex.remainder();
    let mut escaped = false;
    // How many `${` are open, and whether a nested string is open inside one.
    let mut holes = 0usize;
    let mut nested = false;
    let mut chars = rest.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '$' if !nested && chars.peek().map(|(_, c)| *c) == Some('{') => {
                chars.next();
                holes += 1;
            }
            '{' if holes > 0 && !nested => holes += 1,
            '}' if holes > 0 && !nested => holes -= 1,
            '"' if holes > 0 => nested = !nested,
            '"' => {
                lex.bump(i + 1);
                return true;
            }
            '\n' => {
                lex.bump(i);
                return true;
            }
            _ => {}
        }
    }
    lex.bump(rest.len());
    true
}

/// A backtick string, which may cross lines.
///
/// The same scanner as [`lex_string`] with two differences, and they are the
/// whole of the feature: a newline is ordinary rather than the end, and the
/// delimiter is a backtick.
///
/// Interpolation works, because a reader who has seen `"${name}"` will write
/// it here and be right. `\` escapes a literal backtick, and `\$` a literal
/// dollar, which is what a template for some other tool needs.
///
/// **An unterminated one runs to the end of the file**, which sounds worse
/// than it is: so does an unterminated block comment, and the parser reports a
/// literal that swallowed the rest of the program far more clearly than a
/// lexer could. `lex_string` stops at a newline precisely because a `"` is
/// usually a typo when it reaches one; a backtick that reaches one is doing
/// its job.
fn lex_backtick(lex: &mut Lexer<Tok>) -> bool {
    let rest = lex.remainder();
    let mut escaped = false;
    let mut holes = 0usize;
    let mut nested = false;
    let mut chars = rest.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '$' if !nested && chars.peek().map(|(_, c)| *c) == Some('{') => {
                chars.next();
                holes += 1;
            }
            '{' if holes > 0 && !nested => holes += 1,
            '}' if holes > 0 && !nested => holes -= 1,
            '"' if holes > 0 => nested = !nested,
            '`' if holes == 0 => {
                lex.bump(i + 1);
                return true;
            }
            _ => {}
        }
    }
    lex.bump(rest.len());
    true
}

fn to_kind(tok: Tok, text: &str) -> SyntaxKind {
    use SyntaxKind as S;
    match tok {
        Tok::Whitespace => S::WHITESPACE,
        Tok::LineComment => S::LINE_COMMENT,
        Tok::BlockComment => S::BLOCK_COMMENT,
        Tok::FloatLit => S::FLOAT_LIT,
        Tok::IntLit => S::INT_LIT,
        // **An `INT_LIT`, deliberately.** The whole point of matching `0xFF`
        // is to say something about it, and the saying happens in the parser
        // -- but the tree the parser builds is checked against the lexer's own
        // kinds token for token, so inventing a kind here desynchronises them.
        // Lexing it as the thing it was trying to be keeps every later pass
        // unchanged, and the parser recognises it by its text.
        Tok::BasedLit => S::INT_LIT,
        Tok::DecimalLit => S::DECIMAL_LIT,
        Tok::StringLit => S::STRING_LIT,
        Tok::CharLit => S::CHAR_LIT,
        Tok::RowVar => S::ROW_VAR,
        Tok::Ident => {
            if text == "_" {
                S::UNDERSCORE
            } else {
                S::from_keyword(text).unwrap_or(S::IDENT)
            }
        }
        Tok::Semicolon => S::SEMICOLON,
        Tok::Comma => S::COMMA,
        Tok::DotDot => S::DOT_DOT,
        Tok::Dot => S::DOT,
        Tok::ColonColon => S::COLON_COLON,
        Tok::Colon => S::COLON,
        Tok::PipePipeGt => S::PIPE_PIPE_GT,
        Tok::PipeGt => S::PIPE_GT,
        Tok::PipePipe => S::PIPE_PIPE,
        Tok::Pipe => S::PIPE,
        Tok::ThinArrow => S::THIN_ARROW,
        Tok::FatArrow => S::FAT_ARROW,
        Tok::EqEq => S::EQ_EQ,
        Tok::Eq => S::EQ,
        Tok::Plus => S::PLUS,
        Tok::Minus => S::MINUS,
        Tok::Star => S::STAR,
        Tok::Slash => S::SLASH,
        Tok::Percent => S::PERCENT,
        Tok::BangEq => S::BANG_EQ,
        Tok::Bang => S::BANG,
        Tok::AmpAmp => S::AMP_AMP,
        Tok::LtEq => S::LT_EQ,
        Tok::GtEq => S::GT_EQ,
        Tok::Lt => S::LT,
        Tok::Gt => S::GT,
        Tok::LParen => S::L_PAREN,
        Tok::RParen => S::R_PAREN,
        Tok::LBrace => S::L_BRACE,
        Tok::RBrace => S::R_BRACE,
        Tok::LBrack => S::L_BRACK,
        Tok::RBrack => S::R_BRACK,
    }
}

/// A source string plus the flat token stream covering it end to end.
#[derive(Debug, Clone)]
pub struct LexedStr<'a> {
    text: &'a str,
    kinds: Vec<SyntaxKind>,
    /// `starts[i]` is the offset of token `i`; the vector has one extra
    /// trailing entry equal to `text.len()`.
    starts: Vec<TextSize>,
}

impl<'a> LexedStr<'a> {
    pub fn new(text: &'a str) -> LexedStr<'a> {
        let mut kinds = Vec::new();
        let mut starts = Vec::new();
        let mut lexer = Tok::lexer(text);

        while let Some(res) = lexer.next() {
            let span = lexer.span();
            let kind = match res {
                Ok(tok) => to_kind(tok, &text[span.clone()]),
                Err(()) => SyntaxKind::LEX_ERROR,
            };
            // Coalesce runs of unlexable bytes into a single error token.
            if kind == SyntaxKind::LEX_ERROR && kinds.last() == Some(&SyntaxKind::LEX_ERROR) {
                continue;
            }
            kinds.push(kind);
            starts.push(TextSize::new(span.start as u32));
        }
        starts.push(TextSize::new(text.len() as u32));

        LexedStr { text, kinds, starts }
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn kind(&self, i: usize) -> SyntaxKind {
        self.kinds.get(i).copied().unwrap_or(SyntaxKind::EOF)
    }

    pub fn range(&self, i: usize) -> TextRange {
        TextRange::new(self.starts[i], self.starts[i + 1])
    }

    pub fn text(&self, i: usize) -> &'a str {
        &self.text[self.range(i)]
    }

    pub fn source(&self) -> &'a str {
        self.text
    }

    /// Iterates `(kind, text)` pairs including trivia.
    pub fn iter(&self) -> impl Iterator<Item = (SyntaxKind, &'a str)> + '_ {
        (0..self.len()).map(|i| (self.kind(i), self.text(i)))
    }
}

#[cfg(test)]
mod char_lit_tests {
    use super::*;

    fn kinds(text: &str) -> Vec<SyntaxKind> {
        let lexed = LexedStr::new(text);
        (0..lexed.len())
            .map(|i| lexed.kind(i))
            .filter(|k| *k != SyntaxKind::WHITESPACE)
            .collect()
    }

    /// The apostrophe belongs to two tokens and the tie is broken by length.
    ///
    /// `'a'` is three characters and the row variable `'a` is two, so logos
    /// gives it to the literal; `'ef` has no closing quote, nothing matches
    /// three characters, and the row variable wins.
    #[test]
    fn a_char_literal_and_a_row_variable_do_not_collide() {
        for text in ["'a'", "'é'", "'\u{1F600}'"] {
            assert_eq!(kinds(text), vec![SyntaxKind::CHAR_LIT], "lexing {text:?}");
        }
        for text in ["'ef", "'er", "'a"] {
            assert_eq!(kinds(text), vec![SyntaxKind::ROW_VAR], "lexing {text:?}");
        }
    }

    /// Every escape, spelled the way a program spells it.
    #[test]
    fn the_escapes_are_one_token_each() {
        // `'\n'`, `'\''`, `'\\'` and `'\u{1F600}'` as a reader writes them.
        for text in [r"'\n'", r"'\t'", r"'\''", r"'\\'", r"'\u{1F600}'", r"'\u{7}'"] {
            assert_eq!(kinds(text), vec![SyntaxKind::CHAR_LIT], "lexing {text:?}");
        }
    }

    /// A raw newline between the quotes is not a character literal.
    ///
    /// `'\n'` is four characters; an actual line break there is a mistake, and
    /// letting the literal span it would swallow the rest of the program.
    #[test]
    fn a_literal_does_not_cross_a_line() {
        assert_ne!(kinds("'\n'"), vec![SyntaxKind::CHAR_LIT]);
    }

    /// **The `*` version of this regex eats a generic list.**
    ///
    /// `<'a, 'b>` has two apostrophes on one line, and a greedy char literal
    /// matches from the first to the second — six characters, longer than the
    /// row variable — so the list becomes one character containing a comma.
    /// Exactly one character between the quotes is what prevents it.
    #[test]
    fn a_list_of_row_variables_is_not_one_character() {
        assert_eq!(
            kinds("<'a, 'b>"),
            vec![
                SyntaxKind::LT,
                SyntaxKind::ROW_VAR,
                SyntaxKind::COMMA,
                SyntaxKind::ROW_VAR,
                SyntaxKind::GT
            ]
        );
    }

    /// And a `raises` row keeps its variables.
    #[test]
    fn a_raises_row_is_untouched() {
        assert_eq!(
            kinds("raises 'er + Boom"),
            vec![
                SyntaxKind::RAISES_KW,
                SyntaxKind::ROW_VAR,
                SyntaxKind::PLUS,
                SyntaxKind::IDENT
            ]
        );
    }
}
