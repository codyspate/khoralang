//! Completion, from what the compiler already knows rather than from the words
//! on screen.
//!
//! **Completion is asked for in code that does not parse**, and that decides
//! how all of this is written. `s.` is a syntax error and a request for the
//! methods of `s` at the same moment.
//!
//! What the parser does with it is worth knowing, because the obvious
//! implementation does not work: `s.` becomes a `PATH_EXPR` for `s` and then
//! an **`ERROR` node holding the dot**, and `Risk::` the same with the `::`.
//! So the trigger character is not inside the path it appears to belong to, and
//! walking up from it to a `Path` node finds nothing.
//!
//! Everything here therefore reads *the token stream* backwards rather than the
//! node tree. Tokens survive error recovery — rowan keeps every byte — where
//! the shape around them does not. The tree is used only where the code on
//! either side of the cursor is intact, which is the scope case.
//!
//! # The four places, and what each offers
//!
//! | after | offers |
//! | --- | --- |
//! | `.` | the methods of the receiver's type |
//! | `Type::` | that type's own methods and constructors |
//! | `import m::{` | what `m` exports |
//! | anything else | locals, this file's declarations, and what it imported |
//!
//! Which one applies is decided by looking *backwards* from the cursor, since
//! what has been typed is behind it and what is being asked about is not there
//! yet.
//!
//! # What it does not do
//!
//! No ranking, no fuzzy matching, and no filtering by prefix — an editor does
//! all three, and doing them here as well means two answers that disagree
//! about which is best.
//!
//! Nothing after a `with {` yet. The roadmap wants the handlers that satisfy a
//! row there, which needs the row at the cursor and the set of handlers that
//! could produce it; both exist, and joining them is a piece of work rather
//! than an afternoon.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::ItemKind;
use khora_syntax::ast::{AstNode, Path};
use khora_syntax::{SyntaxKind, SyntaxToken};
use lsp_types::CompletionItemKind;
use text_size::TextSize;

/// One thing that could be typed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What to insert.
    pub label: String,
    /// How an editor should draw it.
    pub kind: CompletionItemKind,
    /// The signature or type, shown beside the name.
    pub detail: Option<String>,
}

/// What could be written at `offset`.
pub fn at(db: &dyn Db, root: SourceRoot, file: SourceFile, offset: TextSize) -> Vec<Candidate> {
    let tree = khora_db::parse(db, file).syntax();

    // Backwards, because what decides the context is what has been typed.
    let before = match tree.token_at_offset(offset) {
        rowan::TokenAtOffset::None => return Vec::new(),
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(left, _) => Some(left),
    };
    let Some(before) = before else { return Vec::new() };
    let anchor = skip_back_over_trivia(before);

    match anchor.as_ref().map(SyntaxToken::kind) {
        Some(SyntaxKind::DOT) => {
            after_dot(db, file, &anchor.expect("just matched")).unwrap_or_default()
        }
        Some(SyntaxKind::COLON_COLON) => {
            after_colons(db, root, file, &anchor.expect("just matched")).unwrap_or_default()
        }
        // `import m::{` and `import m::{X, ` both land on a brace or a comma
        // inside an import list.
        Some(SyntaxKind::L_BRACE) | Some(SyntaxKind::COMMA)
            if in_import_list(anchor.as_ref()) =>
        {
            in_scope_of_import(db, root, file, anchor.as_ref()).unwrap_or_default()
        }
        _ => in_scope(db, file, offset),
    }
}

/// The nearest token before this one that is not whitespace or a comment.
fn skip_back_over_trivia(from: SyntaxToken) -> Option<SyntaxToken> {
    let mut at = Some(from);
    while let Some(token) = at {
        if !matches!(
            token.kind(),
            SyntaxKind::WHITESPACE | SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT
        ) {
            return Some(token);
        }
        at = token.prev_token();
    }
    None
}

/// Methods of whatever is to the left of the dot.
///
/// The receiver's *type* comes from the checker, so this is right for a
/// method on a type the reader never named — `request.params.get(..)` offers
/// `Params`'s methods without `Params` being in scope, which is the case a
/// text-matching completion cannot do at all.
fn after_dot(db: &dyn Db, file: SourceFile, dot: &SyntaxToken) -> Option<Vec<Candidate>> {
    let receiver = dot.prev_token().and_then(skip_back_over_trivia)?;
    let end = receiver.text_range().end();

    let checked = khora_types::checked(db, file);
    let mut receiver_type = None;
    for (name, body) in khora_hir::body::bodies(db, file) {
        let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            continue;
        };
        // The innermost expression ending where the receiver ends: `a.b` under
        // a cursor after `b` is about `a.b`, not about `a`.
        let mut best: Option<(text_size::TextRange, khora_types::Type)> = None;
        for (id, _) in body.exprs() {
            let range = body.range(id);
            if range.end() != end {
                continue;
            }
            let ty = types.of(id);
            if matches!(ty, khora_types::Type::Unknown) {
                continue;
            }
            if best.as_ref().is_none_or(|(b, _)| range.len() < b.len()) {
                best = Some((range, ty.clone()));
            }
        }
        if let Some((_, ty)) = best {
            receiver_type = Some(ty);
            break;
        }
    }

    let head = head_of(&receiver_type?)?;
    Some(methods_on(db, file, &head))
}

/// What follows `Type::`: that type's methods and its constructors.
fn after_colons(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    colons: &SyntaxToken,
) -> Option<Vec<Candidate>> {
    let segments = path_before(colons);
    let owner = segments.last()?.clone();

    let mut out = methods_on(db, file, &owner);

    // A module path offers what the module exports; a type offers its cases.
    if let Ok(khora_hir::Resolution::Item { .. }) =
        khora_hir::resolve_path(db, root, file, &segments)
    {
        let map = khora_types::type_map(db, file);
        for variant in map.variants.iter().filter(|v| v.type_name == owner) {
            out.push(Candidate {
                label: variant.name.clone(),
                kind: CompletionItemKind::ENUM_MEMBER,
                detail: Some(owner.clone()),
            });
        }
    }

    if out.is_empty() {
        // Not a type: a module, then. Offer what it exports.
        let graph = khora_hir::module_graph(db, root);
        let module = graph.paths().find(|p| p.to_string().ends_with(&owner))?.clone();
        let target = graph.file(&module)?;
        return Some(exports_of(db, target));
    }
    Some(out)
}

/// The path written immediately before this token, read off the tokens.
///
/// **Not from the `Path` node**, which is the whole point: an incomplete path
/// puts its trailing `::` in an `ERROR` node, so the node says `Risk` where the
/// tokens say `Risk::`. Walking back over alternating identifiers and `::`
/// reconstructs what was typed, whatever the parser made of it.
fn path_before(from: &SyntaxToken) -> Vec<String> {
    let mut segments = Vec::new();
    let mut at = from.prev_token().and_then(skip_back_over_trivia);
    // Alternating: an identifier, then the `::` before it, and so on.
    while let Some(token) = at {
        match token.kind() {
            SyntaxKind::IDENT => segments.push(token.text().to_string()),
            _ => break,
        }
        let separator = token.prev_token().and_then(skip_back_over_trivia);
        match separator {
            Some(ref sep) if sep.kind() == SyntaxKind::COLON_COLON => {
                at = sep.prev_token().and_then(skip_back_over_trivia);
            }
            _ => break,
        }
    }
    segments.reverse();
    segments
}

/// Whether this token sits inside an `import a::b::{ .. }` list.
fn in_import_list(token: Option<&SyntaxToken>) -> bool {
    token.is_some_and(|t| {
        t.parent_ancestors().any(|node| node.kind() == SyntaxKind::IMPORT_DECL)
    })
}

/// What the module named by the enclosing import offers.
fn in_scope_of_import(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    token: Option<&SyntaxToken>,
) -> Option<Vec<Candidate>> {
    let decl = token?
        .parent_ancestors()
        .find(|node| node.kind() == SyntaxKind::IMPORT_DECL)?;
    let path = decl.descendants().find_map(Path::cast)?;
    let segments: Vec<String> = path.segments().filter_map(|s| s.ident()).collect();
    if segments.is_empty() {
        return None;
    }

    let graph = khora_hir::module_graph(db, root);
    let wanted = segments.join("::");
    let module = graph.paths().find(|p| p.to_string() == wanted)?.clone();
    let target = graph.file(&module)?;
    let _ = file;
    Some(exports_of(db, target))
}

/// Locals of the enclosing body, plus everything this file declares or
/// imported.
///
/// **Every local of the body, not only those declared above the cursor.**
/// Lexical scoping at a position is a third thing to get right and the cost of
/// getting it wrong the other way is worse: a `let` being written *now* is
/// exactly the name somebody is about to use on the next line, and hiding it
/// makes completion feel broken in the one place it is used most.
fn in_scope(db: &dyn Db, file: SourceFile, offset: TextSize) -> Vec<Candidate> {
    let mut out = Vec::new();

    let checked = khora_types::checked(db, file);
    for (name, body) in khora_hir::body::bodies(db, file) {
        let covers = body
            .exprs()
            .any(|(id, _)| body.range(id).contains_inclusive(offset));
        if !covers {
            continue;
        }
        let types = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t);
        for (id, local) in body.locals() {
            out.push(Candidate {
                label: local.name.clone(),
                kind: CompletionItemKind::VARIABLE,
                detail: types.map(|t| t.local(id).to_string()),
            });
        }
        break;
    }

    let map = khora_hir::item_map(db, file);
    for item in &map.items {
        out.push(Candidate {
            label: item.name.clone(),
            kind: kind_of(item.kind),
            detail: Some(item.kind.describe().to_string()),
        });
    }
    for import in &map.imports {
        if let khora_hir::ImportKind::Named(names) = &import.kind {
            for imported in names {
                out.push(Candidate {
                    label: imported.alias.clone(),
                    kind: CompletionItemKind::REFERENCE,
                    detail: Some(import.path.to_string()),
                });
            }
        }
    }

    dedup(out)
}

/// What a module offers to whoever imports it.
fn exports_of(db: &dyn Db, file: SourceFile) -> Vec<Candidate> {
    let api = khora_hir::module_api(db, file);
    let mut out: Vec<Candidate> = api
        .items
        .iter()
        .filter(|item| item.is_public)
        .map(|item| Candidate {
            label: item.name.clone(),
            kind: kind_of(item.kind),
            detail: Some(item.kind.describe().to_string()),
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Every method whose impl is on `head`, inherent or through a trait.
///
/// Read off the signature keys — `Trait#Type::method`, and `#Type::method` for
/// an inherent one — because that is where the *types* crate records them.
/// `khora_hir` does not collect impl members at all, which is the absence
/// go-to-definition and the reference generator both work around.
fn methods_on(db: &dyn Db, file: SourceFile, head: &str) -> Vec<Candidate> {
    let map = khora_types::type_map(db, file);
    let mut out = Vec::new();
    for (key, signature) in &map.signatures {
        let Some((owner, method)) = key.split_once("::") else { continue };
        let Some((trait_name, self_name)) = owner.split_once('#') else { continue };
        if self_name != head {
            continue;
        }
        out.push(Candidate {
            label: method.to_string(),
            kind: CompletionItemKind::METHOD,
            detail: Some(if trait_name.is_empty() {
                signature.as_fn().to_string()
            } else {
                format!("{trait_name} — {}", signature.as_fn())
            }),
        });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    dedup(out)
}

/// The head constructor of a type: `Option` for `Option<Int>`.
fn head_of(ty: &khora_types::Type) -> Option<String> {
    match ty {
        khora_types::Type::Adt { name, .. } => Some(name.clone()),
        khora_types::Type::Applied { head, .. } => head_of(head),
        khora_types::Type::Str => Some("String".to_string()),
        khora_types::Type::Int => Some("Int".to_string()),
        khora_types::Type::Float => Some("Float".to_string()),
        khora_types::Type::Bool => Some("Bool".to_string()),
        khora_types::Type::Fixed(kind) => Some(kind.name()),
        _ => None,
    }
}

fn kind_of(kind: ItemKind) -> CompletionItemKind {
    match kind {
        ItemKind::Type => CompletionItemKind::STRUCT,
        ItemKind::Trait => CompletionItemKind::INTERFACE,
        ItemKind::Effect => CompletionItemKind::INTERFACE,
        ItemKind::Context => CompletionItemKind::MODULE,
        ItemKind::Function => CompletionItemKind::FUNCTION,
        ItemKind::Const => CompletionItemKind::CONSTANT,
    }
}

/// One entry per label, keeping the first.
fn dedup(mut items: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.clone()));
    items
}
