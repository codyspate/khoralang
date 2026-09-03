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
//! # Why the missing-clause fix *does* qualify
//!
//! It edits a signature too, so it deserves the same suspicion. It survives
//! because nothing about the edit is a choice: the message names the label and
//! the type in full, there is exactly one function the call sits in, and a row
//! is a set — where in it the entry lands changes nothing. The `T: Ord` case
//! fails all three. And the alternative is not "no edit": it is the reader
//! retyping `with { db: Db }` by hand, which is the same edit with a chance of
//! a typo in it.
//!
//! It is offered with nothing beside it, and deliberately so. Propagating a
//! requirement outwards is one of two answers — the other is to satisfy it
//! here with a `with { db: .. }` block — and only this one is spelled out by
//! the message. An action that guessed between them would be the thing this
//! file refuses to do.
//!
//! # How a diagnostic is recognised
//!
//! By its lint `code` where it has one, and by exact message otherwise. Exact
//! matching on prose is a thing that rots, and the two it is used for are both
//! pinned by tests of their own in `khora-syntax` — so a change to either
//! breaks a test that names it, rather than silently removing a fix.

use text_size::{TextRange, TextSize};

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

/// One row of a signature, as it stands in the file.
///
/// The text comes along with the range because every edit here is decided by
/// what is already written — whether the row is empty, whether it has a tail
/// — and re-reading the document to find that out would make two sources of
/// truth for one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What the row covers, braces and all.
    pub range: TextRange,
    /// The source in that range.
    pub text: String,
}

/// The signature of the function a diagnostic sits inside.
///
/// Assembled by the caller, which has the tree. `None` when the diagnostic is
/// not inside a function declaration at all — at module level, or in a file
/// too broken to parse into one — and the fixes that need it are then simply
/// not offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// Just past the return type: where a clause the function has none of goes.
    ///
    /// **After the return type rather than before the body**, because the space
    /// between them may already hold a clause of the other kind, and landing on
    /// the far side of `raises E` would write `raises E with { .. }` — which
    /// the parser takes, in an order nothing in `std` is written in.
    pub clauses_at: TextSize,
    /// The `with` row, when the function already has one.
    pub with_row: Option<Row>,
    /// The `raises` row, when the function already has one.
    pub raises_row: Option<Row>,
}

/// The fixes for one diagnostic, given the text it points at.
///
/// `code` is the lint name where there is one; `at` is the source the
/// diagnostic covers, which is what lets a replacement keep the part of the
/// line it is not changing. `enclosing` is the signature of the function it
/// sits in, for the fixes that edit one.
pub fn for_diagnostic(
    message: &str,
    code: Option<&str>,
    range: TextRange,
    at: &str,
    enclosing: Option<&Signature>,
) -> Vec<Fix> {
    match code {
        Some("discarded-result") => vec![Fix {
            title: "Bind it to `_` to say the answer was considered".to_string(),
            range,
            // The statement kept intact with a binding in front, which is the
            // spelling the message itself suggests.
            replacement: format!("let _ = {at}"),
        }],
        Some(_) => Vec::new(),
        None => {
            let mut out = for_parse_error(message, range);
            out.extend(enclosing.and_then(|sig| for_missing_clause(message, sig)));
            out.extend(for_missing_try(message, range, at));
            out.extend(for_missing_arms(message, range, at));
            out
        }
    }
}

/// `` `f` needs `db: Db`, which this function does not require ``, made an edit.
///
/// Matched on the tail, which names the clause, with the entry read out of the
/// backticks in front of it. Both halves are written by `Clause::verb` and
/// `Clause::describe` in `khora-types`, and `rows.rs` there pins both sentences
/// whole — so a rewording breaks a test that quotes it, rather than quietly
/// dropping the action.
fn for_missing_clause(message: &str, sig: &Signature) -> Option<Fix> {
    let (head, requires) = match message.strip_suffix(", which this function does not require") {
        Some(head) => (head, true),
        None => (message.strip_suffix(", which this function does not raise")?, false),
    };
    let entry = head.rsplit_once("needs `")?.1.strip_suffix('`')?;
    if entry.is_empty() {
        return None;
    }
    if requires {
        add_to_with(entry, sig)
    } else {
        add_to_raises(entry, sig)
    }
}

/// `with { .. }` is a set, so the entry goes on the end of it.
fn add_to_with(entry: &str, sig: &Signature) -> Option<Fix> {
    let title = format!("Add `{entry}` to this function's `with` clause");
    let Some(row) = &sig.with_row else {
        return Some(Fix {
            title,
            range: TextRange::empty(sig.clauses_at),
            replacement: format!(" with {{ {entry} }}"),
        });
    };
    // Everything up to the closing brace, so that the one comma this needs is
    // decided by what is actually written rather than by whether the row was
    // formatted with spaces inside it.
    let close = row.text.rfind('}')?;
    let kept = row.text[..close].trim_end();
    // `{` or `|` last means the row has no fields — `{}`, or a tail on its own
    // — and a leading comma would make it `{, db: Db }`.
    let separator = if kept.ends_with('{') || kept.ends_with('|') { "" } else { "," };
    Some(Fix {
        title,
        range: TextRange::new(row.range.start() + TextSize::of(kept), row.range.end()),
        replacement: format!("{separator} {entry} }}"),
    })
}

/// `raises A + B` is an open union, so the entry is one more term.
fn add_to_raises(entry: &str, sig: &Signature) -> Option<Fix> {
    let title = format!("Add `{entry}` to this function's `raises` clause");
    let Some(row) = &sig.raises_row else {
        return Some(Fix {
            title,
            range: TextRange::empty(sig.clauses_at),
            replacement: format!(" raises {entry}"),
        });
    };
    // Appended rather than rewritten, so a row spelled `'e + HttpError` keeps
    // its tail in front and stays the row somebody wrote.
    let kept = row.text.trim_end();
    Some(Fix {
        title,
        range: TextRange::new(row.range.start() + TextSize::of(kept), row.range.end()),
        replacement: format!(" + {entry}"),
    })
}

/// `` the call needs `!`: write `f(..)!` `` — so write it.
///
/// **The message already contains the edit**, which is this module's whole
/// rule for what qualifies. A call that can leave the function is marked at
/// the call rather than at the signature, and the mark is one character in one
/// place: there is nothing to choose.
fn for_missing_try(message: &str, range: TextRange, at: &str) -> Option<Fix> {
    if !message.contains("so the call needs `!`") {
        return None;
    }
    // Appended to what the diagnostic covers rather than inserted at an
    // offset, because the range is the call and the `!` goes after all of it.
    let trimmed = at.trim_end();
    if trimmed.is_empty() || trimmed.ends_with('!') {
        return None;
    }
    Some(Fix {
        title: "Add `!` to say this call can leave the function".to_string(),
        range: TextRange::new(range.start() + TextSize::of(trimmed), range.end()),
        replacement: at[trimmed.len()..].to_string() + "!",
    })
}

/// `` this `match` is not exhaustive: pattern `Blue` not covered `` — so cover
/// it.
///
/// **The checker names the pattern, which is the part a person cannot work
/// out from the error.** Writing the arm from there is mechanical: it goes
/// before the closing brace, indented like the arms above it, with a body of
/// `()` so the program still does not compile until somebody says what the
/// case means. An arm that silently returned a plausible value would turn a
/// refusal into a wrong answer, which is the one outcome worse than an error.
///
/// **`()` rather than a `todo`, because Khora has no `todo`.** Rust's assist
/// writes `todo!()` and can, because the macro exists; inventing a name here
/// would fail with "cannot find `todo` in this scope", which tells the author
/// nothing about the arm. The unit value is real, so the error is the type
/// checker's own -- *expected `String`, found `()`* -- pointing at the arm and
/// naming what it has to produce. Where the match is already `()`-typed the
/// stub simply compiles, which is right.
fn for_missing_arms(message: &str, range: TextRange, at: &str) -> Option<Fix> {
    if !message.contains("not exhaustive") {
        return None;
    }
    // The checker joins several with `, ` between backticks, and all of them
    // are missing, so all of them are written. One arm at a time would mean
    // applying the same action once per case with a recompile between.
    let named = message.rsplit_once("pattern `")?.1.rsplit_once("` not covered")?.0;
    let patterns: Vec<&str> = named.split("`, `").filter(|p| !p.is_empty()).collect();
    if patterns.is_empty() {
        return None;
    }

    // The closing brace of the `match`, and the indentation of whatever the
    // last arm was written at. Read from the text the diagnostic covers, so a
    // file indented with tabs gets tabs.
    let close = at.rfind('}')?;
    let before = &at[..close];
    let last = before.rfind('\n')? + 1;
    let indent: String =
        before[last..].chars().take_while(|c| c.is_whitespace() && *c != '\n').collect();
    let inner = if indent.is_empty() { "  ".to_string() } else { indent.clone() };

    // **A bare constructor name is a binding, not a match.** The checker names
    // the missing cases unqualified -- `Green` -- and an arm reading
    // `Green => ..` binds every remaining value to a new local called `Green`
    // and matches all of them. The program then compiles and is wrong, and the
    // arm after it is reported unreachable, which is a confusing way to
    // discover that an assist did this to you.
    //
    // So the qualification is copied from the arms already written rather than
    // guessed: a match whose arms say `Colour::Red` gets `Colour::Green`, and
    // one whose arms are bare -- because the constructors were imported -- gets
    // bare. A match with no arms at all says nothing about which, and is
    // offered nothing.
    let qualifier = qualifier_in(before)?;

    let title = match patterns.as_slice() {
        [one] => format!("Add the missing `{qualifier}{one}` arm"),
        many => format!("Add the {} missing arms", many.len()),
    };
    let written: String = patterns
        .iter()
        .map(|pattern| format!("{inner}{qualifier}{pattern} => (),\n"))
        .collect();
    Some(Fix {
        title,
        range: TextRange::empty(range.start() + TextSize::of(before)),
        replacement: format!("{written}{indent}"),
    })
}

/// How the arms already written spell a constructor: `Colour::` or nothing.
///
/// `None` when there are no arms to copy from, which is the case where this
/// cannot be answered and so is not guessed at.
fn qualifier_in(arms: &str) -> Option<String> {
    let mut seen = false;
    for line in arms.lines() {
        let Some((pattern, _)) = line.split_once("=>") else { continue };
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        seen = true;
        if let Some((head, _)) = pattern.split_once("::") {
            let head = head.trim();
            if !head.is_empty() {
                return Some(format!("{head}::"));
            }
        }
    }
    // Arms exist and none of them qualifies, so the constructors are in scope
    // unqualified and a bare pattern is what this file writes.
    seen.then(String::new)
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
        let fixes = for_diagnostic("", Some("discarded-result"), range(), "db.execute(sql)!;", None);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].replacement, "let _ = db.execute(sql)!;");
    }

    #[test]
    fn the_renamed_keyword_is_offered_the_rename() {
        let fixes = for_diagnostic("`export` is spelled `pub`", None, range(), "export", None);
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
            None,
        );
        assert_eq!(fixes[0].replacement, "const");
    }

    /// **A lint whose fix is a judgement gets no action**, which is the rule
    /// this file exists to hold: an action applied by somebody who read four
    /// words of the message must not guess.
    #[test]
    fn a_lint_that_needs_a_decision_is_offered_nothing() {
        assert!(for_diagnostic("", Some("unused-capability"), range(), "", None).is_empty());
        assert!(for_diagnostic("", Some("reference-cycle"), range(), "", None).is_empty());
    }

    #[test]
    fn an_ordinary_type_error_is_offered_nothing() {
        assert!(for_diagnostic("expected `Int`, found `String`", None, range(), "x", None)
            .is_empty());
    }

    /// A signature over one line, with the offsets read out of it rather than
    /// counted by hand -- a test that miscounts a column proves nothing.
    fn signature(text: &str) -> (String, Signature) {
        let with_row = text.find("with ").map(|at| {
            let start = at + "with ".len();
            let end = text[start..].find('}').expect("a with row ends") + start + 1;
            Row {
                range: TextRange::new((start as u32).into(), (end as u32).into()),
                text: text[start..end].to_string(),
            }
        });
        let raises_row = text.find("raises ").map(|at| {
            let start = at + "raises ".len();
            let end = text[start..].find(" {").map_or(text.len(), |o| o + start);
            Row {
                range: TextRange::new((start as u32).into(), (end as u32).into()),
                text: text[start..end].to_string(),
            }
        });
        let arrow = text.find("-> ").expect("a return type") + "-> ".len();
        let clauses_at = arrow + text[arrow..].find(' ').unwrap_or(text.len() - arrow);
        (
            text.to_string(),
            Signature { clauses_at: (clauses_at as u32).into(), with_row, raises_row },
        )
    }

    /// What an editor would end up with, so the assertions read as source.
    fn applied(text: &str, fix: &Fix) -> String {
        let mut out = text.to_string();
        out.replace_range(
            usize::from(fix.range.start())..usize::from(fix.range.end()),
            &fix.replacement,
        );
        out
    }

    const NEEDS_DB: &str = "`load` needs `db: Db`, which this function does not require";
    const NEEDS_ERR: &str = "`load` needs `DbError`, which this function does not raise";

    #[test]
    fn a_missing_capability_is_added_to_the_row_that_is_there() {
        let (text, sig) = signature("fn main() -> Int with { log: Log } { load() }");
        let fixes = for_diagnostic(NEEDS_DB, None, range(), "load()", Some(&sig));
        assert_eq!(fixes.len(), 1);
        assert_eq!(applied(&text, &fixes[0]), "fn main() -> Int with { log: Log, db: Db } { load() }");
    }

    #[test]
    fn a_function_with_no_with_clause_is_given_one() {
        let (text, sig) = signature("fn main() -> Int { load() }");
        let fixes = for_diagnostic(NEEDS_DB, None, range(), "load()", Some(&sig));
        assert_eq!(applied(&text, &fixes[0]), "fn main() -> Int with { db: Db } { load() }");
    }

    /// **The clause lands before an existing `raises`**, which is the order
    /// every signature in `std` is written in.
    #[test]
    fn a_with_clause_goes_in_front_of_the_raises_clause() {
        let (text, sig) = signature("fn main() -> Int raises IoError { load() }");
        let fixes = for_diagnostic(NEEDS_DB, None, range(), "load()", Some(&sig));
        assert_eq!(
            applied(&text, &fixes[0]),
            "fn main() -> Int with { db: Db } raises IoError { load() }"
        );
    }

    /// An empty row takes no comma, and a row that is only a tail takes none
    /// either -- `{, db: Db }` and `{ 'e |, db: Db }` are both nonsense.
    #[test]
    fn an_empty_row_is_not_given_a_leading_comma() {
        for (before, after) in [
            ("fn main() -> Int with {} { load() }", "fn main() -> Int with { db: Db } { load() }"),
            (
                "fn main() -> Int with { 'e | } { load() }",
                "fn main() -> Int with { 'e | db: Db } { load() }",
            ),
        ] {
            let (text, sig) = signature(before);
            let fixes = for_diagnostic(NEEDS_DB, None, range(), "load()", Some(&sig));
            assert_eq!(applied(&text, &fixes[0]), after, "from {before}");
        }
    }

    #[test]
    fn a_missing_error_becomes_another_term_of_the_union() {
        let (text, sig) = signature("fn main() -> Int raises 'e + IoError { load() }");
        let fixes = for_diagnostic(NEEDS_ERR, None, range(), "load()", Some(&sig));
        assert_eq!(
            applied(&text, &fixes[0]),
            "fn main() -> Int raises 'e + IoError + DbError { load() }"
        );
    }

    #[test]
    fn a_function_that_raises_nothing_is_given_a_raises_clause() {
        let (text, sig) = signature("fn main() -> Int { load() }");
        let fixes = for_diagnostic(NEEDS_ERR, None, range(), "load()", Some(&sig));
        assert_eq!(applied(&text, &fixes[0]), "fn main() -> Int raises DbError { load() }");
    }

    /// **Nothing is offered without a signature to edit**, which is the state a
    /// diagnostic at module level arrives in.
    #[test]
    fn a_missing_clause_outside_a_function_is_offered_nothing() {
        assert!(for_diagnostic(NEEDS_DB, None, range(), "load()", None).is_empty());
    }
}
