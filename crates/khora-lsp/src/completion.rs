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
//! | anything else | locals, this file's declarations and imports, and every public name in the workspace |
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
//! # Names that are not in scope yet
//!
//! The plain case offers what every module exports, not only what this file
//! imported, and a candidate that is not in scope carries the `import` that
//! would bring it in. **This is the completion people actually use in a typed
//! language**: you do not go looking for `chunked` in `std::list`, you type
//! `chun` and expect the editor to know. Until this existed, a name had to be
//! imported before it could be completed, which is the wrong way round — the
//! import is the thing you wanted the editor to write.
//!
//! They sort below everything in scope. A local named `rows` must not be
//! outranked by three hundred names from `std`, and an editor sorts by the
//! `sortText` the server gives it.
//!
//! **They are cheap on purpose.** A candidate from elsewhere carries a name, a
//! kind and its module, and nothing else: reading the `///` above each of a
//! thousand declarations to fill a list where one of them gets read measured
//! 100ms a keystroke against a workspace the shape of `std`, and 6ms without.
//! The documentation arrives through `completionItem/resolve`, for the one item
//! the reader highlighted.
//!
//! # Inside a `with { .. }`
//!
//! The one completion Khora needs that no other language has to answer, and
//! `handlers.rs` has the detail. Two rows are spelled the same and want
//! opposite things — a signature's `with` holds types, an expression's holds
//! handlers — and in an expression the entry offered is a whole handler with
//! every operation the effect declares.
//!
//! **The label comes from the requirement where there is one.** `std` installs
//! `LLMService` as `ai`, which no rule derives from the type; but the calls
//! inside the block say what they still need, and a row entry that answers a
//! requirement has to be spelled the way the requirement is. So an outstanding
//! `ai: LLMService` is offered as `ai: handler for LLMService { .. }`, first,
//! and only where nothing is outstanding does the label fall back to a name
//! made from the effect.

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::ItemKind;
use khora_syntax::ast::{AstNode, Path};
use khora_syntax::{SyntaxKind, SyntaxToken};
use lsp_types::CompletionItemKind;
use text_size::{TextRange, TextSize};

/// One thing that could be typed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What to insert.
    pub label: String,
    /// How an editor should draw it.
    pub kind: CompletionItemKind,
    /// The signature or type, shown beside the name.
    pub detail: Option<String>,
    /// The `///` above the declaration, as Markdown.
    ///
    /// **What the list is for.** A name and the word "function" is enough to
    /// finish typing something you already knew; the sentence its author
    /// wrote is what tells you whether it is the one you want. `std` has one
    /// on nearly everything, and none of it was reaching the editor.
    pub documentation: Option<String>,
    /// The module a not-yet-imported name comes from, to show beside it.
    ///
    /// `None` for everything already in scope, which is most of the list.
    pub source: Option<String>,
    /// The edit that brings the name in, for a candidate that is not in scope.
    ///
    /// Sent as `additionalTextEdits`, so accepting the completion writes the
    /// name and the `import` together.
    pub import: Option<(TextRange, String)>,
    /// What to insert, where that is not the label.
    ///
    /// A handler is a whole row entry and the label is the effect's name,
    /// because the name is what somebody types to find it and the entry is
    /// what they wanted written.
    pub insert: Option<String>,
    /// Sorted ahead of everything else when set.
    ///
    /// For the handler that answers a requirement the enclosing code actually
    /// has: it is not one of several plausible entries, it is the one.
    pub wanted: bool,
}

impl Candidate {
    /// A candidate with nothing to say about itself: a local, an imported
    /// alias, anything whose declaration is not in reach here.
    fn plain(label: String, kind: CompletionItemKind, detail: Option<String>) -> Candidate {
        Candidate {
            label,
            kind,
            detail,
            documentation: None,
            source: None,
            import: None,
            insert: None,
            wanted: false,
        }
    }
}

/// What could be written at `offset`.
///
/// `known` is every file the server has, which is what lets the plain case
/// offer a name this file has not imported yet.
pub fn at(
    db: &dyn Db,
    root: SourceRoot,
    file: SourceFile,
    offset: TextSize,
    known: &[SourceFile],
) -> Vec<Candidate> {
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
        // `with {` and `with { db: Db, ` both land on a brace or a comma
        // whose row is a `with`, which is a different question from either of
        // the two below it.
        Some(SyntaxKind::L_BRACE) | Some(SyntaxKind::COMMA)
            if anchor.as_ref().and_then(crate::handlers::row_at).is_some() =>
        {
            let at = anchor.as_ref().expect("just matched");
            let row = crate::handlers::row_at(at).expect("just matched");
            in_with_row(db, file, known, &tree, offset, row)
        }
        // `import m::{` and `import m::{X, ` both land on a brace or a comma
        // inside an import list.
        Some(SyntaxKind::L_BRACE) | Some(SyntaxKind::COMMA)
            if in_import_list(anchor.as_ref()) =>
        {
            in_scope_of_import(db, root, file, anchor.as_ref()).unwrap_or_default()
        }
        _ => {
            let mut out = in_scope(db, file, offset);
            out.extend(from_elsewhere(db, file, known, &tree, &out));
            out
        }
    }
}

/// Every public name in the workspace that this file has not got.
///
/// **The import is computed here rather than when the completion is accepted**,
/// because an editor applies `additionalTextEdits` without asking the server
/// again. That makes the cost the thing to watch: `imports::declared_in` walks
/// the file once, and each candidate then costs a comparison against the
/// imports already written rather than another walk.
fn from_elsewhere(
    db: &dyn Db,
    file: SourceFile,
    known: &[SourceFile],
    tree: &khora_syntax::SyntaxNode,
    taken: &[Candidate],
) -> Vec<Candidate> {
    let here = khora_hir::module_api(db, file).module.as_ref().map(crate::imports::written);
    let text = file.text(db);
    let existing = crate::imports::declared_in(tree);

    let mut out = Vec::new();
    for other in known {
        if *other == file {
            continue;
        }
        let Some(module) = khora_hir::module_api(db, *other).module.as_ref().map(crate::imports::written)
        else {
            continue;
        };
        if here.as_deref() == Some(module.as_str()) {
            continue;
        }
        for item in &khora_hir::module_api(db, *other).items {
            if !item.is_public {
                continue;
            }
            // A name already offered is already reachable, and offering it
            // twice with an import attached would put an import in the file
            // for something that did not need one.
            if taken.iter().any(|seen| seen.label == item.name) {
                continue;
            }
            // `None` means the name is already imported from here, or arrives
            // through a glob -- either way there is nothing to add.
            let Some(fix) = crate::imports::edit_among(&existing, tree, text, &module, &item.name)
            else {
                continue;
            };
            let mut candidate = Candidate::plain(
                item.name.clone(),
                kind_of(item.kind),
                Some(item.kind.describe().to_string()),
            );
            candidate.source = Some(module.clone());
            candidate.import = Some((fix.range, fix.replacement));
            out.push(candidate);
        }
    }
    out
}

/// What can be written in a `with` row.
///
/// A signature's row takes types, an expression's takes handlers, and the two
/// are told apart in `handlers::row_at`. Everything reachable is offered either
/// way -- this file's declarations first, then the workspace with the `import`
/// that would bring one in.
fn in_with_row(
    db: &dyn Db,
    file: SourceFile,
    known: &[SourceFile],
    tree: &khora_syntax::SyntaxNode,
    offset: TextSize,
    row: crate::handlers::Row,
) -> Vec<Candidate> {
    let text = file.text(db);
    let existing = crate::imports::declared_in(tree);
    let here = khora_hir::module_api(db, file).module.as_ref().map(crate::imports::written);
    // What the code in this function still has to be given. Empty while the
    // block is too broken to check, which is exactly why there is a fallback.
    let outstanding = outstanding_labels(db, file, offset);

    let mut out = Vec::new();
    let mut offer = |decl: &khora_syntax::ast::EffectDecl, module: Option<&str>| {
        let Some(name) = decl.name().and_then(|n| n.ident()) else { return };
        // The requirement's own spelling where there is one: a row entry that
        // answers `ai: LLMService` has to be called `ai`.
        let asked = outstanding.iter().find(|(_, ty)| *ty == name).map(|(label, _)| label.clone());
        let label = asked.clone().unwrap_or_else(|| crate::handlers::label_for(&name));
        let insert = match row {
            crate::handlers::Row::Installed => crate::handlers::skeleton(decl, &label),
            crate::handlers::Row::Declared => Some(format!("{label}: {name}")),
        };
        let Some(insert) = insert else { return };

        let mut candidate = Candidate::plain(
            name.clone(),
            CompletionItemKind::INTERFACE,
            Some(insert.clone()),
        );
        candidate.insert = Some(insert);
        candidate.wanted = asked.is_some();
        if let Some(module) = module {
            let Some(fix) = crate::imports::edit_among(&existing, tree, text, module, &name) else {
                return;
            };
            candidate.source = Some(module.to_string());
            candidate.import = Some((fix.range, fix.replacement));
        }
        out.push(candidate);
    };

    for decl in crate::handlers::declared_in(tree) {
        offer(&decl, None);
    }
    for other in known {
        if *other == file {
            continue;
        }
        let Some(module) = khora_hir::module_api(db, *other).module.as_ref().map(crate::imports::written)
        else {
            continue;
        };
        if here.as_deref() == Some(module.as_str()) {
            continue;
        }
        for decl in crate::handlers::declared_in(&khora_db::parse(db, *other).syntax()) {
            offer(&decl, Some(&module));
        }
    }
    out
}

/// The capabilities calls in the enclosing function still have to be given.
///
/// `(label, the head of the type)`, which is what it takes to match a
/// requirement to an effect declared somewhere. Empty when the cursor is not
/// inside a body the checker could make sense of, which while a `with {` is
/// half typed is often.
fn outstanding_labels(db: &dyn Db, file: SourceFile, offset: TextSize) -> Vec<(String, String)> {
    let checked = khora_types::checked(db, file);
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, body) in khora_hir::body::bodies(db, file) {
        if !body.exprs().any(|(id, _)| body.range(id).contains_inclusive(offset)) {
            continue;
        }
        let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            continue;
        };
        for (_, rows) in types.calls_with_rows() {
            let Some(khora_types::Type::Row { fields, .. }) = rows.requires.as_ref() else {
                continue;
            };
            for (label, ty) in fields {
                let head = ty.to_string();
                let head = head.split(['<', ' ']).next().unwrap_or_default().to_string();
                if !head.is_empty() && !out.iter().any(|(seen, _)| seen == label) {
                    out.push((label.clone(), head));
                }
            }
        }
        break;
    }
    out
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
            out.push(Candidate::plain(
                variant.name.clone(),
                CompletionItemKind::ENUM_MEMBER,
                Some(owner.clone()),
            ));
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
                documentation: None,
                source: None,
                import: None,
                insert: None,
                wanted: false,
            });
        }
        break;
    }

    let map = khora_hir::item_map(db, file);
    for item in &map.items {
        out.push(described(db, file, item));
    }
    for import in &map.imports {
        if let khora_hir::ImportKind::Named(names) = &import.kind {
            for imported in names {
                out.push(Candidate::plain(
                    imported.alias.clone(),
                    CompletionItemKind::REFERENCE,
                    Some(import.path.to_string()),
                ));
            }
        }
    }

    dedup(out)
}

/// What a module offers to whoever imports it.
fn exports_of(db: &dyn Db, file: SourceFile) -> Vec<Candidate> {
    let api = khora_hir::module_api(db, file);
    // **The range comes from `item_map`, not from `module_api`.** An
    // `ApiItem` deliberately carries no `TextRange`: that is what stops an
    // edit to one body invalidating every module that imports from it. Looking
    // the declaration up by name here keeps that property, and costs a map
    // this file has already built.
    let map = khora_hir::item_map(db, file);
    let mut out: Vec<Candidate> = api
        .items
        .iter()
        .filter(|item| item.is_public)
        .map(|item| match map.items.iter().find(|d| d.name == item.name) {
            Some(declared) => described(db, file, declared),
            None => Candidate::plain(
                item.name.clone(),
                kind_of(item.kind),
                Some(item.kind.describe().to_string()),
            ),
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// A declaration as a candidate: its own signature rather than the word
/// "function", and the `///` its author wrote.
///
/// Falling back to the kind when the declaration cannot be read keeps a name
/// in the list either way — an editor that offered nothing because a comment
/// was missing would be worse than one that offered the name alone.
fn described(db: &dyn Db, file: SourceFile, item: &khora_hir::Item) -> Candidate {
    match crate::explain::at(db, file, item.range) {
        Some(explained) => Candidate {
            label: item.name.clone(),
            kind: kind_of(item.kind),
            detail: Some(explained.signature.clone()),
            documentation: explained.docs,
            source: None,
            import: None,
            insert: None,
            wanted: false,
        },
        None => Candidate::plain(
            item.name.clone(),
            kind_of(item.kind),
            Some(item.kind.describe().to_string()),
        ),
    }
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
        out.push(Candidate::plain(
            method.to_string(),
            CompletionItemKind::METHOD,
            Some(if trait_name.is_empty() {
                signature.as_fn().to_string()
            } else {
                format!("{trait_name} — {}", signature.as_fn())
            }),
        ));
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
