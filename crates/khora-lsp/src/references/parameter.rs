//! Renaming a function's parameter, which is also renaming its labels.
//!
//! **A parameter's name is the label a caller may write**, `send(1, keep:
//! true)`, and the checker holds every label to the declaration. A rename that
//! edited the parameter and its uses and stopped there -- which is what a
//! local rename does -- left every labeled caller refused, in files the
//! reader was not looking at.
//!
//! So a parameter of a named function is renamed as a declaration is: its
//! name, its uses in the body, and every label written against it anywhere in
//! the workspace. Three kinds of owner, and one exclusion:
//!
//! - a free function, matched by the callee's resolution;
//! - an inherent method, matched by the checker's key, `#Head::method`;
//! - a trait's declared method, matched by any key the checker reaches it
//!   through -- `Trait::method` through a bound, `Trait#Head::method` through
//!   an impl -- because a trait method's labels are the trait's names however
//!   the call is written;
//! - **an impl of a trait gets no label edits.** Its parameter names are local
//!   to its body, and the labels at its callers are the trait's.
//!
//! What it costs: a method is matched by its key, which names the type (or
//! the trait) and the method but not the module. Two modules that each
//! declare a type (or a trait) and a method of the same names, with a
//! parameter of the same name at the same position, would have each other's
//! labels renamed. A lambda's
//! parameter is left to the local rename: a function value takes no labels.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::body::{Expr, Pat};
use khora_hir::Resolution;
use khora_syntax::ast::{self, AstNode};
use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::{TextRange, TextSize};

use super::Renameable;

/// Whose parameter it is, as the checker's call sites can recognize it.
enum Owner {
    /// `name` in the module this file declares.
    Function { module: khora_hir::ModulePath, name: String },
    /// A method of `impl Head`, keyed `#Head::method`.
    Inherent { key: String },
    /// A method a trait declares, reached through any impl or bound.
    Trait { name: String, method: String },
    /// A method of `impl Trait for Head`, whose names are its own.
    TraitImpl,
}

/// The parameter at `offset`, as a rename over its labels, or `None` when the
/// cursor is not on a function's parameter or on a use of one.
pub(super) fn at(db: &dyn Db, root: SourceRoot, file: SourceFile, offset: TextSize) -> Option<Renameable> {
    let tree = khora_db::parse(db, file).syntax();
    let param = param_named_at(&tree, offset).or_else(|| param_used_at(db, &tree, file, offset))?;
    let name_node = param.children().find(|n| n.kind() == SyntaxKind::NAME)?;
    let name = name_node.text().to_string();
    // `self` is the receiver, which is never labeled -- and renaming it is
    // not something a method can survive anyway.
    if name == "self" {
        return None;
    }
    let list = param.parent().filter(|n| n.kind() == SyntaxKind::PARAM_LIST)?;
    let index = list.children().filter(|n| n.kind() == SyntaxKind::PARAM).position(|p| p == param)?;
    let decl = list.parent().and_then(ast::FnDecl::cast)?;
    let owner = owner_of(db, file, &decl)?;

    // The declaring file: the name, and every use of the parameter in the
    // body. A trait's declaration may have no body, and then has no uses.
    let mut here = vec![name_node.text_range()];
    here.extend(uses_in_body(db, file, &decl, &name_node.text_range()));

    let mut sites: Vec<(SourceFile, Vec<TextRange>)> = vec![(file, here)];
    if !matches!(owner, Owner::TraitImpl) {
        for each in root.files(db) {
            let labels = labels_naming(db, *each, &owner, &name, index);
            if labels.is_empty() {
                continue;
            }
            match sites.iter_mut().find(|(f, _)| f == each) {
                Some((_, ranges)) => ranges.extend(labels),
                None => sites.push((*each, labels)),
            }
        }
    }
    for (_, ranges) in &mut sites {
        ranges.sort_by_key(|r| r.start());
        ranges.dedup();
    }
    Some(Renameable::Item { name, sites })
}

/// The names of the other parameters of the function whose parameter is at
/// `offset`, or `None` when the cursor is not on a parameter or a use of one.
///
/// Read from the declaration rather than a body, because a trait's declared
/// method has no body and its parameters are still one namespace.
pub(super) fn siblings_at(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<(String, Vec<String>)> {
    let tree = khora_db::parse(db, file).syntax();
    let param = param_named_at(&tree, offset).or_else(|| param_used_at(db, &tree, file, offset))?;
    let name_of = |p: &SyntaxNode| p.children().find(|n| n.kind() == SyntaxKind::NAME).map(|n| n.text().to_string());
    let own = name_of(&param)?;
    let list = param.parent().filter(|n| n.kind() == SyntaxKind::PARAM_LIST)?;
    let others = list
        .children()
        .filter(|n| n.kind() == SyntaxKind::PARAM && *n != param)
        .filter_map(|p| name_of(&p))
        .collect();
    Some((own, others))
}

/// The `PARAM` whose name the cursor is on.
fn param_named_at(tree: &SyntaxNode, offset: TextSize) -> Option<SyntaxNode> {
    tree.descendants()
        .filter(|n| n.kind() == SyntaxKind::PARAM)
        .find(|p| {
            p.children()
                .find(|n| n.kind() == SyntaxKind::NAME)
                .is_some_and(|name| name.text_range().contains_inclusive(offset))
        })
}

/// The `PARAM` that a use of a local at the cursor binds.
///
/// A parameter's local is recorded with the whole parameter's range, name
/// and type together, which is how a use is traced back to it here -- and why
/// the rename below narrows to the `NAME` rather than editing that range.
fn param_used_at(db: &dyn Db, tree: &SyntaxNode, file: SourceFile, offset: TextSize) -> Option<SyntaxNode> {
    let local = crate::definition::local_use_at(db, file, offset)?;
    tree.descendants()
        .filter(|n| n.kind() == SyntaxKind::PARAM)
        .find(|p| p.text_range() == local.binding)
}

/// Every use of the parameter declared at `name` in `decl`'s body.
fn uses_in_body(db: &dyn Db, file: SourceFile, decl: &ast::FnDecl, name: &TextRange) -> Vec<TextRange> {
    let Some(block) = decl.body() else { return Vec::new() };
    let within = block.syntax().text_range();
    for (_, body) in khora_hir::body::bodies(db, file) {
        let param = body.params.iter().find_map(|pat| match body.pat(*pat) {
            Pat::Bind(local) if body.local(*local).range.contains_range(*name) => Some(*local),
            _ => None,
        });
        let Some(local) = param else { continue };
        return body
            .exprs()
            .filter(|(_, e)| matches!(e, Expr::Local(l) if *l == local))
            .map(|(id, _)| body.range(id))
            .filter(|r| within.contains_range(*r))
            .collect();
    }
    Vec::new()
}

/// What declares `decl`, in the checker's terms.
fn owner_of(db: &dyn Db, file: SourceFile, decl: &ast::FnDecl) -> Option<Owner> {
    let method = decl.name()?.ident()?;
    let parent = decl.syntax().parent()?;
    if parent.kind() == SyntaxKind::SOURCE_FILE {
        let module = khora_hir::item_map(db, file).module.clone()?;
        return Some(Owner::Function { module, name: method });
    }
    if let Some(t) = ast::TraitDecl::cast(parent.clone()) {
        return Some(Owner::Trait { name: t.name()?.ident()?, method });
    }
    let i = ast::ImplDecl::cast(parent)?;
    if !i.is_inherent() {
        return Some(Owner::TraitImpl);
    }
    let head = i.self_type().as_ref().and_then(khora_hir::body::type_head)?;
    Some(Owner::Inherent { key: format!("#{head}::{method}") })
}

/// Whether the checker's key for a call names `owner`.
fn key_names(owner: &Owner, key: &str) -> bool {
    match owner {
        Owner::Inherent { key: own } => key == own,
        Owner::Trait { name, method } => {
            if key == format!("{name}::{method}") {
                return true;
            }
            let Some((trait_key, rest)) = key.split_once('#') else { return false };
            let trait_name = trait_key.split('<').next().unwrap_or(trait_key);
            trait_name == name && rest.rsplit("::").next() == Some(method.as_str())
        }
        // Matched by resolution, not key: see `labels_naming`.
        Owner::Function { .. } | Owner::TraitImpl => false,
    }
}

/// The name of every label in `file` written against parameter `index` of
/// `owner`, where the label says `name`.
fn labels_naming(db: &dyn Db, file: SourceFile, owner: &Owner, name: &str, index: usize) -> Vec<TextRange> {
    let checked = khora_types::checked(db, file);
    let scope = khora_hir::file_scope(db, file);
    let mut out = Vec::new();
    for (key, body) in khora_hir::body::bodies(db, file) {
        let types = checked.bodies.iter().find(|(n, _)| n == key).map(|(_, t)| t);
        for (callee, labels) in &body.labels {
            let reached = match (owner, body.expr(*callee)) {
                // **Through the import, not the local spelling.** A call
                // through `import net::{send as post}` resolves as `post`,
                // and compared by that it matched nothing: the declaration
                // was renamed and the aliased caller's label was left
                // refused.
                (Owner::Function { module, name: function }, Expr::Path(Resolution::Item { module: m, name: n, .. })) => {
                    let declared = scope
                        .origin(n)
                        .filter(|origin| origin.module == *m)
                        .map_or(n.as_str(), |origin| origin.name.as_str());
                    m == module && declared == function
                }
                (Owner::Function { .. }, _) => false,
                (Owner::TraitImpl, _) => false,
                (Owner::Inherent { .. } | Owner::Trait { .. }, _) => types
                    .and_then(|t| t.instantiation(*callee))
                    .is_some_and(|(k, _)| key_names(owner, k)),
            };
            if !reached {
                continue;
            }
            // `x.f(a)` lowers with the receiver outside the argument list.
            let skip = usize::from(matches!(body.expr(*callee), Expr::Field { .. }));
            for (position, label, at) in labels {
                if label == name && position + skip == index {
                    out.push(TextRange::at(at.start(), TextSize::of(label.as_str())));
                }
            }
        }
    }
    out
}
