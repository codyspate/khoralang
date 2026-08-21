//! Keeps the VS Code TextMate grammar honest.
//!
//! The grammar is a hand-maintained copy of information the lexer already owns,
//! which is exactly the kind of duplication that rots silently: a new keyword
//! gets added to the compiler, nobody touches the editor, and the language
//! quietly stops highlighting correctly. This test fails instead.

use std::path::PathBuf;

use khora_syntax::KEYWORDS;

fn grammar() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode/syntaxes/khora.tmLanguage.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).expect("grammar is not valid JSON")
}

/// Pulls `a|b|c` out of a `\b(a|b|c)\b` style rule.
fn alternatives(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('(') else { return Vec::new() };
    let Some(close) = pattern.rfind(')') else { return Vec::new() };
    if close <= open {
        return Vec::new();
    }
    pattern[open + 1..close]
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()))
        .collect()
}

#[test]
fn grammar_is_valid_json() {
    let g = grammar();
    assert_eq!(g["scopeName"], "source.khora");
    assert!(g["repository"].is_object(), "grammar has no repository");
}

#[test]
fn keywords_match_the_lexer() {
    let g = grammar();
    let patterns = g["repository"]["keywords"]["patterns"]
        .as_array()
        .expect("repository.keywords.patterns missing")
        .clone();

    let mut in_grammar: Vec<String> = patterns
        .iter()
        .filter_map(|p| p["match"].as_str())
        .flat_map(alternatives)
        .collect();
    in_grammar.sort();
    in_grammar.dedup();

    let mut in_lexer: Vec<String> = KEYWORDS.iter().map(|k| k.to_string()).collect();
    in_lexer.sort();

    let missing: Vec<_> = in_lexer.iter().filter(|k| !in_grammar.contains(k)).collect();
    let extra: Vec<_> = in_grammar.iter().filter(|k| !in_lexer.contains(k)).collect();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "editor grammar is out of sync with the lexer.\n  \
         missing from the grammar: {missing:?}\n  \
         not a keyword in the lexer: {extra:?}\n\
         Fix editors/vscode/syntaxes/khora.tmLanguage.json."
    );
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
