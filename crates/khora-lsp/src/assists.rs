//! Assists: edits offered for where the cursor is, rather than for a problem.
//!
//! `fixes.rs` answers diagnostics, and holds a deliberately hard rule — only
//! where the message names one edit and there is nothing to choose — because a
//! quick fix is applied by somebody who read four words of a message they did
//! not go looking for.
//!
//! **An assist is asked for.** Somebody put the cursor on a binding, or
//! selected an expression, and pressed the key. The edit is the answer to a
//! question they posed, so the bar is different: it may restructure code, and
//! it may be one of several offered. What does not change is that it must not
//! alter what the program does.
//!
//! # Why extraction is fussy about where it will work
//!
//! Hoisting an expression out to a `let` runs it earlier, and there are places
//! in Khora where that is a different program: the far side of `&&`, a branch
//! of an `if`, the body of a lambda, an arm of a `match`. Every one of those is
//! code that may not run at all, and moving it above the statement makes it
//! always run.
//!
//! rust-analyzer offers the extraction there regardless and lets the reader
//! notice. This refuses, because the whole point of a language with typed
//! failures and explicit capabilities is that the interesting part of a program
//! is *when things happen*, and an editor that quietly reorders it is arguing
//! with the language. So the walk from the selection to its statement has to
//! pass through nothing conditional; where it does, no action is offered.
//!
//! # Extracting a *function* is a different question, and a looser one
//!
//! All of that is about hoisting. `let extracted = ..` above the statement runs
//! the expression earlier; a call left where the expression was runs it at
//! exactly the same moment. So [`extract_function`] is offered inside an `if`
//! branch, a match arm and a lambda, where the `let` is refused — the refusal
//! above is about evaluation order, and a call does not change it.
//!
//! What it needs instead is a *signature*, and that is where this can do
//! something rust-analyzer cannot. Extracting a Rust function means guessing at
//! borrows. Extracting a Khora function means writing down a capability row and
//! a failure row, and the checker has already computed both: `BodyTypes` records
//! what every call site demanded, so the new function's `with` and `raises`
//! clauses are read off rather than inferred a second time by an editor.

mod bindings;
mod decls;
mod effects;
mod flow;
mod imports;
mod matching;

use std::collections::BTreeMap;

use khora_db::{Db, SourceFile};
use khora_hir::body::{Body, Expr, ExprId};
use khora_syntax::{SyntaxKind, SyntaxNode};
use khora_types::{BodyTypes, Type};
use text_size::{TextRange, TextSize};

/// One edit within an assist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// What to replace; empty for an insertion.
    pub range: TextRange,
    /// What to put there.
    pub replacement: String,
}

/// One offered refactoring, which may take more than one edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assist {
    /// What the action is called in the lightbulb menu.
    pub title: String,
    /// The LSP `CodeActionKind`, which is how an editor groups and filters it.
    pub kind: &'static str,
    /// The edits, which an editor applies as one.
    pub edits: Vec<Edit>,
}

/// Every kind an assist here is ever offered under.
///
/// A client may ask for one kind and not another, and computing the assists is
/// the cost -- so the caller checks this list before doing the work, and the
/// kind on each assist again before offering it.
pub const KINDS: [&str; 2] = ["refactor.rewrite", "refactor.extract"];

/// Every assist available where the cursor is.
pub fn at(db: &dyn Db, file: SourceFile, selection: TextRange) -> Vec<Assist> {
    let text = file.text(db);
    let tree = khora_db::parse(db, file).syntax();
    let mut out = Vec::new();
    out.extend(annotate(db, file, selection));
    out.extend(extract(&tree, text, selection));
    out.extend(extract_function(db, file, &tree, text, selection));
    out.extend(flow::assists(&tree, text, selection));
    out.extend(bindings::assists(db, file, &tree, text, selection));
    out.extend(effects::assists(&tree, text, selection));
    out.extend(matching::assists(&tree, text, selection));
    out.extend(imports::assists(&tree, text, selection));
    out.extend(decls::assists(&tree, text, selection));
    out
}

/// The innermost node of `kind` whose range covers the selection.
///
/// Covers rather than equals: an assist offered where the cursor is has to
/// find the `if` the cursor is *in*, and a person putting a cursor on a
/// keyword has not selected the expression it belongs to.
pub(crate) fn covering(
    tree: &SyntaxNode,
    selection: TextRange,
    kind: SyntaxKind,
) -> Option<SyntaxNode> {
    tree.descendants()
        .filter(|node| node.kind() == kind)
        .filter(|node| node.text_range().contains_range(selection))
        .min_by_key(|node| node.text_range().len())
}

/// The source text a node covers.
pub(crate) fn text_of(text: &str, node: &SyntaxNode) -> String {
    text[usize::from(node.text_range().start())..usize::from(node.text_range().end())].to_string()
}

/// **`let rows = query(db, sql)!` gets its `: List<Row>` written in.**
///
/// The inlay hint already shows this type, and a hint is not text: it cannot be
/// copied, it disappears when the setting is off, and it is gone from the diff
/// a reviewer reads. Writing it down is the difference between the compiler
/// knowing a thing and the file saying it.
///
/// Offered for a `let` the cursor is on and only where the source does not
/// already give the type, which is the same question the hint answers -- so an
/// annotated binding is silent here, rather than offering to rewrite what
/// somebody wrote.
fn annotate(db: &dyn Db, file: SourceFile, selection: TextRange) -> Option<Assist> {
    let checked = khora_types::checked(db, file);
    for (name, body) in khora_hir::body::bodies(db, file) {
        let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            continue;
        };
        for (_, expr) in body.exprs() {
            let khora_hir::body::Expr::Block { stmts, .. } = expr else { continue };
            for stmt in stmts {
                let khora_hir::body::Stmt::Let { pat, ty, init: _ } = stmt else { continue };
                // An annotation already written is the author saying it.
                if ty.is_some() {
                    continue;
                }
                let khora_hir::body::Pat::Bind(local) = body.pat(*pat) else { continue };
                let binding = body.local(*local);
                // The cursor has to be on the name, which is where the
                // annotation goes and what makes the action unambiguous when
                // several bindings share a line.
                if !covers(binding.range, selection.start()) {
                    continue;
                }
                let shown = types.local(*local);
                if matches!(shown, khora_types::Type::Unknown) {
                    continue;
                }
                let written = shown.to_string();
                // A type the checker has not finished with prints with a
                // variable in it, and writing that down would not compile.
                if written.contains('?') {
                    continue;
                }
                return Some(Assist {
                    title: format!("Write the inferred type, `{written}`"),
                    kind: "refactor.rewrite",
                    edits: vec![Edit {
                        range: TextRange::empty(binding.range.end()),
                        replacement: format!(": {written}"),
                    }],
                });
            }
        }
    }
    None
}

/// **A selected expression, lifted into a `let` above the statement it was in.**
///
/// Two edits: the binding goes in on its own line with the statement's own
/// indentation, and the selection becomes the name. Offered only for a
/// selection that lands exactly on one expression -- a selection that stops
/// halfway through one would produce a `let` of a fragment, which is the kind
/// of edit that is worse than none.
fn extract(tree: &SyntaxNode, text: &str, selection: TextRange) -> Option<Assist> {
    if selection.is_empty() {
        return None;
    }
    let node = expression_exactly_at(tree, selection)?;
    let statement = statement_holding(&node)?;

    // The indentation of the line the statement starts on, so the new binding
    // lines up with it rather than with column zero.
    let start = usize::from(statement.text_range().start());
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let indent = &text[line_start..start];
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        // Something else is on the line in front of the statement, so there is
        // no line of its own to copy and no obvious place to put the binding.
        return None;
    }

    let extracted = text.get(usize::from(selection.start())..usize::from(selection.end()))?;
    Some(Assist {
        title: "Extract into a `let`".to_string(),
        kind: "refactor.extract",
        edits: vec![
            Edit {
                range: TextRange::empty(statement.text_range().start()),
                replacement: format!("let extracted = {extracted};\n{indent}"),
            },
            Edit { range: selection, replacement: "extracted".to_string() },
        ],
    })
}

/// **A selected expression, lifted into a function of its own.**
///
/// The signature is the whole of the difficulty and the whole of the value.
/// Parameters are the locals the selection uses and does not bind; the return
/// type is the expression's own; and the `with` and `raises` clauses are the
/// union of what the calls inside it demanded, which the checker recorded at
/// each call site while it was doing the work anyway.
///
/// # What it refuses, and why each one
///
/// **A selection containing a `with` or a `catch`.** Both *discharge* a row:
/// `call_rows` records what a call demanded before anything answered it, so
/// the union over a selection containing a handler over-states what escapes,
/// and the new function would declare a capability it supplies itself or a
/// failure it catches. Getting that right means redoing the subtraction the
/// checker already did, in an editor, on a fragment — so it is refused
/// instead, and the refusal is exact rather than a guess.
///
/// **A selection that assigns to a local it did not bind.** Parameters are
/// values, so the write would land on a copy and the original would silently
/// stop changing. That is the extraction that compiles and is wrong.
///
/// This is reachable only for a block, and the language covers the other way
/// in: assigning to a captured local inside a closure is already a compile
/// error, and for that reason -- *captured by value, so the assignment would
/// not be visible outside*. The check earns its place on `{ n = n + 1; n }`.
///
/// **Anything whose type the checker could not finish.** A signature is text,
/// and a type printed with a variable in it does not compile.
fn extract_function(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    selection: TextRange,
) -> Option<Assist> {
    if selection.is_empty() {
        return None;
    }
    // The same "exactly one expression" rule as the `let`: a selection that
    // stops half way through one has no signature to write.
    //
    // **A block counts here and does not for the `let`.** `is_expression`
    // leaves `BLOCK_EXPR` out because a block hoisted into a binding is a
    // binding holding a block, which is the same code moved sideways. Lifted
    // into a function it is the opposite: a run of statements given a name and
    // a signature is most of what anybody reaches for this for.
    let node = expression_exactly_at(tree, selection)
        .or_else(|| block_exactly_at(tree, selection))?;

    // A handler inside the selection discharges rows, and the union below
    // cannot see that. See the doc comment.
    if node.descendants().any(|d| discharges(d.kind())) {
        return None;
    }

    let owner = enclosing_function(&node)?;
    let checked = khora_types::checked(db, file);

    // The body the selection is in, found by an expression whose range is the
    // selection exactly. Walked here rather than in a helper because `bodies`
    // hands out borrows of a value that lives for this call.
    let mut found = None;
    for (name, body) in khora_hir::body::bodies(db, file) {
        // **The largest expression inside the selection, not one whose range
        // equals it.** `(b + 1)` is a `PAREN_EXPR` in the syntax tree and is
        // just `b + 1` in the HIR -- parentheses are grouping and the HIR has
        // structure instead -- so an exact-range lookup finds nothing for
        // every parenthesised selection, which is most of the ones worth
        // extracting. Safe because the syntax side has already insisted the
        // selection is exactly one expression node.
        let Some(root) = body
            .exprs()
            .map(|(id, _)| id)
            .filter(|id| selection.contains_range(body.range(*id)))
            .max_by_key(|id| (body.range(*id).len(), std::cmp::Reverse(body.range(*id).start())))
        else {
            continue;
        };
        let Some(types) = checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            continue;
        };
        let inside: Vec<ExprId> = body
            .exprs()
            .map(|(id, _)| id)
            .filter(|id| selection.contains_range(body.range(*id)))
            .collect();
        let answer = writable(types.of(root))?;
        let (parameters, arguments) = free_locals(body, types, &inside, selection)?;
        let (requires, raises) = rows_of(types, &inside);
        found = Some((answer, parameters, arguments, requires, raises));
        break;
    }
    let (answer, parameters, arguments, requires, raises) = found?;

    // Where the new function goes: on its own, above the one it came out of.
    let at = owner.text_range().start();
    let line_start = text[..usize::from(at)].rfind('\n').map_or(0, |n| n + 1);
    let indent = &text[line_start..usize::from(at)];
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }

    let extracted = text.get(usize::from(selection.start())..usize::from(selection.end()))?;
    let mut signature = format!("fn extracted({parameters}) -> {answer}");
    if let Some(requires) = &requires {
        signature.push_str(&format!(" with {requires}"));
    }
    if let Some(raises) = &raises {
        signature.push_str(&format!(" raises {raises}"));
    }

    // A function that can fail is called with a `!`, because the failure has
    // to go somewhere and the enclosing function is where it was going before.
    let bang = if raises.is_some() { "!" } else { "" };

    Some(Assist {
        title: "Extract into a function".to_string(),
        kind: "refactor.extract",
        edits: vec![
            Edit {
                range: TextRange::empty(at),
                replacement: format!(
                    "{signature} {{\n{indent}  {extracted}\n{indent}}}\n\n{indent}"
                ),
            },
            Edit { range: selection, replacement: format!("extracted({arguments}){bang}") },
        ],
    })
}

/// The locals a selection uses without binding, as a parameter list and the
/// arguments to pass at the call.
///
/// `None` when one of them cannot be written down: a type the checker did not
/// finish, or a local the selection assigns to. Both are refusals rather than
/// best guesses -- see [`extract_function`].
fn free_locals(
    body: &Body,
    types: &BodyTypes,
    inside: &[ExprId],
    selection: TextRange,
) -> Option<(String, String)> {
    // Sorted by where the binding was written, so the parameter list reads in
    // the order a reader met the names rather than in hash order.
    let mut free: BTreeMap<u32, (String, String)> = BTreeMap::new();

    for id in inside {
        match body.expr(*id) {
            Expr::Local(local) => {
                let binding = body.local(*local);
                // Bound inside the selection, so it travels with it.
                if selection.contains_range(binding.range) {
                    continue;
                }
                let written = writable(types.local(*local))?;
                free.insert(binding.range.start().into(), (binding.name.clone(), written));
            }
            Expr::Assign { target, .. } => {
                // A write to something the selection did not bind cannot
                // survive becoming a parameter.
                if let Expr::Local(local) = body.expr(*target) {
                    if !selection.contains_range(body.local(*local).range) {
                        return None;
                    }
                }
            }
            _ => {}
        }
    }

    let parameters =
        free.values().map(|(name, ty)| format!("{name}: {ty}")).collect::<Vec<_>>().join(", ");
    let arguments = free.values().map(|(name, _)| name.clone()).collect::<Vec<_>>().join(", ");
    Some((parameters, arguments))
}

/// The `with` and `raises` clauses for everything the selection calls.
///
/// `declared` rather than `requires`: `requires` is what a call still had to
/// answer *after* the enclosing scope supplied what it could, and the new
/// function has no enclosing scope. What it must declare is what the callees
/// ask for, which is `declared`.
fn rows_of(types: &BodyTypes, inside: &[ExprId]) -> (Option<String>, Option<String>) {
    let mut requires: BTreeMap<String, String> = BTreeMap::new();
    let mut raises: BTreeMap<String, String> = BTreeMap::new();

    for id in inside {
        let Some(rows) = types.call_rows(*id) else { continue };
        collect(rows.declared.as_ref(), &mut requires);
        collect(rows.raises.as_ref(), &mut raises);
    }

    (written_row(&requires), written_row(&raises))
}

/// Adds a row's entries to `into`, keyed by label so a capability demanded by
/// three calls is written once.
fn collect(row: Option<&Type>, into: &mut BTreeMap<String, String>) {
    let Some(Type::Row { fields, .. }) = row else { return };
    for (label, ty) in fields {
        into.insert(label.clone(), ty.to_string());
    }
}

/// A row as it is written in a signature, or `None` when it is empty.
fn written_row(entries: &BTreeMap<String, String>) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let written: Vec<String> = entries
        .iter()
        // An error is labelled by its own type name, so printing both would
        // say the same word twice. `hints.rs` renders a row the same way.
        .map(|(label, ty)| if label == ty { ty.clone() } else { format!("{label}: {ty}") })
        .collect();
    Some(if written.len() == 1 && !written[0].contains(':') {
        written[0].clone()
    } else {
        format!("{{ {} }}", written.join(", "))
    })
}

/// A type as it would be written in a signature, or `None` if it cannot be.
fn writable(ty: &Type) -> Option<String> {
    if matches!(ty, Type::Unknown) {
        return None;
    }
    let written = ty.to_string();
    // A type the checker has not finished with prints with a variable in it.
    if written.contains('?') {
        return None;
    }
    Some(written)
}

/// The `fn` declaration the node is inside.
fn enclosing_function(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors().find(|a| a.kind() == SyntaxKind::FN_DECL)
}

/// Kinds that answer a row rather than passing it on.
///
/// A `with` block supplies capabilities and a `catch` answers failures, so a
/// selection containing one demands less than the calls inside it asked for --
/// and the union in [`rows_of`] cannot tell.
fn discharges(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::WITH_EXPR | SyntaxKind::CATCH_EXPR | SyntaxKind::HANDLER_EXPR)
}

/// The block whose range is exactly the selection.
///
/// Separate from [`expression_exactly_at`] because the two extractions differ
/// on exactly this kind: see [`extract_function`].
fn block_exactly_at(tree: &SyntaxNode, selection: TextRange) -> Option<SyntaxNode> {
    tree.descendants()
        .filter(|node| node.text_range() == selection)
        .find(|node| matches!(node.kind(), SyntaxKind::BLOCK_EXPR | SyntaxKind::BLOCK))
}

/// The expression node whose range is exactly the selection.
///
/// Exactly, rather than the smallest one containing it: an editor sends the
/// selection a person made, and one that covers `a + b` in `a + b * c` is not
/// a subexpression at all. Answering it with `b * c` would extract something
/// they did not select.
fn expression_exactly_at(tree: &SyntaxNode, selection: TextRange) -> Option<SyntaxNode> {
    tree.descendants()
        .filter(|node| node.text_range() == selection)
        .find(|node| is_expression(node.kind()))
}

/// Whether a node is an expression that may stand on its own as a value.
///
/// `BLOCK_EXPR` is left out: a block extracted into a `let` is a `let` holding
/// a block, which is the same code moved sideways.
fn is_expression(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LITERAL_EXPR
            | SyntaxKind::PATH_EXPR
            | SyntaxKind::RECORD_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::LIST_EXPR
            | SyntaxKind::UNIT_EXPR
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::CALL_EXPR
            | SyntaxKind::FIELD_EXPR
            | SyntaxKind::PIPE_EXPR
            | SyntaxKind::BIN_EXPR
            | SyntaxKind::PREFIX_EXPR
            | SyntaxKind::TRY_EXPR
    )
}

/// The statement the expression sits in, if getting there crosses nothing that
/// decides whether the expression runs.
///
/// See the module docs: the refusal is the feature. Walking up stops at the
/// first node whose parent is a block, and returns nothing if anything on the
/// way could have skipped the expression.
fn statement_holding(node: &SyntaxNode) -> Option<SyntaxNode> {
    let mut here = node.clone();
    loop {
        let parent = here.parent()?;
        if conditional(parent.kind()) {
            return None;
        }
        // `a && expensive()` only runs the right side sometimes, and a `&&`
        // reads as one expression rather than as a branch -- which is exactly
        // why it is worth refusing rather than trusting a reader to notice.
        if parent.kind() == SyntaxKind::BIN_EXPR
            && short_circuits(&parent)
            && !first_in(&parent, &here)
        {
            return None;
        }
        if parent.kind() == SyntaxKind::BLOCK {
            // A statement is the thing a block holds, and the expression that
            // *is* the block's tail is one of them.
            return Some(here);
        }
        here = parent;
    }
}

/// Kinds that decide when, whether, or in what scope what is inside them runs.
///
/// Three reasons, and all three end in the same refusal. An `if`, a `match`,
/// a loop, a lambda body and a flow stage may not run their contents at all.
/// A `with` block puts a capability in scope, so an expression lifted out of
/// one no longer has what it asked for. A `catch` decides which failures are
/// answered, so an expression lifted out of one raises past the handler that
/// was written for it.
fn conditional(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::IF_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::MATCH_ARM
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::LOOP_EXPR
            | SyntaxKind::LAMBDA_EXPR
            | SyntaxKind::HANDLER_EXPR
            | SyntaxKind::CATCH_EXPR
            | SyntaxKind::WITH_EXPR
            | SyntaxKind::FLOW_EXPR
    )
}

/// Whether a `BIN_EXPR` is one of the two operators that may not evaluate its
/// right side.
fn short_circuits(node: &SyntaxNode) -> bool {
    node.children_with_tokens().filter_map(|e| e.into_token()).any(|t| {
        matches!(t.kind(), SyntaxKind::AMP_AMP | SyntaxKind::PIPE_PIPE)
    })
}

/// Whether `child` is the first of `parent`'s child nodes, which for a binary
/// expression means the operand that always runs.
fn first_in(parent: &SyntaxNode, child: &SyntaxNode) -> bool {
    parent.children().next().is_some_and(|first| &first == child)
}

/// Whether `at` is inside `range`, counting its end.
///
/// Counting the end, because a cursor placed just past a name is on it as far
/// as the person holding the keyboard is concerned.
fn covers(range: TextRange, at: TextSize) -> bool {
    range.start() <= at && at <= range.end()
}
