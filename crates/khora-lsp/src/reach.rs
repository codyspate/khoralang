//! What a function absorbs: the failures it handles rather than passes on.
//!
//! **Khora's signatures are already transitive**, which is the whole point of
//! row polymorphism and is why a lens repeating one would be noise. A function
//! that can fail says so, and so does every caller, all the way up. There is
//! exactly one place that stops being true: where a function *discharges*
//! something. A `catch` handles a failure so that it never reaches the
//! `raises` clause, and the signature then says nothing about it.
//!
//! Those are the interesting lines in a program and they are the ones the type
//! system deliberately hides. A signature tells you what a function asks of
//! its caller; nothing tells you what it quietly takes on itself. A `main`
//! that catches four errors has a signature that mentions none of them.
//!
//! So: the union of what the body's calls can raise, minus what the signature
//! declares, is what the function absorbs — and that is the lens.
//!
//! # Why capabilities are not here, and what it would take
//!
//! The same lens for `with` blocks would be better still: a `main` that
//! installs six handlers is absorbing six requirements and says so nowhere.
//! It cannot be built from what the checker publishes today.
//!
//! `BodyTypes::calls_with_rows` records *what each call site asked of the
//! function containing it* — the requirement still outstanding after any
//! enclosing `with` block has satisfied it. Inside such a block that is empty
//! by construction, so a discharged capability is invisible at exactly the
//! call that discharged it. Failures are recorded before their `catch` rather
//! than after, which is why the other half works; the asymmetry is not
//! deliberate, it is what each was needed for.
//!
//! Counting the handlers a `with` block names is not a substitute. A handler
//! may satisfy nothing — that is the `unused-capability` lint's whole subject
//! — and a block may satisfy a requirement raised three calls deep in a
//! function it calls. What would answer it is the callee's declared `with` row
//! recorded beside the outstanding one, which the checker reads at every call
//! to do the subtraction and then discards.

use khora_db::{Db, SourceFile};
use khora_types::Type;
use text_size::TextRange;

/// What one function takes on itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Absorbed {
    /// Where the function's name is, so a lens sits above it.
    pub at: TextRange,
    /// Error types the body can raise and the signature does not declare.
    pub failures: Vec<String>,
}

impl Absorbed {
    /// The lens text, or `None` when there is nothing to say.
    pub fn title(&self) -> Option<String> {
        if self.failures.is_empty() {
            return None;
        }
        Some(format!("catches {}", self.failures.join(", ")))
    }
}

/// Every function in a file that absorbs something.
pub fn in_file(db: &dyn Db, file: SourceFile) -> Vec<Absorbed> {
    let checked = khora_types::checked(db, file);
    let map = khora_hir::item_map(db, file);
    let mut out = Vec::new();

    for (name, _body) in khora_hir::body::bodies(db, file) {
        let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            continue;
        };
        let Some(item) = map.items.iter().find(|item| &item.name == name) else { continue };

        let mut failures: Vec<String> = Vec::new();
        for (_, rows) in types.calls_with_rows() {
            if let Some(row) = rows.raises.as_ref() {
                failures.extend(labels_of(row));
            }
        }

        // What the signature already says is not absorbed: it is passed on,
        // and every caller sees it.
        let declared = declared_labels(db, file, item.range);
        failures.retain(|label| !declared.contains(label));
        failures.sort();
        failures.dedup();

        if failures.is_empty() {
            continue;
        }
        let Some(at) = name_of(db, file, item.range) else { continue };
        out.push(Absorbed { at, failures });
    }

    out.sort_by_key(|found| found.at.start());
    out
}

/// The labels a row carries: the field names of a capability row, the type
/// names of a failure one.
fn labels_of(row: &Type) -> Vec<String> {
    match row {
        Type::Row { fields, .. } => fields.iter().map(|(name, _)| name.clone()).collect(),
        Type::Adt { name, .. } => vec![name.clone()],
        Type::Applied { head, .. } => labels_of(head),
        _ => Vec::new(),
    }
}

/// Every word inside the declaration's `with` and `raises` clauses.
///
/// **Taken from the clause nodes, not by scanning for a brace.** The first `{`
/// in a signature belongs to the capability row — `with { env: Env }` — so
/// cutting the text there loses the `raises` clause after it, and every
/// function that declared both was reported as absorbing its own failures.
///
/// The words are compared as written rather than as types, deliberately. The
/// question is "does the signature mention it", and the signature is what
/// somebody reading the function sees; a normalized type would say `Db` where
/// the author wrote an alias, and the lens would then claim to absorb
/// something that is written on the line above it.
fn declared_labels(db: &dyn Db, file: SourceFile, decl: TextRange) -> Vec<String> {
    use khora_syntax::ast::AstNode;
    let tree = khora_db::parse(db, file).syntax();
    let Some(node) = tree.descendants().filter(|node| node.text_range() == decl).last() else {
        return Vec::new();
    };
    let Some(function) = khora_syntax::ast::FnDecl::cast(node) else { return Vec::new() };

    let mut written = String::new();
    if let Some(row) = function.with_clause().and_then(|c| c.row()) {
        written.push_str(&row.syntax().text().to_string());
        written.push(' ');
    }
    if let Some(row) = function.raises_clause().and_then(|c| c.row()) {
        written.push_str(&row.syntax().text().to_string());
    }

    written
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// The name token of a declaration, so a lens sits above the function rather
/// than above its doc comment.
fn name_of(db: &dyn Db, file: SourceFile, decl: TextRange) -> Option<TextRange> {
    let tree = khora_db::parse(db, file).syntax();
    let node = tree.descendants().filter(|node| node.text_range() == decl).last()?;
    node.children()
        .find(|child| child.kind() == khora_syntax::SyntaxKind::NAME)
        .map(|name| name.text_range())
}
