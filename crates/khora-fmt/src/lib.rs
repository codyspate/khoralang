//! The canonical Khora formatter.
//!
//! # Approach
//!
//! The formatter **preserves where the author broke lines and normalizes
//! everything else**: indentation, spacing around punctuation and operators,
//! blank-line runs, and the contents of import lists.
//!
//! It deliberately does not reflow. A formatter that decides line breaks from a
//! width budget has to understand the semantics of every construct to break
//! them well, and gets the interesting cases — long pipelines, nested records —
//! wrong in ways that are hard to argue with. §6.2 asks for two-space indent,
//! explicit semicolons, pipeline continuations aligned to their expression, and
//! sorted imports. All of that is achievable without moving a single line break
//! the author chose.
//!
//! That choice buys two properties which are otherwise hard to guarantee and
//! which the tests check directly:
//!
//! - **Idempotence.** Formatting twice equals formatting once, because the
//!   output's line structure is the input's.
//! - **Token preservation.** Tokens are emitted in the order the parser saw
//!   them, so no code can be lost or reordered. Import lists are the single
//!   deliberate exception.
//!
//! # Broken input
//!
//! [`format`] refuses to touch a file that does not parse. Reformatting a file
//! mid-edit, while a brace is unbalanced, is exactly when a formatter can do the
//! most damage.

use khora_syntax::{ParseError, SyntaxElement, SyntaxKind, SyntaxKind::*, SyntaxNode};

const INDENT: &str = "  ";

/// Formats a source file.
///
/// Returns the parse diagnostics unchanged if the input does not parse; the
/// caller decides whether to report them.
pub fn format(src: &str) -> Result<String, Vec<ParseError>> {
    let parse = khora_syntax::parse(src);
    if !parse.errors().is_empty() {
        return Err(parse.errors().to_vec());
    }
    let mut f = Formatter::new();
    f.node(&parse.syntax(), SOURCE_FILE);
    Ok(f.finish())
}

/// Whether `src` is already formatted.
pub fn is_formatted(src: &str) -> Result<bool, Vec<ParseError>> {
    format(src).map(|formatted| formatted == src)
}

/// What should separate the previous token from the next one.
///
/// Ordered so that a stronger separator wins when several are requested for the
/// same gap, which is what makes runs of blank lines collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sep {
    None,
    Space,
    Newline,
    Blank,
}

struct Formatter {
    out: String,
    depth: usize,
    pending: Sep,
    /// The token most recently written, with the node that owned it. Spacing
    /// rules need both: `.` is tight in a path but spaced in a `forall`.
    prev: Option<(SyntaxKind, SyntaxKind)>,
    /// Depth of enclosing `<..>` type brackets. Tracked separately because a
    /// token's immediate parent is its own node (`NAME`, `PATH`), not the
    /// bracket list, so the immediate parent cannot answer this.
    type_depth: usize,
    started: bool,
}

impl Formatter {
    fn new() -> Formatter {
        Formatter {
            out: String::new(),
            depth: 0,
            pending: Sep::None,
            prev: None,
            type_depth: 0,
            started: false,
        }
    }

    fn finish(mut self) -> String {
        // Exactly one trailing newline, whatever the input had.
        while self.out.ends_with([' ', '\n']) {
            self.out.pop();
        }
        self.out.push('\n');
        self.out
    }

    fn node(&mut self, node: &SyntaxNode, parent: SyntaxKind) {
        // Import lists are the one place tokens are reordered.
        if node.kind() == IMPORT_LIST {
            self.import_list(node);
            return;
        }
        let brackets = matches!(node.kind(), TYPE_ARGS | TYPE_PARAMS);
        if brackets {
            self.type_depth += 1;
        }
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Node(n) => {
                    let kind = n.kind();
                    self.node(&n, kind);
                }
                SyntaxElement::Token(t) => self.token(t.kind(), t.text(), parent),
            }
        }
        if brackets {
            self.type_depth -= 1;
        }
    }

    fn token(&mut self, kind: SyntaxKind, text: &str, parent: SyntaxKind) {
        match kind {
            WHITESPACE => {
                let newlines = text.bytes().filter(|b| *b == b'\n').count();
                let sep = match newlines {
                    0 => Sep::Space,
                    1 => Sep::Newline,
                    _ => Sep::Blank,
                };
                self.pending = self.pending.max(sep);
                return;
            }
            R_BRACE | R_PAREN | R_BRACK => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }

        self.write(kind, text, parent);

        if matches!(kind, L_BRACE | L_PAREN | L_BRACK) {
            self.depth += 1;
        }
    }

    fn write(&mut self, kind: SyntaxKind, text: &str, parent: SyntaxKind) {
        if !self.started {
            self.out.push_str(text);
            self.started = true;
            self.prev = Some((kind, parent));
            self.pending = Sep::None;
            return;
        }

        let sep = self.separator(kind, parent);
        match sep {
            Sep::None => {}
            Sep::Space => self.out.push(' '),
            Sep::Newline | Sep::Blank => {
                if sep == Sep::Blank {
                    self.out.push('\n');
                }
                self.out.push('\n');
                // A line that opens with a continuation gets one extra level,
                // which is what "aligned to the source expression" means for a
                // multi-line pipeline.
                let extra = usize::from(is_continuation(kind, parent));
                for _ in 0..self.depth + extra {
                    self.out.push_str(INDENT);
                }
            }
        }

        self.out.push_str(text);
        self.prev = Some((kind, parent));
        self.pending = Sep::None;
    }

    /// Resolves the gap before `kind` into a concrete separator.
    fn separator(&self, kind: SyntaxKind, parent: SyntaxKind) -> Sep {
        // An author's line break is always honoured.
        if self.pending >= Sep::Newline {
            // A blank line inside a signature or before a closing brace reads
            // as an accident; collapse it.
            if self.pending == Sep::Blank && matches!(kind, R_BRACE | R_PAREN | R_BRACK) {
                return Sep::Newline;
            }
            return self.pending;
        }

        let Some((prev, prev_parent)) = self.prev else {
            return Sep::None;
        };

        if self.no_space_between(prev, prev_parent, kind, parent) {
            Sep::None
        } else {
            Sep::Space
        }
    }

    fn no_space_between(
        &self,
        prev: SyntaxKind,
        prev_parent: SyntaxKind,
        next: SyntaxKind,
        parent: SyntaxKind,
    ) -> bool {
        // Order matters: bracket adjacency is tested before the rules about
        // what opens a call, or `f((1))` gains a stray space.

        // `::` joins path segments and is always tight.
        if prev == COLON_COLON || next == COLON_COLON {
            return true;
        }
        // `.` is tight in field access, but is a separator in
        // `forall <T> . Type` and needs its spaces there.
        if next == DOT {
            return parent != FORALL_TYPE;
        }
        if prev == DOT {
            return prev_parent != FORALL_TYPE;
        }
        // Anything immediately inside a bracket pair hugs it.
        if matches!(prev, L_PAREN | L_BRACK) || matches!(next, R_PAREN | R_BRACK) {
            return true;
        }
        if matches!((prev, next), (L_BRACE, R_BRACE)) {
            return true;
        }
        // §6.2 spells imports `{A, B, C}`; rows elsewhere keep their spaces.
        if parent == IMPORT_LIST && (prev == L_BRACE || next == R_BRACE) {
            return true;
        }
        if matches!(next, COMMA | SEMICOLON | COLON) {
            return true;
        }
        // Generic brackets are tight inside, but `<` only hugs a type name —
        // `forall <T>` keeps its space because a keyword precedes it.
        if self.type_depth > 0 && (prev == LT || next == GT) {
            return true;
        }
        if next == LT && matches!(parent, TYPE_ARGS | TYPE_PARAMS) {
            // `impl<A>` hugs even though a keyword precedes it: the parameters
            // belong to the impl, there is no name for them to attach to, and
            // `impl <A>` reads as a stray space next to `fn f<A>`. `forall <T>`
            // keeps its space — it is a binder terminated by `.`, where the gap
            // separates the bound names from the type that follows.
            return matches!(prev, IDENT | NAME_REF | GT | IMPL_KW);
        }
        // A call or index hugs what it applies to; `fn (a, b)` and `if (c)` do
        // not, because a keyword precedes the bracket.
        if next == L_PAREN {
            return matches!(prev, IDENT | NAME_REF | GT | R_PAREN | R_BRACK | UNDERSCORE);
        }
        if next == L_BRACK {
            return matches!(prev, IDENT | NAME_REF | R_PAREN | R_BRACK);
        }
        // Postfix `!` binds to the call it marks; prefix `!` binds to its operand.
        if next == BANG {
            return true;
        }
        if prev == BANG {
            return prev_parent == PREFIX_EXPR;
        }
        false
    }

    /// Emits `{A, B, C}` sorted and deduplicated, per §6.2.
    fn import_list(&mut self, node: &SyntaxNode) {
        let mut items: Vec<String> = node
            .children()
            .filter(|n| n.kind() == IMPORT_ITEM)
            .map(|n| normalize_import_item(&n))
            .collect();
        items.sort();
        items.dedup();

        self.write(L_BRACE, "{", IMPORT_LIST);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.write(COMMA, ",", IMPORT_LIST);
            }
            self.write(IDENT, item, IMPORT_LIST);
        }
        self.write(R_BRACE, "}", IMPORT_LIST);
    }
}

/// `X` or `X as Y`, with internal whitespace normalized so sorting and
/// deduplication compare like with like.
fn normalize_import_item(node: &SyntaxNode) -> String {
    let parts: Vec<String> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string())
        .collect();
    if parts.is_empty() {
        node.text().to_string().split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        parts.join(" ")
    }
}

/// Whether a token starting a line continues the line above rather than
/// beginning something new. Continuations get one extra level of indent, which
/// is what §6.2 means by a pipeline aligned to its expression.
///
/// This has to consider the parent, because the same token does both jobs:
/// `with` continues a signature but *starts* a handler region.
fn is_continuation(kind: SyntaxKind, parent: SyntaxKind) -> bool {
    match kind {
        PIPE_GT | DOT | THIN_ARROW => true,
        PIPE => parent == VARIANT_CASE,
        WITH_KW => matches!(parent, WITH_CLAUSE | WITH_EXPR),
        RAISES_KW => parent == RAISES_CLAUSE,
        CATCH_KW => parent == CATCH_EXPR,
        _ => false,
    }
}
