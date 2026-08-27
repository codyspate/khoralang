//! Inlay hints: what a call costs, shown where the call is.
//!
//! **Every other language's inlay hints show inferred types**, because a type
//! is the only thing their checker works out that the source does not say.
//! Khora infers two more, and they are the ones a reader is actually missing:
//!
//! ```khora
//! let answer = charge(account, amount);   // with { db: Db, clock: Clock } raises DbError
//! ```
//!
//! Neither row is written at the call. Both are computed there — the checker
//! has to, to do row subtraction — and until `BodyTypes::call_rows` existed,
//! both were dropped on the floor. So this reads a fact the compiler already
//! knew and nobody could see.
//!
//! # What is shown, and what is left out
//!
//! A call that needs nothing and cannot fail gets no hint. That is most calls,
//! and a hint on every line is a hint nobody reads — the point of this is that
//! the marked lines are the ones where something crosses a boundary.
//!
//! Type hints are deliberately absent. Khora's signatures are explicit at
//! function boundaries by design (`docs/design/associated-items.md`), so the
//! types a reader wants are already on the screen; adding them again is the
//! noise other languages put up with because they have no choice.

use khora_db::{Db, SourceFile};
use khora_types::Type;
use text_size::TextSize;

/// One hint, and where it hangs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// End of the call, so the hint sits after it rather than inside it.
    pub at: TextSize,
    /// What to draw, already rendered.
    pub label: String,
    /// The rows in full, for the hover an editor shows on the hint.
    pub detail: String,
}

/// Every call in `file` that asks for something.
pub fn in_file(db: &dyn Db, file: SourceFile) -> Vec<Hint> {
    let checked = khora_types::checked(db, file);
    let mut out = Vec::new();

    for (name, body) in khora_hir::body::bodies(db, file) {
        let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            continue;
        };
        for (id, rows) in types.calls_with_rows() {
            let mut parts = Vec::new();
            if let Some(requires) = rows.requires.as_ref().and_then(labels_of) {
                parts.push(format!("with {requires}"));
            }
            if let Some(raises) = rows.raises.as_ref().and_then(labels_of) {
                parts.push(format!("raises {raises}"));
            }
            if parts.is_empty() {
                continue;
            }
            out.push(Hint {
                at: body.range(id).end(),
                label: parts.join(" "),
                detail: parts.join("\n"),
            });
        }
    }

    out.sort_by_key(|hint| hint.at);
    out.dedup_by(|a, b| a.at == b.at && a.label == b.label);
    out
}

/// A row as a reader wants it: `{ db: Db, clock: Clock }`, or the error names.
///
/// `None` for a row with nothing in it. **An open tail alone is not something
/// to show**: `'e` says "possibly more" rather than "at least one", and a hint
/// reading `raises 'e` on every call in a generic function would be noise
/// carrying no information the signature does not already have.
fn labels_of(row: &Type) -> Option<String> {
    let Type::Row { fields, .. } = row else { return None };
    if fields.is_empty() {
        return None;
    }
    let mut names: Vec<String> =
        fields.iter().map(|(label, ty)| render(label, ty)).collect();
    names.sort();
    Some(if names.len() == 1 && !names[0].contains(':') {
        // One error type reads better bare: `raises DbError`, not
        // `raises { DbError }`.
        names.remove(0)
    } else {
        format!("{{ {} }}", names.join(", "))
    })
}

/// One row entry. A capability is `label: Type`; an error is labelled by its
/// own type name, so printing both would say the same word twice.
fn render(label: &str, ty: &Type) -> String {
    let written = ty.to_string();
    if written == label {
        written
    } else {
        format!("{label}: {written}")
    }
}
