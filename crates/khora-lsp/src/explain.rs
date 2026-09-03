//! What a declaration is, in the words its author already wrote.
//!
//! **Hover and completion want the same two things about a name**, and were
//! answering with neither. Hover printed the type of the expression under the
//! cursor, which tells you `List<Int>` and not what the function you are
//! calling does with it. Completion offered a list of names whose only
//! annotation was the word "function". Both had the answer within reach: the
//! declaration is in the tree, and the sentence explaining it is the `///`
//! directly above it, which somebody has already written for most of `std`.
//!
//! So this is the one place that turns a declaration into the two things a
//! person needs while typing: the line that declares it, and the prose above
//! it. `khora doc` publishes the same block from the same reader, so a page on
//! the website and a hover in the editor cannot disagree about what a function
//! is for.
//!
//! # Why the signature is cut rather than rendered
//!
//! A signature could be rebuilt from the checked types, and it would be wrong
//! in the way that matters: the author wrote `fn read<A: Decode>(raw: Raw) ->
//! Validated<A, Rejection>`, and a renderer that walked the type would print
//! the same thing with its parameter names lost and its bounds normalized.
//! What a reader wants to see is what is in the file. So the text is taken
//! from the source and stopped where the body begins.

use khora_db::{Db, SourceFile};
use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::TextRange;

/// A declaration, as a reader wants it explained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explained {
    /// The declaration line, without its body.
    pub signature: String,
    /// The `///` block above it, joined, or `None` when there is none.
    pub docs: Option<String>,
    /// Whether the declaration takes type parameters.
    ///
    /// **The one case where the signature is not the whole answer.** A
    /// generic declaration says `A`, and at a call site `A` is something in
    /// particular; which one it is cannot be worked out from the declaration
    /// and is usually the thing the reader is hovering to find out.
    pub generic: bool,
}

impl Explained {
    /// Markdown for a hover: what this use of the declaration is, then the
    /// declaration itself, then the prose.
    ///
    /// `here` is the checked type of the expression under the cursor, and it
    /// leads when the declaration is generic — at a call site `A` is something
    /// in particular, and that is the half a reader cannot work out for
    /// themselves. For anything else it would repeat the signature below it in
    /// a second notation, so it is left out.
    pub fn markdown_at(&self, here: Option<&str>) -> String {
        let mut out = String::new();
        match (self.generic, here) {
            // **The instantiated type first, and the declaration under
            // it.** At a call site `A` is something in particular, and
            // which one it is cannot be read off the declaration -- so it
            // is the answer, and the generic form is the reference below
            // it. TypeScript orders them this way for the same reason.
            (true, Some(here)) => {
                out.push_str(&format!("```khora\n{here}\n```"));
                out.push_str(&format!(
                    "\n\ndeclared as:\n\n```khora\n{}\n```",
                    self.signature
                ));
            }
            _ => out.push_str(&format!("```khora\n{}\n```", self.signature)),
        }
        if let Some(docs) = &self.docs {
            out.push_str("\n\n");
            out.push_str(docs);
        }
        out
    }
}

/// The declaration covering `range` in `file`.
///
/// `range` is a declaration's own range, as `ItemMap` and `definition::at`
/// both report it. The node is found rather than stored, because rowan keeps
/// every byte and the tree is already built.
pub fn at(db: &dyn Db, file: SourceFile, range: TextRange) -> Option<Explained> {
    let parsed = khora_db::parse(db, file);
    let node = declaration_covering(&parsed.syntax(), range)?;
    Some(Explained {
        signature: signature_of(&node),
        docs: docs_of(&node),
        generic: is_generic(&node),
    })
}

/// The innermost declaration whose range covers `range`.
fn declaration_covering(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    let mut best: Option<SyntaxNode> = None;
    for node in root.descendants() {
        if !is_declaration(node.kind()) || !node.text_range().contains_range(range) {
            continue;
        }
        // An `impl` holds functions, and a method's own `fn` is the answer
        // rather than the block around it.
        let smaller = best.as_ref().is_none_or(|seen| {
            node.text_range().len() < seen.text_range().len()
        });
        if smaller {
            best = Some(node);
        }
    }
    best
}

/// Whether the declaration takes type parameters.
///
/// Asked of the syntax rather than of the checked types, because this decides
/// what to *show* and the author's `<A, B>` is what makes the signature
/// ambiguous on its own.
fn is_generic(node: &SyntaxNode) -> bool {
    use khora_syntax::ast::AstNode;
    if let Some(f) = khora_syntax::ast::FnDecl::cast(node.clone()) {
        return f.type_params().is_some();
    }
    if let Some(t) = khora_syntax::ast::TypeDecl::cast(node.clone()) {
        return t.type_params().is_some();
    }
    false
}

fn is_declaration(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FN_DECL
            | SyntaxKind::TYPE_DECL
            | SyntaxKind::TRAIT_DECL
            | SyntaxKind::EFFECT_DECL
            | SyntaxKind::CONTEXT_DECL
            | SyntaxKind::CONST_DECL
            | SyntaxKind::ASSOC_TYPE_DECL
    )
}

/// The declaration without its body, on one line.
///
/// Stopped at the first `{` that opens a body or the `=` of a type alias,
/// whichever comes first — and *not* at a brace inside a parameter's type,
/// which is why this counts depth rather than taking the first one it sees. A
/// capability row is written `with { env: Env }` and appears before the body
/// in exactly the signatures worth reading.
fn signature_of(node: &SyntaxNode) -> String {
    let text = node.text().to_string();
    let mut depth = 0i32;
    let mut end = text.len();
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            // `with { .. }` and `raises` clauses sit at depth zero too, so a
            // brace only ends the signature when what precedes it is not one
            // of those. Simpler and good enough: the body is the last brace
            // group, and a row is followed by more signature.
            '{' if depth == 0 => {
                if !after_row(&text[..at]) {
                    end = at;
                    break;
                }
                depth += 1;
            }
            '}' if depth > 0 => depth -= 1,
            // A type alias body, and an assignment in a `const`. Not `==`,
            // which is why the next character is looked at.
            '=' if depth == 0
                && !matches!(chars.peek(), Some((_, '=')))
                && matches!(node.kind(), SyntaxKind::TYPE_DECL | SyntaxKind::CONST_DECL) =>
            {
                end = at;
                break;
            }
            _ => {}
        }
    }

    let mut out = String::new();
    for word in text[..end].split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Whether the text just before a `{` makes it a capability row rather than a
/// body: `with` is the only clause that opens one.
fn after_row(before: &str) -> bool {
    before.trim_end().ends_with("with")
}

/// The `///` block above the declaration.
///
/// The same reader `khora doc` publishes from, so a hover and a rendered page
/// cannot disagree about where a comment ends.
fn docs_of(node: &SyntaxNode) -> Option<String> {
    let lines = khora_syntax::doc::doc_comment(node);
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}
