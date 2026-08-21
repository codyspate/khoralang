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

/// Every rule in the grammar that carries a regex, as that regex paired with
/// every scope name the rule assigns — its own `name` plus the `name` of each
/// of its captures.
///
/// The walk covers the whole document rather than one repository entry: a rule
/// can sit in the root `patterns`, in a `repository` entry, or nested inside a
/// capture, and a test that looked in only one of those places would go quiet
/// the moment a rule moved between them.
fn rules(value: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    fn walk(value: &serde_json::Value, out: &mut Vec<(String, Vec<String>)>) {
        match value {
            serde_json::Value::Object(map) => {
                let regex = map.get("match").or_else(|| map.get("begin")).and_then(|v| v.as_str());
                if let Some(regex) = regex {
                    let mut scopes: Vec<String> =
                        map.get("name").and_then(|n| n.as_str()).map(str::to_string).into_iter().collect();
                    for key in ["captures", "beginCaptures", "endCaptures"] {
                        let Some(captures) = map.get(key).and_then(|c| c.as_object()) else {
                            continue;
                        };
                        scopes.extend(
                            captures.values().filter_map(|c| c["name"].as_str()).map(str::to_string),
                        );
                    }
                    out.push((regex.to_string(), scopes));
                }
                for child in map.values() {
                    walk(child, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
            _ => {}
        }
    }

    let mut out = Vec::new();
    walk(value, &mut out);
    out
}

/// True if `pattern` contains a character class admitting both letters and a
/// literal `.` — the shape of an identifier regex written for the universal
/// dot, such as `[A-Za-z0-9_.]`.
///
/// Escaped pairs are dropped rather than inspected, so `[+\-*/%]` is not read
/// as containing a range and `[,:.]`, which has a dot but no letters, is left
/// alone: that dot is the `forall <A> . Type` separator, not a path.
fn has_dotted_identifier_class(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    let mut class: Option<String> = None;

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' if class.is_none() => class = Some(String::new()),
            ']' => {
                if let Some(body) = class.take() {
                    if body.contains('.') && (body.contains("a-z") || body.contains("A-Z")) {
                        return true;
                    }
                }
            }
            _ => {
                if let Some(body) = class.as_mut() {
                    body.push(c);
                }
            }
        }
    }
    false
}

/// The scopes assigned by every pattern of one repository rule.
fn scopes_of(rule: &str) -> Vec<String> {
    let g = grammar();
    let patterns = g["repository"][rule]["patterns"]
        .as_array()
        .unwrap_or_else(|| panic!("repository.{rule}.patterns missing"))
        .clone();
    rules(&serde_json::Value::Array(patterns)).into_iter().flat_map(|(_, scopes)| scopes).collect()
}

/// The order in which the root includes its repository rules. TextMate takes
/// the leftmost match and breaks a tie by this order, so it is behavior.
fn root_includes() -> Vec<String> {
    let g = grammar();
    g["patterns"]
        .as_array()
        .expect("grammar has no root patterns")
        .iter()
        .filter_map(|p| p["include"].as_str())
        .map(str::to_string)
        .collect()
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
/// lexer behavior, and the grammar would highlight it twice.
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

/// `::` is a token the lexer has (`SyntaxKind::COLON_COLON`) and the grammar
/// must color. Without a rule it does not go uncolored — it falls through to
/// the `:` in the punctuation class and gets read twice as the separator of a
/// type annotation, which is precisely the wrong reading.
///
/// The scope must also be the separator's alone. Sharing one with the
/// arithmetic or comparison operators would make a namespace qualification look
/// like an expression, and telling those two apart is what the `::`/`.` split
/// was for.
#[test]
fn the_path_separator_has_a_scope_of_its_own() {
    let g = grammar();
    let all = rules(&g);

    let separator_scopes: Vec<String> = all
        .iter()
        .filter(|(regex, _)| regex.contains("::"))
        .flat_map(|(_, scopes)| scopes.iter().cloned())
        .filter(|scope| scope.starts_with("keyword.operator."))
        .collect();

    assert!(
        !separator_scopes.is_empty(),
        "no rule in the grammar scopes `::` as an operator. The lexer produces \
         COLON_COLON for it; editors/vscode/syntaxes/khora.tmLanguage.json must \
         give it a scope."
    );

    for scope in &separator_scopes {
        let also_used_by: Vec<&String> = all
            .iter()
            .filter(|(regex, scopes)| !regex.contains("::") && scopes.contains(scope))
            .map(|(regex, _)| regex)
            .collect();
        assert!(
            also_used_by.is_empty(),
            "`{scope}` is meant to be the path separator's own scope, but these \
             rules assign it too: {also_used_by:?}"
        );
    }
}

/// Paths are `::`-separated. They used to be dotted, and the grammar spelled
/// that as character classes like `[A-Za-z0-9_.]` — every one of which is now
/// wrong in a way no editor will report: the regex still matches, it just stops
/// at the first `:` and colors half a path.
#[test]
fn no_rule_spells_a_path_with_dots() {
    let g = grammar();

    for (regex, _) in rules(&g) {
        assert!(
            !has_dotted_identifier_class(&regex),
            "this rule matches an identifier that may contain `.`, which is how \
             a path was written before `::` split compile-time paths from \
             runtime projection:\n  {regex}"
        );

        // The declaration rule, which captures the path after the keyword —
        // not the bare-word rule in #keywords, whose group lists every hard
        // declaration keyword and stops there.
        if regex.contains("(module|import)") {
            assert!(
                regex.contains("::"),
                "the module/import rule does not mention `::`, so it cannot be \
                 matching a whole path:\n  {regex}"
            );
        }
    }
}

/// `#paths` and `#projections` are the rules the split made possible, and both
/// are dead unless they run *before* `#types` — which colors any capitalized
/// identifier and would otherwise claim a constructor after `::` and a field
/// after `.` alike. TextMate breaks a tie between two rules matching at the
/// same position by the order they are listed, so this ordering is behavior
/// and not presentation.
#[test]
fn the_path_rules_run_before_the_blunt_type_rule() {
    let includes = root_includes();
    let position = |name: &str| {
        includes
            .iter()
            .position(|include| include == name)
            .unwrap_or_else(|| panic!("the root patterns never include {name}: {includes:?}"))
    };

    let types = position("#types");
    for rule in ["#paths", "#projections"] {
        assert!(
            position(rule) < types,
            "{rule} is included after #types, so #types claims the names it was \
             written to classify"
        );
    }
}

/// A name after a `.` is a record field or a method — runtime projection — and
/// never a type. Under the universal dot the grammar could not know that, so
/// `RiskLevel.Low` and `report.risk` were colored the same; this test pins the
/// half of the distinction a regex can now make.
#[test]
fn a_name_after_a_dot_is_never_a_type() {
    let offenders: Vec<String> =
        scopes_of("projections").into_iter().filter(|s| s.contains("entity.name.type")).collect();

    assert!(
        offenders.is_empty(),
        "#projections colors the tail of a `.` as a type: {offenders:?}. After \
         the `::`/`.` split a `.` introduces a field or a method, and a type \
         can only follow `::`."
    );
}

/// The other half: a `::` path distinguishes the module segments from the type
/// they qualify. A grammar that scoped every segment identically would be no
/// better than the universal dot it replaced.
#[test]
fn a_path_distinguishes_modules_from_types() {
    let scopes = scopes_of("paths");
    for expected in ["entity.name.namespace.khora", "entity.name.type.khora"] {
        assert!(
            scopes.iter().any(|s| s == expected),
            "#paths never assigns `{expected}`, so a lowercase module qualifier \
             and a capitalized type qualifier are being colored the same: \
             {scopes:?}"
        );
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
