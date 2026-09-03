//! `khora std search` from the outside.
//!
//! **The index this reads was machine-only until now.** `khora mcp` exposed a
//! searchable, accurate view of the standard library to coding agents, and a
//! person had no equivalent — no subcommand, and a website describing a
//! different revision. Four independent evaluators, given the published pages
//! and a released toolchain, each concluded the compiler was broken; the one
//! who recovered did it by reaching for the agent's tool.
//!
//! So the thing worth testing from out here is not the search algorithm, which
//! `khora-mcp` already covers. It is that the command exists, reads the real
//! `std`, and answers a miss in a way that does not read like a bug.

use std::process::Command;

fn search(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .arg("std")
        .arg("search")
        .args(args)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text.replace("\r\n", "\n"))
}

/// **Derived from the source, not from a list.** The signature comes out of the
/// declaration's own range and the description out of the `///` above it, so
/// this passing means the command reached the real `std` rather than something
/// checked in beside it.
#[test]
fn a_name_in_std_is_found_with_its_signature_and_its_documentation() {
    let (ok, said) = search(&["Nursery", "--limit", "3"]);
    assert!(ok, "{said}");
    assert!(said.contains("std::core::Nursery"), "the module it lives in: {said}");
    assert!(said.contains("(effect)"), "what kind of thing it is: {said}");
    assert!(said.contains("Where a spawned fiber goes"), "its own doc comment: {said}");
}

/// Documentation is searched as well as names, which is what makes the command
/// useful to somebody who knows what they want and not what it is called.
#[test]
fn a_phrase_from_a_doc_comment_finds_the_item() {
    let (ok, said) = search(&["spawned fiber", "--limit", "5"]);
    assert!(ok, "{said}");
    assert!(said.contains("std::core::"), "{said}");
}

/// **A miss says the library is small, rather than nothing.** The failure this
/// guards against is a reader assuming an absent function exists because every
/// other language has it — which is exactly what four evaluators did, at
/// length, against a documentation set that promised one.
#[test]
fn a_miss_says_so_and_says_why() {
    let (ok, said) = search(&["unwrap_or_default"]);
    assert!(ok, "a miss is an answer, not a failure: {said}");
    assert!(said.contains("Nothing in `std` matches"), "{said}");
    assert!(said.contains("may not exist"), "{said}");
}

/// The limit is honoured and the remainder is counted, so a broad query says
/// how much it did not show rather than silently truncating.
#[test]
fn a_broad_query_is_capped_and_says_how_many_more() {
    let (ok, said) = search(&["a", "--limit", "2"]);
    assert!(ok, "{said}");
    assert_eq!(said.matches("\n--- ").count() + usize::from(said.starts_with("--- ")), 2, "{said}");
    assert!(said.contains("and"), "it should account for the rest: {said}");
    assert!(said.contains("more"), "{said}");
}

/// Private items are left out on purpose: an agent or a person who learns about
/// one writes code that does not compile, and `not exported` is a worse teacher
/// than never having seen it.
#[test]
fn nothing_private_is_offered() {
    let (ok, said) = search(&["allow_read", "--limit", "20"]);
    assert!(ok, "{said}");
    assert!(
        !said.contains("--- std::fs::allow_read"),
        "`allow_read` is a private helper in `std::fs`: {said}"
    );
}
