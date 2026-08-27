//! The outline of a file, and the search across a workspace.
//!
//! `textDocument/documentSymbol` is Ctrl+Shift+O; `workspace/symbol` is
//! Ctrl+T. Both are `khora_hir::item_map` read out in a different shape, which
//! is why they are together and why they are short.
//!
//! **Read-only, and that is what makes them cheap to trust.** A symbol list
//! that is missing something is a nuisance; the same gap in a rename is a
//! corrupted repository. So these cover everything `ItemMap` records —
//! functions, types, traits, effects, contexts, constants, and a type's
//! constructors — without any of the care [`crate::references`] has to take.
//!
//! What is *not* here is impl members, for the reason go-to-definition gives:
//! `khora_hir::collect_decl` returns early on `Decl::Impl`, so no method has a
//! range to point at. It is the third feature to want that fixed.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::{ItemKind, ModulePath};
use lsp_types::SymbolKind;
use text_size::TextRange;

/// One entry in an outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// What it is called, without its module.
    pub name: String,
    /// The module that declares it, for a workspace search that has to say
    /// which of three `parse` functions it found.
    pub module: Option<ModulePath>,
    /// How an editor should draw it.
    pub kind: SymbolKind,
    /// The whole declaration, which is what an outline selects.
    pub range: TextRange,
}

/// Everything `file` declares, in the order it was written.
///
/// Declaration order rather than sorted: an outline is a map of the file, and
/// a map whose landmarks are alphabetical is not a map. Editors sort on their
/// own when asked to.
pub fn in_file(db: &dyn Db, file: SourceFile) -> Vec<Symbol> {
    let map = khora_hir::item_map(db, file);
    let module = map.module.clone();
    let mut out: Vec<Symbol> = map
        .items
        .iter()
        .map(|item| Symbol {
            name: item.name.clone(),
            module: module.clone(),
            kind: kind_of(item.kind),
            range: item.range,
        })
        .collect();

    // A `test` block has a name a person wrote and no name a program can use,
    // which is exactly what an outline is for: finding the one you meant.
    for test in &map.tests {
        out.push(Symbol {
            name: test.name.clone(),
            module: module.clone(),
            kind: SymbolKind::EVENT,
            range: test.range,
        });
    }

    out.sort_by_key(|s| s.range.start());
    out
}

/// Everything in the workspace whose name contains `query`, case-insensitively.
///
/// Substring rather than fuzzy. A fuzzy matcher is a ranking function and a
/// pile of taste, and `Ctrl+T` with three letters of the right name is what
/// people actually do — `khora-rt`'s test filter made the same call.
///
/// An empty query returns everything, which is what an editor sends to populate
/// the picker before anybody has typed.
pub fn in_workspace(db: &dyn Db, root: SourceRoot, query: &str) -> Vec<(SourceFile, Symbol)> {
    let wanted = query.to_lowercase();
    let mut out = Vec::new();
    for file in root.files(db) {
        for symbol in in_file(db, *file) {
            if wanted.is_empty() || symbol.name.to_lowercase().contains(&wanted) {
                out.push((*file, symbol));
            }
        }
    }
    out
}

/// How an editor draws each kind of declaration.
///
/// `Effect` and `Context` have no LSP counterpart — no other language has
/// them — so they borrow the nearest shape rather than falling back to
/// something meaningless. An effect is a set of operations, which is an
/// interface; a context is a named row of bindings, which is a namespace.
fn kind_of(kind: ItemKind) -> SymbolKind {
    match kind {
        ItemKind::Type => SymbolKind::STRUCT,
        ItemKind::Trait => SymbolKind::INTERFACE,
        ItemKind::Effect => SymbolKind::INTERFACE,
        ItemKind::Context => SymbolKind::NAMESPACE,
        ItemKind::Function => SymbolKind::FUNCTION,
        ItemKind::Const => SymbolKind::CONSTANT,
    }
}
