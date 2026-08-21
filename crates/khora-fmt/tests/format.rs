//! Formatter tests.
//!
//! The two property tests matter more than the appearance tests: a formatter
//! that loses code is worse than no formatter, and one that is not idempotent
//! turns every save into a diff.

use std::path::{Path, PathBuf};

use khora_fmt::{format, is_formatted};
use khora_syntax::LexedStr;

/// The non-trivia token stream — what must survive formatting.
fn tokens(src: &str) -> Vec<(String, String)> {
    let lexed = LexedStr::new(src);
    (0..lexed.len())
        .filter(|i| !lexed.kind(*i).is_trivia())
        .map(|i| (format!("{:?}", lexed.kind(i)), lexed.text(i).to_string()))
        .collect()
}

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut files = Vec::new();
    collect(&root.join("std"), &mut files);
    collect(&root.join("examples"), &mut files);
    files.sort();
    files
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

#[test]
fn formatting_is_idempotent() {
    for file in corpus() {
        let src = std::fs::read_to_string(&file).unwrap();
        let once = format(&src).expect("corpus should parse");
        let twice = format(&once).expect("formatted output should parse");
        assert_eq!(once, twice, "{} is not stable under a second pass", file.display());
    }
}

/// Import lists are reordered by design, so this compares the token multiset
/// rather than the sequence. `preserves_the_token_sequence` covers order on
/// input that has no imports.
#[test]
fn formatting_never_loses_a_token() {
    for file in corpus() {
        let src = std::fs::read_to_string(&file).unwrap();
        let out = format(&src).expect("corpus should parse");

        let mut before = tokens(&src);
        let mut after = tokens(&out);
        before.sort();
        after.sort();
        assert_eq!(before, after, "{} lost or gained tokens", file.display());
    }
}

#[test]
fn preserves_the_token_sequence() {
    let cases = [
        "module m;\nfn f() { let x = 1; x }\n",
        "module m;\nexport fn g(a: Int, b: Int) -> Int { a + b }\n",
        "module m;\nfn h() { xs |> map(f) |> filter(g) }\n",
        "module m;\nexport effect E { op: String -> Int raises Err }\n",
        "module m;\nfn k() { if a { b } else { c } }\n",
    ];
    for src in cases {
        let out = format(src).expect("should parse");
        assert_eq!(tokens(src), tokens(&out), "token sequence changed for {src:?}");
    }
}

#[test]
fn the_corpus_is_already_formatted() {
    for file in corpus() {
        let src = std::fs::read_to_string(&file).unwrap();
        let out = format(&src).expect("corpus should parse");
        assert_eq!(
            out,
            src,
            "{} is not canonically formatted; run `khora fmt`",
            file.display()
        );
    }
}

#[test]
fn imports_are_sorted_and_deduplicated() {
    let src = "module m;\nimport std::core::{Scope, Option, Scope, Never};\n";
    let out = format(src).unwrap();
    assert!(
        out.contains("import std::core::{Never, Option, Scope};"),
        "imports not sorted and deduplicated:\n{out}"
    );
}

#[test]
fn indentation_is_normalized_to_two_spaces() {
    let src = "module m;\nfn f() {\n        let x = 1;\n\tlet y = 2;\n}\n";
    let out = format(src).unwrap();
    assert!(out.contains("\n  let x = 1;"), "{out}");
    assert!(out.contains("\n  let y = 2;"), "{out}");
}

/// §6.2: a multi-line pipeline is indented relative to its expression.
#[test]
fn pipeline_continuations_are_indented() {
    let src = "module m;\nfn f() {\n  xs\n|> map(g)\n|> filter(h)\n}\n";
    let out = format(src).unwrap();
    assert!(out.contains("\n    |> map(g)"), "continuation not indented:\n{out}");
}

/// A line break the author chose is never moved.
#[test]
fn author_line_breaks_are_preserved() {
    let inline = format("module m;\nfn f() { g(1, 2) }\n").unwrap();
    assert!(inline.contains("fn f() { g(1, 2) }"), "{inline}");

    let broken = format("module m;\nfn f() {\n  g(1, 2)\n}\n").unwrap();
    assert!(broken.contains("fn f() {\n  g(1, 2)\n}"), "{broken}");
}

#[test]
fn runs_of_blank_lines_collapse_to_one() {
    let out = format("module m;\n\n\n\n\nfn f() { 1 }\n").unwrap();
    assert!(!out.contains("\n\n\n"), "blank lines not collapsed:\n{out}");
    assert!(out.contains("\n\nfn f()"), "blank line removed entirely:\n{out}");
}

#[test]
fn comments_survive() {
    let src = "module m;\n// leading\nfn f() {\n  // inside\n  1 // trailing\n}\n";
    let out = format(src).unwrap();
    for comment in ["// leading", "// inside", "// trailing"] {
        assert!(out.contains(comment), "lost {comment}:\n{out}");
    }
}

/// Reformatting mid-edit, with a brace unbalanced, is when a formatter can do
/// the most damage. It must decline instead.
#[test]
fn broken_input_is_refused() {
    let result = format("module m;\nfn f() { let x = ;\n");
    assert!(result.is_err(), "formatter should refuse input that does not parse");
}

#[test]
fn is_formatted_agrees_with_format() {
    let tidy = "module m;\n\nfn f() { 1 }\n";
    assert!(is_formatted(tidy).unwrap(), "should already be formatted: {tidy:?}");
    assert!(!is_formatted("module m;\nfn f() {    1 }\n").unwrap());
}

#[test]
fn a_file_with_only_a_module_declaration_formats() {
    assert_eq!(format("module m;").unwrap(), "module m;\n");
}
