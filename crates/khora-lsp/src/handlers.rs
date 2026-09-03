//! What can be written inside a `with { .. }`.
//!
//! **The one completion Khora needs that no other language has to answer.** A
//! `with` block is where a capability stops being a requirement and becomes
//! something the code supplies, and writing one means naming an effect, then
//! naming every operation it declares, then giving each a closure of the right
//! arity. All of that is in the effect declaration, usually in another file.
//!
//! ```khora
//! with { clock: handler for Clock { now: fn () => todo() } } { .. }
//! ```
//!
//! Nobody types that from memory. They open `std::time`, read `effect Clock`,
//! count the operations, and transcribe. It is the same transcription job as
//! the missing trait members, and it has the same answer: copy the
//! declaration, not a rendering of it.
//!
//! # Two rows called `with`, and they want opposite things
//!
//! `fn f() -> Int with { db: Db }` is a *signature*, and its row holds types.
//! `with { db: handler for Db { .. } } { .. }` is an *expression*, and its row
//! holds values. The same three characters open both, so the completion has to
//! know which one it is in — a handler skeleton offered in a signature is
//! nonsense, and a bare type name offered in an expression does not compile.
//!
//! # Why the skeleton is one line
//!
//! It is inserted where the cursor is, which may be halfway along a line, in a
//! file whose indentation this cannot see the shape of. One line is correct
//! everywhere and ugly in the multi-operation case; `khora fmt` is what makes
//! it pretty, and it is already bound to save in the extension.

use khora_syntax::ast::{AstNode, EffectDecl, Type};
use khora_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Whether a row is being written for a signature or for a `with` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// `fn f() -> Int with { .. }`: the entries are types.
    Declared,
    /// `with { .. } { .. }`: the entries are handlers.
    Installed,
}

/// Which kind of `with` row the cursor is in, if it is in one.
///
/// **Read backwards from the token, not up from the tree.** The whole reason
/// this is asked is that somebody has typed `with {` and nothing after it,
/// which is a syntax error — so the node that would say what the brace belongs
/// to is exactly the node that does not exist yet. The tokens are still there,
/// because rowan keeps every byte.
pub fn row_at(anchor: &SyntaxToken) -> Option<Row> {
    let opening = match anchor.kind() {
        SyntaxKind::L_BRACE => anchor.clone(),
        // `with { db: Db, ` — the same row, one entry further along.
        SyntaxKind::COMMA => enclosing_brace(anchor)?,
        _ => return None,
    };
    let before = opening.prev_token().and_then(skip_trivia)?;
    if before.kind() != SyntaxKind::WITH_KW {
        return None;
    }
    // A `with` in a signature is under a `WITH_CLAUSE`; one in an expression is
    // not. The clause parses even while the row is empty, which is what makes
    // this readable at the moment the question is asked.
    let declared = before
        .parent_ancestors()
        .any(|node| node.kind() == SyntaxKind::WITH_CLAUSE);
    Some(if declared { Row::Declared } else { Row::Installed })
}

/// The `{` this token is inside, counting nesting.
fn enclosing_brace(from: &SyntaxToken) -> Option<SyntaxToken> {
    let mut depth = 0usize;
    let mut at = from.prev_token();
    while let Some(token) = at {
        match token.kind() {
            SyntaxKind::R_BRACE => depth += 1,
            SyntaxKind::L_BRACE if depth == 0 => return Some(token),
            SyntaxKind::L_BRACE => depth -= 1,
            _ => {}
        }
        at = token.prev_token();
    }
    None
}

/// The nearest earlier token that is not whitespace or a comment.
fn skip_trivia(from: SyntaxToken) -> Option<SyntaxToken> {
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

/// Every `effect` a file declares, with the node that declares it.
pub fn declared_in(tree: &SyntaxNode) -> Vec<EffectDecl> {
    tree.descendants().filter_map(EffectDecl::cast).collect()
}

/// A whole handler for an effect, ready to be a row entry.
///
/// `label` is what the capability is called at the use site, which is not
/// derivable from the type — `std` installs `LLMService` as `ai` — so it is
/// passed in by whoever knows: the required row where the checker could say,
/// and a name made from the effect where it could not.
pub fn skeleton(effect: &EffectDecl, label: &str) -> Option<String> {
    let name = effect.name()?.ident()?;
    let operations: Vec<String> = effect
        .operations()
        .filter_map(|op| {
            let called = op.name()?.ident()?;
            Some(format!("{called}: {}", closure(op.ty().as_ref())))
        })
        .collect();
    Some(format!("{label}: handler for {name} {{ {} }}", operations.join(", ")))
}

/// A closure of the arity an operation's type asks for, with a hole in it.
///
/// The arity rule is `khora-types`' own, in `type_of_syntax`: the parameters
/// parse as whatever shape the parentheses made of them — a tuple for several,
/// a paren for one, a unit for none — and all three mean the same thing.
fn closure(ty: Option<&Type>) -> String {
    let names = match ty {
        Some(Type::Fn(f)) => match f.param_type() {
            Some(Type::Tuple(t)) => t.elements().count(),
            Some(Type::Unit(_)) | None => 0,
            Some(_) => 1,
        },
        // Not a function type at all: an operation this cannot read gets a
        // hole rather than a guess at a shape.
        _ => return "todo()".to_string(),
    };
    match names {
        0 => "fn () => todo()".to_string(),
        1 => "fn a => todo()".to_string(),
        n => {
            let params: Vec<String> =
                (0..n).map(|i| ((b'a' + (i % 26) as u8) as char).to_string()).collect();
            format!("fn ({}) => todo()", params.join(", "))
        }
    }
}

/// A type's name as a capability label: `Clock` is `clock`, `LLMService` is
/// `llm_service`.
///
/// A guess, and only used where the checker could not say what the label
/// actually has to be. It is the convention `std` follows everywhere the name
/// is not an acronym, which is most places.
pub fn label_for(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            // A boundary is a lower-then-upper, or the last capital of a run
            // before a lowercase -- so `LLMService` breaks before `S`, not
            // between every pair of capitals.
            let after_lower = chars[i - 1].is_lowercase();
            let ends_a_run = chars.get(i + 1).is_some_and(|next| next.is_lowercase());
            if after_lower || ends_a_run {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_the_type_in_snake_case() {
        assert_eq!(label_for("Clock"), "clock");
        assert_eq!(label_for("Db"), "db");
        assert_eq!(label_for("LLMService"), "llm_service");
        assert_eq!(label_for("HttpClient"), "http_client");
    }

    fn effect(source: &str) -> EffectDecl {
        let parsed = khora_syntax::parse(source);
        declared_in(&parsed.syntax()).into_iter().next().expect("an effect")
    }

    #[test]
    fn an_operation_gets_a_closure_of_its_own_arity() {
        let decl = effect(
            "module m;\npub effect Clock {\n  now: () -> Int,\n  at: (Int, Int) -> Int,\n  \
             after: (Int) -> Int,\n}\n",
        );
        assert_eq!(
            skeleton(&decl, "clock").as_deref(),
            Some(
                "clock: handler for Clock { now: fn () => todo(), at: fn (a, b) => todo(), \
                 after: fn a => todo() }"
            )
        );
    }

    /// An effect with nothing in it is still a handler, and an empty one is
    /// what the declaration says.
    #[test]
    fn an_effect_with_no_operations_is_still_a_handler() {
        let decl = effect("module m;\npub effect Nothing {\n}\n");
        assert_eq!(
            skeleton(&decl, "nothing").as_deref(),
            Some("nothing: handler for Nothing {  }")
        );
    }
}
