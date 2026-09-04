//! Words from other languages, in the position they would hold there.
//!
//! **`struct Point { .. }` is not a typo.** It is somebody writing the
//! language they already know, in a file they have just created, and
//! *expected a declaration* is true, unhelpful, and gives them nothing to
//! search for. The first thing a person does with an unfamiliar compiler is
//! type what they know and read what comes back; that message tells them the
//! line is wrong and not one thing about what would be right.
//!
//! `decls.rs` already had this shape for `export`, which was Khora's own older
//! spelling of `pub`. These are the same idea pointed outward.
//!
//! # What the message has to do
//!
//! Name the Khora word, and say enough that the reader stops looking for the
//! foreign one. `struct` and `enum` both answer `type`, and a reader told only
//! "write `type`" will reasonably assume there is a separate word for variants
//! somewhere — so both messages say that one word declares every shape.

use khora_syntax::parse;

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

fn declaring(word: &str) -> Vec<String> {
    errors(&format!("module m;\n\n{word} Point {{ x: Int }}\n"))
}

#[test]
fn struct_is_answered_with_type() {
    let found = declaring("struct");
    assert!(found.iter().any(|e| e.contains("no `struct`") && e.contains("`type")), "{found:?}");
}

/// `class` and `record` reach the same answer, because they are the same
/// question.
#[test]
fn class_and_record_are_answered_the_same_way() {
    for word in ["class", "record"] {
        let found = declaring(word);
        assert!(found.iter().any(|e| e.contains("declares every shape")), "{word}: {found:?}");
    }
}

/// **A variant type gets the variant spelling**, not just the word `type`. A
/// reader told only "write `type`" would look for a separate word for cases.
#[test]
fn enum_is_shown_the_variant_spelling() {
    let found = declaring("enum");
    assert!(
        found.iter().any(|e| e.contains("| Red | Green")),
        "the shape is the useful half: {found:?}"
    );
}

#[test]
fn interface_is_answered_with_trait() {
    let found = declaring("interface");
    assert!(found.iter().any(|e| e.contains("`trait`")), "{found:?}");
}

#[test]
fn the_words_for_a_function_are_answered_with_fn() {
    for word in ["func", "function", "def", "fun"] {
        let found = declaring(word);
        assert!(found.iter().any(|e| e.contains("`fn`")), "{word}: {found:?}");
    }
}

/// `var` needs both halves, because Khora's answer depends on where you are.
#[test]
fn var_is_told_about_let_and_const() {
    let found = declaring("var");
    assert!(
        found.iter().any(|e| e.contains("`let`") && e.contains("`const`")),
        "the answer differs by position: {found:?}"
    );
}

/// **`async` has no Khora spelling at all**, and saying "write `fn`" would
/// suggest it is a rename. What a reader needs is that the distinction does
/// not exist here — no marked functions, and no second colour of caller.
#[test]
fn async_is_told_the_distinction_does_not_exist() {
    let found = declaring("async");
    assert!(
        found.iter().any(|e| e.contains("nothing to mark")),
        "not a rename: {found:?}"
    );
}

/// **The guard.** These are only declaration keywords in declaration position.
/// A binding called `class` is an ordinary identifier, and flagging it would
/// make the hint worse than the message it replaced.
#[test]
fn a_binding_named_like_a_foreign_keyword_is_left_alone() {
    for word in ["class", "struct", "var", "async", "record", "union"] {
        let source =
            format!("module m;\n\nfn go() -> Int {{\n  let {word} = 1;\n  {word}\n}}\n");
        assert!(errors(&source).is_empty(), "`{word}` as a binding: {:?}", errors(&source));
    }
}

/// And as a field name, which is the other place these words turn up
/// innocently.
#[test]
fn a_field_named_like_a_foreign_keyword_is_left_alone() {
    let source = "module m;\n\npub type Row = { class: Int, package: Int };\n";
    assert!(errors(source).is_empty(), "{:?}", errors(source));
}
