//! Type names that name nothing.
//!
//! `named_type` turns a name nothing declares into `Type::Adt { home: None }`,
//! and the comment there says the mistake is "already an error where the name
//! was resolved". For a *value* name that is true — `khora-hir` reports
//! `cannot find x in this scope`. For a type name nothing resolved it and
//! nothing complained, so `fn f(x: Wibble) -> Int { 1 }` type-checked clean.
//!
//! What made it survive is that the consequence is mild-looking. An
//! unresolved name becomes a nominal type that is distinct from everything, so
//! it does not unify with anything and genuine mismatches are still caught —
//! but they are reported as ``this function returns `Wibble`, but its body has
//! type `Int` ``, which reads as a disagreement between two real types and
//! sends the reader hunting for the wrong thing. A typo in a signature is the
//! ordinary way to meet this, and the ordinary case is the one that matters.
//!
//! Walked over the syntax rather than over resolved types, because the point
//! is to report the *written* name at the place it was written, and by the
//! time a `Type` exists the range is gone.

use std::collections::HashSet;

use khora_db::SourceFile;
use khora_hir::HirError;
use khora_syntax::ast::{self, AstNode};
use khora_syntax::{SyntaxKind, SyntaxNode};

use crate::{type_homes, Db, IntKind};

/// Every written type name in `file` that resolves to nothing.
pub(crate) fn unresolved_type_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let homes = type_homes(db, file);
    let parsed = khora_db::parse(db, file);
    let mut found = Vec::new();

    for decl in parsed.source_file().decls() {
        let node = decl.syntax().clone();
        // **Every type parameter anywhere in the declaration, as one scope.**
        // Deliberately an over-approximation: a method's `T` counts as in
        // scope for its sibling, which can only ever make this report *less*.
        // A diagnostic that has never existed before earns its place by never
        // being wrong, not by being complete — and the precise version wants
        // scoping machinery this walk does not have.
        let mut in_scope: HashSet<String> = HashSet::new();
        in_scope.insert("Self".to_string());
        for param in node.descendants().filter(|n| n.kind() == SyntaxKind::TYPE_PARAM) {
            let Some(param) = ast::TypeParam::cast(param) else { continue };
            if let Some(name) = param.name().and_then(|n| n.ident()) {
                in_scope.insert(name);
            }
            if let Some(row) = param.row_var() {
                in_scope.insert(row);
            }
        }

        for path in node.descendants().filter(|n| n.kind() == SyntaxKind::PATH_TYPE) {
            if inside_an_import(&path) {
                continue;
            }
            let Some(path_type) = ast::PathType::cast(path.clone()) else { continue };
            // A bare `'r` is a row variable and has no `Path` under it.
            if path_type.row_var().is_some() {
                continue;
            }
            let Some(name) = path_type.path().map(|p| p.text_path()) else { continue };
            if resolves(&name, &in_scope, homes) {
                continue;
            }
            found.push(HirError {
                message: format!(
                    "cannot find type `{name}` in this scope; nothing declared or imported \
                     here goes by that name"
                ),
                range: path.text_range(),
            });
        }
    }

    found
}

/// Whether a written type name names something.
fn resolves(name: &str, in_scope: &HashSet<String>, homes: &crate::TypeHomes) -> bool {
    if name.is_empty() {
        return true;
    }
    // `T::Item` is a projection when `T` is a parameter. A qualified name that
    // is *not* one is passed over rather than reported: `named_type` looks the
    // whole dotted string up in `TypeHomes`, which is keyed by single
    // identifiers, so a wrong answer here would be this check's own confusion
    // rather than the program's. Narrower than it could be, on purpose.
    if let Some((owner, _)) = name.split_once("::") {
        let _ = owner;
        return true;
    }
    matches!(name, "Int" | "I64" | "Float" | "Bool" | "String" | "Ptr" | "Unit" | "Never")
        || IntKind::parse(name).is_some()
        || in_scope.contains(name)
        || homes.of(name).is_some()
}

/// Import paths are spelled with the same node and resolve elsewhere.
fn inside_an_import(node: &SyntaxNode) -> bool {
    node.ancestors().any(|a| {
        matches!(
            a.kind(),
            SyntaxKind::IMPORT_DECL
                | SyntaxKind::IMPORT_LIST
                | SyntaxKind::IMPORT_ITEM
                | SyntaxKind::IMPORT_GLOB
        )
    })
}
