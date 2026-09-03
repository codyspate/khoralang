//! Bringing a name into scope, from the diagnostic that says it is not.
//!
//! **The compiler already knows the answer.** `cannot find `List` in this
//! scope` is emitted with the module list in hand, and one of the messages
//! goes as far as naming it — `` `[a, b, c]` builds a `List`; import it from
//! `std::core` ``. Until this existed the reader read that sentence, scrolled
//! to the top of the file, and typed the import out. It is the most-used
//! refactor in any typed language and it fires on nearly every new line that
//! reaches for `std`.
//!
//! # Found by looking, not by reading the message
//!
//! Five different diagnostics mean "this name is not in scope" and they are
//! worded five different ways. Matching each would be a list to keep in step
//! with wordings that `khora-types` is free to improve, and the two fixes in
//! `fixes.rs` that *do* match on a message have a test quoting the sentence
//! whole for exactly that reason.
//!
//! So nothing here reads the message for meaning. Every backticked identifier
//! in it is looked up in what the modules export, and a name that resolves is
//! offered. A message that mentions a name incidentally costs one lookup and
//! offers nothing, because nothing exports it.

use khora_db::{Db, SourceFile};
use khora_syntax::ast::{AstNode, ImportDecl};
use khora_syntax::{SyntaxKind, SyntaxNode};
use text_size::{TextRange, TextSize};

use crate::fixes::Fix;

/// Every name a diagnostic mentions that could be a type or a function.
///
/// The first segment of a backticked path, because `List::length` is not in
/// scope when `List` is not: importing the owner is what fixes it.
pub fn mentioned(message: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for quoted in message.split('`').skip(1).step_by(2) {
        let qualified = quoted.contains("::");
        let head = quoted.split("::").next().unwrap_or_default();
        let head = head.split('(').next().unwrap_or_default().trim();
        if head.is_empty() || out.iter().any(|seen| seen == head) {
            continue;
        }
        // **A module path is not a name to import.** `std::core` and
        // `List::length` are both qualified and only one of them has an owner
        // worth bringing into scope; a module's segments are lowercase and a
        // type's name is not, which is the whole of the difference. Without
        // this the hint that *names the module* also asked whether anything
        // exports `std`.
        if qualified && !head.starts_with(|c: char| c.is_uppercase()) {
            continue;
        }
        // An identifier, and one that could name an exported item. A lowercase
        // start is a local or a function; both are worth offering, since a
        // module exports functions too.
        let shaped = head.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && head.chars().all(|c| c.is_alphanumeric() || c == '_');
        if shaped {
            out.push(head.to_string());
        }
    }
    out
}

/// The modules that export `name`, in the order they should be offered.
///
/// `std` first and then by path, so the answer to an ambiguous name is stable
/// between runs and the likeliest one is at the top. Nothing here guesses
/// which the reader meant — each is offered and the choice is theirs.
pub fn providers(db: &dyn Db, files: &[SourceFile], name: &str, skip: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for file in files {
        let api = khora_hir::module_api(db, *file);
        // A file with no `module` line declares nothing anybody can import.
        let Some(module) = api.module.as_ref().map(written) else { continue };
        if module == skip || out.iter().any(|seen| seen == &module) {
            continue;
        }
        if api.items.iter().any(|item| item.is_public && item.name == name) {
            out.push(module);
        }
    }
    out.sort_by_key(|module| (!module.starts_with("std"), module.clone()));
    out
}

/// A module path as an `import` spells it.
///
/// **`Display` is not this.** `ModulePath` prints with dots -- `std.core` --
/// which is what diagnostics say and what nothing in the language accepts. An
/// action built from it produced `import std.core::{List};`, which does not
/// parse, and compared unequal to the `std::core` already imported two lines
/// above, so it also failed to merge and added a second import of the same
/// module. One wrong separator, three wrong behaviours.
pub fn written(path: &khora_hir::ModulePath) -> String {
    path.segments().join("::")
}

/// The edit that brings `name` in from `module`.
///
/// Two shapes, and which one applies is a question about the file rather than
/// about the name: a module already imported gains an entry in the braces it
/// already has, and one that is not gains a line.
pub fn edit(tree: &SyntaxNode, text: &str, module: &str, name: &str) -> Option<Fix> {
    edit_among(&declared_in(tree), tree, text, module, name)
}

/// Every `import` in a file, in the order they are written.
///
/// **Collected once and handed back.** Completion asks for an edit per
/// candidate and there may be hundreds on one keystroke; walking the whole file
/// for each of them is the same answer computed hundreds of times.
pub fn declared_in(tree: &SyntaxNode) -> Vec<ImportDecl> {
    tree.descendants()
        .filter(|node| node.kind() == SyntaxKind::IMPORT_DECL)
        .filter_map(ImportDecl::cast)
        .collect()
}

/// `edit`, given the imports already found.
pub fn edit_among(
    existing: &[ImportDecl],
    tree: &SyntaxNode,
    text: &str,
    module: &str,
    name: &str,
) -> Option<Fix> {
    for decl in existing {
        let path = decl.path().map(|p| p.text_path()).unwrap_or_default();
        if path != module {
            continue;
        }
        // A glob already brings everything, so there is nothing to add and
        // offering an edit that changes nothing would be worse than silence.
        if decl.is_glob() {
            return None;
        }
        let mut names: Vec<String> = decl.items().filter_map(|i| i.name()?.ident()).collect();
        if names.iter().any(|seen| seen == name) {
            return None;
        }
        names.push(name.to_string());
        names.sort();
        let list = list_range(&decl.syntax().clone())?;
        return Some(Fix {
            title: format!("Import `{name}` from `{module}`"),
            range: list,
            replacement: format!("{{{}}}", names.join(", ")),
        });
    }

    // No import of this module yet, so a whole line. Placed among the imports
    // in sorted order where there are any, and after the `module` declaration
    // where there are none -- which is where `khora fmt` would put it either
    // way, so accepting the fix does not leave the file needing a format.
    let mut line = format!("import {module}::{{{name}}};\n");
    let at = match existing.iter().find(|decl| {
        decl.path().map(|p| p.text_path()).unwrap_or_default().as_str() > module
    }) {
        Some(later) => start_of_line(text, later.syntax().text_range().start()),
        None => match existing.last() {
            Some(last) => after_line(text, last.syntax().text_range().end()),
            None => {
                // The first import in the file, so it brings the blank line
                // that separates the import block from the declarations under
                // it. Without one the fix welds `import ..;` to the `pub fn`
                // below -- which compiles, and reads as though the tool made a
                // mess on its way past.
                line.push('\n');
                after_module_declaration(tree, text)?
            }
        },
    };
    Some(Fix {
        title: format!("Import `{name}` from `{module}`"),
        range: TextRange::empty(at),
        replacement: line,
    })
}

/// The `{ .. }` of an import, which is the part an added name replaces.
fn list_range(decl: &SyntaxNode) -> Option<TextRange> {
    decl.descendants()
        .find(|node| node.kind() == SyntaxKind::IMPORT_LIST)
        .map(|list| list.text_range())
}

/// The start of the line `at` sits on.
fn start_of_line(text: &str, at: TextSize) -> TextSize {
    let upto = &text[..usize::from(at)];
    TextSize::from(upto.rfind('\n').map_or(0, |n| n + 1) as u32)
}

/// The start of the line after the one `at` ends on.
fn after_line(text: &str, at: TextSize) -> TextSize {
    let rest = &text[usize::from(at)..];
    let step = rest.find('\n').map_or(rest.len(), |n| n + 1);
    at + TextSize::from(step as u32)
}

/// Where the first import would go in a file that has none.
///
/// After the `module` line and the blank line under it, which is where every
/// file in `std` puts the first one.
fn after_module_declaration(tree: &SyntaxNode, text: &str) -> Option<TextSize> {
    let decl = tree.descendants().find(|node| node.kind() == SyntaxKind::MODULE_DECL)?;
    let mut at = after_line(text, decl.text_range().end());
    // Past a blank line if there is one, so the import joins the block rather
    // than sitting alone above it.
    if text[usize::from(at)..].starts_with('\n') {
        at += TextSize::from(1u32);
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file with `name` brought in from `module`, or `None` if there is
    /// nothing to do.
    fn applied(source: &str, module: &str, name: &str) -> Option<String> {
        let parse = khora_syntax::parse(source);
        let fix = edit(&parse.syntax(), source, module, name)?;
        let mut out = source.to_string();
        out.replace_range(
            usize::from(fix.range.start())..usize::from(fix.range.end()),
            &fix.replacement,
        );
        Some(out)
    }

    /// A module already imported gains an entry in the braces it has.
    ///
    /// Sorted, because `khora fmt` sorts them and a fix that leaves the file
    /// needing a format is half a fix.
    #[test]
    fn an_imported_module_gains_the_name() {
        let out = applied(
            "module m;\n\nimport std::core::{print};\n\nfn f() -> Int { 0 }\n",
            "std::core",
            "List",
        );
        assert_eq!(
            out.as_deref(),
            Some("module m;\n\nimport std::core::{List, print};\n\nfn f() -> Int { 0 }\n")
        );
    }

    /// A module not yet imported gains a line, in sorted position.
    #[test]
    fn a_new_module_gains_a_line_in_order() {
        let out = applied(
            "module m;\n\nimport std::core::{print};\n\nfn f() -> Int { 0 }\n",
            "std::decimal",
            "Decimal",
        );
        assert_eq!(
            out.as_deref(),
            Some(
                "module m;\n\nimport std::core::{print};\nimport std::decimal::{Decimal};\n\n\
                 fn f() -> Int { 0 }\n"
            )
        );

        let before = applied(
            "module m;\n\nimport std::decimal::{Decimal};\n\nfn f() -> Int { 0 }\n",
            "std::core",
            "List",
        );
        assert_eq!(
            before.as_deref(),
            Some(
                "module m;\n\nimport std::core::{List};\nimport std::decimal::{Decimal};\n\n\
                 fn f() -> Int { 0 }\n"
            ),
            "and before one that sorts after it"
        );
    }

    /// The first import in a file brings the blank line under it.
    ///
    /// Without it the fix welds the import to the declaration below, which
    /// compiles and reads as though the tool made a mess on its way past.
    #[test]
    fn the_first_import_is_separated_from_the_code() {
        let out = applied("module m;\n\nfn f() -> Int { 0 }\n", "std::core", "List");
        assert_eq!(
            out.as_deref(),
            Some("module m;\n\nimport std::core::{List};\n\nfn f() -> Int { 0 }\n")
        );
    }

    /// Nothing is offered when there is nothing to do.
    #[test]
    fn an_import_that_would_change_nothing_is_not_offered() {
        assert!(
            applied("module m;\n\nimport std::core::{List};\n", "std::core", "List").is_none(),
            "already imported"
        );
        assert!(
            applied("module m;\n\nimport std::core::*;\n", "std::core", "List").is_none(),
            "a glob already brings it"
        );
    }

    #[test]
    fn names_come_out_of_the_backticks() {
        assert_eq!(mentioned("cannot find `List` in this scope"), vec!["List"]);
        assert_eq!(
            mentioned("cannot resolve `List::length` in this scope"),
            vec!["List"],
            "the owner is what needs importing"
        );
        assert_eq!(
            mentioned("`[a, b, c]` builds a `List`; import it from `std::core`"),
            vec!["List"],
            "punctuation and paths are not identifiers"
        );
        assert!(mentioned("expected `;`").is_empty());
        assert!(mentioned("no backticks here").is_empty());
    }
}
