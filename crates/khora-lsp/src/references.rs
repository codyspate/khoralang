//! Find all references, and rename.
//!
//! Both are the same question — *where else does this name appear* — asked once
//! for reading and once for writing. Writing is the one that can do damage, so
//! the two do not have the same reach, and the difference is deliberate:
//!
//! | | references | rename |
//! | --- | --- | --- |
//! | a local | yes | yes |
//! | an item | yes | yes, across the workspace |
//! | a trait member | yes | **refused, with a reason** |
//! | a constructor | yes | **refused, with a reason** |
//!
//! # Why references can be exact
//!
//! Not by matching text. Every `::` path in every file is *resolved*, and two
//! paths refer to the same thing when their [`khora_hir::Resolution`] is equal.
//! That is the compiler's own answer, so an alias resolves correctly, a name
//! shadowed in one file does not match, and two modules that each declare a
//! `Point` stay apart — the identity is the declaration, not the spelling.
//!
//! It costs a resolve per path per file, which is a user-initiated action on a
//! single-threaded server and has not been worth caching.
//!
//! # What rename does, and what it still refuses
//!
//! **A rename that misses one occurrence produces source that does not
//! compile, silently, across files somebody was not looking at.** That is why
//! this refused to leave a body for as long as it did, and the two things
//! standing in the way were specific rather than vague. Both are answered now:
//!
//! - The declaration's *name* had no range. `khora_hir::Item::range` is the
//!   whole declaration, so renaming through it would have replaced the body
//!   too. `name_range` narrows it to the name token, which was always in the
//!   tree.
//! - `import m::{foo as bar}` has to rename the `foo` and leave the `bar`, and
//!   an import list is not a `::` path so nothing looked at it at all. It is
//!   searched directly, and every edit is checked against the declaration's
//!   own spelling — so the `foo` is renamed, the `bar` and its uses are not,
//!   and a fully qualified `m::foo` in the same file still is.
//!
//! Two things are still refused, each with a reason the editor shows:
//!
//! - **A trait member.** The name belongs to the trait and to every impl of
//!   it, and `khora_hir` resolves `Type::method` without saying which impl, so
//!   an edit here would change one of several declarations that must agree.
//! - **A constructor.** `khora_hir::Variant` records a name and a type and no
//!   range, so there is nothing to edit.
//!

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::Resolution;
use khora_syntax::ast::{AstNode, Path};
use text_size::{TextRange, TextSize};

use crate::definition;

/// Everywhere one thing is named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    /// Ranges per file, including the declaration when it is known.
    pub sites: Vec<(SourceFile, Vec<TextRange>)>,
}

/// Every mention of whatever is at `offset`.
///
/// A local first, because a local shadows an item — the same order
/// [`definition::at`] uses, and for the same reason.
pub fn at(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    offset: TextSize,
    include_declaration: bool,
) -> Option<References> {
    if let Some(local) = definition::local_use_at(db, file, offset) {
        let ranges = if include_declaration { local.everywhere() } else { local.uses.clone() };
        return Some(References { sites: vec![(file, ranges)] });
    }

    let found = resolution_at(db, root, file, offset)
        .or_else(|| declared_at(db, file, offset));
    let Some(target) = found else {
        // No path either, so a binding is what is left — see
        // `definition`'s note on why this order rather than the other.
        let local = definition::local_binding_at(db, file, offset)?;
        let ranges = if include_declaration { local.everywhere() } else { local.uses.clone() };
        return Some(References { sites: vec![(file, ranges)] });
    };
    let mut sites = Vec::new();
    for each in root.files(db) {
        let mut ranges = paths_resolving_to(db, root, *each, &target);
        ranges.extend(imports_naming(db, root, *each, &target));
        ranges.sort_by_key(|r| r.start());
        ranges.dedup();
        if !ranges.is_empty() {
            sites.push((*each, ranges));
        }
    }

    if include_declaration {
        if let Some(found) = definition::at(db, root, file, offset) {
            // **The name, not the declaration.** `Item::range` is the whole
            // thing — signature, body and all — so adding it unnarrowed makes
            // find-references highlight a page and makes rename replace one.
            let at = name_range(db, found.file, found.range).unwrap_or(found.range);
            match sites.iter_mut().find(|(f, _)| *f == found.file) {
                Some((_, ranges)) if !ranges.contains(&at) => {
                    ranges.push(at);
                    ranges.sort_by_key(|r| r.start());
                }
                None => sites.push((found.file, vec![at])),
                _ => {}
            }
        }
    }

    Some(References { sites })
}

/// The declaration's own name, whatever a file may call it locally.
fn declared_name(resolution: &Resolution) -> Option<&str> {
    match resolution {
        Resolution::Item { name, .. } => Some(name),
        Resolution::Variant { name, .. } => Some(name),
        Resolution::TraitItem { name, .. } => Some(name),
        Resolution::Unsupported(_) => None,
    }
}

/// The text a range covers, or empty when it is out of bounds.
fn spelled(text: &str, range: TextRange) -> &str {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return "";
    }
    &text[start..end]
}

/// The item this file declares, when the cursor is on its name.
///
/// **A declaration's name is not a path**, so nothing resolves it and renaming
/// from the `fn` line itself found nothing — which is the place somebody is
/// most likely to start a rename. The `ItemMap` knows what this file declares
/// and where; narrowing each to its name token says which one the cursor is
/// in.
fn declared_at(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<Resolution> {
    let map = khora_hir::item_map(db, file);
    let module = map.module.clone()?;
    for item in &map.items {
        let Some(at) = name_range(db, file, item.range) else { continue };
        if at.contains_inclusive(offset) {
            return Some(Resolution::Item {
                module,
                name: item.name.clone(),
                kind: item.kind,
            });
        }
    }
    None
}

/// The name token inside a declaration whose range is `decl`.
///
/// `khora_hir::Item::range` covers the declaration entire, which is right for
/// "where is this" and wrong for "what do I edit". The token is in the tree;
/// it was simply never asked for.
pub fn name_range(db: &dyn Db, file: SourceFile, decl: TextRange) -> Option<TextRange> {
    let tree = khora_db::parse(db, file).syntax();
    // The innermost node with exactly that range: a declaration and the
    // node wrapping it can share one, and the name belongs to the inner.
    let node = tree.descendants().filter(|node| node.text_range() == decl).last()?;
    node.children()
        .find(|child| child.kind() == khora_syntax::SyntaxKind::NAME)
        .map(|name| name.text_range())
}

/// Where an import list names the target: the `foo` of `import m::{foo}`, and
/// the `foo` alone in `import m::{foo as bar}`.
///
/// **Renaming an item without these produces files that do not compile.** The
/// uses are `::` paths and are found by resolution; the import that brings the
/// name into the file is not a path at all, so nothing else looks at it. That
/// gap is the reason rename refused to leave a body until now.
fn imports_naming(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    target: &Resolution,
) -> Vec<TextRange> {
    let Resolution::Item { module, name, .. } = target else { return Vec::new() };
    let tree = khora_db::parse(db, file).syntax();
    let mut out = Vec::new();
    let _ = root;

    for node in tree.descendants() {
        let Some(import) = khora_syntax::ast::ImportDecl::cast(node) else { continue };
        let path: Vec<String> = import
            .path()
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();
        if khora_hir::ModulePath::new(path) != *module {
            continue;
        }
        for item in import.items() {
            let Some(written) = item.name() else { continue };
            if written.ident().as_deref() == Some(name.as_str()) {
                out.push(written.syntax().text_range());
            }
        }
    }
    out
}

/// What a rename may do, or why it may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renameable {
    /// A local, and every range to edit within its file.
    Local { name: String, ranges: Vec<TextRange> },
    /// A declaration, and every range to edit in every file that names it.
    Item { name: String, sites: Vec<(SourceFile, Vec<TextRange>)> },
    /// Something that resolves, but not to anything this may safely edit.
    Refused(&'static str),
    /// Nothing under the cursor.
    Nothing,
}

/// Whether the thing at `offset` can be renamed.
pub fn renameable(db: &dyn Db, root: SourceRoot, file: SourceFile, offset: TextSize) -> Renameable {
    if let Some(local) = definition::local_use_at(db, file, offset) {
        return Renameable::Local { name: local.name.clone(), ranges: local.everywhere() };
    }
    let found = resolution_at(db, root, file, offset)
        .or_else(|| declared_at(db, file, offset));
    match found {
        // **Every mention, or none.** The two things that used to make this
        // unsafe are answered rather than avoided: the declaration's name is
        // narrowed out of its range, and an import list is searched for the
        // written name so `import m::{foo as bar}` renames the `foo` and
        // leaves the `bar`. What is still refused is anything that resolves
        // to a trait member or a constructor, where one name may stand for
        // several declarations and a rename would edit the wrong one.
        Some(Resolution::TraitItem { .. }) => Renameable::Refused(
            "renaming a trait member is not supported: the name belongs to the trait and to \
             every impl of it, and editing one of those without the others produces a program \
             that does not compile. Rename the trait's declaration instead.",
        ),
        Some(Resolution::Variant { .. }) => Renameable::Refused(
            "renaming a constructor is not supported yet: a case is declared inside its type \
             and `khora_hir::Variant` records no range for it, so there is nothing to edit.",
        ),
        Some(resolution) => match at(db, root, file, offset, true) {
            Some(found) if !found.sites.is_empty() => {
                let Some(declared) = declared_name(&resolution) else {
                    return Renameable::Nothing;
                };
                // **Only what is spelled with the declaration's own name.**
                // A file that writes `import m::{add as plus}` calls it
                // `plus`, and `plus` is that file's word rather than this
                // declaration's -- renaming it would break the file the
                // rename was supposed to fix. The import's `add` is renamed,
                // the alias and its uses are not, and a fully qualified
                // `m::add` in the same file still is, because it is spelled
                // with the name.
                let sites: Vec<(SourceFile, Vec<TextRange>)> = found
                    .sites
                    .into_iter()
                    .map(|(each, ranges)| {
                        let text = each.text(db);
                        let kept = ranges
                            .into_iter()
                            .filter(|range| spelled(text, *range) == declared)
                            .collect::<Vec<_>>();
                        (each, kept)
                    })
                    .filter(|(_, ranges)| !ranges.is_empty())
                    .collect();
                if sites.is_empty() {
                    return Renameable::Nothing;
                }
                Renameable::Item { name: declared.to_string(), sites }
            }
            _ => Renameable::Nothing,
        },
        None => match definition::local_binding_at(db, file, offset) {
            Some(local) => {
                Renameable::Local { name: local.name.clone(), ranges: local.everywhere() }
            }
            None => Renameable::Nothing,
        },
    }
}

/// What the path at `offset` resolves to.
fn resolution_at(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    offset: TextSize,
) -> Option<Resolution> {
    let tree = khora_db::parse(db, file).syntax();
    let token = tree.token_at_offset(offset).right_biased()?;
    let path = token.parent_ancestors().find_map(Path::cast)?;

    let mut segments = Vec::new();
    for segment in path.segments() {
        let range = segment.syntax().text_range();
        segments.push(segment.ident()?);
        if range.end() >= offset {
            break;
        }
    }
    if segments.is_empty() {
        return None;
    }
    khora_hir::resolve_path(db, root, file, &segments).ok()
}

/// Every path in `file` that resolves to `target`, by the range of its last
/// segment.
///
/// **The last segment, not the whole path.** A reference to `add` in
/// `helper::add` is the `add`, and highlighting `helper::add` entire would put
/// a mark on the module as well — which is a different thing that a reader may
/// be about to ask about separately.
fn paths_resolving_to(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    target: &Resolution,
) -> Vec<TextRange> {
    let tree = khora_db::parse(db, file).syntax();
    let mut out = Vec::new();

    for node in tree.descendants() {
        let Some(path) = Path::cast(node) else { continue };
        let segments: Vec<String> = path.segments().filter_map(|s| s.ident()).collect();
        if segments.is_empty() {
            continue;
        }
        let Ok(resolved) = khora_hir::resolve_path(db, root, file, &segments) else { continue };
        if &resolved != target {
            continue;
        }
        if let Some(last) = path.segments().last() {
            out.push(last.syntax().text_range());
        }
    }

    out.sort_by_key(|r| r.start());
    out.dedup();
    out
}
