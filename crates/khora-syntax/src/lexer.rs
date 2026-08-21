//! `logos`-backed tokenizer.
//!
//! The lexer is lossless: whitespace and comments are emitted as ordinary
//! tokens so the parser can build a CST whose text is byte-identical to the
//! input. Unrecognised bytes become [`SyntaxKind::LEX_ERROR`] tokens rather
//! than aborting the lex, which keeps the LSP useful inside broken files.

use logos::{Lexer, Logos};
use text_size::{TextRange, TextSize};

use crate::kind::SyntaxKind;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    #[regex(r"[ \t\r\n\x0c]+")]
    Whitespace,
    #[regex(r"//[^\r\n]*")]
    LineComment,
    #[token("/*", lex_block_comment)]
    BlockComment,

    #[regex(r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9]+)?")]
    FloatLit,
    #[regex(r"[0-9][0-9_]*")]
    IntLit,
    #[token("\"", lex_string)]
    StringLit,

    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,
    #[regex(r"'[A-Za-z_][A-Za-z0-9_]*")]
    RowVar,

    #[token(";")]
    Semicolon,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("::")]
    ColonColon,
    #[token(":")]
    Colon,
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
fn lex_string(lex: &mut Lexer<Tok>) -> bool {
    let rest = lex.remainder();
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
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

fn to_kind(tok: Tok, text: &str) -> SyntaxKind {
    use SyntaxKind as S;
    match tok {
        Tok::Whitespace => S::WHITESPACE,
        Tok::LineComment => S::LINE_COMMENT,
        Tok::BlockComment => S::BLOCK_COMMENT,
        Tok::FloatLit => S::FLOAT_LIT,
        Tok::IntLit => S::INT_LIT,
        Tok::StringLit => S::STRING_LIT,
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
        Tok::Dot => S::DOT,
        Tok::ColonColon => S::COLON_COLON,
        Tok::Colon => S::COLON,
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
