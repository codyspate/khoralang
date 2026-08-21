//! Front-end integration tests.
//!
//! The corpus tests parse the real `std` and example sources shipped in the
//! repository, so a grammar regression fails here before it reaches the CLI.

use std::path::{Path, PathBuf};

use khora_syntax::{parse, SyntaxNode};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn kh_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            kh_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

#[test]
fn corpus_parses_without_errors() {
    let root = workspace_root();
    let mut files = Vec::new();
    kh_files(&root.join("std"), &mut files);
    kh_files(&root.join("examples"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no corpus files found under {}", root.display());

    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let parse = parse(&text);
        assert_eq!(
            parse.syntax().text().to_string(),
            text,
            "{} did not round-trip",
            file.display()
        );
        assert!(parse.errors().is_empty(), "{}: {:?}", file.display(), parse.errors());
    }
}

/// Losslessness is the invariant the whole CST design exists to provide.
fn assert_round_trip(src: &str) -> SyntaxNode {
    let parse = parse(src);
    assert_eq!(parse.syntax().text().to_string(), src, "lost source text");
    parse.syntax()
}

fn assert_clean(src: &str) -> SyntaxNode {
    let parse = parse(src);
    assert_eq!(parse.syntax().text().to_string(), src, "lost source text");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    parse.syntax()
}

#[test]
fn pipes_are_left_associative() {
    let tree = assert_clean("module m;\nfn f() { a |> b |> c }\n").to_string();
    let _ = tree;
    let dump = parse("module m;\nfn f() { a |> b |> c }\n").debug_tree();
    // The outer pipe's left child is itself a pipe: ((a |> b) |> c).
    let first = dump.find("PIPE_EXPR").unwrap();
    let second = dump[first + 1..].find("PIPE_EXPR").unwrap();
    let indent_of = |idx: usize| dump[..idx].rfind('\n').map_or(idx, |nl| idx - nl - 1);
    assert!(
        indent_of(first + 1 + second) > indent_of(first),
        "expected the second PIPE_EXPR to be nested:\n{dump}"
    );
}

#[test]
fn placeholder_argument_is_preserved() {
    let dump = parse("module m;\nfn f() { x |> g(_, 2) }\n").debug_tree();
    assert!(dump.contains("PLACEHOLDER_EXPR"), "{dump}");
}

#[test]
fn capability_reference_parses() {
    let dump = parse("module m;\nfn f() { ask(:ledger.get_history) }\n").debug_tree();
    assert!(dump.contains("CAPABILITY_EXPR"), "{dump}");
}

#[test]
fn match_scrutinee_does_not_swallow_the_arm_list() {
    let src = "module m;\nfn f() { match report.risk { RiskLevel.Low => 1, _ => 2, } }\n";
    let dump = parse(src).debug_tree();
    assert!(dump.contains("MATCH_EXPR"), "{dump}");
    assert_eq!(dump.matches("MATCH_ARM").count(), 2, "{dump}");
    assert!(!dump.contains("RECORD_EXPR"), "scrutinee brace was read as a record:\n{dump}");
}

#[test]
fn record_literal_is_distinguished_from_a_block() {
    let record = parse("module m;\nfn f() { { a: 1 } }\n").debug_tree();
    assert!(record.contains("RECORD_EXPR"), "{record}");

    let block = parse("module m;\nfn f() { { let a = 1; a } }\n").debug_tree();
    assert!(!block.contains("RECORD_EXPR"), "{block}");
    assert!(block.contains("LET_DECL"), "{block}");
}

#[test]
fn if_else_chain_parses() {
    let src = "module m;
fn f(n: Int) -> String {
  if n < 0 { \"neg\" }
  else if n == 0 { \"zero\" }
  else { \"pos\" }
}
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert_eq!(dump.matches("IF_EXPR").count(), 2, "expected a nested else-if:
{dump}");
}

#[test]
fn if_without_else_parses() {
    let parse = parse("module m;
fn f(n: Int) {
  if n > 0 { print(\"hi\"); }
}
");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

/// The `{` after the condition opens the branch, never a record literal.
#[test]
fn if_condition_does_not_swallow_the_branch() {
    let dump = parse("module m;
fn f() { if ready { 1 } else { 2 } }
").debug_tree();
    assert!(dump.contains("IF_EXPR"), "{dump}");
    assert!(!dump.contains("RECORD_EXPR"), "condition brace read as a record:
{dump}");
}

#[test]
fn function_bodies_take_no_equals_sign() {
    let defined = parse("module m;
pub fn f() -> Int { 1 }
");
    assert!(defined.errors().is_empty(), "{:?}", defined.errors());

    // A signature with no body still ends in a semicolon.
    let declared = parse("module m;
pub fn f() -> Int;
");
    assert!(declared.errors().is_empty(), "{:?}", declared.errors());
}

/// The old `fn f() = { .. };` spelling should say what to do, not just fail.
#[test]
fn the_old_equals_form_gets_a_pointed_diagnostic() {
    let parse = parse("module m;
pub fn f() -> Int = { 1 };
");
    let msgs: Vec<_> = parse.errors().iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("a function body is a block")),
        "unhelpful diagnostics: {msgs:?}"
    );
}

#[test]
fn match_guard_parses() {
    let src = "module m;\nfn f() { match x { n if n > 0 => 1, _ => 0, } }\n";
    let dump = parse(src).debug_tree();
    assert!(dump.contains("MATCH_GUARD"), "{dump}");
}

#[test]
fn variant_declaration_parses() {
    let src = "module m;\npub type RiskLevel =\n  | Low\n  | Moderate(reason: String);\n";
    let dump = parse(src).debug_tree();
    assert_eq!(dump.matches("VARIANT_CASE").count(), 2, "{dump}");
}

#[test]
fn open_row_type_parses() {
    let src = "module m;\nfn ask() -> Effect<T, { label: T | 'r }, Never>;\n";
    let dump = parse(src).debug_tree();
    assert!(dump.contains("ROW_TAIL"), "{dump}");
    assert!(dump.contains("ROW_VAR"), "{dump}");
}

#[test]
fn row_tail_accepts_further_labels() {
    let src = "module m;\nfn f() -> Effect<A, { R1 | R2 | scope: Scope }, E>;\n";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

#[test]
fn const_generics_and_forall_parse() {
    let src = "module m;\npub fn embed<const Dim: Int>(s: String) -> Embedding<Dim, F32>;\n\
               pub type S = forall <Schema> . (Prompt, Schema.Spec) -> Effect<Schema, {}, E>;\n";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    assert!(parse.debug_tree().contains("FORALL_TYPE"));
}

#[test]
fn variance_markers_parse() {
    let parse = parse("module m;\npub type Effect<+A, -R, +E>;\n");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

#[test]
fn import_forms_parse() {
    let parse = parse("module m;\nimport std.effect.{Effect, Layer as L};\nimport std.ai.*;\n");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

#[test]
fn nested_block_comments_round_trip() {
    assert_round_trip("module m; /* outer /* inner */ still outer */ fn f() { 1 }");
}

#[test]
fn unterminated_string_does_not_lose_text() {
    assert_round_trip("module m;\nfn f() { \"oops\n }\n");
}

#[test]
fn recovery_keeps_later_declarations() {
    let src = "module m;\ntype = ;\npub fn good() = { 1 };\n";
    let parse = parse(src);
    assert!(!parse.errors().is_empty());
    assert_eq!(parse.syntax().text().to_string(), src);
    assert!(parse.debug_tree().contains("FN_DECL"), "recovery lost the good decl");
}

#[test]
fn arbitrary_bytes_never_panic() {
    for src in [
        "",
        "\u{0}\u{1}\u{2}",
        "module",
        "fn fn fn",
        "{{{{{{",
        "|>|>|>",
        "match match {",
        "type T = | | |;",
        "'''",
        "0.0.0.0",
    ] {
        assert_round_trip(src);
    }
}
