//! Hand-written recursive-descent parser with Pratt-style expression parsing.
//!
//! The parser sees a *trivia-free* token stream and never touches text; it only
//! emits [`Event`]s. Error recovery is token-set based: when a construct fails
//! we record a diagnostic and skip tokens until we reach something plausible,
//! so a single typo does not cascade.

mod decls;
mod exprs;
mod patterns;
mod types;

use text_size::TextRange;

use crate::event::{Event, ParseError};
use crate::kind::SyntaxKind::{self, *};
use crate::lexer::LexedStr;

/// Number of consecutive non-consuming steps tolerated before we declare a
/// grammar bug. Keeps a malformed file from hanging the LSP.
const STEP_LIMIT: u32 = 10_000;

pub(crate) struct Parser {
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

impl Parser {
    pub(crate) fn new(lexed: &LexedStr<'_>) -> Parser {
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
    pub(crate) fn without_record_literals<T>(&mut self, f: impl FnOnce(&mut Parser) -> T) -> T {
        let saved = std::mem::replace(&mut self.record_literals_allowed, false);
        let out = f(self);
        self.record_literals_allowed = saved;
        out
    }

    /// Runs `f` with record literals permitted again (inside parentheses,
    /// argument lists and arm bodies).
    pub(crate) fn with_record_literals<T>(&mut self, f: impl FnOnce(&mut Parser) -> T) -> T {
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

    // --- token consumption -----------------------------------------------

    pub(crate) fn bump_any(&mut self) {
        let kind = self.current();
        if kind == EOF {
            return;
        }
        self.do_bump(kind);
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

    // --- diagnostics ------------------------------------------------------

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

    /// Records an error and skips tokens until one in `recovery` (or EOF).
    pub(crate) fn err_recover(&mut self, message: impl Into<String>, recovery: &[SyntaxKind]) {
        if self.at_any(recovery) || self.at(EOF) {
            self.error(message);
            return;
        }
        let m = self.start();
        self.error(message);
        while !self.at(EOF) && !self.at_any(recovery) {
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
    pub(crate) fn complete(mut self, p: &mut Parser, kind: SyntaxKind) -> CompletedMarker {
        self.completed = true;
        match &mut p.events[self.pos] {
            slot @ Event::Tombstone => *slot = Event::Start { kind, forward_parent: None },
            other => unreachable!("marker slot already filled with {other:?}"),
        }
        p.events.push(Event::Finish);
        CompletedMarker { pos: self.pos, kind }
    }

    pub(crate) fn abandon(mut self, p: &mut Parser) {
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

    pub(crate) fn precede(self, p: &mut Parser) -> Marker {
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
pub(crate) fn source_file(p: &mut Parser) {
    let m = p.start();
    decls::source_file_contents(p);
    m.complete(p, SOURCE_FILE);
}
