//! Go to definition: a cursor, and the declaration it names.
//!
//! **The index the roadmap asked for is not here, and that is the finding.**
//! 14.3 assumed a position → resolution map had to be built and stored. It does
//! not: `khora_hir::resolve_path` already answers the hard half, and the
//! syntax tree already answers "what is under this offset" — rowan keeps every
//! byte, so the path a cursor sits in is a node lookup rather than something to
//! be recorded during lowering.
//!
//! So this is a walk from a token up to the nearest [`Path`], and a resolution
//! into [`khora_hir::Resolution`], which the name resolver was producing all
//! along and nothing was asking for by position.
//!
//! # What it can and cannot answer
//!
//! A `::` path is a declaration somewhere, and that is what this handles:
//! functions, types, traits, effects, contexts, constants, and a constructor
//! by way of the type that declares it.
//!
//! **Locals are deliberately not here.** `x` in `let x = 1; x + 1` resolves in
//! a body rather than in the module, and jumping to a binding three lines up is
//! the case where the answer is already on the screen. It is also the case
//! where getting it wrong is most visible. `Body` records what would be needed
//! and 14.8 wants the same information for rename, so this is a gap to fill
//! once rather than twice.
//!
//! **A method is answered by its trait, not by its `impl`.** `khora_hir` does
//! not collect impl members — `collect_decl` returns early on `Decl::Impl`,
//! which is the same absence `khora-doc` had to read the syntax tree to work
//! around — so `Show::show` lands on `trait Show`. Better than nothing and
//! honest about which thing it found.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::{ItemMap, ModulePath, Resolution};
use khora_syntax::ast::{AstNode, Path};
use khora_syntax::{SyntaxNode, SyntaxToken};
use text_size::{TextRange, TextSize};

/// Where a declaration is: the file that holds it, and the range of its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The file the declaration is in, which is often not the one asked about.
    pub file: SourceFile,
    /// The declaration's own range.
    pub range: TextRange,
}

/// The declaration named by whatever is at `offset`, if that is a path.
pub fn at(db: &dyn Db, root: SourceRoot, file: SourceFile, offset: TextSize) -> Option<Definition> {
    let tree = khora_db::parse(db, file).syntax();
    let path = path_at(&tree, offset)?;

    // **Up to and including the segment under the cursor**, not the whole path.
    // In `std::core::print`, clicking `core` should reach the module and
    // clicking `print` the function, and truncating is the whole of the
    // difference between those two answers.
    let segments = segments_through(&path, offset)?;
    let resolution = khora_hir::resolve_path(db, root, file, &segments).ok()?;

    locate(db, root, &resolution)
}

/// The innermost `PATH` covering `offset`.
///
/// A token first, then its ancestors, because a cursor is a position between
/// bytes and the interesting node is whichever one owns the byte after it.
fn path_at(tree: &SyntaxNode, offset: TextSize) -> Option<Path> {
    let token = token_at(tree, offset)?;
    token.parent_ancestors().find_map(Path::cast)
}

/// The token covering `offset`, preferring the one that starts there.
///
/// At a boundary two tokens touch, and the cursor sitting at the end of a name
/// should mean that name — which is where somebody who has just typed it is.
fn token_at(tree: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    let at = tree.token_at_offset(offset);
    match at {
        rowan::TokenAtOffset::None => None,
        rowan::TokenAtOffset::Single(token) => Some(token),
        // Between two, take the left: the cursor after `print` is on `print`
        // rather than on the `(` that follows it.
        rowan::TokenAtOffset::Between(left, right) => {
            if left.kind() == khora_syntax::SyntaxKind::IDENT {
                Some(left)
            } else {
                Some(right)
            }
        }
    }
}

/// The path's segments, cut off after the one the cursor is in.
fn segments_through(path: &Path, offset: TextSize) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for segment in path.segments() {
        let range = segment.syntax().text_range();
        let name = segment.ident()?;
        out.push(name);
        if range.end() >= offset {
            return Some(out);
        }
    }
    // Past the last segment — a cursor on the trailing `::` of an unfinished
    // path. The whole thing is the best reading of that.
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Turns a resolution into a place on disk.
fn locate(db: &dyn Db, root: SourceRoot, resolution: &Resolution) -> Option<Definition> {
    let graph = &khora_hir::module_graph(db, root);
    match resolution {
        Resolution::Item { module, name, .. } => item_in(db, graph, module, name),
        // A constructor has no range of its own — `khora_hir::Variant` records
        // the name and the type and nothing else — so this lands on the type
        // that declares it, which is where a reader wants to end up anyway.
        Resolution::Variant { module, type_name, .. } => item_in(db, graph, module, type_name),
        // The trait, for the reason in the module documentation: impl members
        // are not collected, so there is no method to land on.
        Resolution::TraitItem { owner, .. } => {
            graph.paths().find_map(|module| {
                let found = item_in(db, graph, module, owner)?;
                Some(found)
            })
        }
        Resolution::Unsupported(_) => None,
    }
}

fn item_in(
    db: &dyn Db,
    graph: &khora_hir::ModuleGraph,
    module: &ModulePath,
    name: &str,
) -> Option<Definition> {
    let file = graph.file(module)?;
    let map: &ItemMap = khora_hir::item_map(db, file);
    let item = map.item(name)?;
    Some(Definition { file, range: item.range })
}
