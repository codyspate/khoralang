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
        "module m;\npub fn g(a: Int, b: Int) -> Int { a + b }\n",
        "module m;\nfn h() { xs |> map(f) |> filter(g) }\n",
        "module m;\npub effect E { op: String -> Int raises Err }\n",
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

/// **An alias survives being formatted.**
///
/// It did not. A name is a `NAME_REF` node with its `IDENT` inside, so the
/// direct tokens of an import item are just the `as` — and `receive as
/// tls_receive` normalized to `"as"`, which the formatter wrote back over the
/// source. Four aliases became `{as, as, as, as}` and the file stopped parsing.
///
/// The unaliased case has no direct tokens at all, so it took a fallback and
/// came out right, which is why every import in the corpus was fine until one
/// needed an alias.
#[test]
fn an_aliased_import_survives_formatting() {
    let src = "module m;
import std::net::tls::{transmit as tls_transmit, secure};
";
    let out = format(src).expect("this parses");
    assert!(
        out.contains("{secure, transmit as tls_transmit}"),
        "an alias was lost or mangled:
{out}"
    );
    // And the result is still a program. `formatting_never_loses_a_token`
    // above would catch this too, now that the corpus has an alias in it —
    // which it did not until `std::net::http` grew one.
    assert!(
        format(&out).is_ok(),
        "formatting produced something that does not parse:
{out}"
    );
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

/// `derive(Eq)`, not `derive (Eq)`: the argument list belongs to the word, the
/// same way an impl's parameters belong to `impl`.
#[test]
fn a_derive_clause_hugs_its_traits() {
    let src = "module m;\n\nderive(Eq, Ord)\npub type Point = { x: Int, y: Int };\n";
    assert_eq!(format(src).unwrap(), src);
}

#[test]
fn a_file_with_only_a_module_declaration_formats() {
    assert_eq!(format("module m;").unwrap(), "module m;\n");
}

/// A doc comment is indented like the thing it documents.
///
/// A sum type's cases are continuations, so `| Ok` gets an extra level. The
/// comment above it used to print at column 0 against a case indented two
/// spaces — and it round-tripped, so the corpus test was happy and nothing
/// noticed until something in `std` finally documented a variant.
#[test]
fn a_variant_doc_comment_is_indented_with_its_variant() {
    let src = "module m;\n\n\
               pub type Level =\n  \
               /// Fine.\n  \
               | Ok\n  \
               /// Not fine.\n  \
               | Bad;\n";
    assert_eq!(format(src).unwrap(), src);
}

/// The half that was never broken, asserted so a fix to the other half cannot
/// quietly change it: a record field's comment takes the brace's indent, not a
/// continuation's.
#[test]
fn a_field_doc_comment_keeps_its_own_indent() {
    let src = "module m;\n\n\
               pub type Point = {\n  \
               /// Across.\n  \
               x: Int,\n\
               };\n";
    assert_eq!(format(src).unwrap(), src);
}

/// Several lines of one comment move as a block. Looking only at the very next
/// token would indent the last line and leave the rest behind.
#[test]
fn a_multi_line_doc_comment_moves_together() {
    let src = "module m;\n\n\
               pub type Level =\n  \
               /// One.\n  \
               /// Two.\n  \
               | Ok;\n";
    assert_eq!(format(src).unwrap(), src);
}

/// The bug produced output that was still valid and still round-tripped, so
/// idempotence alone would not have caught it. Asserted anyway, because a
/// lookahead is exactly the kind of change that breaks it.
#[test]
fn formatting_a_documented_variant_twice_is_formatting_it_once() {
    let messy = "module m;\npub type Level =\n/// Fine.\n| Ok\n/// Not fine.\n| Bad;\n";
    let once = format(messy).unwrap();
    assert_eq!(format(&once).unwrap(), once, "not idempotent:\n{once}");
}

// --- the flow operator -------------------------------------------------------

/// Short enough to read on one line stays on one line.
#[test]
fn a_short_flow_stays_compact() {
    let src = "module m;\nfn f() { apply(||> normalize |> validate, 1) }\n";
    assert_eq!(format(src).unwrap(), src, "nothing to break up");
}

/// **The stages align with the operator**, rather than indenting under it.
/// `||>` is not a continuation of anything -- there is no expression before it
/// -- so a pipe that follows it is a sibling, and lining them up is what makes
/// the shape of the pipeline visible.
#[test]
fn a_multiline_flow_aligns_its_stages() {
    let src = "module m;\nfn f() {\n  apply(\n    ||> normalize\n|> validate\n|> persist\n  , 1)\n}\n";
    let out = format(src).unwrap();
    assert!(out.contains("    ||> normalize\n    |> validate\n    |> persist"), "{out}");
}

/// And an ordinary pipeline still indents under what it continues, which is
/// the case the flow rule must not disturb.
#[test]
fn an_ordinary_pipeline_still_indents() {
    let src = "module m;\nfn f() {\n  xs\n|> map(g)\n|> filter(h)\n}\n";
    let out = format(src).unwrap();
    assert!(out.contains("  xs\n    |> map(g)\n    |> filter(h)"), "{out}");
}

#[test]
fn a_flow_survives_a_round_trip() {
    let src = "module m;\nfn f() {\n  apply(\n    ||> normalize\n    |> validate\n    |> persist\n    , 1)\n}\n";
    let once = format(src).unwrap();
    assert_eq!(format(&once).unwrap(), once, "formatting is idempotent");
    assert!(is_formatted(&once).unwrap(), "and reports itself formatted");
}

/// **`a > (b)` keeps its space.**
///
/// `>` closes a type argument list and is also greater-than, and the rule that
/// lets `Foo<Bar>(x)` hug its call could not tell the two apart -- so
/// `value > (largest - digit) / 10` was reformatted to `value >(largest -
/// digit) / 10`, which reads as a call on a stray angle bracket.
///
/// Only `>` had it. `<`, `>=`, `<=`, `==` and `+` before a parenthesis were
/// all correct, which is what made it look like a deliberate rule rather than
/// a mistake. Found because formatting `std` after an edit changed a line
/// nobody had touched.
#[test]
fn a_comparison_does_not_hug_a_parenthesis() {
    let src = "module m;\nfn f(a: Int, b: Int, c: Int) -> Bool { a > (b - c) }\n";
    assert_eq!(format(src).unwrap(), src, "`>` is an operator here, not a bracket");
}

/// The whole family, because the fix was to one of them and the others were
/// already right.
#[test]
fn every_comparison_keeps_its_space_before_a_parenthesis() {
    for op in [">", "<", ">=", "<=", "==", "!="] {
        let src = format!("module m;\nfn f(a: Int, b: Int) -> Bool {{ a {op} (b - 1) }}\n");
        assert_eq!(format(&src).unwrap(), src, "`{op}` before a parenthesis");
    }
}

/// And a call still hugs what it applies to, which is the rule the fix had to
/// leave alone.
#[test]
fn a_call_still_hugs_its_arguments() {
    let src = "module m;\nfn f() -> Int { g(1) + h(2)(3) }\n";
    assert_eq!(format(src).unwrap(), src, "a call hugs its parenthesis");
}


/// **`khora fmt --check` was permanently red on Windows.**
///
/// The formatter works in `\n`, so a correctly formatted file written by an
/// editor that uses `\r\n` came back differing on every line — and `--check`
/// printed only the filename, so finding out why took a `diff` and an `od -c`.
/// `testing.md` recommends the flag for CI, so the first thing a Windows
/// contributor met was a red build about nothing.
///
/// Whichever ending appears *first* decides. A file that mixes them is being
/// rewritten by two tools, and picking the majority would quietly pick a side.
#[test]
fn the_line_ending_a_file_uses_is_the_one_it_keeps() {
    assert_eq!(khora_fmt::line_ending("a\r\nb\r\n"), "\r\n");
    assert_eq!(khora_fmt::line_ending("a\nb\n"), "\n");
    // No newline at all, and a lone carriage return, are both `\n`: there is
    // nothing to preserve and `\r` on its own has not been a line ending since
    // Mac OS 9.
    assert_eq!(khora_fmt::line_ending("a"), "\n");
    assert_eq!(khora_fmt::line_ending("a\rb"), "\n");
    // First one wins.
    assert_eq!(khora_fmt::line_ending("a\nb\r\n"), "\n");
    assert_eq!(khora_fmt::line_ending("a\r\nb\n"), "\r\n");
}

/// Applying an ending is idempotent, which is what lets the caller run it over
/// output it has already run it over.
#[test]
fn rewriting_line_endings_twice_is_rewriting_them_once() {
    let once = khora_fmt::with_line_ending("a\nb\n", "\r\n");
    assert_eq!(once, "a\r\nb\r\n");
    assert_eq!(khora_fmt::with_line_ending(&once, "\r\n"), once);
    assert_eq!(khora_fmt::with_line_ending(&once, "\n"), "a\r\nb\r\n");
}

/// **A formatted file with `\r\n` is formatted**, which is the whole of the
/// bug: the check compares the formatter's output rewritten into the file's
/// own endings, so the two agree.
#[test]
fn a_correctly_formatted_file_with_carriage_returns_needs_no_reformatting() {
    let source = "module m;\n\nfn main() -> () {\n  0\n}\n";
    let formatted = khora_fmt::format(source).expect("it parses");
    assert_eq!(formatted, source, "the fixture has to be canonical to start with");

    let windows = khora_fmt::with_line_ending(source, "\r\n");
    let out = khora_fmt::format(&windows).expect("it parses");
    assert_eq!(
        khora_fmt::with_line_ending(&out, khora_fmt::line_ending(&windows)),
        windows,
        "a formatted file should not be reported as needing reformatting"
    );
}

/// **A prefix `-` hugs its operand.**
///
/// It did not: `-1` came back as `- 1` and `n * -1` as `n * - 1`. Both still
/// parse, which is why nothing caught it — the formatter turned a number into
/// an operator and an operand, and the result reads like a typo somebody left
/// in.
///
/// The rule beside it was written for prefix `!` and `-` was left to the
/// default. Only the space *after* the minus is decided here: `n * -1` wants
/// the one before it, and `(-1)` is already tight because the bracket rule runs
/// first. All three are checked, because a fix for the first that broke either
/// of the others would look correct in isolation.
#[test]
fn a_prefix_minus_hugs_its_operand() {
    let out = format(
        "module m;\n\nfn f(n: Int) -> Int {\n  let a = -1;\n  let b = n * -1;\n  \
         let c = (-1);\n  a + b + c\n}\n",
    )
    .expect("should parse");

    assert!(out.contains("let a = -1;"), "{out}");
    assert!(out.contains("let b = n * -1;"), "{out}");
    assert!(out.contains("let c = (-1);"), "{out}");
    assert!(!out.contains("- 1"), "no minus should be separated from its operand:\n{out}");
}

/// And subtraction keeps its spaces, which is the same token doing the other
/// job.
#[test]
fn subtraction_is_still_spaced() {
    let out = format("module m;\n\nfn f(a: Int, b: Int) -> Int {\n  a - b\n}\n")
        .expect("should parse");
    assert!(out.contains("a - b"), "{out}");
}

/// **A value written on its own line is indented under what it continues.**
///
/// It was not: a `const` initializer went to column 0 and a match arm's body to
/// the arm's own column. The match-arm shape is what this project's
/// documentation is written in, so the formatter would have reformatted its own
/// examples.
#[test]
fn a_value_on_its_own_line_is_indented() {
    let out = format(
        "module m;\n\nconst LIMIT: Int =\n1000;\n\n\
         fn f(s: Int) -> String {\n  match s {\n    1 =>\n\"one\",\n    _ =>\n\"other\",\n  }\n}\n",
    )
    .expect("should parse");

    assert!(out.contains("const LIMIT: Int =\n  1000;"), "{out}");
    assert!(out.contains("    1 =>\n      \"one\","), "{out}");
}

/// **And for as long as it continues, which is the part that took two goes.**
///
/// A value that opens a block has the block's contents and its closing brace
/// inside it. Indenting only the line that opens it leaves the body behind:
///
/// ```text
/// Option::Some(m) =>
///   if m > 59 {
///   Option::None      <- one level short
/// } else {
/// ```
///
/// Across this corpus that was 45 of 155 reformatted lines, which is worse than
/// the de-indent it fixes. The indent is a scope over the value's node rather
/// than a property of its first token, so the brace adds its level on top as
/// usual.
#[test]
fn a_value_that_opens_a_block_carries_the_block_with_it() {
    let out = format(
        "module m;\n\nfn f(n: Int) -> Int {\n  match n {\n    1 =>\nif n > 0 {\n2\n} else {\n3\n},\n    _ => 0,\n  }\n}\n",
    )
    .expect("should parse");

    assert!(out.contains("    1 =>\n      if n > 0 {"), "the arm's value indents:\n{out}");
    assert!(out.contains("        2\n"), "and so does what is inside it:\n{out}");
    assert!(out.contains("      } else {"), "and the brace that closes it:\n{out}");
}

/// **A value that starts on the same line is not a continuation.**
///
/// `=> match .. {` opens on the line above, so its arms are relative to *that*
/// line. Indenting them anyway adds a level per nesting, and a chain of matches
/// becomes a staircase — which is what the first attempt did to `std/trace.kh`.
#[test]
fn a_value_beginning_on_the_same_line_is_not_indented_again() {
    let out = format(
        "module m;\n\nfn f(a: Int, b: Int) -> Int {\n  match a {\n    1 => match b {\n      1 => 2,\n      _ => 3,\n    },\n    _ => 0,\n  }\n}\n",
    )
    .expect("should parse");

    // The inner arms sit one level in from the `match` that owns them, not two.
    assert!(out.contains("    1 => match b {\n      1 => 2,"), "{out}");
}
