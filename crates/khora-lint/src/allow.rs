//! `// @klint allow <lint>`: saying at the line that this one is deliberate.
//!
//! `[lints]` in the manifest sets a level per lint per package, and that is a
//! blunt instrument. Six more lints with no finer dial than that produces a
//! manifest with half of them switched off, which is worse than not having
//! them — a lint people disable wholesale stops being evidence about anything,
//! and it is invisible at every line it would have fired on.
//!
//! ```khora
//! // @klint allow discarded-result
//! save_to_disk(record);
//!
//! save_to_disk(record); // @klint allow discarded-result
//! ```
//!
//! `docs/design/lint-hatch.md` weighs this against attributes and says why
//! this is the answer for now. Roadmap 14.22's prerequisite.
//!
//! # Why it is checked, and why that is the whole argument
//!
//! The usual and correct objection to a magic comment is that it fails
//! silently: misspell the lint and nothing suppresses, nothing complains, and
//! the reader believes the line is handled. So this one is checked. A name
//! that is not a lint is reported as [`crate::UNKNOWN_ALLOW`]; a pragma that
//! suppressed nothing is reported as [`crate::USELESS_ALLOW`].
//!
//! Without those two, a comment pragma would be the one place in this
//! toolchain that goes wrong quietly.
//!
//! # `@klint`, not `khora:`
//!
//! Chosen to be *unmissable* rather than tidy. A directive that changes what
//! the compiler reports should not read like an aside, and `@klint` is
//! greppable, cannot be mistaken for prose, and does not look like an
//! attribute — which would raise the question of why it is not one.
//!
//! # One statement, never a block
//!
//! A pragma governs the statement it trails, or the one on the next line that
//! has code on it. Nothing wider. A block-level allow is how a suppression
//! written for one line grows to cover cases nobody has looked at since.

use khora_syntax::{LexedStr, SyntaxKind};
use text_size::TextRange;

/// The marker that makes a comment a directive.
pub const MARKER: &str = "@klint";

/// One `// @klint allow <lint>` in a file.
#[derive(Debug, Clone)]
pub struct Allow {
    /// The lint it names, exactly as written.
    pub lint: String,
    /// The line it governs, zero-based.
    pub line: u32,
    /// Where the pragma itself is, for reporting about the pragma.
    pub range: TextRange,
    /// Whether anything was actually suppressed by it.
    pub used: bool,
}

/// Every pragma in `text`, with the line each one governs.
///
/// Read from the token stream rather than the text, so that `@klint` inside a
/// string literal is a string literal. Doc comments are skipped: `/// // @klint
/// allow x` in an example is documentation about the feature, not a use of it.
pub fn allows(text: &str) -> Vec<Allow> {
    let lexed = LexedStr::new(text);
    let starts = line_starts(text);

    // Which lines have code on them, so that a pragma on its own line can find
    // the next line that is actually a statement rather than a blank or
    // another comment.
    let mut has_code: Vec<bool> = vec![false; starts.len()];
    let mut comment_at: Vec<(usize, TextRange)> = Vec::new();
    for index in 0..lexed.len() {
        let kind = lexed.kind(index);
        let range = lexed.range(index);
        let line = line_of(&starts, u32::from(range.start()));
        if kind == SyntaxKind::LINE_COMMENT || kind == SyntaxKind::BLOCK_COMMENT {
            comment_at.push((line, range));
        } else if kind != SyntaxKind::WHITESPACE {
            has_code[line] = true;
        }
    }

    let mut out = Vec::new();
    for (line, range) in comment_at {
        let Some(lint) = directive(&text[range]) else { continue };
        // On a line with code, it trails that statement. On a line of its own,
        // it governs the next line that has any.
        let governed = if has_code[line] {
            line
        } else {
            match (line + 1..has_code.len()).find(|&later| has_code[later]) {
                Some(later) => later,
                // A pragma at the end of a file governs nothing, which
                // `useless-allow` is there to say.
                None => line,
            }
        };
        out.push(Allow {
            lint,
            line: governed as u32,
            range,
            used: false,
        });
    }
    out
}

/// The lint a comment names, if it is a directive at all.
///
/// Deliberately strict about shape. `// @klint allow foo` and nothing else:
/// no other verb yet, and refusing to guess at one now means `deny` or `warn`
/// can be added later without an older toolchain having silently accepted
/// something it did not understand.
fn directive(comment: &str) -> Option<String> {
    let body = comment.strip_prefix("//")?;
    // `///` is documentation. A pragma in an example belongs to the example.
    if body.starts_with('/') {
        return None;
    }
    let mut words = body.split_whitespace();
    if words.next()? != MARKER {
        return None;
    }
    if words.next()? != "allow" {
        return None;
    }
    let lint = words.next()?.to_string();
    // One lint per pragma. Two on a line is ambiguous about which governs
    // what, and writing two lines costs nothing.
    if words.next().is_some() {
        return None;
    }
    Some(lint)
}

/// The byte offset each line starts at.
fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(at as u32 + 1);
        }
    }
    starts
}

/// Which line an offset is on, zero-based.
fn line_of(starts: &[u32], offset: u32) -> usize {
    match starts.binary_search(&offset) {
        Ok(exact) => exact,
        Err(after) => after - 1,
    }
}
