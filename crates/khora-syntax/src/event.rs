//! Parser output as a flat event stream, and the pass that turns it into a
//! `rowan` green tree.
//!
//! Decoupling "what the parser decided" from "how the tree is built" is what
//! makes [`Marker::precede`](crate::parser::CompletedMarker::precede) possible:
//! a node can be wrapped in a new parent *after* it has been completed, which
//! is how left-associative operators are parsed without backtracking.

use rowan::{GreenNode, GreenNodeBuilder};
use text_size::TextRange;

use crate::kind::{Khora, SyntaxKind};
use crate::lexer::LexedStr;

#[derive(Debug, Clone)]
pub(crate) enum Event {
    Start {
        kind: SyntaxKind,
        /// Relative offset to a `Start` event that should become this node's
        /// parent, set by `precede`.
        forward_parent: Option<u32>,
    },
    Finish,
    Token {
        kind: SyntaxKind,
    },
    /// Produced by an abandoned marker; skipped during tree building.
    Tombstone,
}

/// A diagnostic produced while parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub range: TextRange,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}..{}: {}",
            u32::from(self.range.start()),
            u32::from(self.range.end()),
            self.message
        )
    }
}

/// Replays `events` against `lexed`, re-inserting trivia so the resulting tree
/// covers every byte of the source.
pub(crate) fn build_tree(lexed: &LexedStr<'_>, mut events: Vec<Event>) -> GreenNode {
    let mut builder = GreenNodeBuilder::new();
    let mut next_tok = 0usize;
    let mut forward_parents = Vec::new();

    let flush_trivia = |builder: &mut GreenNodeBuilder<'_>, next_tok: &mut usize| {
        while *next_tok < lexed.len() && lexed.kind(*next_tok).is_trivia() {
            let kind = lexed.kind(*next_tok);
            builder.token(rowan_kind(kind), lexed.text(*next_tok));
            *next_tok += 1;
        }
    };

    for i in 0..events.len() {
        match std::mem::replace(&mut events[i], Event::Tombstone) {
            Event::Tombstone => {}
            Event::Start { kind, forward_parent } => {
                // Walk the `forward_parent` chain: those nodes were created by
                // `precede` and must be opened outermost-first.
                forward_parents.push(kind);
                let mut idx = i;
                let mut fp = forward_parent;
                while let Some(delta) = fp {
                    idx += delta as usize;
                    fp = match std::mem::replace(&mut events[idx], Event::Tombstone) {
                        Event::Start { kind, forward_parent } => {
                            forward_parents.push(kind);
                            forward_parent
                        }
                        other => unreachable!("forward parent pointed at {other:?}"),
                    };
                }

                // Trivia belongs to the enclosing node, not the one we open.
                if i != 0 {
                    flush_trivia(&mut builder, &mut next_tok);
                }
                for kind in forward_parents.drain(..).rev() {
                    builder.start_node(rowan_kind(kind));
                }
            }
            Event::Finish => {
                // Trailing trivia at end of file must land inside the root.
                if i == events.len() - 1 {
                    flush_trivia(&mut builder, &mut next_tok);
                }
                builder.finish_node()
            }
            Event::Token { kind } => {
                flush_trivia(&mut builder, &mut next_tok);
                // A contextual keyword is lexed as `IDENT` and remapped by the
                // parser, so the two kinds legitimately differ there — and only
                // there.
                debug_assert!(
                    lexed.kind(next_tok) == kind
                        || (lexed.kind(next_tok) == SyntaxKind::IDENT
                            && kind.is_contextual_keyword()),
                    "token stream desynchronised at token {next_tok}: lexer said {:?}, parser said {kind:?}",
                    lexed.kind(next_tok)
                );
                builder.token(rowan_kind(kind), lexed.text(next_tok));
                next_tok += 1;
            }
        }
    }

    builder.finish()
}

fn rowan_kind(kind: SyntaxKind) -> rowan::SyntaxKind {
    <Khora as rowan::Language>::kind_to_raw(kind)
}
