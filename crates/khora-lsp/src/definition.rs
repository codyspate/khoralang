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
//! **Locals are here too**, and are a different question with a different
//! answer. A local resolves in a body rather than in the module, so
//! `resolve_path` never sees it — but `Body` already records a range per
//! binding and an `Expr::Local(id)` per use, which is everything a definition,
//! a reference list and a rename need. [`local_at`] is that lookup, and it is
//! shared: 14.8 asks it the same question with a different answer in mind.
//!
//! # The order the three lookups run in, which is load-bearing
//!
//! 1. A **use** of a local: `Expr::Local` carries the binding's id, and its
//!    range is one name.
//! 2. A **path**, resolved against the module graph.
//! 3. A **binding**: a `let`, or a parameter.
//!
//! Binding last, and that is not arbitrary. A parameter's range covers its
//! annotation as well as its name — `p: shapes::Point` entire — so a binding
//! check that ran first would answer "the parameter `p`" for a cursor on
//! `Point`, and go-to-definition on a type in a signature would land three
//! characters to the left instead of in another file. Paths are narrower and
//! go first.
//!
//! Within that, **a local shadows an item**, which is what shadowing means
//! everywhere else in the language.
//!
//! **A method is answered by its trait, not by its `impl`.** `khora_hir` does
//! not collect impl members — `collect_decl` returns early on `Decl::Impl`,
//! which is the same absence `khora-doc` had to read the syntax tree to work
//! around — so `Show::show` lands on `trait Show`. Better than nothing and
//! honest about which thing it found.
//!
//! **A local answers from both ends, because a rename is asked for from
//! either.** Somebody renaming `total` is as likely to have the cursor on
//! the `let` as on one of the uses, and a lookup that only understood uses
//! would refuse the more natural of the two. So [`local_use_at`] and
//! [`local_binding_at`] are a pair, and [`at`] tries them in that order.
//!
//! Bodies are searched in order and the first hit wins. A body's ranges do
//! not overlap another's — a function is one body — so there is no
//! ambiguity to resolve, only a scan to stop early.

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

/// A local binding, and every place in its body that names it.
///
/// One structure for definition, references and rename, because all three want
/// the same set: for a definition it is `binding`, for references it is `uses`,
/// and for a rename it is both — a rename that edited the uses and left the
/// `let` alone would produce a program that does not compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinding {
    /// What it is called, which a rename has to be given a new value for.
    pub name: String,
    /// Where it was introduced: a `let`, a parameter, or a pattern binding.
    pub binding: TextRange,
    /// Every `Expr::Local` naming it, in the order they were lowered.
    pub uses: Vec<TextRange>,
}

impl LocalBinding {
    /// The binding and its uses together, which is what a rename edits.
    pub fn everywhere(&self) -> Vec<TextRange> {
        let mut all = Vec::with_capacity(self.uses.len() + 1);
        all.push(self.binding);
        all.extend(self.uses.iter().copied());
        all.sort_by_key(|r| r.start());
        all.dedup();
        all
    }
}

/// A local named by a *use* at `offset`.
///
/// The narrow half, and the one that runs before paths: an `Expr::Local`'s
/// range is a single name, so a hit here is unambiguous.
pub fn local_use_at(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<LocalBinding> {
    for (_, body) in khora_hir::body::bodies(db, file) {
        let found = body.exprs().find_map(|(id, expr)| match expr {
            khora_hir::body::Expr::Local(local) if body.range(id).contains_inclusive(offset) => {
                Some(*local)
            }
            _ => None,
        });
        if let Some(local) = found {
            return Some(gather(body, local));
        }
    }
    None
}

/// A local named by its *binding* at `offset`.
///
/// **The wide half, and it must run after paths.** A parameter's range is the
/// whole pattern including its annotation, so `p: shapes::Point` answers to a
/// cursor anywhere in it — including on `Point`, which is a different question
/// with an answer in another file.
pub fn local_binding_at(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<LocalBinding> {
    for (_, body) in khora_hir::body::bodies(db, file) {
        // The narrowest, so an annotation mentioning a type does not beat a
        // binding written inside it.
        let found = body
            .locals()
            .filter(|(_, local)| local.range.contains_inclusive(offset))
            .min_by_key(|(_, local)| local.range.len())
            .map(|(id, _)| id);
        if let Some(local) = found {
            return Some(gather(body, local));
        }
    }
    None
}

/// The binding and every use of one local.
fn gather(body: &khora_hir::body::Body, local: khora_hir::body::LocalId) -> LocalBinding {
    let declared = body.local(local);
    let uses = body
        .exprs()
        .filter(|(_, expr)| matches!(expr, khora_hir::body::Expr::Local(l) if *l == local))
        .map(|(id, _)| body.range(id))
        .collect();
    LocalBinding { name: declared.name.clone(), binding: declared.range, uses }
}

/// The declaration named by whatever is at `offset`, if that is a path.
pub fn at(db: &dyn Db, root: SourceRoot, file: SourceFile, offset: TextSize) -> Option<Definition> {
    // A use of a local shadows an item, and is narrower than either.
    if let Some(local) = local_use_at(db, file, offset) {
        return Some(Definition { file, range: local.binding });
    }

    let tree = khora_db::parse(db, file).syntax();
    let Some(path) = path_at(&tree, offset) else {
        // No path: a binding is the remaining reading, and the cursor may be
        // sitting on the `let` or the parameter name itself.
        return local_binding_at(db, file, offset)
            .map(|local| Definition { file, range: local.binding });
    };

    // **Up to and including the segment under the cursor**, not the whole path.
    // In `std::core::print`, clicking `core` should reach the module and
    // clicking `print` the function, and truncating is the whole of the
    // difference between those two answers.
    let segments = segments_through(&path, offset);
    let by_path = segments
        .as_ref()
        .and_then(|segments| khora_hir::resolve_path(db, root, file, segments).ok())
        .and_then(|resolution| locate(db, root, &resolution));

    by_path
        .or_else(|| segments.as_deref().and_then(|segments| method_by_name(db, root, segments)))
        .or_else(|| {
            local_binding_at(db, file, offset).map(|local| Definition { file, range: local.binding })
        })
}

/// `Type::method`, when the name resolver could not say what `Type` is.
///
/// **The resolver only reads `Type::method` as one when the type is declared
/// in the same file.** `Int`, `String` and `List` are not declared anywhere —
/// they are the language's own, and the checker knows them by other means — so
/// `Int::to_string` reaches no resolution at all and go-to-definition on the
/// commonest call in the language answered with nothing.
///
/// An editor can afford to be more willing than a compiler here. If the path
/// is two segments and there is exactly one method of that name on that type
/// anywhere in the graph, that is the answer; a wrong jump is recoverable and
/// no jump is what this replaces. It runs only after resolution has failed, so
/// nothing it does can override a real answer.
fn method_by_name(db: &dyn Db, root: SourceRoot, segments: &[String]) -> Option<Definition> {
    let [type_name, method] = segments else { return None };
    if !type_name.starts_with(char::is_uppercase) {
        return None;
    }
    let graph = &khora_hir::module_graph(db, root);
    method_on(db, graph, type_name, method)
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
        // **The method first, and the trait only if there is no method.**
        // `owner` is written as it appears, so `Int::to_string` and
        // `Show::show` both arrive here and mean different things: the first
        // names a type and wants the function inside an `impl Int`, the
        // second names a trait and wants the declaration in it. Impl members
        // are collected now, so the first has somewhere to land -- it used to
        // fall through to a search for a trait called `Int`, find nothing,
        // and answer with no definition at all.
        Resolution::TraitItem { owner, name } => method_on(db, graph, owner, name)
            .or_else(|| graph.paths().find_map(|module| item_in(db, graph, module, owner))),
        Resolution::Unsupported(_) => None,
    }
}

/// A method `name` written in an `impl` for `type_name`, wherever it is.
///
/// **Every module, because an impl does not have to be beside its type.** A
/// trait impl is written where either the trait or the type is, and for a
/// method on a `std` type declared in an application that is a third file
/// again. The type's own module is tried first so an inherent method wins
/// over a trait one with the same name.
fn method_on(
    db: &dyn Db,
    graph: &khora_hir::ModuleGraph,
    type_name: &str,
    name: &str,
) -> Option<Definition> {
    let mut fallback = None;
    for module in graph.paths() {
        let Some(file) = graph.file(module) else { continue };
        let map: &ItemMap = khora_hir::item_map(db, file);
        let Some(method) = map.method(type_name, name) else { continue };
        let found = Definition { file, range: method.range };
        if method.trait_name.is_none() {
            return Some(found);
        }
        fallback.get_or_insert(found);
    }
    fallback
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
