//! Find all references, and rename.
//!
//! Both are the same question — *where else does this name appear* — asked once
//! for reading and once for writing. Writing is the one that can do damage, so
//! the two do not have the same reach, and the difference is deliberate:
//!
//! | | references | rename |
//! | --- | --- | --- |
//! | a local | yes | yes |
//! | an item | yes | **refused, with a reason** |
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
//! # Why rename stops at locals
//!
//! **A rename that misses one occurrence produces source that does not
//! compile, silently, across files somebody was not looking at.** For a local
//! the set is provably complete: one body, and the HIR already recorded which
//! uses bind to which binding, because the compiler had to know that to check
//! the program at all.
//!
//! For an item it is not, and the gap is specific rather than vague:
//!
//! - The declaration's *name* has no range. `khora_hir::Item::range` is the
//!   whole declaration, so renaming through it would replace the body too. The
//!   name token is recoverable from the syntax tree; it is simply not recorded.
//! - `import m::{foo as bar}` has to rename the `foo` and leave the `bar`, and
//!   `khora_hir::ImportedName::range` covers `foo as bar` entire.
//!
//! Neither is hard. Both are the kind of thing that is wrong once and then
//! wrong in somebody's repository, so they are named here and in the roadmap
//! rather than guessed at. `prepareRename` refuses an item and says which of
//! these is missing, which is a better failure than an edit nobody reviewed.

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

    let Some(target) = resolution_at(db, root, file, offset) else {
        // No path either, so a binding is what is left — see
        // `definition`'s note on why this order rather than the other.
        let local = definition::local_binding_at(db, file, offset)?;
        let ranges = if include_declaration { local.everywhere() } else { local.uses.clone() };
        return Some(References { sites: vec![(file, ranges)] });
    };
    let mut sites = Vec::new();
    for each in root.files(db) {
        let ranges = paths_resolving_to(db, root, *each, &target);
        if !ranges.is_empty() {
            sites.push((*each, ranges));
        }
    }

    if include_declaration {
        if let Some(found) = definition::at(db, root, file, offset) {
            match sites.iter_mut().find(|(f, _)| *f == found.file) {
                Some((_, ranges)) if !ranges.contains(&found.range) => {
                    ranges.push(found.range);
                    ranges.sort_by_key(|r| r.start());
                }
                None => sites.push((found.file, vec![found.range])),
                _ => {}
            }
        }
    }

    Some(References { sites })
}

/// What a rename may do, or why it may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renameable {
    /// A local, and every range to edit within its file.
    Local { name: String, ranges: Vec<TextRange> },
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
    match resolution_at(db, root, file, offset) {
        Some(_) => Renameable::Refused(
            "renaming a declaration is not supported yet: its name has no recorded range, \
             and an import written `as` would need the original renamed and the alias left \
             alone. Renaming a local binding works.",
        ),
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
