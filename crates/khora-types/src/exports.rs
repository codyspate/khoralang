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

        // **A `with` clause reverses meaning across this boundary, and that is
        // worth its own message.** `ffi.md` §3 says a `with` clause on a
        // foreign *import* is a permission and nothing is appended to the
        // call — the requirement governs who may bind the symbol, and the C
        // function never sees it. Somebody who has read that will reasonably
        // expect the same of an export, and it is the opposite: an exported
        // function's body genuinely needs the evidence, evidence is an
        // appended argument, and C has none to append.
        //
        // Without this the wrapper was built from the foreign view of the
        // signature — no evidence parameters — and called a target that
        // expected them. LLVM's verifier caught it, which is the good case,
        // and reported "Incorrect number of arguments passed to called
        // function!" against line 1 of the file under the heading "which is a
        // compiler bug". It was a compiler bug. It was not the reader's.
        if is_open(&signature.requires) {
            found.push(HirError {
                message: format!(
                    "`{name}` is exported to C and has a `with` clause, so it needs evidence \
                     that C has none of. On a foreign *import* a `with` clause is a \
                     permission and nothing is passed — `docs/design/ffi.md` §3 — but an \
                     export runs Khora code, and a capability is an argument. Take what it \
                     needs as parameters, or wrap it in a function that constructs the \
                     capability itself"
                ),
                range,
            });
            continue;
        }

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

/// Whether a row asks for anything: a named field, or a variable that could
/// still be solved to one.
fn is_open(row: &crate::Type) -> bool {
    match row {
        crate::Type::Row { fields, tail } => !fields.is_empty() || tail.is_some(),
        _ => false,
    }
}
