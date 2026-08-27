//! Signature help: the parameters of the call being typed.
//!
//! Like completion, this runs on a line that does not parse — `charge(` is a
//! syntax error and a request for `charge`'s parameters at the same moment.
//! So it reads the token stream rather than the tree, for the reason
//! [`crate::completion`] sets out at length.
//!
//! # Finding the call
//!
//! Backwards from the cursor to the **unmatched** `(`. Counting depth is the
//! whole of it: `outer(inner(a, b), ` is inside `outer`, and a scan that
//! stopped at the first `(` it saw would answer about `inner` — which is the
//! one case somebody typing a nested call actually needs help with.
//!
//! Commas at depth zero relative to that paren give the active parameter, so
//! the editor can bold the one being typed.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_syntax::{SyntaxKind, SyntaxToken};
use text_size::TextSize;

/// The call being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// The callee as written, for the label.
    pub name: String,
    /// One entry per declared parameter, rendered.
    pub parameters: Vec<String>,
    /// The return type, so the label is a whole signature.
    pub returns: String,
    /// Which parameter the cursor is in.
    pub active: usize,
}

impl Help {
    /// The signature as one line, which is what the popup shows.
    pub fn label(&self) -> String {
        format!("{}({}) -> {}", self.name, self.parameters.join(", "), self.returns)
    }

    /// Where each parameter sits in [`Help::label`], so the editor can bold
    /// the active one.
    ///
    /// Offsets rather than strings: the protocol accepts either, and a string
    /// is matched by *substring*, which highlights the wrong parameter the
    /// moment two of them share a type.
    pub fn spans(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let mut at = self.name.len() as u32 + 1;
        for (index, parameter) in self.parameters.iter().enumerate() {
            if index > 0 {
                at += 2;
            }
            let length = parameter.len() as u32;
            out.push((at, at + length));
            at += length;
        }
        out
    }
}

/// The call the cursor is inside, if it is inside one.
pub fn at(db: &dyn Db, root: SourceRoot, file: SourceFile, offset: TextSize) -> Option<Help> {
    let tree = khora_db::parse(db, file).syntax();
    let from = match tree.token_at_offset(offset) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => t,
        rowan::TokenAtOffset::Between(left, _) => left,
    };

    let (open, active) = enclosing_call(&from)?;
    let name = callee_of(&open)?;

    // The path first, so `helper::add` finds `add` in `helper` rather than a
    // local namesake; then the bare name, which is how an imported function is
    // written at the call.
    let (signature, home) = signature_of(db, root, file, &name)?;

    // **Names where they can be had, types where they cannot.** `Signature`
    // records `Vec<Type>` and nothing else -- the checker never needed the
    // names -- but the callee's `Body` has them, one `Pat::Bind` per
    // parameter. `charge(account: Account, amount: Decimal)` is worth the
    // lookup over `charge(Account, Decimal)`, which says nothing a reader
    // could not already see.
    let names = parameter_names(db, home, name.last()?);
    let parameters = signature
        .params
        .iter()
        .enumerate()
        .map(|(index, ty)| match names.get(index) {
            Some(label) => format!("{label}: {ty}"),
            None => ty.to_string(),
        })
        .collect();

    Some(Help {
        name: name.join("::"),
        parameters,
        returns: signature.ret.to_string(),
        active,
    })
}

/// The callee's signature, and the file that declares it.
///
/// The file comes back too because the parameter *names* are in that file's
/// bodies, not in the signature.
fn signature_of(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    name: &[String],
) -> Option<(khora_types::Signature, SourceFile)> {
    // Resolution first: it is the answer that is right about aliases and about
    // two modules declaring the same name.
    if let Ok(khora_hir::Resolution::Item { module, name: item, .. }) =
        khora_hir::resolve_path(db, root, file, name)
    {
        let graph = khora_hir::module_graph(db, root);
        if let Some(target) = graph.file(&module) {
            if let Some(found) = khora_types::type_map(db, target).signatures.get(item.as_str()) {
                return Some((found.clone(), target));
            }
        }
    }

    // Otherwise this file's own map, which also holds what it imported.
    let map = khora_types::type_map(db, file);
    let key = name.join("::");
    let found = map
        .signatures
        .get(key.as_str())
        .or_else(|| map.signatures.get(name.last()?.as_str()))?;
    Some((found.clone(), file))
}

/// The parameter names of `function`, read off its lowered body.
///
/// Empty when the body is not in `file` — a foreign declaration has no body at
/// all — and the caller falls back to showing types.
fn parameter_names(db: &dyn Db, file: SourceFile, function: &str) -> Vec<String> {
    for (name, body) in khora_hir::body::bodies(db, file) {
        if name != function {
            continue;
        }
        return body
            .params
            .iter()
            .map(|pat| match body.pat(*pat) {
                khora_hir::body::Pat::Bind(local) => body.local(*local).name.clone(),
                // A pattern parameter has no one name. `_` reads better than
                // an invented one.
                _ => "_".to_string(),
            })
            .collect();
    }
    Vec::new()
}

/// The unmatched `(` before this token, and how many commas follow it.
///
/// **Depth, not the first paren.** `outer(inner(a, b), ` closes `inner`'s
/// paren on the way back, so the unmatched one is `outer`'s — which is the
/// call the cursor is actually in.
fn enclosing_call(from: &SyntaxToken) -> Option<(SyntaxToken, usize)> {
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut at = Some(from.clone());

    while let Some(token) = at {
        match token.kind() {
            SyntaxKind::R_PAREN => depth += 1,
            SyntaxKind::L_PAREN => {
                if depth == 0 {
                    return Some((token, commas));
                }
                depth -= 1;
            }
            SyntaxKind::COMMA if depth == 0 => commas += 1,
            // A statement boundary means there is no call to be inside.
            SyntaxKind::SEMICOLON | SyntaxKind::L_BRACE if depth == 0 => return None,
            _ => {}
        }
        at = token.prev_token();
    }
    None
}

/// The path immediately before an opening paren.
fn callee_of(open: &SyntaxToken) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut at = previous(open);
    while let Some(token) = at {
        if token.kind() != SyntaxKind::IDENT {
            break;
        }
        segments.push(token.text().to_string());
        match previous(&token) {
            Some(sep) if sep.kind() == SyntaxKind::COLON_COLON => at = previous(&sep),
            _ => break,
        }
    }
    if segments.is_empty() {
        return None;
    }
    segments.reverse();
    Some(segments)
}

/// The previous token that is not whitespace or a comment.
fn previous(from: &SyntaxToken) -> Option<SyntaxToken> {
    let mut at = from.prev_token();
    while let Some(token) = at {
        if !matches!(
            token.kind(),
            SyntaxKind::WHITESPACE | SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT
        ) {
            return Some(token);
        }
        at = token.prev_token();
    }
    None
}
