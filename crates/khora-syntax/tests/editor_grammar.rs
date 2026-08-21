//! Keeps the VS Code TextMate grammar honest.
//!
//! The grammar is a hand-maintained copy of information the lexer already owns,
//! which is exactly the kind of duplication that rots silently: a new keyword
//! gets added to the compiler, nobody touches the editor, and the language
//! quietly stops highlighting correctly. This test fails instead.

use std::path::PathBuf;

use khora_syntax::{CONTEXTUAL_KEYWORDS, KEYWORDS};

fn grammar() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode/syntaxes/khora.tmLanguage.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).expect("grammar is not valid JSON")
}

/// Pulls the alphabetic alternatives out of every *capturing* group in a rule,
/// so `\b(a|b)\b` yields `[a, b]`.
///
/// Groups beginning with `?` — lookarounds and non-capturing groups — are
/// skipped. A contextual keyword's rule has to pin the word to the position
/// where it is a keyword, and that machinery must not be mistaken for keywords.
fn alternatives(pattern: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut group_starts: Vec<usize> = Vec::new();
    let mut in_class = false;
    let mut chars = pattern.char_indices();

    while let Some((i, c)) = chars.next() {
        match c {
            // An escaped `(`, `[` or `]` is a literal, not structure.
            '\\' => {
                chars.next();
            }
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class => group_starts.push(i + 1),
            ')' if !in_class => {
                let Some(start) = group_starts.pop() else { continue };
                let body = &pattern[start..i];
                if body.starts_with('?') {
                    continue;
                }
                out.extend(
                    body.split('|')
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()))
                        .map(str::to_string),
                );
            }
            _ => {}
        }
    }
    out
}

/// Compares the words a repository rule matches against the list the lexer owns.
fn assert_rule_lists(rule: &str, expected: &[&str]) {
    let g = grammar();
    let patterns = g["repository"][rule]["patterns"]
        .as_array()
        .unwrap_or_else(|| panic!("repository.{rule}.patterns missing"))
        .clone();

    let mut in_grammar: Vec<String> = patterns
        .iter()
        .filter_map(|p| p["match"].as_str())
        .flat_map(alternatives)
        .collect();
    in_grammar.sort();
    in_grammar.dedup();

    let mut in_lexer: Vec<String> = expected.iter().map(|k| k.to_string()).collect();
    in_lexer.sort();

    let missing: Vec<_> = in_lexer.iter().filter(|k| !in_grammar.contains(k)).collect();
    let extra: Vec<_> = in_grammar.iter().filter(|k| !in_lexer.contains(k)).collect();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "editor grammar rule `{rule}` is out of sync with the lexer.\n  \
         missing from the grammar: {missing:?}\n  \
         not in this list in the lexer: {extra:?}\n\
         Fix editors/vscode/syntaxes/khora.tmLanguage.json."
    );
}

#[test]
fn grammar_is_valid_json() {
    let g = grammar();
    assert_eq!(g["scopeName"], "source.khora");
    assert!(g["repository"].is_object(), "grammar has no repository");
}

#[test]
fn keywords_match_the_lexer() {
    assert_rule_lists("keywords", KEYWORDS);
}

/// The contextual keywords get their own rule set because they must *not* be
/// highlighted everywhere — a parameter named `handler` is an identifier. Each
/// rule reproduces the position in which the parser reads the word as a
/// keyword, which is the closest a regex can come to what the parser does.
#[test]
fn contextual_keywords_match_the_lexer() {
    assert_rule_lists("contextual-keywords", CONTEXTUAL_KEYWORDS);
}

/// A repository rule nothing includes highlights nothing.
#[test]
fn contextual_keywords_are_reachable_from_the_root() {
    let g = grammar();
    let patterns = g["patterns"].as_array().expect("grammar has no root patterns");
    assert!(
        patterns.iter().any(|p| p["include"] == "#contextual-keywords"),
        "the root patterns never include #contextual-keywords"
    );
}

/// No word may be both hard and contextual: the two lists drive different
/// lexer behaviour, and the grammar would highlight it twice.
#[test]
fn the_keyword_lists_are_disjoint() {
    let overlap: Vec<_> = CONTEXTUAL_KEYWORDS.iter().filter(|k| KEYWORDS.contains(k)).collect();
    assert!(overlap.is_empty(), "words listed as both hard and contextual keywords: {overlap:?}");
}

/// Every JSON file the extension ships must parse. This test exists because
/// it did not: `language-configuration.json` shipped invalid escapes for a
/// while, and nothing caught it — the grammar test only read the other two.
#[test]
fn every_extension_json_file_is_valid() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editors/vscode");
    for rel in ["package.json", "language-configuration.json", "syntaxes/khora.tmLanguage.json"] {
        let path = dir.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
        assert!(parsed.is_ok(), "{rel} is not valid JSON: {}", parsed.unwrap_err());
    }
}

#[test]
fn extension_declares_the_kh_file_type() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode/package.json");
    let text = std::fs::read_to_string(&path).expect("reading package.json");
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("package.json is invalid");

    let langs = manifest["contributes"]["languages"].as_array().expect("no languages");
    let exts = langs[0]["extensions"].as_array().expect("no extensions");
    assert!(exts.iter().any(|e| e == ".kh"), "extension does not claim .kh");
}
