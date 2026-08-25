//! `export extern fn` — a function C can call.
//!
//! `docs/design/c-export.md`. The marker is two words the language already
//! has: `extern` means the C boundary, `export` means visible outside. A body
//! is what tells the two directions apart, which is a rule that already
//! existed — errata 5 makes a body optional, so `extern fn` without one is a
//! symbol to be found at link time and with one is a symbol to be published.
//!
//! # Why this is checked here and not where a call is generated
//!
//! Foreign *imports* are checked at the call site on purpose: a binding to
//! something one target does not have should not be an error on a target that
//! never calls it. An **export** is the other way round. It is part of the
//! library's published ABI whether or not any Khora code calls it — that is
//! the entire point of writing one — so a signature C could not call is wrong
//! at the declaration, and `khora check` should say so rather than leaving it
//! to whoever eventually runs a `--lib` build.

use khora_db::SourceFile;
use khora_hir::HirError;
use khora_syntax::ast::{self, AstNode};

use crate::{foreign_signature_obstacle, type_map, Db};

/// Everything wrong with this file's `extern fn` definitions.
pub(crate) fn export_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let mut found = Vec::new();
    let signatures = &type_map(db, file).signatures;

    for decl in khora_db::parse(db, file).source_file().decls() {
        let ast::Decl::Fn(f) = decl else { continue };
        // Without a body it is an import, and `ffi.md`'s rules for those are
        // applied where the call is generated.
        if !f.is_extern() || f.body().is_none() {
            continue;
        }
        let Some(name) = f.name().and_then(|n| n.ident()) else { continue };
        let range = f.syntax().text_range();

        // **`export` is not decoration here.** A C symbol is reachable by
        // anything that links the library, so a private one is a contradiction
        // rather than a narrower promise — and the reader who wrote `extern fn`
        // meaning "call out" and got "publish" deserves to be told which one
        // this is.
        if !f.is_exported() {
            found.push(HirError {
                message: format!(
                    "`{name}` has a body and is `extern`, which publishes it as a C symbol — \
                     so it cannot be private. Write `export extern fn {name}` if that is what \
                     you meant, or drop `extern` if it is an ordinary Khora function"
                ),
                range,
            });
            continue;
        }

        let Some(signature) = signatures.get(&name) else { continue };
        if let Some(why) = foreign_signature_obstacle(signature) {
            found.push(HirError {
                message: format!(
                    "`{name}` is exported to C, and {why}. Only scalars and pointers cross \
                     — for text, take a buffer and a capacity and return the length, the way \
                     `khora_float_text` does. `docs/design/c-export.md`"
                ),
                range,
            });
        }
    }

    found
}
