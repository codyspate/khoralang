//! Hand-written recursive-descent parser with Pratt-style expression parsing.
//!
//! The parser sees a *trivia-free* token stream and emits [`Event`]s. Error
//! recovery is predicate-based: when a construct fails we record a diagnostic
//! and skip tokens until we reach something plausible, so a single typo does
//! not cascade.
//!
//! # Why the parser holds the source text
//!
//! It borrows the source for one purpose: recognizing **contextual keywords**.
//! `handler`, `for`, `context`, `test`, `bench` and `derive` are lexed as
//! `IDENT` so they remain usable as parameter names, fields, locals and types; the parser reads
//! their spelling in the one position where each is a keyword and remaps the
//! token with [`Parser::bump_contextual`].
//!
//! The alternative — keeping the parser text-free and recognizing these words
//! positionally by kind and lookahead — cannot work: every one of them arrives
//! as `IDENT`, so lookahead alone can never say *which* identifier it is. Any
//! scheme that avoids the borrow ends up smuggling the text in anyway (a
//! side table of interned spellings, a pre-pass that rewrites kinds), which is
//! the same coupling with more moving parts. rust-analyzer settled here for the
//! same reason, and the cost is one lifetime on a type that already borrows the
//! [`LexedStr`] it was built from.

mod decls;
mod exprs;
mod patterns;
mod types;

use text_size::{TextRange, TextSize};

use crate::event::{Event, ParseError};
use crate::kind::SyntaxKind::{self, *};
use crate::lexer::LexedStr;

/// Number of consecutive non-consuming steps tolerated before we declare a
/// grammar bug. Keeps a malformed file from hanging the LSP.
const STEP_LIMIT: u32 = 10_000;

pub(crate) struct Parser<'a> {
    /// The whole source, read only through [`Parser::nth_text`].
    text: &'a str,
    kinds: Vec<SyntaxKind>,
    ranges: Vec<TextRange>,
    pos: usize,
    steps: u32,
    events: Vec<Event>,
    errors: Vec<ParseError>,
    /// Cleared while parsing a `match` scrutinee, where a following `{` opens
    /// the arm list rather than a record literal.
    record_literals_allowed: bool,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(lexed: &LexedStr<'a>) -> Parser<'a> {
        let mut kinds = Vec::with_capacity(lexed.len());
        let mut ranges = Vec::with_capacity(lexed.len());
        for i in 0..lexed.len() {
            if !lexed.kind(i).is_trivia() {
                kinds.push(lexed.kind(i));
                ranges.push(lexed.range(i));
            }
        }
        let end = text_size::TextSize::new(lexed.source().len() as u32);
        kinds.push(EOF);
        ranges.push(TextRange::empty(end));

        Parser {
            text: lexed.source(),
            kinds,
            ranges,
            pos: 0,
            steps: 0,
            events: Vec::new(),
            errors: Vec::new(),
            record_literals_allowed: true,
        }
    }

    /// Runs `f` with record literals disabled, restoring the previous setting.
    pub(crate) fn without_record_literals<T>(&mut self, f: impl FnOnce(&mut Parser<'a>) -> T) -> T {
        let saved = std::mem::replace(&mut self.record_literals_allowed, false);
        let out = f(self);
        self.record_literals_allowed = saved;
        out
    }

    /// Runs `f` with record literals permitted again (inside parentheses,
    /// argument lists and arm bodies).
    pub(crate) fn with_record_literals<T>(&mut self, f: impl FnOnce(&mut Parser<'a>) -> T) -> T {
        let saved = std::mem::replace(&mut self.record_literals_allowed, true);
        let out = f(self);
        self.record_literals_allowed = saved;
        out
    }

    pub(crate) fn record_literals_allowed(&self) -> bool {
        self.record_literals_allowed
    }

    pub(crate) fn finish(self) -> (Vec<Event>, Vec<ParseError>) {
        (self.events, self.errors)
    }

    // --- token inspection ------------------------------------------------

    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        self.kinds.get(self.pos + n).copied().unwrap_or(EOF)
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    pub(crate) fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        self.nth(n) == kind
    }

    pub(crate) fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.contains(&self.current())
    }

    pub(crate) fn current_range(&self) -> TextRange {
        self.ranges[self.pos.min(self.ranges.len() - 1)]
    }

    /// True at a token that can begin a declaration, including the contextual
    /// declaration keywords. The recovery point after a broken declaration.
    pub(crate) fn at_decl_start(&self) -> bool {
        self.current().is_decl_start()
            || self.at_contextual(CONTEXT_KW)
            || self.at_contextual(TEST_KW)
            || self.at_contextual(BENCH_KW)
            || self.at_contextual(DERIVE_KW)
    }

    // --- contextual keywords ----------------------------------------------

    /// The source text of the token `n` ahead. Empty at EOF.
    fn nth_text(&self, n: usize) -> &'a str {
        match self.ranges.get(self.pos + n) {
            Some(range) => &self.text[*range],
            None => "",
        }
    }

    /// True when the current token is the `IDENT` spelling the contextual
    /// keyword `kind`.
    pub(crate) fn at_contextual(&self, kind: SyntaxKind) -> bool {
        self.nth_at_contextual(0, kind)
    }

    /// [`Parser::at_contextual`] with lookahead.
    pub(crate) fn nth_at_contextual(&self, n: usize, kind: SyntaxKind) -> bool {
        debug_assert!(kind.is_contextual_keyword(), "{kind:?} is not a contextual keyword");
        self.nth(n) == IDENT && kind.contextual_keyword_text() == Some(self.nth_text(n))
    }

    /// Consumes the current `IDENT` and records it in the tree as `kind`.
    ///
    /// Remapping rather than leaving the token an `IDENT` keeps the CST — and
    /// so every consumer of it, from `debug_tree` to the formatter — identical
    /// to what a hard keyword would have produced.
    pub(crate) fn bump_contextual(&mut self, kind: SyntaxKind) {
        assert!(self.at_contextual(kind), "expected the contextual keyword {kind:?}");
        self.do_bump(kind);
    }

    // --- token consumption -----------------------------------------------

    pub(crate) fn bump_any(&mut self) {
        let kind = self.current();
        if kind == EOF {
            return;
        }
        if kind == STRING_LIT {
            self.check_escapes();
        }
        self.do_bump(kind);
    }

    /// Refuses a backslash escape the language does not know.
    ///
    /// **Because the alternative is silence.** An unrecognised escape used to
    /// be kept as the two characters it was written with, so `"\\u{0}"` -- the
    /// spelling Rust, JavaScript and Python all use -- became six literal
    /// characters beginning with a backslash. That compiled, ran, and produced
    /// a string nobody wanted; the one place it mattered, it packed an
    /// argument buffer with `\\u{0}` between the arguments and every command
    /// failed to start, reporting the error a *missing program* reports. An
    /// hour went into the wrong file.
    ///
    /// A typo in an escape is never what somebody meant. Saying so costs one
    /// scan of a token the parser is already holding.
    fn check_escapes(&mut self) {
        let text = self.nth_text(0);
        let start = usize::from(self.current_range().start());
        let mut chars = text.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c != '\\' {
                continue;
            }
            let Some((_, escape)) = chars.next() else { break };
            match escape {
                'n' | 'r' | 't' | '0' | '\\' | '"' | '\'' | '`' | '$' => {}
                // A line continuation: the newline and the indentation
                // after it are not in the string. What a long message in
                // a deeply indented file is written with, and what every
                // neighbouring language spells the same way.
                '\n' | '\r' => {}
                // `\u{1F600}`, the spelling every neighbouring language uses.
                'u' => {
                    let bad = TextRange::new(
                        TextSize::from((start + at) as u32),
                        TextSize::from((start + at + 2) as u32),
                    );
                    match take_unicode(&mut chars) {
                        Unicode::Ok => {}
                        Unicode::Malformed => self.error_at(
                            bad,
                            "a `\\u` escape is written `\\u{..}` around one to six hex digits",
                        ),
                        Unicode::NotACharacter => self.error_at(
                            bad,
                            "that is not a character: a `\\u` escape names a Unicode scalar \
                             value, so it has to be at most `10FFFF` and not a surrogate \
                             between `D800` and `DFFF`",
                        ),
                    }
                }
                other => {
                    let width = 1 + other.len_utf8();
                    let bad = TextRange::new(
                        TextSize::from((start + at) as u32),
                        TextSize::from((start + at + width) as u32),
                    );
                    self.error_at(
                        bad,
                        format!(
                            "`\\{other}` is not an escape. The escapes are \
                             `\\n`, `\\r`, `\\t`, `\\0`, `\\\\`, `\\\"`, `\\'`, `` \\` ``, \
                             `\\$` and `\\u{{..}}` -- write `\\\\{other}` for a backslash \
                             followed by `{other}`"
                        ),
                    );
                }
            }
        }
    }

    pub(crate) fn bump(&mut self, kind: SyntaxKind) {
        assert_eq!(self.current(), kind, "expected to bump {kind:?}");
        self.do_bump(kind);
    }

    fn do_bump(&mut self, kind: SyntaxKind) {
        self.pos += 1;
        self.steps = 0;
        self.events.push(Event::Token { kind });
    }

    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.do_bump(kind);
            true
        } else {
            false
        }
    }

    /// Consumes `kind` if present, otherwise records an error and returns false.
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        self.error(format!("expected {}", describe(kind)));
        false
    }

    // --- delimiters -------------------------------------------------------

    /// Consumes an opening delimiter, remembering where it was.
    ///
    /// Paired with [`Parser::close`]. An unclosed brace is usually reported at
    /// the end of the file, which is the one place in the program the reader
    /// cannot use: the mistake is wherever the brace was opened, often hundreds
    /// of lines earlier.
    pub(crate) fn open(&mut self, kind: SyntaxKind) -> Option<TextRange> {
        let range = self.current_range();
        if self.eat(kind) {
            return Some(range);
        }
        self.error(format!("expected {}", describe(kind)));
        None
    }

    /// Consumes a closing delimiter, blaming the opener when it is missing.
    pub(crate) fn close(&mut self, kind: SyntaxKind, opened: Option<TextRange>) {
        if self.eat(kind) {
            return;
        }
        match opened {
            Some(at) => {
                let opener = match kind {
                    R_BRACE => "{",
                    R_PAREN => "(",
                    _ => "[",
                };
                self.error_at(at, format!("this `{opener}` is never closed"));
            }
            // The opener was missing too, so there is nothing better to point
            // at than where the closer should have been.
            None => self.error(format!("expected {}", describe(kind))),
        }
    }

    // --- diagnostics ------------------------------------------------------

    /// Reports at a range other than the current token.
    pub(crate) fn error_at(&mut self, range: TextRange, message: impl Into<String>) {
        let message = message.into();
        if self.errors.last().is_some_and(|e| e.range == range && e.message == message) {
            return;
        }
        self.errors.push(ParseError { message, range });
    }

    pub(crate) fn error(&mut self, message: impl Into<String>) {
        let range = self.current_range();
        let message = message.into();
        // Suppress duplicates at the same offset: nested rules usually report
        // the same underlying mistake more than once.
        if self.errors.last().is_some_and(|e| e.range == range && e.message == message) {
            return;
        }
        self.errors.push(ParseError { message, range });
    }

    /// Records an error and wraps the offending token in an `ERROR` node.
    pub(crate) fn err_and_bump(&mut self, message: impl Into<String>) {
        let m = self.start();
        self.error(message);
        self.bump_any();
        m.complete(self, ERROR);
    }

    /// Records an error and skips tokens until `at_recovery` accepts one (or
    /// EOF).
    ///
    /// The recovery point is a predicate rather than a token set because a
    /// contextual keyword is not identifiable by kind: `context` and `test`
    /// both arrive as `IDENT`, so "the next declaration starts here" can only
    /// be answered by [`Parser::at_decl_start`].
    pub(crate) fn err_recover(
        &mut self,
        message: impl Into<String>,
        at_recovery: impl Fn(&Parser<'a>) -> bool,
    ) {
        if at_recovery(self) || self.at(EOF) {
            self.error(message);
            return;
        }
        let m = self.start();
        self.error(message);
        while !self.at(EOF) && !at_recovery(self) {
            self.bump_any();
        }
        m.complete(self, ERROR);
    }

    /// Guards a grammar loop against making no progress. Returns `false` once
    /// the step budget is exhausted, at which point the caller must break.
    pub(crate) fn tick(&mut self) -> bool {
        self.steps += 1;
        self.steps <= STEP_LIMIT
    }

    // --- markers ----------------------------------------------------------

    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Tombstone);
        Marker { pos, completed: false }
    }
}

fn describe(kind: SyntaxKind) -> &'static str {
    match kind {
        SEMICOLON => "`;`",
        COMMA => "`,`",
        DOT => "`.`",
        COLON => "`:`",
        EQ => "`=`",
        THIN_ARROW => "`->`",
        FAT_ARROW => "`=>`",
        PIPE => "`|`",
        L_PAREN => "`(`",
        R_PAREN => "`)`",
        L_BRACE => "`{`",
        R_BRACE => "`}`",
        L_BRACK => "`[`",
        R_BRACK => "`]`",
        LT => "`<`",
        GT => "`>`",
        IDENT => "an identifier",
        MODULE_KW => "`module`",
        IMPORT_KW => "`import`",
        TYPE_KW => "`type`",
        FN_KW => "`fn`",
        MATCH_KW => "`match`",
        LET_KW => "`let`",
        _ => "a token",
    }
}

/// An open node. Must be `complete`d or `abandon`ed.
#[must_use]
pub(crate) struct Marker {
    pos: usize,
    completed: bool,
}

impl Marker {
    pub(crate) fn complete(mut self, p: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        self.completed = true;
        match &mut p.events[self.pos] {
            slot @ Event::Tombstone => *slot = Event::Start { kind, forward_parent: None },
            other => unreachable!("marker slot already filled with {other:?}"),
        }
        p.events.push(Event::Finish);
        CompletedMarker { pos: self.pos, kind }
    }

    pub(crate) fn abandon(mut self, p: &mut Parser<'_>) {
        self.completed = true;
        if self.pos == p.events.len() - 1 {
            // Nothing was emitted after this marker: drop the slot entirely.
            match p.events.pop() {
                Some(Event::Tombstone) => {}
                other => unreachable!("abandoned marker was overwritten by {other:?}"),
            }
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if !self.completed && !std::thread::panicking() {
            panic!("marker dropped without being completed or abandoned");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletedMarker {
    pos: usize,
    kind: SyntaxKind,
}

impl CompletedMarker {
    /// Opens a node that becomes the parent of this one. This is how
    /// left-associative operators are built without backtracking: parse the
    /// left operand, then wrap it once the operator is seen.
    pub(crate) fn kind(self) -> SyntaxKind {
        self.kind
    }

    pub(crate) fn precede(self, p: &mut Parser<'_>) -> Marker {
        let new_marker = p.start();
        match &mut p.events[self.pos] {
            Event::Start { forward_parent, .. } => {
                *forward_parent = Some((new_marker.pos - self.pos) as u32);
            }
            other => unreachable!("preceding a non-start event {other:?}"),
        }
        new_marker
    }
}

/// Grammar entry point.
pub(crate) fn source_file(p: &mut Parser<'_>) {
    let m = p.start();
    decls::source_file_contents(p);
    m.complete(p, SOURCE_FILE);
}

/// Consumes `{` one to six hex digits `}` from `chars`, and says what it was.
///
/// The caller has already taken the `u`. Three answers rather than two,
/// because a number that is not a character is a different mistake from a
/// malformed escape and deserves a different sentence.
enum Unicode {
    /// A character.
    Ok,
    /// Not written `\u{..}` around hex digits at all.
    Malformed,
    /// Well-formed and not a Unicode scalar value: past `10FFFF`, or one half
    /// of a surrogate pair.
    NotACharacter,
}

fn take_unicode(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Unicode {
    if chars.peek().map(|(_, c)| *c) != Some('{') {
        return Unicode::Malformed;
    }
    chars.next();
    let mut value: u32 = 0;
    let mut digits = 0;
    while let Some((_, c)) = chars.peek() {
        let Some(digit) = c.to_digit(16) else { break };
        value = value * 16 + digit;
        digits += 1;
        chars.next();
    }
    if digits == 0 || digits > 6 || chars.peek().map(|(_, c)| *c) != Some('}') {
        return Unicode::Malformed;
    }
    chars.next();
    // `char::from_u32` refuses both of the things that are not characters, so
    // there is nothing to spell out here that it does not already know.
    if char::from_u32(value).is_some() { Unicode::Ok } else { Unicode::NotACharacter }
}

