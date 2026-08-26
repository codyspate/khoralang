//! String in, string out. Nothing here reads a file.

use super::*;

fn module(source: &str) -> Module {
    let parsed = khora_syntax::parse(source);
    assert!(parsed.ok(), "the fixture should parse: {:?}", parsed.errors());
    module_of(&parsed.source_file())
}

fn only(source: &str) -> Item {
    let m = module(source);
    assert_eq!(m.items.len(), 1, "expected exactly one item: {:?}", m.items);
    m.items.into_iter().next().unwrap()
}

// --- what counts as documentation -------------------------------------------

#[test]
fn a_doc_comment_attaches_to_the_declaration_below_it() {
    let item = only("module m;\n\n/// The first line.\n/// The second.\nexport fn f() -> Int { 1 }\n");
    assert_eq!(item.doc, ["The first line.", "The second."]);
}

/// The distinction the whole convention rests on: `///` is published and `//`
/// is a note to whoever maintains the file.
#[test]
fn a_plain_comment_is_not_documentation() {
    let item = only("module m;\n\n// A note to a maintainer.\nexport fn f() -> Int { 1 }\n");
    assert!(item.doc.is_empty(), "a `//` comment should not be published: {:?}", item.doc);
}

/// A `//` between the block and the item ends it, which is what stops an
/// internal note from being pulled in behind real documentation.
#[test]
fn a_plain_comment_underneath_ends_the_block() {
    let item = only(
        "module m;\n\n/// Published.\n// TODO: this is slow.\nexport fn f() -> Int { 1 }\n",
    );
    assert!(item.doc.is_empty(), "the note broke the run: {:?}", item.doc);
}

/// Two blocks separated by a blank line are two different things -- the upper
/// one is usually about the item above, or about the section.
#[test]
fn a_blank_line_ends_the_block() {
    let item = only(
        "module m;\n\n/// About something else.\n\n/// About this.\nexport fn f() -> Int { 1 }\n",
    );
    assert_eq!(item.doc, ["About this."]);
}

/// A run of slashes is a divider somebody drew.
#[test]
fn a_divider_is_not_documentation() {
    let item =
        only("module m;\n\n////////////////\nexport fn f() -> Int { 1 }\n");
    assert!(item.doc.is_empty(), "{:?}", item.doc);
}

#[test]
fn one_space_after_the_marker_is_removed_and_no_more() {
    let item = only(
        "module m;\n///     indented, as in a code block\n///not indented\nexport fn f() -> Int { 1 }\n",
    );
    assert_eq!(item.doc, ["    indented, as in a code block", "not indented"]);
}

// --- module documentation ---------------------------------------------------

#[test]
fn module_docs_come_from_bang_comments() {
    let m = module("module m;\n\n//! What this module is.\n//!\n//! More.\n\nexport fn f() -> Int { 1 }\n");
    assert_eq!(m.doc, ["What this module is.", "", "More."]);
    assert_eq!(m.path.as_deref(), Some("m"));
}

/// `std` writes its narration after the imports, and a reader might reasonably
/// write it before them. Neither position is wrong.
#[test]
fn module_docs_may_sit_either_side_of_the_imports() {
    let before = module("module m;\n//! Above.\nimport std::core::{Int};\n\nexport fn f() -> Int { 1 }\n");
    let after = module("module m;\nimport std::core::{Int};\n//! Below.\n\nexport fn f() -> Int { 1 }\n");
    assert_eq!(before.doc, ["Above."]);
    assert_eq!(after.doc, ["Below."]);
}

/// After a declaration, a `//!` is describing something narrower than the
/// module, so it is not the module's text.
#[test]
fn a_bang_comment_after_a_declaration_is_not_the_modules() {
    let m = module("module m;\n//! Mine.\nexport fn f() -> Int { 1 }\n//! Not mine.\n");
    assert_eq!(m.doc, ["Mine."]);
}

#[test]
fn a_module_path_with_several_segments_survives() {
    assert_eq!(module("module std::net::socket;\n").path.as_deref(), Some("std::net::socket"));
}

// --- what is exported -------------------------------------------------------

#[test]
fn an_unexported_item_is_not_documented() {
    let m = module("module m;\n\n/// Private.\nfn f() -> Int { 1 }\n\n/// Public.\nexport fn g() -> Int { 2 }\n");
    assert_eq!(m.items.len(), 1);
    assert_eq!(m.items[0].name, "g");
}

/// The rule that makes `std` documentable at all: `export` means nothing inside
/// an impl, so an impl's methods follow the visibility of the type.
#[test]
fn methods_of_an_exported_type_are_documented_without_the_keyword() {
    let m = module(
        "module m;\n\n\
         export type Counter = { n: Int };\n\n\
         impl Counter {\n  /// How many.\n  fn count(self) -> Int { self.n }\n}\n",
    );
    let methods = m.items.iter().find(|i| i.kind == Kind::Methods).expect("an impl block");
    assert_eq!(methods.name, "Counter");
    assert_eq!(methods.members.len(), 1);
    assert_eq!(methods.members[0].name, "count");
    assert_eq!(methods.members[0].doc, ["How many."]);
}

#[test]
fn methods_of_an_unexported_type_are_not_documented() {
    let m = module(
        "module m;\n\ntype Hidden = { n: Int };\n\nimpl Hidden {\n  fn count(self) -> Int { self.n }\n}\n",
    );
    assert!(m.items.is_empty(), "{:?}", m.items);
}

/// A generic type's impl is written `impl<A> List<A>` and the declaration is
/// `export type List<A>`; matching on the base name is what connects them.
#[test]
fn a_generic_impl_is_matched_to_its_declaration() {
    let m = module(
        "module m;\n\n\
         export type List<A> = | Nil | Cons(head: A, tail: List<A>);\n\n\
         impl<A> List<A> {\n  /// How many.\n  fn length(self) -> Int { 0 }\n}\n",
    );
    let methods = m.items.iter().find(|i| i.kind == Kind::Methods).expect("an impl block");
    assert_eq!(methods.name, "List<A>");
    assert_eq!(methods.members[0].name, "length");
}

#[test]
fn a_trait_implementation_is_named_by_both_halves() {
    let m = module(
        "module m;\n\n\
         export type Money = { units: Int };\n\
         export trait Show { fn show(self) -> String; }\n\n\
         impl Show for Money {\n  /// Digits.\n  fn show(self) -> String { \"\" }\n}\n",
    );
    let found = m.items.iter().find(|i| i.kind == Kind::TraitImpl).expect("a trait impl");
    assert_eq!(found.name, "Show for Money");
}

/// An impl of somebody else's trait for somebody else's type is not this
/// module's API, whatever file it happens to be written in.
#[test]
fn an_impl_for_a_type_from_elsewhere_is_not_documented() {
    let m = module(
        "module m;\n\nimpl Show for Int {\n  fn show(self) -> String { \"\" }\n}\n",
    );
    assert!(m.items.is_empty(), "{:?}", m.items);
}

// --- signatures -------------------------------------------------------------

#[test]
fn a_function_is_its_signature_and_never_its_body() {
    let item = only("module m;\nexport fn add(a: Int, b: Int) -> Int { a + b }\n");
    assert_eq!(item.signature, "export fn add(a: Int, b: Int) -> Int");
}

/// A signature wrapped over several lines is a formatting decision about a
/// source file. A reference wants one line to scan.
#[test]
fn a_wrapped_signature_is_collapsed() {
    let item = only(
        "module m;\nexport fn open(\n  host: String,\n  port: Int,\n) -> Result<Connection, PgError> { todo }\n",
    );
    assert_eq!(item.signature, "export fn open(host: String, port: Int) -> Result<Connection, PgError>");
}

#[test]
fn effect_rows_and_generics_survive() {
    let item = only(
        "module m;\nexport fn run<A>(f: A) -> A with { io: Io } raises IoError { f }\n",
    );
    assert_eq!(item.signature, "export fn run<A>(f: A) -> A with { io: Io } raises IoError");
}

/// A type's shape is the documentation, so it is printed as written rather than
/// squeezed onto one line.
#[test]
fn a_type_keeps_the_shape_it_was_written_in() {
    let item = only("module m;\nexport type Cell =\n  | Null\n  | Text(String)\n  | Number(Int);\n");
    assert_eq!(item.signature, "export type Cell =\n  | Null\n  | Text(String)\n  | Number(Int);");
}

/// A `///` inside a record belongs to its field and is emitted there. Leaving
/// it in the type's own block would print it twice.
#[test]
fn comments_inside_a_type_are_not_repeated_in_its_signature() {
    let item = only(
        "module m;\nexport type Row = {\n  /// The cells.\n  cells: List<Cell>,\n};\n",
    );
    assert!(!item.signature.contains("The cells"), "{}", item.signature);
    assert_eq!(item.members.len(), 1);
    assert_eq!(item.members[0].name, "cells");
    assert_eq!(item.members[0].doc, ["The cells."]);
}

#[test]
fn a_variant_case_carries_its_own_documentation() {
    let item = only(
        "module m;\nexport type E =\n  /// Nothing answered.\n  | Unreachable(String)\n  /// It said no.\n  | Refused(String);\n",
    );
    let names: Vec<&str> = item.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Unreachable", "Refused"]);
    assert_eq!(item.members[0].doc, ["Nothing answered."]);
    assert_eq!(item.members[1].doc, ["It said no."]);
    assert_eq!(item.members[1].signature, "| Refused(String)");
}

#[test]
fn a_trait_shows_its_header_and_its_functions_separately() {
    let item = only(
        "module m;\nexport trait Ord: Eq {\n  /// Compare.\n  fn cmp(self, other: Self) -> Ordering;\n}\n",
    );
    assert_eq!(item.signature, "export trait Ord: Eq");
    assert_eq!(item.members.len(), 1);
    assert_eq!(item.members[0].signature, "fn cmp(self, other: Self) -> Ordering");
}

#[test]
fn an_associated_type_is_a_member() {
    let item = only(
        "module m;\nexport trait Iter {\n  /// What it yields.\n  type Item;\n  fn next(self) -> Option<Self::Item>;\n}\n",
    );
    let assoc = item.members.iter().find(|m| m.kind == Kind::AssocType).expect("an assoc type");
    assert_eq!(assoc.name, "Item");
    assert_eq!(assoc.doc, ["What it yields."]);
}

#[test]
fn a_constant_is_printed_whole() {
    let item = only("module m;\n/// The digits.\nexport const digits = \"0123456789\";\n");
    assert_eq!(item.name, "digits");
    assert_eq!(item.kind, Kind::Const);
    assert_eq!(item.signature, "export const digits = \"0123456789\";");
}

/// The comment stripper counts quotes rather than assuming a `//` is a comment,
/// because getting it wrong would silently corrupt a value.
#[test]
fn a_double_slash_inside_a_string_is_not_a_comment() {
    let item = only("module m;\nexport const url = \"https://example.com\";\n");
    assert_eq!(item.signature, "export const url = \"https://example.com\";");
}

// --- the page ---------------------------------------------------------------

#[test]
fn a_page_has_frontmatter_and_a_section_per_kind() {
    let page = markdown(&module(
        "module std::thing;\n\n//! What it is.\n\n\
         /// A number holder.\nexport type N = { n: Int };\n\n\
         /// Adds.\nexport fn add(a: Int) -> Int { a }\n",
    ));
    assert!(page.starts_with("---\ntitle: std::thing\n"), "{page}");
    assert!(page.contains("description: \"What it is\""), "{page}");
    assert!(page.contains("\n## Types\n"), "{page}");
    assert!(page.contains("\n## Functions\n"), "{page}");
    // Types before functions: an argument type has to mean something first.
    assert!(page.find("## Types") < page.find("## Functions"), "{page}");
    assert!(page.contains("```khora\nexport fn add(a: Int) -> Int\n```"), "{page}");
}

/// A colon in the first sentence is a YAML document boundary if it is not
/// quoted, and this project's prose is full of them.
#[test]
fn a_description_with_a_colon_is_quoted() {
    let page = markdown(&module("module m;\n//! Two things: this, and that.\n"));
    assert!(page.contains("description: \"Two things: this, and that\""), "{page}");
}

/// A `# Heading` in a doc comment is a heading within that item. Emitted as
/// written it would sit above the page's own `##` sections.
#[test]
fn headings_inside_a_doc_comment_are_pushed_below_their_item() {
    let page = markdown(&module(
        "module m;\n\n/// Text.\n///\n/// # Why\n/// Because.\nexport fn f() -> Int { 1 }\n",
    ));
    assert!(page.contains("\n#### Why\n"), "a `#` under an `###` item should be `####`: {page}");
    assert!(!page.contains("\n# Why\n"), "{page}");
}

#[test]
fn a_hash_inside_a_fenced_block_is_left_alone() {
    let page = markdown(&module(
        "module m;\n\n/// Text.\n///\n/// ```\n/// # not a heading\n/// ```\nexport fn f() -> Int { 1 }\n",
    ));
    assert!(page.contains("\n# not a heading\n"), "{page}");
}

#[test]
fn a_module_with_nothing_exported_says_so() {
    let page = markdown(&module("module m;\n\nfn hidden() -> Int { 1 }\n"));
    assert!(page.contains("This module exports nothing."), "{page}");
}

/// A generated file that always differs from itself is a generated file nobody
/// can review.
#[test]
fn the_same_input_produces_the_same_page() {
    let source = "module m;\n//! Text.\n/// Doc.\nexport fn f() -> Int { 1 }\n";
    assert_eq!(markdown(&module(source)), markdown(&module(source)));
}

/// A doc comment is wrapped to fit a source file, so the first line ends
/// wherever eighty columns fell. Taking that as the description produced a
/// sentence cut in half, which looks deliberate and is worse than nothing.
#[test]
fn the_description_is_a_sentence_and_not_a_source_line() {
    let page = markdown(&module(
        "module m;\n\
         //! Exact decimal arithmetic, for the numbers that are counted\n\
         //! rather than measured. A second sentence, not wanted here.\n",
    ));
    assert!(
        page.contains(
            "description: \"Exact decimal arithmetic, for the numbers that are counted rather than measured\""
        ),
        "{page}"
    );
}

/// The page title is the `h1`, so a `#` in the module's own text is a sibling
/// of `## Types` rather than its parent.
#[test]
fn module_headings_sit_beside_the_api_sections() {
    let page = markdown(&module(
        "module m;\n//! Text.\n//!\n//! # Background\n//! Why.\n\nexport fn f() -> Int { 1 }\n",
    ));
    assert!(page.contains("\n## Background\n"), "{page}");
    assert!(page.contains("\n## Functions\n"), "{page}");
}

// --- several files, one module ----------------------------------------------

/// `std::net::socket` is written once per platform and exactly one is compiled.
/// A reference that documented whichever the directory walk reached last would
/// change when somebody renamed a file.
#[test]
fn files_sharing_a_module_path_become_one_page() {
    let linux = module("module p::sock;\n//! Sockets.\n/// Opens.\nexport fn open() -> Int { 1 }\n");
    let windows = module("module p::sock;\n/// Opens.\nexport fn open() -> Int { 1 }\n/// Only here.\nexport fn wsa() -> Int { 2 }\n");
    let merged = merge(vec![linux, windows]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].doc, ["Sockets."]);
    assert_eq!(merged[0].doc.len(), 1, "one block, said once");
    let names: Vec<&str> = merged[0].items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["open", "wsa"], "a function only one platform has still appears");
}

#[test]
fn merging_sorts_by_module_path_and_leaves_others_alone() {
    let merged = merge(vec![
        module("module p::z;\nexport fn a() -> Int { 1 }\n"),
        module("module p::a;\nexport fn a() -> Int { 1 }\n"),
    ]);
    let paths: Vec<&str> = merged.iter().filter_map(|m| m.path.as_deref()).collect();
    assert_eq!(paths, ["p::a", "p::z"]);
}

/// Each variant's block describes that variant. Publishing one of them as the
/// module's own would be publishing Linux's notes on a page a Windows reader
/// is reading, and which one it was would depend on alphabetical order.
#[test]
fn every_platforms_module_block_is_kept() {
    let linux = module("module p::sock;\n//! Sockets, on Linux.\n//!\n//! `epoll`.\n");
    let windows = module("module p::sock;\n//! Sockets, on Windows.\n//!\n//! Winsock.\n");
    let merged = merge(vec![linux, windows]);
    assert_eq!(
        merged[0].doc,
        [
            "Sockets, on Linux.",
            "",
            "`epoll`.",
            "",
            "Sockets, on Windows.",
            "",
            "Winsock."
        ]
    );
}

/// A module whose variants say the same thing should read as though it were
/// one file.
#[test]
fn an_identical_block_is_not_repeated() {
    let a = module("module p::sock;\n//! Sockets.\n");
    let b = module("module p::sock;\n//! Sockets.\n");
    assert_eq!(merge(vec![a, b])[0].doc, ["Sockets."]);
}

/// These pages are `.md`. `{/* */}` is MDX syntax, which plain markdown
/// renders as literal text -- the note meant for whoever opens the file would
/// have appeared at the top of the page instead.
#[test]
fn the_generated_by_note_is_a_markdown_comment() {
    let page = markdown(&module("module m;\n//! Text.\n"));
    assert!(page.contains("<!-- Generated by `khora doc`"), "{page}");
    assert!(!page.contains("{/*"), "{page}");
}
