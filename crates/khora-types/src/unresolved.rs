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
        let mut reported_rows: HashSet<String> = HashSet::new();
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
            // **A bare `'r` is a row variable, and it has to be declared too.**
            // It has no `Path` under it, so it used to be passed over here and
            // was checked nowhere else. `reference/generics.md` says a row
            // variable "is introduced directly in the generic parameter list",
            // so `fn run_it<A>(action: () -> A with 'ef raises 'zz)` is a typo
            // for `<A, 'ef, 'zz>` — and it was accepted in silence. The
            // signature it builds cannot be called by anybody: the undeclared
            // names become rigid parameters the caller is held to, so
            //
            //     run_it(pure_action)
            //
            // — a pure, effect-free, infallible argument — collected four
            // errors, all of them in the caller's file, none of them saying
            // the signature was the problem. For a library that is four
            // errors in a downstream package that did nothing wrong.
            if let Some(row) = path_type.row_var() {
                let row = row.text().to_string();
                // An effect operation quantifies over its rows implicitly —
                // `adopt: (Slot<(), 'er>) -> ()` is the `Forall` this walk's
                // `rows_written_as_types` note already leaves alone — so there
                // is no parameter list for `'er` to be missing from.
                //
                // Once per name per declaration, at the first place it is
                // written. A row variable is usually written two or three
                // times in one signature — once in the argument's type and
                // again in each clause that forwards it — and there is one
                // fix for all of them: the parameter list. Repeating the
                // sentence per occurrence would trade four errors in the
                // caller for four in the declaration.
                if !in_scope.contains(&row)
                    && !inside_an_effect(&path)
                    && reported_rows.insert(row.clone())
                {
                    found.push(HirError {
                        message: format!(
                            "cannot find the row variable `{row}` in this scope; a row \
                             variable is introduced in the generic parameter list, as \
                             `<{row}>`"
                        ),
                        range: path.text_range(),
                    });
                }
                continue;
            }
            let Some(name) = path_type.path().map(|p| p.text_path()) else { continue };
            // A trait where a trait belongs -- a bound, a supertrait, the
            // trait an `impl` is for -- is a trait mention, not a type one.
            if homes.is_trait(&name) && !in_scope.contains(&name) {
                if !names_a_trait_here(&path) {
                    found.push(HirError {
                        message: trait_as_type(&name, &path, homes, &in_scope),
                        range: path.text_range(),
                    });
                }
                continue;
            }
            if resolves(&name, &in_scope, homes) {
                continue;
            }
            // A `row` declaration is not a type and so is not in `homes`, but
            // `with Deps` names one and is not a missing type.
            if homes.row(&name).is_some() {
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
/// fn color(x: | Red | Blue) -> Int        // `Red` and `Blue` undeclared,
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
            SyntaxKind::UNION_TYPE if !inside_a_row(&written) && !raised_argument(&written) => found.push(HirError {
                message: "`+` builds a `raises` or `with` row, and this is not one. A \
                          `Result` holds one error type; to hold either of several, \
                          write `Raised<A + B>`, or handle them with `catch`"
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

/// Whether this `+` is the argument of a `Raised<..>`, which takes a row.
///
/// `A + B + C` nests one union inside another, so the climb starts above
/// the outermost of them. Qualified spellings (`std::core::Raised`) are
/// recognized by their last segment.
fn raised_argument(node: &SyntaxNode) -> bool {
    let mut at = node.clone();
    while let Some(parent) = at.parent().filter(|p| p.kind() == SyntaxKind::UNION_TYPE) {
        at = parent;
    }
    let Some(args) = at.parent().filter(|p| p.kind() == SyntaxKind::TYPE_ARGS) else {
        return false;
    };
    let Some(path_type) = args.parent() else { return false };
    path_type.children().find(|c| c.kind() == SyntaxKind::PATH).is_some_and(|path| {
        let text = path.text().to_string();
        text.trim().rsplit("::").next().map(str::trim) == Some(crate::RAISED_TYPE)
    })
}

/// Whether this sits under a `with` or `raises` clause, where a row belongs.
fn inside_a_row(node: &SyntaxNode) -> bool {
    node.ancestors()
        .any(|a| matches!(a.kind(), SyntaxKind::WITH_CLAUSE | SyntaxKind::RAISES_CLAUSE))
}

/// Whether a written name is in one of the places a trait belongs: a bound
/// (`<A: Show>`, `trait Ord: Eq`, `type Key: Eq`), or the trait an `impl` is
/// for.
fn names_a_trait_here(path: &SyntaxNode) -> bool {
    let Some(parent) = path.parent() else { return false };
    match parent.kind() {
        SyntaxKind::TYPE_BOUNDS => true,
        SyntaxKind::IMPL_DECL => ast::ImplDecl::cast(parent)
            .and_then(|i| i.trait_())
            .is_some_and(|t| t.syntax() == path),
        _ => false,
    }
}

/// What to say about a trait written where a type goes.
///
/// **Rust says `impl Show` or `dyn Show`; Khora has neither**, so the only
/// spelling is a type parameter bounded by the trait. Where the mention is a
/// function's parameter, the message is that function's signature rewritten,
/// one fresh parameter per trait mention, so it can be copied as it stands --
/// two mentions of `Show` are two parameters, because two arguments may be
/// two different types. Anywhere else there is no signature to rewrite, and
/// the shape is given instead.
fn trait_as_type(
    name: &str,
    path: &SyntaxNode,
    homes: &crate::TypeHomes,
    in_scope: &HashSet<String>,
) -> String {
    let head = format!(
        "`{name}` is a trait, not a type: it says what a type can do, and a value has to \
         have one particular type"
    );
    match generic_signature(path, homes, in_scope) {
        Some(signature) => format!(
            "{head}. A function over any `{name}` takes a type parameter bounded by it: \
             `{signature}`"
        ),
        None => match placement(path) {
            // **Not `fn f<A: Eq>() -> A`**, which reads as the fix and does
            // not compile: the caller chooses `A`, and the body cannot make
            // a value of a type it did not choose.
            Placement::Return => format!(
                "{head}. A function cannot return \"any `{name}`\": whoever calls it would \
                 choose which, and the body cannot make a value of a type it did not choose. \
                 Name the type it returns"
            ),
            Placement::Field(Some(shape)) => format!(
                "{head}. A record that holds any `{name}` takes a type parameter bounded by \
                 it: `{shape}`"
            ),
            Placement::Field(None) | Placement::Elsewhere => format!(
                "{head}. Write the type itself here, or make it a type parameter bounded by \
                 `{name}`, as `fn f<A: {name}>(value: A)`"
            ),
        },
    }
}

/// Where a trait-as-type that is not in a parameter list was written.
enum Placement {
    /// In a function's return type.
    Return,
    /// In a field of a `type` declaration: the declaration rewritten with a
    /// bounded parameter, when it could be rewritten.
    Field(Option<String>),
    /// A `let`, an alias, a type argument outside either: no one shape fits.
    Elsewhere,
}

/// Which of [`Placement`] the mention at `path` is.
fn placement(path: &SyntaxNode) -> Placement {
    for node in path.ancestors() {
        if let Some(decl) = ast::FnDecl::cast(node.clone()) {
            let in_return = decl
                .return_type()
                .is_some_and(|ret| ret.syntax().text_range().contains_range(path.text_range()));
            return if in_return { Placement::Return } else { Placement::Elsewhere };
        }
        if let Some(decl) = ast::TypeDecl::cast(node.clone()) {
            return Placement::Field(bounded_type_decl(&decl, path));
        }
    }
    Placement::Elsewhere
}

/// `type Box<A: Show> = { item: A }`, for a `Show` written as `item`'s type.
///
/// Only where the declaration has no type parameters yet and this is the one
/// path type in it naming the trait: the shape that is certainly right.
/// Anything else gets `None` and the general advice.
fn bounded_type_decl(decl: &ast::TypeDecl, path: &SyntaxNode) -> Option<String> {
    if decl.type_params().is_some() {
        return None;
    }
    let name = decl.name()?.ident()?;
    let definition = decl.definition()?;
    let trait_name = ast::PathType::cast(path.clone())?.path()?.text_path();
    let mentions = definition
        .syntax()
        .descendants()
        .filter_map(ast::PathType::cast)
        .filter(|p| p.path().is_some_and(|p| p.text_path() == trait_name))
        .count();
    if mentions != 1 {
        return None;
    }
    let start = definition.syntax().text_range().start();
    let text = definition.syntax().text().to_string();
    let from = usize::from(path.text_range().start() - start);
    let to = usize::from(path.text_range().end() - start);
    let body = format!("{}A{}", &text[..from], &text[to..]);
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!("type {name}<A: {trait_name}> = {body}"))
}

/// `fn render_one<A: Show>(item: A)`, for a `Show` written in `item`'s type.
///
/// `None` when the mention is not in a function's parameter list: a return
/// type of `Show` is not fixed by a type parameter, which the caller would
/// choose, and a field or a `let` has no parameter list to add one to.
fn generic_signature(
    path: &SyntaxNode,
    homes: &crate::TypeHomes,
    in_scope: &HashSet<String>,
) -> Option<String> {
    let param_list = path.ancestors().find(|a| a.kind() == SyntaxKind::PARAM_LIST)?;
    let decl = ast::FnDecl::cast(param_list.parent()?)?;
    let name = decl.name()?.ident()?;

    // Fresh names, skipping every parameter the declaration already has.
    let mut fresh = ('A'..='Z').map(String::from).filter(|n| !in_scope.contains(n));
    let mut added: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();
    for param in param_list.children().filter(|n| n.kind() == SyntaxKind::PARAM) {
        let start = param.text_range().start();
        let text = param.text().to_string();
        // In source order, which `descendants` already is.
        let mentions: Vec<(SyntaxNode, String)> = param
            .descendants()
            .filter_map(|n| written_trait(&n, homes, in_scope).map(|t| (n, t)))
            .collect();
        let mut rewritten = String::new();
        let mut at = 0usize;
        for (mention, trait_name) in mentions {
            let letter = fresh.next()?;
            let from = usize::from(mention.text_range().start() - start);
            let to = usize::from(mention.text_range().end() - start);
            rewritten.push_str(&text[at..from]);
            rewritten.push_str(&letter);
            added.push(format!("{letter}: {trait_name}"));
            at = to;
        }
        rewritten.push_str(&text[at..]);
        params.push(rewritten.split_whitespace().collect::<Vec<_>>().join(" "));
    }

    // The new parameters go after the existing type parameters and before any
    // row variable, which is where a list written by hand has them.
    let existing: Vec<String> = decl
        .type_params()
        .map(|tp| {
            tp.params()
                .map(|p| p.syntax().text().to_string().split_whitespace().collect::<Vec<_>>().join(" "))
                .collect()
        })
        .unwrap_or_default();
    let (rows, types): (Vec<String>, Vec<String>) =
        existing.into_iter().partition(|p| p.starts_with('\''));
    let generics: Vec<String> = types.into_iter().chain(added).chain(rows).collect();
    Some(format!("fn {name}<{}>({})", generics.join(", "), params.join(", ")))
}

/// The trait a `PATH_TYPE` names in a type position, if it names one: the
/// same test the walk applies, so every mention the rewritten signature
/// replaces is one the walk would report.
fn written_trait(
    node: &SyntaxNode,
    homes: &crate::TypeHomes,
    in_scope: &HashSet<String>,
) -> Option<String> {
    let name = ast::PathType::cast(node.clone())?.path()?.text_path();
    (homes.is_trait(&name) && !in_scope.contains(&name) && !names_a_trait_here(node))
        .then_some(name)
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
    matches!(
        name,
        "Int" | "I64" | "Float" | "Bool" | "Char" | "String" | "Ptr" | "Unit" | "Never"
    )
        || IntKind::parse(name).is_some()
        || in_scope.contains(name)
        || homes.of(name).is_some()
}

/// Whether this sits under an `effect` declaration, whose operations bind
/// their own row variables without writing a parameter list.
fn inside_an_effect(node: &SyntaxNode) -> bool {
    node.ancestors().any(|a| a.kind() == SyntaxKind::EFFECT_DECL)
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
