//! Semantic tokens: highlighting the compiler decides, not a regular
//! expression.
//!
//! A TextMate grammar matches text. It cannot tell a local binding from an
//! imported name, or a field from a method, because both distinctions need
//! resolution — `editors/vscode/README.md` has said so since it was written.
//! This is the answer, and it reaches every editor with an LSP client rather
//! than only the ones that read TextMate grammars.
//!
//! # Where each classification comes from
//!
//! | drawn as | decided by |
//! | --- | --- |
//! | a local, a parameter | `Expr::Local` and `Body::locals`, which the checker resolved |
//! | a field, a method | `Expr::Field`, and whether a `Call` has it as callee |
//! | a function, type, trait, constructor | `khora_hir::Resolution` of the path |
//! | a module | every path segment but the last |
//!
//! # Two rules the protocol imposes, and one it does not
//!
//! Tokens must be **sorted and non-overlapping**. Two sources feed this — the
//! bodies, and a walk over the syntax tree for paths — so they are collected
//! independently and then sorted, with anything overlapping what came before it
//! dropped. Dropping rather than splitting: an overlap means two passes
//! disagreed about the same bytes, and picking the earlier one is at least
//! deterministic.
//!
//! Lengths are in the **client's** units, so a token containing an accent is
//! two long in UTF-16 and three in UTF-8. `LineIndex` converts. A token is
//! assumed not to span a line, which no name does.
//!
//! What the protocol does not impose, and this does not do, is completeness.
//! Anything not classified here falls through to the TextMate grammar
//! underneath, which is why keywords and literals are absent: the grammar
//! already gets those right and a second opinion would only be a chance to
//! disagree.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::{ItemKind, Resolution};
use khora_syntax::ast::{AstNode, Path};
use text_size::TextRange;

/// The token types this server emits, in the order the legend declares them.
///
/// The index into this array *is* the wire encoding, so the order is part of
/// the protocol and appending is the only safe change.
pub const TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "interface",
    "enumMember",
    "function",
    "method",
    "property",
    "parameter",
    "variable",
];

/// The one modifier that is used, likewise positional.
pub const TOKEN_MODIFIERS: &[&str] = &["declaration"];

const NAMESPACE: u32 = 0;
const TYPE: u32 = 1;
const INTERFACE: u32 = 2;
const ENUM_MEMBER: u32 = 3;
const FUNCTION: u32 = 4;
const METHOD: u32 = 5;
const PROPERTY: u32 = 6;
const PARAMETER: u32 = 7;
const VARIABLE: u32 = 8;

const DECLARATION: u32 = 1;

/// One classified span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub range: TextRange,
    pub kind: u32,
    pub modifiers: u32,
}

/// Everything in `file` this server can classify, sorted and non-overlapping.
pub fn tokens(db: &dyn Db, root: SourceRoot, file: SourceFile) -> Vec<Token> {
    let mut out = Vec::new();
    from_bodies(db, file, &mut out);
    from_paths(db, root, file, &mut out);

    out.sort_by_key(|t| (t.range.start(), t.range.end()));
    // An overlap means two passes disagreed about the same bytes. Keeping the
    // first is deterministic, which matters more here than being clever.
    let mut kept: Vec<Token> = Vec::with_capacity(out.len());
    for token in out {
        if kept.last().is_some_and(|last| last.range.end() > token.range.start()) {
            continue;
        }
        kept.push(token);
    }
    kept
}

/// Locals, parameters, fields and methods — everything that needed the checker.
fn from_bodies(db: &dyn Db, file: SourceFile, out: &mut Vec<Token>) {
    for (_, body) in khora_hir::body::bodies(db, file) {
        // A binding's range covers its annotation as well as its name —
        // `p: shapes::Point` entire — so only the leading `name.len()` bytes
        // are the name. The rest is a type, and the path pass classifies it.
        for (id, local) in body.locals() {
            let start = local.range.start();
            let name_end = start + text_size::TextSize::of(local.name.as_str());
            if name_end > local.range.end() {
                continue;
            }
            let declared_by_a_parameter = body
                .params
                .iter()
                .chain(body.evidence.iter().map(|(_, pat)| pat))
                .any(|pat| pat_binds(body, *pat, id));
            out.push(Token {
                range: TextRange::new(start, name_end),
                kind: if declared_by_a_parameter { PARAMETER } else { VARIABLE },
                modifiers: DECLARATION,
            });
        }

        // Which field expressions are being called, so a method is not drawn
        // as a property.
        let called: std::collections::HashSet<_> = body
            .exprs()
            .filter_map(|(_, expr)| match expr {
                khora_hir::body::Expr::Call { callee, .. } => Some(*callee),
                _ => None,
            })
            .collect();

        for (id, expr) in body.exprs() {
            match expr {
                khora_hir::body::Expr::Local(local) => {
                    let is_parameter = body
                        .params
                        .iter()
                        .chain(body.evidence.iter().map(|(_, pat)| pat))
                        .any(|pat| pat_binds(body, *pat, *local));
                    out.push(Token {
                        range: body.range(id),
                        kind: if is_parameter { PARAMETER } else { VARIABLE },
                        modifiers: 0,
                    });
                }
                khora_hir::body::Expr::Field { name, .. } => {
                    // `a.b` ends with `b`, so the name is the tail of the range.
                    let whole = body.range(id);
                    let length = text_size::TextSize::of(name.as_str());
                    if length > whole.len() {
                        continue;
                    }
                    out.push(Token {
                        range: TextRange::new(whole.end() - length, whole.end()),
                        kind: if called.contains(&id) { METHOD } else { PROPERTY },
                        modifiers: 0,
                    });
                }
                _ => {}
            }
        }
    }
}

/// Whether `pat` introduces `local`.
fn pat_binds(body: &khora_hir::body::Body, pat: khora_hir::body::PatId, local: khora_hir::body::LocalId) -> bool {
    matches!(body.pat(pat), khora_hir::body::Pat::Bind(bound) if *bound == local)
}

/// Path segments, classified by what they resolve to.
///
/// Every segment but the last is a module. The last is whatever the resolution
/// says — which is how an imported function is drawn differently from a local
/// of the same name, the distinction a grammar cannot make.
fn from_paths(db: &dyn Db, root: SourceRoot, file: SourceFile, out: &mut Vec<Token>) {
    let tree = khora_db::parse(db, file).syntax();
    for node in tree.descendants() {
        let Some(path) = Path::cast(node) else { continue };
        let segments: Vec<_> = path.segments().collect();
        if segments.is_empty() {
            continue;
        }
        let names: Vec<String> = segments.iter().filter_map(|s| s.ident()).collect();
        if names.len() != segments.len() {
            continue;
        }

        let last = match khora_hir::resolve_path(db, root, file, &names) {
            Ok(Resolution::Item { kind, .. }) => Some(match kind {
                ItemKind::Type => TYPE,
                ItemKind::Trait | ItemKind::Effect => INTERFACE,
                ItemKind::Context => NAMESPACE,
                ItemKind::Function => FUNCTION,
                ItemKind::Const => VARIABLE,
            }),
            Ok(Resolution::Variant { .. }) => Some(ENUM_MEMBER),
            Ok(Resolution::TraitItem { .. }) => Some(FUNCTION),
            // Unresolved, or something the resolver declines to guess at. Left
            // to the grammar rather than coloured wrongly.
            _ => None,
        };

        for (index, segment) in segments.iter().enumerate() {
            let range = segment.syntax().text_range();
            let is_last = index + 1 == segments.len();
            let kind = if is_last {
                match last {
                    Some(kind) => kind,
                    None => continue,
                }
            } else {
                NAMESPACE
            };
            out.push(Token { range, kind, modifiers: 0 });
        }
    }
}

