//! Writing out the trait members an impl has not written.
//!
//! **The transcription is the work.** `this impl is missing `cmp`` names the
//! member; what it takes and what it answers live in a trait declaration in
//! another file, written against `Self` rather than against the type being
//! implemented. Copying that across by hand is the sort of task where one
//! wrong character produces a second error about the signature not matching,
//! and then a third about the one after it.
//!
//! Everything here is text: the signature is copied out of the trait's own
//! source rather than rendered from a type, so what lands in the impl is what
//! the trait author wrote, with `Self` swapped and a `todo()` body. A rendered
//! signature would normalise the spacing, drop the parameter names, and print
//! a row in whatever order the checker happens to hold it.

use text_size::{TextRange, TextSize};

/// The members a `this impl is missing` message names, and the trait they came
/// from.
///
/// The message is built by `check_methods` in `khora-types`, which joins the
/// names with `` `, ` `` and ends with the trait -- so both halves are read
/// back out of it rather than recomputed.
pub fn missing(message: &str) -> Option<(Vec<String>, String)> {
    let rest = message.strip_prefix("this impl is missing `")?;
    let (names, owner) = rest.split_once("` from `")?;
    let owner = owner.strip_suffix('`')?;
    if names.is_empty() || owner.is_empty() {
        return None;
    }
    Some((names.split("`, `").map(str::to_string).collect(), owner.to_string()))
}

/// One trait member's declaration, turned into an impl's version of it.
///
/// The `;` that ends a declaration becomes a body, and `Self` becomes the type
/// the impl is for. The substitution is on whole words, so a type called
/// `SelfTest` is left alone.
pub fn body_for(declaration: &str, me: &str) -> String {
    let signature = declaration.trim().trim_end_matches(';').trim_end();
    format!("{} {{ todo() }}", substitute_self(signature, me))
}

/// `Self` replaced by `me`, wherever it stands as a word on its own.
fn substitute_self(text: &str, me: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("Self") {
        let before = &rest[..at];
        let after = &rest[at + 4..];
        let bounded = !before.chars().next_back().is_some_and(is_word)
            && !after.chars().next().is_some_and(is_word);
        out.push_str(before);
        out.push_str(if bounded { me } else { "Self" });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Whether a character can be part of a name, which is what makes `Self` in
/// `SelfTest` a different word.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where the new members go, and what to put there.
///
/// Just inside the impl's closing brace, indented one step past the impl's own
/// line. The blank line in front is what the formatter would put between two
/// members anyway, and leaving it out makes the first written member run into
/// the last hand-written one.
pub fn insertion(text: &str, imp: TextRange, members: &[String]) -> Option<(TextRange, String)> {
    if members.is_empty() {
        return None;
    }
    let close = usize::from(imp.start())
        + text.get(usize::from(imp.start())..usize::from(imp.end()))?.rfind('}')?;

    // The impl's own indentation, so the members line up one step inside it.
    let start = usize::from(imp.start());
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let outer = &text[line_start..start];
    if !outer.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    let inner = format!("{outer}  ");

    // An impl written on one line -- `impl Eq for P { }` -- has no line for a
    // member to go on, so one is made. An impl already spread over lines keeps
    // its shape and the insertion lands on the line above the brace.
    let empty = text[..close].trim_end().ends_with('{');
    let mut written = String::new();
    for member in members {
        if !empty || !written.is_empty() {
            written.push('\n');
        }
        written.push('\n');
        written.push_str(&inner);
        written.push_str(member);
    }
    written.push('\n');
    written.push_str(outer);

    // Back over the whitespace already in front of the brace, so the result is
    // not the new lines plus whatever spacing was there.
    let from = text[..close].trim_end().len();
    Some((TextRange::new(TextSize::new(from as u32), TextSize::new(close as u32)), written))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_gives_up_its_names_and_its_trait() {
        assert_eq!(
            missing("this impl is missing `cmp` from `Ord`"),
            Some((vec!["cmp".to_string()], "Ord".to_string()))
        );
        assert_eq!(
            missing("this impl is missing `eq`, `ne` from `Eq`"),
            Some((vec!["eq".to_string(), "ne".to_string()], "Eq".to_string()))
        );
    }

    /// A different sentence with the same words in it is not this one.
    #[test]
    fn another_missing_message_is_not_read_as_members() {
        assert_eq!(missing("this `Point` is missing `y`"), None);
    }

    #[test]
    fn a_declaration_becomes_a_body_with_self_spelled_out() {
        assert_eq!(
            body_for("fn eq(self, other: Self) -> Bool;", "Point"),
            "fn eq(self, other: Point) -> Bool { todo() }"
        );
    }

    /// **Whole words only**, so a type whose name starts with `Self` survives.
    #[test]
    fn a_longer_name_beginning_with_self_is_left_alone() {
        assert_eq!(
            body_for("fn check(self) -> SelfTest;", "Point"),
            "fn check(self) -> SelfTest { todo() }"
        );
    }

    #[test]
    fn a_member_lands_inside_the_braces() {
        let text = "impl Eq for Point {\n}\n";
        let (range, written) =
            insertion(text, TextRange::new(0.into(), 21.into()), &["fn eq() {}".to_string()])
                .expect("an insertion");
        let mut out = text.to_string();
        out.replace_range(usize::from(range.start())..usize::from(range.end()), &written);
        assert_eq!(out, "impl Eq for Point {\n  fn eq() {}\n}\n");
    }

    /// A member written after one that is already there gets the blank line
    /// between them that the formatter would have put in.
    #[test]
    fn a_member_after_an_existing_one_keeps_them_apart() {
        let text = "impl Eq for Point {\n  fn ne() {}\n}\n";
        let (range, written) = insertion(
            text,
            TextRange::new(0.into(), (text.len() as u32 - 1).into()),
            &["fn eq() {}".to_string()],
        )
        .expect("an insertion");
        let mut out = text.to_string();
        out.replace_range(usize::from(range.start())..usize::from(range.end()), &written);
        assert_eq!(out, "impl Eq for Point {\n  fn ne() {}\n\n  fn eq() {}\n}\n");
    }
}
