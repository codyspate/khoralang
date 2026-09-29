//! The `idiomatic` group's lints: a second way of writing something Khora
//! has a first way of writing.
//!
//! None of these is a mistake. Each fires on code that is correct and that
//! says the same thing as a shorter, canonical form, so each is `allow` until
//! a project switches the group on (`[lints.idiomatic]`), and runs at the
//! level `std/lints/idiomatic.toml` gives it after that.
//!
//! # A fix is applied by somebody who did not read it
//!
//! Each lint's finding carries a [`Fix`] where the rewrite is certain, and
//! `khora check --fix` and the language server's "Apply idiomatic fixes"
//! apply it without asking. So **each lint's doc comment gives the case
//! analysis for why the rewritten program is the same program**, and the
//! cases a plausible rewrite gets wrong are exclusions, tested one by one in
//! `tests/idiomatic.rs`. Where a case could not be argued, the finding is not
//! made, or is made with no fix: `module-path` outside an entry file, and
//! `needless-return` where the statement before it would become the value.
//!
//! What this costs: the lints are narrower than their names. A `+` chain that
//! spans lines, a lambda parameter with a comment in its brackets, `0.0 - x`,
//! all go unreported, because the rewrite for each would change something.

use std::path::Path;

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, Literal};
use khora_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use khora_types::{BodyTypes, Type};
use text_size::TextRange;

use crate::Finding;

/// `"a " + x + "!"`, where `"a ${x}!"` says it.
pub const CONCATENATED_STRING: &str = "concatenated-string";
/// `return e;` as a function's last statement, where the tail `e` says it.
pub const NEEDLESS_RETURN: &str = "needless-return";
/// `0 - 1`, where `-1` says it.
pub const SUBTRACTION_FROM_ZERO: &str = "subtraction-from-zero";
/// `fn (x) =>`, where `fn x =>` says it.
pub const PARENTHESIZED_PARAMETER: &str = "parenthesized-parameter";
/// `b == true` and `b == false`, where `b` and `!b` say them.
pub const BOOL_COMPARISON: &str = "bool-comparison";
/// `module main;` in a package, where `module <package>::main;` says it.
pub const MODULE_PATH: &str = "module-path";

/// Every lint in the `idiomatic` group, for [`crate::LINTS`], for the
/// group-off default in [`crate::default_level`], and for the check that
/// `std/lints/idiomatic.toml` holds exactly these.
///
/// **One list, so the four cannot drift apart.** `unlabeled-flag` and
/// `method-call` live in modules of their own, because each reads types and
/// resolves the callee; they are here all the same, since what makes a lint a
/// member is the group, not the code that finds it. A member added to the toml
/// and not here fails
/// `the_idiomatic_group_holds_its_lints_at_warn`.
pub const ALL: &[&str] = &[
    BOOL_COMPARISON,
    CONCATENATED_STRING,
    crate::method_call::METHOD_CALL,
    MODULE_PATH,
    NEEDLESS_RETURN,
    PARENTHESIZED_PARAMETER,
    SUBTRACTION_FROM_ZERO,
    crate::unlabeled_flag::UNLABELED_FLAG,
];

/// One replacement of a range of the file's text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edit {
    /// The text to replace, as offsets into the file the finding came from.
    pub range: TextRange,
    /// What to put there.
    pub replacement: String,
}

/// The edits that rewrite a finding into the canonical form.
///
/// Offered only where the message names one edit and there is nothing to
/// choose, which is the language server's bar for a quick fix too. A fix may
/// carry more than one edit: `method-call` rewrites the call and may add the
/// import its owner needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fix {
    /// Non-overlapping, in any order.
    pub edits: Vec<Edit>,
}

impl Fix {
    fn one(range: TextRange, replacement: impl Into<String>) -> Fix {
        Fix { edits: vec![Edit { range, replacement: replacement.into() }] }
    }

    /// The smallest range covering every edit.
    pub fn span(&self) -> Option<TextRange> {
        self.edits.iter().map(|e| e.range).reduce(|a, b| a.cover(b))
    }
}

/// `text` with `fixes` applied, and how many were taken.
///
/// **A fix that overlaps one already taken is skipped, not merged.** Two
/// fixes on nested ranges -- `("a" + x == "a!") == true` holds a string fix
/// inside a comparison fix -- were each computed against the original text,
/// so splicing both would write one answer into the other's stale copy. The
/// caller runs the lints again on the result and takes the skipped one then;
/// `khora check --fix` repeats until nothing is left.
pub fn apply(text: &str, fixes: &[&Fix]) -> (String, usize) {
    let taken = select(fixes);
    let edits = distinct_edits(&taken);
    let mut out = text.to_string();
    for edit in edits {
        out.replace_range(std::ops::Range::<usize>::from(edit.range), &edit.replacement);
    }
    (out, taken.len())
}

/// The fixes [`apply`] takes from `fixes` in one pass: no edit of one
/// overlapping an edit of another, earliest first. The language server sends
/// these as one edit, which the protocol requires to be free of overlaps.
///
/// **Edits are compared, not the ranges they span.** A `method-call` fix that
/// adds an import spans from the imports to the call, so comparing spans let
/// one such fix into a pass per file, and a file with thirty calls needed
/// thirty passes where `khora check --fix` stops at eight. Two fixes making
/// the *same* edit -- two calls needing one import -- agree rather than
/// clash, and [`distinct_edits`] makes it once.
pub fn select<'a>(fixes: &[&'a Fix]) -> Vec<&'a Fix> {
    let mut taken: Vec<&Fix> = Vec::new();
    let mut sorted: Vec<&Fix> = fixes.to_vec();
    sorted.sort_by_key(|fix| fix.span().map(|r| (r.start(), r.end())));
    for fix in sorted {
        if fix.edits.is_empty() {
            continue;
        }
        let clashes = taken.iter().flat_map(|other| other.edits.iter()).any(|o| {
            fix.edits.iter().any(|e| {
                e != o && o.range.intersect(e.range).is_some_and(|i| !i.is_empty() || o.range == e.range)
            })
        });
        if !clashes {
            taken.push(fix);
        }
    }
    taken
}

/// Every edit of `fixes`, an edit two of them share counted once, latest
/// first so each applies against text the ones before it did not move.
pub fn distinct_edits<'a>(fixes: &[&'a Fix]) -> Vec<&'a Edit> {
    let mut edits: Vec<&Edit> = Vec::new();
    for edit in fixes.iter().flat_map(|fix| fix.edits.iter()) {
        if !edits.contains(&edit) {
            edits.push(edit);
        }
    }
    edits.sort_by_key(|edit| std::cmp::Reverse((edit.range.start(), edit.range.end())));
    edits
}

/// The six lints of this module over one file. `unlabeled-flag` runs with
/// the per-body lints in [`crate::findings`].
pub(crate) fn findings(
    db: &dyn Db,
    file: SourceFile,
    typed: &[(&Body, &BodyTypes)],
    out: &mut Vec<Finding>,
) {
    let parse = khora_db::parse(db, file);
    // A tree with errors in it has holes, and an edit computed across a hole
    // is an edit to text nobody can see the shape of.
    if !parse.errors().is_empty() {
        return;
    }
    let text = file.text(db);
    let tree = parse.syntax();
    for node in tree.descendants() {
        match node.kind() {
            SyntaxKind::BIN_EXPR => {
                concatenated_string(&node, text, typed, out);
                bool_comparison(&node, text, out);
            }
            SyntaxKind::FN_DECL => needless_return(&node, text, out),
            SyntaxKind::LAMBDA_EXPR => parenthesized_parameter(&node, text, out),
            SyntaxKind::MODULE_DECL => module_path(db, file, &node, out),
            _ => {}
        }
    }
    for (body, types) in typed {
        subtraction_from_zero(body, types, text, out);
    }
    for finding in out.iter_mut().filter(|f| ALL.contains(&f.lint)) {
        if finding.fix.as_ref().is_some_and(|fix| fix.edits.iter().any(|e| joins_the_statement_before(&tree, e))) {
            finding.fix = None;
        }
    }
}

/// Whether `edit` would be read as continuing the statement before it.
///
/// **A block-like statement needs no `;`**, so `if c { 10 } else { 20 }`
/// followed by `0 - 1` on the next line is two statements. Rewritten to `-1`,
/// the `-` is read as a binary minus on the `if`'s value, and the function
/// returns 9 where it returned -1 -- with `khora check` green. `(c)` in the
/// same place is a call of the block's value. A fix whose text starts with a
/// token that can continue an expression, placed right after a `}`, is
/// withheld; the finding stays.
///
/// Here rather than in each lint, so a lint written later cannot forget it.
/// Bracketing is no answer: `(` continues an expression too. What it costs:
/// any such fix after a `}` is withheld, including the ones where the `}`
/// ends something that could not be continued; that is the quiet side.
fn joins_the_statement_before(tree: &SyntaxNode, edit: &Edit) -> bool {
    let lexed = khora_syntax::LexedStr::new(&edit.replacement);
    let first = (0..lexed.len()).map(|i| lexed.kind(i)).find(|k| !k.is_trivia());
    let Some(first) = first else { return false };
    if starts_a_statement_safely(first) {
        return false;
    }
    let mut before = tree.token_at_offset(edit.range.start()).left_biased();
    // `left_biased` gives the token ending at the offset, which is the one
    // before the edit unless the offset is inside a token.
    while let Some(token) = before.as_ref().filter(|t| t.kind().is_trivia() || t.text_range().end() > edit.range.start()) {
        before = token.prev_token();
    }
    before.is_some_and(|t| t.kind() == SyntaxKind::R_BRACE)
}

/// The first tokens that cannot continue the expression before them: a
/// literal, a name, and prefix `!` (which the parser does not read as postfix
/// after a block-like expression).
fn starts_a_statement_safely(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::IDENT
            | SyntaxKind::INT_LIT
            | SyntaxKind::FLOAT_LIT
            | SyntaxKind::STRING_LIT
            | SyntaxKind::CHAR_LIT
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
            | SyntaxKind::BANG
    )
}

fn finding(lint: &'static str, message: &str, range: TextRange, fix: Option<Fix>) -> Finding {
    Finding { lint, message: message.to_string(), range, fix }
}

fn slice(text: &str, range: TextRange) -> &str {
    &text[std::ops::Range::<usize>::from(range)]
}

/// Whether a comment sits anywhere inside `node`. An edit that rewrites the
/// node would drop it or move it somewhere it no longer describes.
fn has_comment(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .any(|t| matches!(t.kind(), SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT))
}

fn operator(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .map(|t| t.kind())
        .find(|k| !k.is_trivia())
}

// --- concatenated-string -----------------------------------------------------

/// **`"a " + x + "!"` becomes `"a ${x}!"`.**
///
/// Reported on the outermost `+` chain holding at least one `"` literal, and
/// only when every piece can be carried across. Why the result is the same
/// string, piece by piece:
///
/// - **A literal piece** keeps its text, escapes and all, inside the one new
///   literal: both are `"` strings with the same escapes.
/// - **Any other piece** becomes a hole, `${piece}`. `+` joins only `String`s,
///   so the piece is already a `String` and the hole inserts it unchanged; no
///   `Show` conversion appears. Holes are evaluated left to right, as the
///   operands of the chain were, so side effects keep their order.
/// - **A `$` that meets a `{` across a join** is written `\$`. `"$" + "{a}"`
///   is the text `${a}`; pasted together without the escape it would be a
///   hole reading `a`.
///
/// And the pieces refused, each because the rewrite would change the string:
///
/// - a piece that is already interpolated: the result would nest holes, and
///   which hole a `$` belongs to is exactly what could shift;
/// - a backtick piece, whether it is the piece or sits inside one: it may
///   cross lines and strips indentation, so it is a different literal
///   syntax, and inside a hole its own `"` would end the new literal early;
/// - a hole piece holding a character literal: `'"'` or `'}'` inside `${..}`
///   is read by the lexer as the end of a nested string or of the hole;
/// - a hole piece holding a string that is itself interpolated: the lexer reads
///   one level of string inside a hole and not two, so
///   `wrap("${g("}")}")` moved into a hole ends the new literal early and
///   the file no longer parses;
/// - a hole piece whose type is not `String`;
/// - and, reported but with no fix, a chain with a piece that uses a local
///   nobody wrote a type for (`uses_an_untyped_local`): `s + "!"` is also
///   what tells the checker `s` is a `String`, and a hole takes any `Show`;
/// - a chain that spans lines, because a `"` string cannot;
/// - a chain with a comment in it, which the edit would delete.
fn concatenated_string(node: &SyntaxNode, text: &str, typed: &[(&Body, &BodyTypes)], out: &mut Vec<Finding>) {
    if operator(node) != Some(SyntaxKind::PLUS) {
        return;
    }
    // Outermost only, so a three-piece message is one finding.
    if node.parent().is_some_and(|p| p.kind() == SyntaxKind::BIN_EXPR && operator(&p) == Some(SyntaxKind::PLUS)) {
        return;
    }
    let mut pieces = Vec::new();
    flatten_plus(node, &mut pieces);
    let whole = slice(text, node.text_range());
    if whole.contains('\n') || has_comment(node) {
        return;
    }
    let mut built = String::new();
    let mut any_literal = false;
    let mut unpinned = false;
    for piece in &pieces {
        let written = slice(text, piece.text_range()).trim();
        if let Some(inner) = written.strip_prefix('"').and_then(|w| w.strip_suffix('"')).filter(|_| piece.kind() == SyntaxKind::LITERAL_EXPR) {
            if contains_hole(inner) {
                return;
            }
            if inner.starts_with('{') && ends_in_bare_dollar(&built) {
                built.pop();
                built.push_str("\\$");
            }
            built.push_str(inner);
            any_literal = true;
        } else {
            let refused = piece.descendants_with_tokens().filter_map(SyntaxElement::into_token).any(|t| {
                t.kind() == SyntaxKind::CHAR_LIT
                    || (t.kind() == SyntaxKind::STRING_LIT && t.text().starts_with('`'))
                    || (t.kind() == SyntaxKind::STRING_LIT && contains_hole(t.text()))
            });
            if refused || !a_string_here(piece, typed) {
                return;
            }
            unpinned |= uses_an_untyped_local(piece);
            built.push_str("${");
            built.push_str(written);
            built.push('}');
        }
    }
    if !any_literal {
        return;
    }
    let fix = (!unpinned).then(|| Fix::one(node.text_range(), format!("\"{built}\"")));
    out.push(finding(
        CONCATENATED_STRING,
        "this joins strings with `+`; write it as one interpolated string",
        node.text_range(),
        fix,
    ));
}

/// Whether the checker gave `piece` the type `String`. A piece it has no type
/// for is refused too: every piece of a `+` chain that type-checked has one,
/// so a miss means the range was not matched, and a guess is not a fix.
fn a_string_here(piece: &SyntaxNode, typed: &[(&Body, &BodyTypes)]) -> bool {
    let range = piece.text_range();
    let mut seen = false;
    for (body, types) in typed {
        for (id, _) in body.exprs() {
            if body.range(id) == range {
                if *types.of(id) != Type::Str {
                    return false;
                }
                seen = true;
            }
        }
    }
    seen
}

/// Whether rewriting `operand` out of its `+` or `==` could leave its type
/// undecided: it is, or contains, a call, or a use of a local whose type
/// nobody wrote -- a `let` with no `: T`, a binding in a `match`, `for` or
/// `catch` pattern, or a parameter with no type.
///
/// Public because the editor's "Write it as one interpolated string" assist
/// makes the same rewrite, and must refuse it in the same places.
///
/// **The failure it prevents: a fix that removes the only thing deciding a
/// type.** `s + "!"` tells the checker `s` is a `String`, and `b == true`
/// tells it `b` is a `Bool`. `"${s}!"` and `b` do not, because a hole takes
/// anything printable and a bare `b` takes anything. In a closure nobody
/// calls, or an arm that never matches, nothing else decides it either: the
/// fixed program passes `khora check` and fails to build, with "the type of
/// `s` is not determined". The backstop in `fixing` cannot see that, because
/// the error comes from building, not checking.
///
/// A lint that asks "is anything else pinning it" would be a second checker,
/// so this refuses every such use, and a fix whose replacement pins the type
/// itself (`!b`) does not call it. What it costs: fixes that were safe,
/// because a call or another use pinned the type. The rehearsal counts them.
/// A name bound by a typed binding that shadows an untyped one is resolved to
/// the nearest binding, so it is not refused.
///
/// **A call is refused whatever it calls.** `fn make<A>() -> A` returns
/// whatever its caller needs, so `make() + "!"` is what makes it a `String`
/// (std's `todo` is the one such function most code meets). Telling a generic
/// callee from a plain one needs name resolution, which a syntax lint does not
/// have. What it costs: every chain or `== true` holding a call keeps its
/// finding and loses its fix.
pub fn uses_an_untyped_local(operand: &SyntaxNode) -> bool {
    if operand.descendants().any(|n| n.kind() == SyntaxKind::CALL_EXPR) {
        return true;
    }
    operand.descendants().filter(|n| n.kind() == SyntaxKind::PATH_EXPR).any(|expr| {
        let Some(path) = expr.children().find(|n| n.kind() == SyntaxKind::PATH) else { return false };
        let qualified = path.children_with_tokens().any(|e| e.kind() == SyntaxKind::COLON_COLON);
        let refs: Vec<SyntaxNode> = path.children().filter(|n| n.kind() == SyntaxKind::NAME_REF).collect();
        match (qualified, refs.as_slice()) {
            (false, [name]) => binding_is_untyped(&expr, name.text().to_string().trim()) == Some(true),
            _ => false,
        }
    })
}

/// The nearest binding of `name` in scope at `at`: `Some(true)` if nobody
/// wrote its type, `Some(false)` if somebody did, `None` if it is not a local.
fn binding_is_untyped(at: &SyntaxNode, name: &str) -> Option<bool> {
    let start = at.text_range().start();
    let mut child = at.clone();
    for scope in at.ancestors().skip(1) {
        match scope.kind() {
            SyntaxKind::LAMBDA_EXPR | SyntaxKind::FN_DECL => {
                if let Some(list) = scope.children().find(|n| n.kind() == SyntaxKind::PARAM_LIST) {
                    for param in list.children().filter(|n| n.kind() == SyntaxKind::PARAM) {
                        if binds(&param, name) {
                            return Some(!param.children_with_tokens().any(|e| e.kind() == SyntaxKind::COLON));
                        }
                    }
                }
            }
            SyntaxKind::MATCH_ARM | SyntaxKind::FOR_EXPR => {
                // The pattern is the arm's or loop's first node; a use inside it,
                // or in a `for`'s iterable, is not in the pattern's scope.
                let pattern = scope.children().next();
                let in_scope = match scope.kind() {
                    SyntaxKind::FOR_EXPR => child.kind() == SyntaxKind::BLOCK,
                    _ => pattern.as_ref() != Some(&child),
                };
                if in_scope && pattern.is_some_and(|p| binds(&p, name)) {
                    return Some(true);
                }
            }
            SyntaxKind::BLOCK => {
                let earlier = scope
                    .children()
                    .filter(|n| n.kind() == SyntaxKind::LET_DECL && n.text_range().end() <= start)
                    .collect::<Vec<_>>();
                if let Some(decl) = earlier.iter().rev().find(|decl| {
                    decl.children().find(|n| n.kind() != SyntaxKind::PATH_TYPE).is_some_and(|p| binds(&p, name))
                }) {
                    return Some(!decl.children_with_tokens().any(|e| e.kind() == SyntaxKind::COLON));
                }
            }
            _ => {}
        }
        child = scope;
    }
    None
}

/// Whether the pattern (or parameter) `node` binds `name`.
fn binds(node: &SyntaxNode, name: &str) -> bool {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_node)
        .filter(|n| n.kind() == SyntaxKind::NAME)
        .any(|n| n.text().to_string().trim() == name)
}

/// Whether a literal's contents hold a `${` that opens a hole.
fn contains_hole(inner: &str) -> bool {
    let mut escaped = false;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '$' if chars.peek() == Some(&'{') => return true,
            _ => {}
        }
    }
    false
}

/// Whether `built` ends in a `$` no backslash escapes. Counted, because `\\$`
/// is an escaped backslash and then a bare dollar.
fn ends_in_bare_dollar(built: &str) -> bool {
    let Some(before) = built.strip_suffix('$') else { return false };
    before.chars().rev().take_while(|&c| c == '\\').count() % 2 == 0
}

fn flatten_plus(node: &SyntaxNode, into: &mut Vec<SyntaxNode>) {
    if node.kind() == SyntaxKind::BIN_EXPR && operator(node) == Some(SyntaxKind::PLUS) {
        for child in node.children() {
            flatten_plus(&child, into);
        }
    } else {
        into.push(node.clone());
    }
}

// --- bool-comparison ---------------------------------------------------------

/// **`b == true` becomes `b`, and `b == false` becomes `!b`.**
///
/// `==` compares two values of one type, so `b` is a `Bool`, and for a `Bool`
/// `b == true` is `b` and `b == false` is `!b` -- there is no third value.
/// The literal side has no effects, so dropping it changes no order of
/// evaluation. `true == b` is the same case written the other way round.
///
/// Placement: `b` has the precedence of an operand of `==`, so it stands
/// wherever the comparison stood. `!` binds tighter than every binary
/// operator, so an operand that is not already atomic -- `x < y == false` --
/// is bracketed: `!(x < y)`.
///
/// `b == true` is reported with no fix where `b` uses a local nobody wrote a
/// type for (`uses_an_untyped_local`): the `==` may be what makes it a `Bool`.
///
/// `!=` is not reported: `b != false` is a double negative, but rewriting it
/// is a second rule nobody asked for yet. `true == false` is left alone,
/// because neither side is the one the reader meant to keep.
fn bool_comparison(node: &SyntaxNode, text: &str, out: &mut Vec<Finding>) {
    if operator(node) != Some(SyntaxKind::EQ_EQ) || has_comment(node) {
        return;
    }
    let sides: Vec<SyntaxNode> = node.children().collect();
    let [lhs, rhs] = sides.as_slice() else { return };
    let (operand, literal) = match (bool_literal(lhs), bool_literal(rhs)) {
        (None, Some(value)) => (lhs, value),
        (Some(value), None) => (rhs, value),
        (Some(_), Some(_)) | (None, None) => return,
    };
    // `!b` still says `b` is a `Bool`; a bare `b` does not.
    let unpinned = literal && uses_an_untyped_local(operand);
    let written = slice(text, operand.text_range());
    let replacement = if literal {
        written.to_string()
    } else if atomic(operand.kind()) {
        format!("!{written}")
    } else {
        format!("!({written})")
    };
    let message = if literal {
        "comparing a `Bool` with `true` is the `Bool` itself; write it alone"
    } else {
        "comparing a `Bool` with `false` is its negation; write `!` before it"
    };
    let fix = (!unpinned).then(|| Fix::one(node.text_range(), replacement));
    out.push(finding(BOOL_COMPARISON, message, node.text_range(), fix));
}

fn bool_literal(node: &SyntaxNode) -> Option<bool> {
    if node.kind() != SyntaxKind::LITERAL_EXPR {
        return None;
    }
    let token = node.children_with_tokens().filter_map(SyntaxElement::into_token).find(|t| !t.kind().is_trivia())?;
    match token.kind() {
        SyntaxKind::TRUE_KW => Some(true),
        SyntaxKind::FALSE_KW => Some(false),
        _ => None,
    }
}

/// Whether a prefix `!` applies to all of this without brackets.
fn atomic(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PATH_EXPR
            | SyntaxKind::CALL_EXPR
            | SyntaxKind::FIELD_EXPR
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::LITERAL_EXPR
            | SyntaxKind::PREFIX_EXPR
    )
}

// --- needless-return ---------------------------------------------------------

/// **A function whose last statement is `return e;` becomes one whose tail is
/// `e`; a last `return;` is deleted.**
///
/// A block's value is its tail, and a function's value is its block's, so
/// `return e;` in last position and `e` as the tail leave the function with
/// the same value by the same route: `e` is evaluated once, last, and handed
/// back. For `return;` the function returns `()` either way, since the
/// statement before it ends in `;` (below).
///
/// Only a function's own block. A `return` in a lambda returns from the
/// lambda, and one in a nested block is an early exit, which is what `return`
/// is for.
///
/// The fix is withheld, and the finding still made, in three cases:
///
/// - the statement before it has no `;` (an `if` or `match` used as a
///   statement): that one would become the tail, and a statement whose value
///   was being discarded would be returned instead;
/// - the value starts with `{`, which as a tail reads as a block rather than
///   the record it was;
/// - there is a comment inside the statement.
fn needless_return(node: &SyntaxNode, text: &str, out: &mut Vec<Finding>) {
    let Some(block) = node.children().find(|n| n.kind() == SyntaxKind::BLOCK) else { return };
    let items: Vec<SyntaxNode> = block.children().collect();
    let Some(last) = items.last() else { return };
    if last.kind() != SyntaxKind::EXPR_STMT {
        return;
    }
    let Some(ret) = last.children().find(|n| n.kind() == SyntaxKind::RETURN_EXPR) else { return };
    let value = ret.children().next();
    let previous_open = items.len() >= 2 && {
        let before = &items[items.len() - 2];
        before.kind() == SyntaxKind::EXPR_STMT && operator_last(before) != Some(SyntaxKind::SEMICOLON)
    };
    let value_text = value.as_ref().map(|v| slice(text, v.text_range()));
    let fix = if previous_open || has_comment(last) || value_text.is_some_and(|v| v.starts_with('{')) {
        None
    } else {
        match value_text {
            Some(value) => Some(Fix::one(last.text_range(), value)),
            // Deleted with the whitespace before it, so no blank line is left
            // where it stood.
            None => {
                let start = last
                    .prev_sibling_or_token()
                    .filter(|t| t.kind() == SyntaxKind::WHITESPACE)
                    .map_or(last.text_range().start(), |t| t.text_range().start());
                Some(Fix::one(TextRange::new(start, last.text_range().end()), ""))
            }
        }
    };
    out.push(finding(
        NEEDLESS_RETURN,
        "a function's last expression is its value; drop the `return` and the `;`",
        ret.text_range(),
        fix,
    ));
}

fn operator_last(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens().filter_map(SyntaxElement::into_token).map(|t| t.kind()).filter(|k| !k.is_trivia()).last()
}

// --- parenthesized-parameter -------------------------------------------------

/// **`fn (x) => e` becomes `fn x => e`.**
///
/// Both spellings parse to a lambda with one parameter list holding one
/// parameter; the brackets are not part of it. So the lowered lambda is the
/// same, and so is everything after.
///
/// Only an untyped parameter: `fn (x: Int) =>` needs its brackets, since the
/// bare form takes a name or `_` and nothing else. Not with a trailing comma
/// or a comment inside the brackets, both of which the edit would drop.
fn parenthesized_parameter(node: &SyntaxNode, text: &str, out: &mut Vec<Finding>) {
    let Some(list) = node.children().find(|n| n.kind() == SyntaxKind::PARAM_LIST) else { return };
    let tokens: Vec<SyntaxKind> =
        list.children_with_tokens().filter_map(SyntaxElement::into_token).map(|t| t.kind()).filter(|k| !k.is_trivia()).collect();
    if tokens != [SyntaxKind::L_PAREN, SyntaxKind::R_PAREN] || has_comment(&list) {
        return;
    }
    let params: Vec<SyntaxNode> = list.children().collect();
    let [param] = params.as_slice() else { return };
    let inside: Vec<SyntaxKind> = param
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .map(|t| t.kind())
        .filter(|k| !k.is_trivia())
        .collect();
    if !matches!(inside.as_slice(), [SyntaxKind::IDENT] | [SyntaxKind::UNDERSCORE]) {
        return;
    }
    let range = list.text_range();
    let mut replacement = slice(text, param.text_range()).trim().to_string();
    // `fn(x)` has no space to keep the keyword and the name apart once the
    // bracket is gone.
    let before = text[..usize::from(range.start())].chars().next_back();
    if !before.is_some_and(char::is_whitespace) {
        replacement.insert(0, ' ');
    }
    out.push(finding(
        PARENTHESIZED_PARAMETER,
        "a lambda's single parameter needs no brackets: `fn x => ...`",
        range,
        Some(Fix::one(range, replacement)),
    ));
}

// --- subtraction-from-zero ---------------------------------------------------

/// **`0 - 1` becomes `-1`.** Integer literals only.
///
/// For `Int`, `0 - n` and `-n` are the same number for every `n` a literal can
/// spell: `n` is at most `i64::MAX`, so neither form overflows, and
/// `0 - 9223372036854775807 - 1` becomes `-9223372036854775807 - 1`, which is
/// `i64::MIN` either way. The left operand of `0 - n` is a literal with no
/// effects, so dropping it changes no evaluation order. `-` as a prefix binds
/// tighter than any binary operator, so `-1` stands wherever `0 - 1` stood.
///
/// Excluded, each because the two forms differ:
///
/// - **`Float`**: `0.0 - 0.0` is `+0.0` and `-0.0` is not, and `1.0 /` tells
///   them apart. A float literal is never `Literal::Int`, so this falls out
///   of the pattern below;
/// - **the fixed-width integers**: `0 - 1` in `U8` is an overflow at run time
///   and `-1` is a different error, found at a different time. Kept out by
///   the type, which is the one guard here that needs types;
/// - **a subtrahend that is not a literal**: `0 - x` is not `-x` when `x` is
///   `i64::MIN`. The pattern refuses it, and the text is checked to be
///   `0 - <digits>` besides, so the edit is to exactly what the tree says.
fn subtraction_from_zero(body: &Body, types: &BodyTypes, text: &str, out: &mut Vec<Finding>) {
    for (id, expr) in body.exprs() {
        let Expr::Binary { op: BinOp::Sub, lhs, rhs } = expr else { continue };
        let (Expr::Literal(Literal::Int(zero)), Expr::Literal(Literal::Int(n))) = (body.expr(*lhs), body.expr(*rhs))
        else {
            continue;
        };
        if zero != "0" || *types.of(id) != Type::Int {
            continue;
        }
        let range = body.range(id);
        let Some(written) = text.get(std::ops::Range::<usize>::from(range)) else { continue };
        // The text has to be exactly what the tree says, so the edit is to the
        // expression it describes and not to a derived body's borrowed span.
        let Some((left, right)) = written.split_once('-') else { continue };
        let digits = right.trim();
        if left.trim() != "0"
            || digits != n
            || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
        {
            continue;
        }
        out.push(finding(
            SUBTRACTION_FROM_ZERO,
            "subtracting from zero is a negative number; write it with `-`",
            range,
            Some(Fix::one(range, format!("-{digits}"))),
        ));
    }
}

// --- module-path -------------------------------------------------------------

/// **`module main;` in a package becomes `module <package>::main;`.**
///
/// The path a file declares is what other files import it by, and `khora new`
/// writes `module <package>::main;`. Reported on `module main;` in any file
/// under a package's `src` (a `src` directory with a `khora.toml` beside it
/// that names a `[package]`).
///
/// The fix is offered only in an entry file, `src/main.kh` or
/// `src/bin/<name>.kh`, which nothing imports: renaming its module changes no
/// other file's meaning. Elsewhere, renaming a module breaks every file that
/// imports it, so the finding is made with no fix.
///
/// **An entry file can still be imported, by a test.** `src/main_test.kh`
/// writing `import main::{helper}` compiles, and renaming `main` breaks it. So
/// the fix is withheld when any file in the compilation names the module
/// `main` in an import or a path; the finding stays.
///
/// **What it costs:** the package's name is read from the manifest on disk
/// while the lints run. An editor that renames the package sees the old name
/// in this fix until the file itself is edited again. And the importers are
/// found by parsing every file in the compilation, once per finding, which
/// is rare: only a package's entry files can have one.
fn module_path(db: &dyn Db, file: SourceFile, node: &SyntaxNode, out: &mut Vec<Finding>) {
    let path: &Path = file.path(db);
    let Some(written) = node.children().find(|n| n.kind() == SyntaxKind::PATH) else { return };
    if written.text().to_string().trim() != "main" {
        return;
    }
    let parts: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let Some(src) = parts.iter().rposition(|part| part == "src") else { return };
    let package_dir: std::path::PathBuf = path.components().take(src).collect();
    let Ok(parsed) = khora_manifest::Manifest::load_for_resolution(&package_dir.join("khora.toml")) else { return };
    let Some(package) = parsed.manifest.package else { return };
    let after: &[String] = &parts[src + 1..];
    let entry = match after {
        [only] if only == "main.kh" => Some("main".to_string()),
        [folder, file] if folder == "bin" => file.strip_suffix(".kh").filter(|n| is_identifier(n)).map(str::to_string),
        _ => None,
    };
    let entry = entry.filter(|_| !anything_imports_main(db, file));
    let fix = entry.map(|entry| Fix::one(written.text_range(), format!("{}::{entry}", package.name)));
    let message = if fix.is_some() {
        "a package's module path starts with the package name, as `khora new` writes it"
    } else {
        "a package's module path starts with the package name. Renaming this module breaks what imports it, so there is no automatic fix"
    };
    out.push(finding(MODULE_PATH, message, written.text_range(), fix));
}

/// Whether any other file in the compilation reaches the module `main`: an
/// `import main::..`, or a path starting `main::`.
fn anything_imports_main(db: &dyn Db, file: SourceFile) -> bool {
    let Some(root) = khora_db::source_root(db) else { return false };
    root.files(db).iter().filter(|other| **other != file).any(|other| {
        khora_db::parse(db, *other).syntax().descendants().filter(|n| n.kind() == SyntaxKind::PATH).any(|path| {
            let first = path.children().find(|n| n.kind() == SyntaxKind::NAME_REF);
            let named_main = first.is_some_and(|n| n.text().to_string().trim() == "main");
            let qualified = path.children_with_tokens().any(|e| e.kind() == SyntaxKind::COLON_COLON);
            let imported = path.parent().is_some_and(|p| p.kind() == SyntaxKind::IMPORT_DECL);
            named_main && (qualified || imported)
        })
    })
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
