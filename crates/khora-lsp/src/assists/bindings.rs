//! Assists on `let` bindings.
//!
//! **The one that needs an argument is inlining**, and the argument is that a
//! binding is not always only a name. `let n = count();` used twice is two
//! calls once it is inlined, and `count()` may talk to a database. So a
//! binding is inlined when it is used once — where the work happens exactly as
//! often either way — or when what it holds is a literal or another name,
//! which costs nothing to repeat.
//!
//! Everything else here is small, and small is the point: they are the edits
//! somebody would otherwise make by hand while thinking about something else,
//! which is when a `mut` gets left behind on a binding nothing writes to.

use khora_db::{Db, SourceFile};
use khora_hir::body::{Expr, Stmt};
use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::{TextRange, TextSize};

use super::{Assist, Edit, covering, text_of};

/// Every binding assist available at the cursor.
pub fn assists(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Vec<Assist> {
    let mut out = Vec::new();
    out.extend(inline(db, file, tree, text, selection));
    out.extend(drop_annotation(db, file, tree, text, selection));
    out.extend(unprefix(tree, text, selection));
    out.extend(drop_mut(db, file, tree, selection));
    out.extend(extract_every(tree, text, selection));
    out.extend(add_mut(tree, text, selection));
    out
}

/// The parts of a `LET_DECL` the assists here read.
struct Binding {
    /// The whole declaration, including its semicolon.
    whole: SyntaxNode,
    /// The pattern, which is the name for the simple case.
    pattern: SyntaxNode,
    /// The written type, when there is one.
    annotation: Option<SyntaxNode>,
    /// What it is bound to.
    initializer: SyntaxNode,
    /// Whether it was written `let mut`.
    is_mut: bool,
}

/// Reads a `LET_DECL` into its parts.
///
/// By position, the way the grammar writes it: a pattern, then optionally a
/// type, then the initializer. The type is told from the initializer by
/// kind — every type node's name ends in `_TYPE`, and no expression's does.
fn binding(node: &SyntaxNode) -> Option<Binding> {
    let mut children = node.children();
    let pattern = children.next()?;
    let next = children.next()?;
    let (annotation, initializer) =
        if is_type(next.kind()) { (Some(next), children.next()?) } else { (None, next) };
    let is_mut = node.children_with_tokens().any(|e| e.kind() == SyntaxKind::MUT_KW);
    Some(Binding { whole: node.clone(), pattern, annotation, initializer, is_mut })
}

fn is_type(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PATH_TYPE
            | SyntaxKind::TUPLE_TYPE
            | SyntaxKind::UNIT_TYPE
            | SyntaxKind::PAREN_TYPE
            | SyntaxKind::FN_TYPE
            | SyntaxKind::RECORD_TYPE
            | SyntaxKind::FORALL_TYPE
            | SyntaxKind::LITERAL_TYPE
            | SyntaxKind::UNION_TYPE
            | SyntaxKind::VARIANT_TYPE
    )
}

/// **A binding replaced by what it holds, and the `let` line removed.**
///
/// Offered when the binding is used once, or holds a literal or a name. Two
/// uses of `let n = count();` become two calls, and whether that is the same
/// program depends on what `count` does — which an editor cannot know and
/// should not assume.
///
/// Refused for `mut`, because a binding that is written to is not its
/// initializer.
fn inline(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LET_DECL)?;
    let parts = binding(&node)?;
    if parts.is_mut {
        return None;
    }
    // A pattern that takes something apart binds several names, and replacing
    // one of them means rebuilding the value.
    if parts.pattern.kind() != SyntaxKind::IDENT_PAT {
        return None;
    }
    let name = text_of(text, &parts.pattern);
    let held = text_of(text, &parts.initializer);

    let uses = uses_of(db, file, &name, parts.whole.text_range());
    if uses.is_empty() {
        return None;
    }
    let cheap = matches!(
        parts.initializer.kind(),
        SyntaxKind::LITERAL_EXPR | SyntaxKind::PATH_EXPR | SyntaxKind::UNIT_EXPR
    );
    if uses.len() > 1 && !cheap {
        return None;
    }

    // Parenthesised unless it is already one thing: `let n = a + b` inlined
    // into `n * 2` is `(a + b) * 2`, and without the brackets it is not.
    let written = if cheap || parts.initializer.kind() == SyntaxKind::PAREN_EXPR {
        held
    } else {
        format!("({held})")
    };

    let mut edits: Vec<Edit> =
        uses.iter().map(|at| Edit { range: *at, replacement: written.clone() }).collect();
    edits.push(Edit { range: with_line(text, &parts.whole), replacement: String::new() });
    Some(Assist { title: format!("Inline `{name}`"), kind: "refactor.inline", edits })
}

/// **A written type the checker would have inferred anyway, removed.**
///
/// The opposite of `annotate`, and offered only where the two agree: an
/// annotation that *narrows* — a literal written as one type and used as
/// another — is load-bearing, and removing it would change the program or
/// stop it compiling.
fn drop_annotation(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LET_DECL)?;
    let parts = binding(&node)?;
    let annotation = parts.annotation?;
    let written = text_of(text, &annotation);

    let name = text_of(text, &parts.pattern);
    let inferred = inferred_type(db, file, &name, parts.whole.text_range())?;
    if inferred != written.trim() {
        return None;
    }

    // From the colon to the end of the type, so the space in front goes too.
    let from = text[..usize::from(annotation.text_range().start())].rfind(':')?;
    Some(Assist {
        title: "Remove the type, which is inferred".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::new((from as u32).into(), annotation.text_range().end()),
            replacement: String::new(),
        }],
    })
}

/// **`_name` back to `name`, for a binding that turned out to be used.**
///
/// The underscore is how a Khora program says *deliberately unused*, and
/// `fixes.rs` offers to add it. This is the way back, for when the code grew
/// a use and the name is now telling the reader something untrue.
fn unprefix(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::IDENT_PAT)?;
    let name = text_of(text, &node);
    let bare = name.strip_prefix('_')?;
    if bare.is_empty() {
        return None;
    }
    Some(Assist {
        title: format!("Rename to `{bare}`, which says it is used"),
        kind: "refactor.rewrite",
        edits: vec![Edit { range: node.text_range(), replacement: bare.to_string() }],
    })
}

/// **`mut` removed from a binding nothing writes to.**
///
/// A `mut` that is not needed is a claim about the code that is not true, and
/// the next reader has to check. Offered only when nothing assigns to it.
fn drop_mut(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    selection: TextRange,
) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LET_DECL)?;
    let keyword = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::MUT_KW)?;

    // The HIR knows which binding this is and whether anything assigns to it,
    // which reading the text cannot answer: a name may be shadowed.
    if assigned_in_file(db, file, node.text_range()) {
        return None;
    }
    Some(Assist {
        title: "Remove `mut`, which nothing uses".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            // The keyword and the space after it.
            range: TextRange::new(
                keyword.text_range().start(),
                keyword.text_range().end() + TextSize::from(1),
            ),
            replacement: String::new(),
        }],
    })
}

/// Where a binding declared inside `declaration` is mentioned, other than at
/// its own declaration.
fn uses_of(db: &dyn Db, file: SourceFile, name: &str, declaration: TextRange) -> Vec<TextRange> {
    let mut out = Vec::new();
    for (_, body) in khora_hir::body::bodies(db, file) {
        let Some(local) = body
            .locals()
            .find(|(_, l)| l.name == name && declaration.contains_range(l.range))
            .map(|(id, _)| id)
        else {
            continue;
        };
        for (id, expr) in body.exprs() {
            if matches!(expr, Expr::Local(l) if *l == local) {
                out.push(body.range(id));
            }
        }
    }
    out
}

/// The checker's type for the binding declared in `declaration`.
fn inferred_type(
    db: &dyn Db,
    file: SourceFile,
    name: &str,
    declaration: TextRange,
) -> Option<String> {
    let checked = khora_types::checked(db, file);
    for (owner, body) in khora_hir::body::bodies(db, file) {
        let Some(local) = body
            .locals()
            .find(|(_, l)| l.name == name && declaration.contains_range(l.range))
            .map(|(id, _)| id)
        else {
            continue;
        };
        let types = checked.bodies.iter().find(|(n, _)| n == owner).map(|(_, t)| t)?;
        let written = types.local(local).to_string();
        if written.contains('?') || written == "?" {
            return None;
        }
        return Some(written);
    }
    None
}

/// Whether anything assigns to the binding declared in `declaration`.
fn assigned_in_file(db: &dyn Db, file: SourceFile, declaration: TextRange) -> bool {
    for (_, body) in khora_hir::body::bodies(db, file) {
        let locals: Vec<_> = body
            .locals()
            .filter(|(_, l)| declaration.contains_range(l.range))
            .map(|(id, _)| id)
            .collect();
        if locals.is_empty() {
            continue;
        }
        for (_, expr) in body.exprs() {
            let Expr::Assign { target, .. } = expr else { continue };
            if matches!(body.expr(*target), Expr::Local(l) if locals.contains(l)) {
                return true;
            }
            // `for` and `while` bodies reach the same locals; the walk above
            // covers every expression in the body, so nothing is missed.
        }
        // A `let mut` rebound by a statement is an assignment too.
        for (_, expr) in body.exprs() {
            let Expr::Block { stmts, .. } = expr else { continue };
            for stmt in stmts {
                if let Stmt::Expr(id) = stmt {
                    if let Expr::Assign { target, .. } = body.expr(*id) {
                        if matches!(body.expr(*target), Expr::Local(l) if locals.contains(l)) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// A statement's range including the whole line it is on, so removing it does
/// not leave an empty line behind.
fn with_line(text: &str, node: &SyntaxNode) -> TextRange {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let indent_only = text[line..start].chars().all(|c| c == ' ' || c == '\t');
    let from = if indent_only { line } else { start };
    let end = usize::from(node.text_range().end());
    let to = text[end..].find('\n').map_or(text.len(), |at| end + at + 1);
    TextRange::new((from as u32).into(), (to as u32).into())
}

/// **`mut` added to a binding that has none.**
///
/// The other direction from `drop_mut`, and unconditional where that one is
/// careful: adding `mut` to a binding nothing writes to costs a warning, and
/// leaving it off one that is written to costs a compile error somebody has to
/// read. The cheap mistake is the one to make easy to fix.
fn add_mut(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    let node = covering(tree, selection, SyntaxKind::LET_DECL)?;
    let parts = binding(&node)?;
    if parts.is_mut {
        return None;
    }
    let keyword = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::LET_KW)?;
    let _ = text;
    Some(Assist {
        title: "Make it `mut`".to_string(),
        kind: "refactor.rewrite",
        edits: vec![Edit {
            range: TextRange::empty(keyword.text_range().end()),
            replacement: " mut".to_string(),
        }],
    })
}

/// **A repeated expression bound once, and every copy replaced.**
///
/// The extraction in `assists.rs` replaces the selection. This finds the other
/// places the same expression is written and replaces those too, which is what
/// somebody means by "extract this": a second copy left behind is the bug the
/// refactoring was supposed to prevent.
///
/// Text equality rather than structural, deliberately. Two expressions that
/// are spelled the same are the same expression to a reader, and one that is
/// spelled differently is a decision this should not make for them.
fn extract_every(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    if selection.is_empty() {
        return None;
    }
    let node = tree.descendants().find(|n| n.text_range() == selection)?;
    if !matches!(
        node.kind(),
        SyntaxKind::CALL_EXPR | SyntaxKind::FIELD_EXPR | SyntaxKind::BIN_EXPR | SyntaxKind::PATH_EXPR
    ) {
        return None;
    }
    let written = text_of(text, &node);

    // Every other node with the same text, inside the same function.
    let owner = node.ancestors().find(|a| a.kind() == SyntaxKind::FN_DECL)?;
    let same: Vec<SyntaxNode> = owner
        .descendants()
        .filter(|n| n.kind() == node.kind())
        .filter(|n| text_of(text, n) == written)
        .collect();
    if same.len() < 2 {
        return None;
    }

    // The statement to put the binding above: the first occurrence's.
    let first = same.first()?;
    let statement = statement_of(first)?;
    let indent = indent_of(text, &statement);

    let mut edits: Vec<Edit> = same
        .iter()
        .map(|at| Edit { range: at.text_range(), replacement: "extracted".to_string() })
        .collect();
    edits.push(Edit {
        range: TextRange::empty(statement.text_range().start()),
        replacement: format!("let extracted = {written};\n{indent}"),
    });
    Some(Assist {
        title: format!("Extract all {} occurrences into a `let`", same.len()),
        kind: "refactor.extract",
        edits,
    })
}

/// The statement a node sits in.
fn statement_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    let mut here = node.clone();
    loop {
        let parent = here.parent()?;
        if parent.kind() == SyntaxKind::BLOCK {
            return Some(here);
        }
        here = parent;
    }
}

/// The whitespace in front of the line a node starts on.
fn indent_of(text: &str, node: &SyntaxNode) -> String {
    let start = usize::from(node.text_range().start());
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    text[line..start].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}
