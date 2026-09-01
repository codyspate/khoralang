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

        found.extend(rows_written_as_types(&node));
    }

    found
}

/// A `+` union or an inline variant written where an ordinary type goes.
///
/// **Both became `Type::Unknown`, and `Unknown` agrees with everything.**
/// `type_of_syntax` has an arm for each type it understands and `_ =>
/// Type::Unknown` under them, so a construct it does not handle did not fail
/// -- it switched off the checking of that position:
///
/// ```text
/// fn hold(r: Result<Int, A + B>) -> Int    // accepted a Result<Int, C>
/// fn colour(x: | Red | Blue) -> Int        // `Red` and `Blue` undeclared,
///                                          // and nothing said so
/// ```
///
/// Errata 60 named this shape twice already, about `_ => Type::Unknown` in two
/// other matches: "a permissive default is not a small bug, and it hides in
/// the arm nobody wrote". This is the third.
///
/// Reported here rather than fixed there because `type_of_syntax` returns a
/// `Type` and has no channel to complain on, and because this walk has the
/// range the construct was written at.
///
/// `Forall` is the fourth form in that arm and is left alone: an effect
/// operation with one is already reported as a type that was "never worked
/// out", so it is not silent.
fn rows_written_as_types(node: &SyntaxNode) -> Vec<HirError> {
    let mut found = Vec::new();
    for written in node.descendants() {
        match written.kind() {
            // `+` builds a row, and a row is what `raises` and `with` take.
            // Anywhere else there is nothing for it to mean.
            SyntaxKind::UNION_TYPE if !inside_a_row(&written) => found.push(HirError {
                message: "`+` builds a `raises` or `with` row, and this is not one. A \
                          `Result` holds one error type; handle a wider row with `catch`, \
                          which matches per type"
                    .to_string(),
                range: written.text_range(),
            }),
            // `| A | B` is how a type is *declared*, not how one is written.
            SyntaxKind::VARIANT_TYPE if !inside_a_declaration(&written) => {
                found.push(HirError {
                    message: "a variant type is declared with `type Name = | A | B` and \
                              then written by its name; it cannot be spelled out here"
                        .to_string(),
                    range: written.text_range(),
                })
            }
            _ => {}
        }
    }
    found
}

/// Whether this sits under a `with` or `raises` clause, where a row belongs.
fn inside_a_row(node: &SyntaxNode) -> bool {
    node.ancestors()
        .any(|a| matches!(a.kind(), SyntaxKind::WITH_CLAUSE | SyntaxKind::RAISES_CLAUSE))
}

/// Whether this is the definition of a `type` declaration, where a variant is
/// exactly what belongs.
fn inside_a_declaration(node: &SyntaxNode) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(parent.kind(), SyntaxKind::TYPE_DECL | SyntaxKind::ASSOC_TYPE_DECL)
    })
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
