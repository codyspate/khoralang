//! `pub` on a field: where it is read, and where it is refused.
//!
//! A record type and a row are one production, so the grammar alone cannot
//! tell `type T = { pub x: Int }` from `with { pub clock: Clock }`. Only the
//! first means anything. A `pub` that parsed and was then read by nobody
//! would tell a reader who wrote it to open or hide something that it
//! worked, so everywhere but a declared record it is refused where it stands.

use khora_syntax::parse;

fn errors(src: &str) -> Vec<String> {
    parse(src).errors().iter().map(|e| e.message.clone()).collect()
}

#[test]
fn pub_and_pub_mut_on_a_record_field_parse() {
    let found = errors("module m;\ntype T = { pub x: Int, pub mut y: Int, z: Int, mut w: Int };\n");
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn pub_before_a_newtypes_type_parses() {
    let found = errors("module m;\npub type UserId = pub Int;\ntype P = pub List<Int>;\n");
    assert!(found.is_empty(), "{found:?}");
}

/// **P3.** `pub` inside a row is reported, in a `with` clause, in a `row`
/// declaration, and in a row type written anywhere else.
#[test]
fn pub_in_a_row_is_refused() {
    for src in [
        "module m;\nfn f() -> Int with { pub clock: Clock } { 1 }\n",
        "module m;\nrow Deps = { pub clock: Clock };\n",
        "module m;\nfn f<'r>(p: { pub x: Int | 'r }) -> Int { 1 }\n",
        "module m;\nfn f() -> Int with { 'r | pub clock: Clock } { 1 }\n",
    ] {
        let found = errors(src);
        assert!(
            found.iter().any(|e| e.starts_with("`pub` marks a field of a `type` declaration, and this is a row")),
            "{src}: {found:?}"
        );
        assert_eq!(found.len(), 1, "one error, not a cascade: {src}: {found:?}");
    }
}

/// **`mut` in a row is refused the same way**, in every shape a row takes: a
/// row's entry is a name against a type with nothing behind it to assign, so
/// a `mut` there was parsed and then read by nobody.
#[test]
fn mut_in_a_row_is_refused() {
    for src in [
        "module m;\nfn f() -> Int with { mut clock: Clock } { 1 }\n",
        "module m;\nrow Deps = { mut clock: Clock };\n",
        "module m;\nfn f<'r>(p: { mut x: Int | 'r }) -> Int { 1 }\n",
        "module m;\nfn f() -> Int with { 'r | mut clock: Clock } { 1 }\n",
        "module m;\ntype T = { f: ({ mut x: Int }) -> Int };\n",
    ] {
        let found = errors(src);
        assert_eq!(
            found,
            ["`mut` marks a field of a `type` declaration, and this is a row: its entries \
              belong to no value, so there is nothing to assign. Delete the `mut`"],
            "{src}"
        );
    }
}

/// And a record's field, where `mut` means something, still takes it --
/// with and without `pub`, and in a record whose field type holds a row.
#[test]
fn mut_on_a_record_field_still_parses() {
    let found = errors(
        "module m;\ntype T = { mut x: Int, pub mut y: Int, f: ({ x: Int }) -> Int };\n",
    );
    assert!(found.is_empty(), "{found:?}");
}

/// A record *nested* in a record's field type is a row again, not a second
/// declared record.
#[test]
fn pub_in_a_brace_nested_in_a_record_field_is_refused() {
    let found = errors("module m;\ntype T = { pub f: ({ pub x: Int }) -> Int };\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("this is a row"), "{found:?}");
}

#[test]
fn pub_on_a_case_payload_is_refused() {
    let found = errors("module m;\ntype S = | Circle(pub radius: Int) | Dot;\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("a case's payload is always public"), "{found:?}");
}

#[test]
fn pub_before_a_variant_type_is_refused() {
    let found = errors("module m;\ntype S = pub | A | B;\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("a case's payload is always public"), "{found:?}");
}

#[test]
fn pub_before_a_record_type_names_the_per_field_spelling() {
    let found = errors("module m;\ntype T = pub { x: Int };\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`{ pub x: Int }`"), "{found:?}");
}

#[test]
fn pub_on_an_effect_operation_is_refused() {
    let found = errors("module m;\neffect Clock { pub now: () -> Int }\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("an effect's operations are public"), "{found:?}");
}
