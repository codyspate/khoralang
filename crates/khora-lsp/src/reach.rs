//! What a function absorbs: the authority it installs and the failures it
//! handles, rather than passes on.
//!
//! **Khora's signatures are already transitive**, which is the whole point of
//! row polymorphism and is why a lens repeating one would be noise. A function
//! that needs a database says so, and so does every caller, all the way up.
//! There is exactly one place that stops being true: where a function
//! *discharges* something. A `with { db: .. }` block satisfies a requirement so
//! that it never reaches the signature; a `catch` handles a failure so that it
//! never reaches the `raises`.
//!
//! Those are the interesting lines in a program and they are the ones the type
//! system deliberately hides. A signature tells you what a function asks of
//! its caller; nothing tells you what it quietly takes on itself. A `main`
//! that installs six handlers and catches four errors has a signature that
//! mentions none of it.
//!
//! # Where the two halves come from
//!
//! Failures are the easy half: `CallRows::raises` is recorded before any
//! `catch` subtracts from it, so what a body's calls can raise, minus what the
//! signature declares, is what the function swallows.
//!
//! Capabilities were the hard half and were missing until `CallRows::declared`
//! existed. `requires` is what a call *still* has to answer, so inside a `with`
//! block it is empty by construction and a discharged capability was invisible
//! at exactly the call that discharged it. Counting the handlers a block names
//! is not a substitute: a handler may satisfy nothing, which is the
//! `unused-capability` lint's whole subject, and a block may satisfy a
//! requirement raised three calls deep in a function it calls. `declared` is
//! the callee's own row, kept before the subtraction, and the difference is
//! the answer.

use khora_db::{Db, SourceFile};
use khora_types::Type;
use text_size::TextRange;

/// What one function takes on itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Absorbed {
    /// Where the function's name is, so a lens sits above it.
    pub at: TextRange,
    /// Capability labels the body's calls ask for and the signature does not
    /// declare, because something in the function answered them.
    pub capabilities: Vec<String>,
    /// Error types the body can raise and the signature does not declare.
    pub failures: Vec<String>,
}

impl Absorbed {
    /// The lens text, or `None` when there is nothing to say.
    pub fn title(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.capabilities.is_empty() {
            parts.push(format!("installs {{ {} }}", self.capabilities.join(", ")));
        }
        if !self.failures.is_empty() {
            parts.push(format!("catches {}", self.failures.join(", ")));
        }
        if parts.is_empty() {
            return None;
        }
        Some(parts.join(" · "))
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

        let mut capabilities: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for (_, rows) in types.calls_with_rows() {
            if let Some(row) = rows.raises.as_ref() {
                failures.extend(labels_of(row));
            }
            // What the callee asked for, minus what this call still owes: the
            // rest was answered here.
            if let Some(asked) = rows.declared.as_ref() {
                let outstanding: Vec<String> =
                    rows.requires.as_ref().map(labels_of).unwrap_or_default();
                for label in labels_of(asked) {
                    if !outstanding.contains(&label) {
                        capabilities.push(label);
                    }
                }
            }
        }

        // What the signature already says is not absorbed: it is passed on,
        // and every caller sees it.
        let declared = declared_labels(db, file, item.range);
        capabilities.retain(|label| !declared.contains(label));
        failures.retain(|label| !declared.contains(label));
        capabilities.sort();
        capabilities.dedup();
        failures.sort();
        failures.dedup();

        if capabilities.is_empty() && failures.is_empty() {
            continue;
        }
        let Some(at) = name_of(db, file, item.range) else { continue };
        out.push(Absorbed { at, capabilities, failures });
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
