//! What the standard library offers, as prose an agent can read.
//!
//! # Why the source text rather than the inferred signature
//!
//! `khora_types::Signature` holds everything: parameter types, the requirement
//! row, the failure row. Rendering it back into something readable means
//! reimplementing the syntax, in a second place, that will drift from the
//! first.
//!
//! The declaration as it was written cannot drift, and it is what a person
//! reading `std` would see. So an entry is a slice of the file: the `///` lines
//! above the item, and the declaration up to the point where the body starts.
//!
//! The cost is that this is only as good as `std`'s own formatting, which is
//! checked by `khora fmt --check` in the baseline. That is a reasonable thing
//! to depend on.

use khora_db::{Db, SourceFile};
use khora_hir::{ItemKind, ModulePath};

/// One public item, as an agent should see it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    /// `std::core`, as written in an `import`.
    pub module: String,
    /// What it is called, without its module.
    pub name: String,
    /// `function`, `type`, `trait`, `effect`, `constant`.
    pub kind: String,
    /// The declaration, without its body.
    pub signature: String,
    /// The `///` lines above it, joined, with the slashes removed.
    pub doc: String,
}

/// Every public item in a file.
///
/// Private items are left out on purpose: an agent that learns about one will
/// write code that does not compile, and the error it gets back — "not
/// exported" — is a worse teacher than never having seen it.
pub fn public_items(db: &dyn Db, file: SourceFile) -> Vec<Entry> {
    let map = khora_hir::item_map(db, file);
    let Some(module) = map.module.as_ref() else { return Vec::new() };
    let text = file.text(db);

    let mut out = Vec::new();
    for item in map.items.iter().filter(|i| i.is_public) {
        let start = usize::from(item.range.start()).min(text.len());
        let end = usize::from(item.range.end()).min(text.len());
        out.push(Entry {
            module: spell(module),
            name: item.name.clone(),
            kind: describe(item.kind).to_string(),
            signature: signature_of(&text[start..end], item.kind),
            doc: doc_above(text, start),
        });
    }
    out
}

/// `std::core`, which is how an `import` spells it.
fn spell(path: &ModulePath) -> String {
    path.segments().join("::")
}

fn describe(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Type => "type",
        ItemKind::Trait => "trait",
        ItemKind::Effect => "effect",
        ItemKind::Function => "function",
        ItemKind::Const => "constant",
        ItemKind::Context => "context",
    }
}

/// The declaration without whatever follows it.
///
/// A function is cut at its body, because the body is not the interface. A type
/// or a trait keeps everything: its cases and its methods *are* the interface,
/// and they are short. Anything long is truncated rather than dumped, since an
/// agent reading a hundred-line record learns nothing it did not learn in the
/// first ten.
fn signature_of(text: &str, kind: ItemKind) -> String {
    let cut = match kind {
        ItemKind::Function => text
            .find(['{', '='])
            .map(|at| text[..at].trim_end())
            .unwrap_or(text),
        _ => text,
    };
    let trimmed = cut.trim();

    const LIMIT: usize = 40;
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= LIMIT {
        return trimmed.to_string();
    }
    let mut out = lines[..LIMIT].join("\n");
    out.push_str(&format!("\n  // ... {} more lines", lines.len() - LIMIT));
    out
}

/// The `///` lines immediately above `start`, with the slashes taken off.
///
/// Contiguous only: a blank line ends the comment, because a doc comment that
/// is separated from its item by a blank line is documenting something else.
fn doc_above(text: &str, start: usize) -> String {
    let mut before = &text[..start];

    // If the item is indented, `before` ends mid-line with that indentation.
    // Drop it — but only when it really is a partial line, because a blank
    // line is a separator and must end the comment rather than be skipped.
    if !before.ends_with('\n') {
        before = &before[..before.rfind('\n').map_or(0, |i| i + 1)];
    }

    let mut lines: Vec<&str> = Vec::new();
    for line in before.lines().rev() {
        let trimmed = line.trim();
        match trimmed.strip_prefix("///") {
            Some(rest) => lines.push(rest.trim()),
            // An attribute or a `derive` sits between the comment and the item
            // and does not end it.
            None if trimmed.starts_with("derive(") => continue,
            None => break,
        }
    }
    lines.reverse();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_doc_comment_above_an_item_is_found() {
        let text = "module m;\n\n/// One.\n/// Two.\npub fn f() -> Int { 1 }\n";
        let at = text.find("pub").expect("the item");
        assert_eq!(doc_above(text, at), "One.\nTwo.");
    }

    /// A blank line means the comment belongs to whatever was above it.
    #[test]
    fn a_blank_line_ends_a_doc_comment() {
        let text = "module m;\n\n/// Not mine.\n\npub fn f() -> Int { 1 }\n";
        let at = text.find("pub").expect("the item");
        assert_eq!(doc_above(text, at), "");
    }

    #[test]
    fn an_item_with_no_doc_comment_has_none() {
        let text = "module m;\n\npub fn f() -> Int { 1 }\n";
        let at = text.find("pub").expect("the item");
        assert_eq!(doc_above(text, at), "");
    }

    /// The body is not the interface.
    #[test]
    fn a_function_is_cut_at_its_body() {
        let decl = "pub fn add(a: Int, b: Int) -> Int {\n  a + b\n}";
        assert_eq!(signature_of(decl, ItemKind::Function), "pub fn add(a: Int, b: Int) -> Int");
    }

    #[test]
    fn a_declaration_with_no_body_survives_whole() {
        let decl = "pub fn opaque(a: Int) -> Int;";
        assert_eq!(signature_of(decl, ItemKind::Function), "pub fn opaque(a: Int) -> Int;");
    }

    /// A type's cases *are* its interface, so they stay.
    #[test]
    fn a_type_keeps_its_cases() {
        let decl = "pub type Option<A> = | Some(value: A) | None;";
        assert_eq!(signature_of(decl, ItemKind::Type), decl);
    }

    #[test]
    fn something_enormous_is_truncated_rather_than_dumped() {
        let decl = "  x: Int,\n".repeat(200);
        let out = signature_of(&decl, ItemKind::Type);
        assert!(out.lines().count() < 50, "{} lines", out.lines().count());
        assert!(out.contains("more lines"), "{out}");
    }
}
