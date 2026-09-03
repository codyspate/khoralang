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
//! # Type hints, where the type is actually hidden
//!
//! **Not on function boundaries, which is where every other language puts
//! them.** Khora's parameters and returns are written out by design
//! (`docs/design/associated-items.md`), so a hint there repeats what is on the
//! screen — that is the noise other languages put up with because they have no
//! choice, and this had the right instinct in refusing it.
//!
//! It went one step too far. A `let` with no annotation is inferred, and its
//! type is on nobody's screen:
//!
//! ```khora
//! let rows = query(db, "select ..")!;    // : List<Row>
//! ```
//!
//! So a hint is shown for exactly the bindings whose type the source does not
//! give: no annotation, and an initializer that does not say the type itself.
//! A literal says it (`let n = 1` needs no `: Int`), and so does a constructor
//! call whose name is the type (`let s = Settings::new()`). What is left is
//! the case the reader cannot answer without asking, which is the case worth
//! answering for them.

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
        for hint in let_types(body, types) {
            out.push(hint);
        }
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

/// The inferred type of every `let` that does not say its own type.
///
/// Walked over the body's blocks rather than its expressions, because the
/// annotation is on the statement and the whole question is whether one was
/// written.
fn let_types(body: &khora_hir::body::Body, types: &khora_types::BodyTypes) -> Vec<Hint> {
    let mut out = Vec::new();
    for (_, expr) in body.exprs() {
        let khora_hir::body::Expr::Block { stmts, .. } = expr else { continue };
        for stmt in stmts {
            let khora_hir::body::Stmt::Let { pat, ty, init } = stmt else { continue };
            // An annotation is the author saying it, which is the whole point.
            if ty.is_some() {
                continue;
            }
            let khora_hir::body::Pat::Bind(local) = body.pat(*pat) else { continue };
            let shown = types.local(*local);
            if init.is_some_and(|init| says_its_own_type(body, init, shown)) {
                continue;
            }
            if matches!(shown, Type::Unknown) {
                continue;
            }
            let binding = body.local(*local);
            // A binding nobody reads is already a lint; a hint on it as well
            // is two voices for one thing.
            if binding.name.starts_with('_') {
                continue;
            }
            out.push(Hint {
                at: binding.range.end(),
                label: format!(": {shown}"),
                detail: shown.to_string(),
            });
        }
    }
    out
}

/// Whether the initializer already tells the reader the type.
///
/// **A hint that repeats the line it is on is worse than no hint**, because it
/// costs the same screen space and carries nothing. Two cases qualify:
///
/// - a literal, where the value *is* the type;
/// - a call through a path whose head is the type it returns, so
///   `Settings::read(..)` and `Schema::record(..)` say `Settings` and `Schema`
///   on the line already.
///
/// The second is a comparison rather than a guess about naming, which is the
/// version that survives contact with `std`: `List::length(xs)` is a path call
/// whose head is `List` and whose type is `Int`, and hiding that one would
/// hide the hints most worth having.
fn says_its_own_type(
    body: &khora_hir::body::Body,
    init: khora_hir::body::ExprId,
    shown: &Type,
) -> bool {
    match body.expr(init) {
        khora_hir::body::Expr::Literal(_) => true,
        khora_hir::body::Expr::Call { callee, .. } => {
            let khora_hir::body::Expr::Path(resolution) = body.expr(*callee) else {
                return false;
            };
            let Some(head) = head_of(shown) else { return false };
            named_owner(resolution).is_some_and(|owner| owner == head)
        }
        _ => false,
    }
}

/// The type a path is written through: `Settings` in `Settings::read`.
fn named_owner(resolution: &khora_hir::Resolution) -> Option<&str> {
    match resolution {
        khora_hir::Resolution::TraitItem { owner, .. } => Some(owner),
        khora_hir::Resolution::Variant { type_name, .. } => Some(type_name),
        _ => None,
    }
}

/// The constructor at the head of a type: `List` for `List<Int>`.
fn head_of(ty: &Type) -> Option<String> {
    match ty {
        Type::Adt { name, .. } => Some(name.clone()),
        Type::Applied { head, .. } => head_of(head),
        Type::Str => Some("String".to_string()),
        Type::Int => Some("Int".to_string()),
        Type::Float => Some("Float".to_string()),
        Type::Bool => Some("Bool".to_string()),
        Type::Fixed(kind) => Some(kind.name()),
        _ => None,
    }
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
