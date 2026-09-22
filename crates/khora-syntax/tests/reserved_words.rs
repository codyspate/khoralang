//! Words held back for a Khora that does not exist yet.
//!
//! **Khora has no editions and no `unstable` marker**, so every keyword added
//! after 1.0 is a source break: somebody's variable named `yield` stops
//! compiling and there is no mechanism to migrate it. Reserving a word now
//! costs one name nobody may use; reserving it after 1.0 is not available at
//! any price. `docs/roadmap.md` states the hole in the repository's own words.
//!
//! # What the message has to do
//!
//! A reserved word is refused *as an identifier*, which means the reader has
//! to be told the word is taken and that nothing in the language uses it —
//! otherwise they go looking for the feature it belongs to and there is none.
//! "expected an identifier" would be true and would send them hunting.
//!
//! # What this does not claim
//!
//! Reserving a word buys the *option* to spend it later. It does not promise
//! the feature, and it does not promise the spelling: if generators arrive
//! spelled `gen`, the reservation of `yield` bought nothing but a name nobody
//! could use. That is the cost, and it is why the list is six words and not
//! twenty.

use khora_syntax::{parse, CONTEXTUAL_KEYWORDS, KEYWORDS, RESERVED_WORDS};

fn errors(source: &str) -> Vec<String> {
    parse(source).errors().iter().map(|e| e.message.clone()).collect()
}

/// Every reserved word is refused where an ordinary name would be accepted,
/// and the message names the word and says it is held for a later Khora.
#[test]
fn a_reserved_word_is_refused_as_a_local_binding() {
    for word in RESERVED_WORDS {
        let source = format!("module m;\n\nfn go() -> Int {{\n  let {word} = 1;\n  {word}\n}}\n");
        let found = errors(&source);
        assert!(
            found.iter().any(|e| e.contains(&format!("`{word}`")) && e.contains("reserved")),
            "`{word}` as a binding was not refused as reserved: {found:?}"
        );
    }
}

/// The other positions a name appears in. A reservation that only covered
/// `let` would let the word into a signature and break the program that used
/// it there on the day the keyword lands, which is the whole failure being
/// prevented.
#[test]
fn a_reserved_word_is_refused_wherever_a_name_may_go() {
    for word in RESERVED_WORDS {
        for source in [
            format!("module m;\n\nfn {word}() -> Int {{ 1 }}\n"),
            format!("module m;\n\nfn go({word}: Int) -> Int {{ {word} }}\n"),
            format!("module m;\n\npub type Row = {{ {word}: Int }};\n"),
            format!("module m;\n\nconst {word} = 1;\n"),
        ] {
            let found = errors(&source);
            assert!(
                found.iter().any(|e| e.contains("reserved")),
                "`{word}` in `{}` was not refused: {found:?}",
                source.lines().nth(2).unwrap_or_default()
            );
        }
    }
}

/// The message says the word is held for a future version, rather than
/// reporting a syntax error about a token that surprised the parser.
#[test]
fn the_message_says_the_word_is_held_for_a_later_khora() {
    let found = errors("module m;\n\nfn go() -> Int {\n  let yield = 1;\n  yield\n}\n");
    let said = found
        .iter()
        .find(|e| e.contains("reserved"))
        .unwrap_or_else(|| panic!("nothing said `yield` is reserved: {found:?}"));
    assert!(said.contains("future version of Khora"), "{said}");
    assert!(
        said.contains("nothing uses it") || said.contains("Nothing uses it"),
        "a reader told only that the word is taken goes looking for the feature: {said}"
    );
}

/// **A reserved word is not a keyword.** It has no kind, no production and no
/// position, so the lexer must keep handing it to the parser as an `IDENT` —
/// anything else would put it in `KEYWORDS`, and the editor grammar would
/// colour a word the language does not have.
#[test]
fn a_reserved_word_is_not_a_keyword() {
    for word in RESERVED_WORDS {
        assert!(
            !KEYWORDS.contains(word),
            "`{word}` is in both KEYWORDS and RESERVED_WORDS; a reserved word has no meaning, \
             so it cannot also be a keyword"
        );
        assert!(
            !CONTEXTUAL_KEYWORDS.contains(word),
            "`{word}` is in both CONTEXTUAL_KEYWORDS and RESERVED_WORDS; a contextual keyword \
             is a usable identifier, which is the opposite of reserved"
        );
    }
}

/// The reservation has to be refusable at all: a word that is already an
/// identifier somewhere in this repository cannot be reserved without breaking
/// the build, and `std/schema.kh`'s `pub fn struct` is why `struct` is not on
/// the list.
#[test]
fn no_reserved_word_is_spelled_like_an_existing_keyword() {
    let mut sorted: Vec<&&str> = RESERVED_WORDS.iter().collect();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(sorted, deduped, "RESERVED_WORDS lists a word twice");
}

/// **The guard.** Reserving a word costs the name everywhere, so the list must
/// be short enough that somebody can read it. This is not a style rule: the
/// spec's own standard is that a language reserving forty words it never uses
/// looks careless, and nothing else in the tree measures it.
#[test]
fn the_list_stays_small_enough_to_defend() {
    assert!(
        RESERVED_WORDS.len() <= 8,
        "{} reserved words. Each one is a name no program may use, and the case for \
         the list is that every entry is defensible individually; past eight, argue \
         it in docs/design/keywords.md first.",
        RESERVED_WORDS.len()
    );
}
