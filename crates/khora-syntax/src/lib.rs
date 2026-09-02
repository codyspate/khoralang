//! Front end for the Khora language: tokenizer, lossless CST parser and a
//! typed AST view over the tree.
//!
//! The parser never fails. It always returns a tree covering the whole input,
//! plus a list of diagnostics — a hard requirement for the LSP, and the reason
//! the CST is built with `rowan` rather than a plain AST.
//!
//! ```
//! let parse = khora_syntax::parse("module app::main;");
//! assert!(parse.errors().is_empty());
//! ```

pub mod ast;
pub mod doc;
mod event;
mod kind;
mod lexer;
mod parser;

use rowan::GreenNode;

pub use event::ParseError;
pub use kind::{
    Khora, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, CONTEXTUAL_KEYWORDS, KEYWORDS,
};
pub use lexer::LexedStr;

/// The result of parsing one source file.
///
/// `PartialEq` matters beyond convenience: it lets the query database backdate
/// a reparse, so an edit that happens to produce an identical tree does not
/// invalidate anything downstream. Green nodes are hash-consed, so the
/// comparison is cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    green: GreenNode,
    errors: Vec<ParseError>,
}

impl Parse {
    /// The root `SOURCE_FILE` node. Cheap to call; the tree is shared.
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn errors(&self) -> &[ParseError] {
        &self.errors
    }

    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn source_file(&self) -> ast::SourceFile {
        use ast::AstNode;
        ast::SourceFile::cast(self.syntax()).expect("root is always a SOURCE_FILE")
    }

    /// An indented dump of the tree, used by the CLI and by snapshot tests.
    pub fn debug_tree(&self) -> String {
        let mut out = String::new();
        write_node(&mut out, &self.syntax(), 0);
        for err in &self.errors {
            out.push_str(&format!("error: {err}\n"));
        }
        out
    }
}

fn write_node(out: &mut String, node: &SyntaxNode, indent: usize) {
    use std::fmt::Write;
    let range = node.text_range();
    let _ = writeln!(
        out,
        "{:indent$}{:?}@{}..{}",
        "",
        node.kind(),
        u32::from(range.start()),
        u32::from(range.end()),
        indent = indent
    );
    for child in node.children_with_tokens() {
        match child {
            rowan::NodeOrToken::Node(n) => write_node(out, &n, indent + 2),
            rowan::NodeOrToken::Token(t) => {
                let range = t.text_range();
                let _ = writeln!(
                    out,
                    "{:indent$}{:?}@{}..{} {:?}",
                    "",
                    t.kind(),
                    u32::from(range.start()),
                    u32::from(range.end()),
                    t.text(),
                    indent = indent + 2
                );
            }
        }
    }
}

/// Parses a whole source file. Never panics on malformed input.
pub fn parse(text: &str) -> Parse {
    let lexed = LexedStr::new(text);
    let mut p = parser::Parser::new(&lexed);
    parser::source_file(&mut p);
    let (events, errors) = p.finish();
    let green = event::build_tree(&lexed, events);
    Parse { green, errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single most important invariant: the tree reproduces the input.
    fn assert_lossless(src: &str) {
        let parse = parse(src);
        assert_eq!(parse.syntax().text().to_string(), src, "tree lost source text");
    }

    #[test]
    fn empty_input_round_trips() {
        assert_lossless("");
        assert_lossless("   \n\t ");
        assert_lossless("// just a comment\n");
    }

    #[test]
    fn module_decl_parses() {
        let parse = parse("module app::main;\n");
        assert!(parse.ok(), "{:?}", parse.errors());
        assert_lossless("module app::main;\n");
    }

    #[test]
    fn garbage_still_round_trips() {
        assert_lossless("module ~~~ ;;; fn (((");
    }
}
