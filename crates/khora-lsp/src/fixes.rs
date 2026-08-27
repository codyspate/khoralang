//! Quick fixes: turning a diagnostic's own sentence into an edit.
//!
//! Khora's messages were written to say what to do rather than only what is
//! wrong — "`export` is spelled `pub`", "write `let _ =` to say the answer was
//! considered". Where the sentence names an edit exactly, this makes it one.
//!
//! # The rule for what qualifies
//!
//! **Only where the message names one edit and there is nothing to choose.**
//! A quick fix is applied by somebody who read four words of it, so an action
//! that guesses is worse than no action: they get a change they did not read
//! and did not want, in a file they will not re-read.
//!
//! So `Add the bound, as `T: Ord`` is *not* here, even though it names the
//! text. Where the bound goes depends on which parameter list the reader meant
//! and whether one already exists, and getting that wrong edits a signature.
//! Nor is `unused-capability`, whose fix is to delete part of a signature —
//! the lint exists precisely because the capability may be deliberate.
//!
//! # How a diagnostic is recognised
//!
//! By its lint `code` where it has one, and by exact message otherwise. Exact
//! matching on prose is a thing that rots, and the two it is used for are both
//! pinned by tests of their own in `khora-syntax` — so a change to either
//! breaks a test that names it, rather than silently removing a fix.

use text_size::TextRange;

/// One offered edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// What the action is called in the lightbulb menu.
    pub title: String,
    /// The range to replace.
    pub range: TextRange,
    /// What to put there.
    pub replacement: String,
}

/// The fixes for one diagnostic, given the text it points at.
///
/// `code` is the lint name where there is one; `at` is the source the
/// diagnostic covers, which is what lets a replacement keep the part of the
/// line it is not changing.
pub fn for_diagnostic(message: &str, code: Option<&str>, range: TextRange, at: &str) -> Vec<Fix> {
    match code {
        Some("discarded-result") => vec![Fix {
            title: "Bind it to `_` to say the answer was considered".to_string(),
            range,
            // The statement kept intact with a binding in front, which is the
            // spelling the message itself suggests.
            replacement: format!("let _ = {at}"),
        }],
        Some(_) => Vec::new(),
        None => for_parse_error(message, range),
    }
}

/// The two parser messages that name a keyword to swap.
///
/// Both are pinned by tests in `khora-syntax`, so the exact-match here cannot
/// rot without something failing that says so.
fn for_parse_error(message: &str, range: TextRange) -> Vec<Fix> {
    if message.contains("`export` is spelled `pub`") {
        return vec![Fix {
            title: "Replace `export` with `pub`".to_string(),
            range,
            replacement: "pub".to_string(),
        }];
    }
    if message.contains("a binding at module level is a `const`, not a `let`") {
        return vec![Fix {
            title: "Replace `let` with `const`".to_string(),
            range,
            replacement: "const".to_string(),
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range() -> TextRange {
        TextRange::new(0.into(), 6.into())
    }

    #[test]
    fn a_discarded_result_is_offered_a_binding() {
        let fixes = for_diagnostic("", Some("discarded-result"), range(), "db.execute(sql)!;");
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].replacement, "let _ = db.execute(sql)!;");
    }

    #[test]
    fn the_renamed_keyword_is_offered_the_rename() {
        let fixes = for_diagnostic("`export` is spelled `pub`", None, range(), "export");
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].replacement, "pub");
    }

    #[test]
    fn a_module_level_let_is_offered_a_const() {
        let fixes = for_diagnostic(
            "a binding at module level is a `const`, not a `let`",
            None,
            range(),
            "let",
        );
        assert_eq!(fixes[0].replacement, "const");
    }

    /// **A lint whose fix is a judgement gets no action**, which is the rule
    /// this file exists to hold: an action applied by somebody who read four
    /// words of the message must not guess.
    #[test]
    fn a_lint_that_needs_a_decision_is_offered_nothing() {
        assert!(for_diagnostic("", Some("unused-capability"), range(), "").is_empty());
        assert!(for_diagnostic("", Some("reference-cycle"), range(), "").is_empty());
    }

    #[test]
    fn an_ordinary_type_error_is_offered_nothing() {
        assert!(for_diagnostic("expected `Int`, found `String`", None, range(), "x").is_empty());
    }
}
