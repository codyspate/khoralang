//! Front-end integration tests.
//!
//! The corpus tests parse the real `std` and example sources shipped in the
//! repository, so a grammar regression fails here before it reaches the CLI.

use std::path::{Path, PathBuf};

use khora_syntax::ast::{Decl, Expr};
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
fn match_scrutinee_does_not_swallow_the_arm_list() {
    let src = "module m;\nfn f() { match report.risk { RiskLevel::Low => 1, _ => 2, } }\n";
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
export fn f() -> Int { 1 }
");
    assert!(defined.errors().is_empty(), "{:?}", defined.errors());

    // A signature with no body still ends in a semicolon.
    let declared = parse("module m;
export fn f() -> Int;
");
    assert!(declared.errors().is_empty(), "{:?}", declared.errors());
}

/// `extern fn` says the body is a C symbol, found at link time.
///
/// Contextual, so the word is still an ordinary identifier everywhere else —
/// adding it could not break a program that was already using it for
/// something.
#[test]
fn extern_marks_a_foreign_declaration() {
    for source in [
        "module m;
extern fn fopen(path: Ptr, mode: Ptr) -> Ptr;
",
        "module m;
export extern fn strlen(s: Ptr) -> Int;
",
    ] {
        let parsed = parse(source);
        assert!(parsed.errors().is_empty(), "{source}: {:?}", parsed.errors());
    }

    let ordinary = parse("module m;
fn f() -> Int { let extern = 1; extern }
");
    assert!(ordinary.errors().is_empty(), "{:?}", ordinary.errors());
}

/// The old `fn f() = { .. };` spelling should say what to do, not just fail.
#[test]
fn the_old_equals_form_gets_a_pointed_diagnostic() {
    let parse = parse("module m;
export fn f() -> Int = { 1 };
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
    let src = "module m;\nexport type RiskLevel =\n  | Low\n  | Moderate(reason: String);\n";
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
    let src = "module m;\nexport fn embed<const Dim: Int>(s: String) -> Embedding<Dim, F32>;\n\
               export type S = forall <Schema> . (Prompt, Schema::Spec) -> Effect<Schema, {}, E>;\n";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    assert!(parse.debug_tree().contains("FORALL_TYPE"));
}

#[test]
fn variance_markers_parse() {
    let parse = parse("module m;\nexport type Effect<+A, -R, +E>;\n");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

#[test]
fn import_forms_parse() {
    let parse = parse("module m;\nimport std::core::{Option, Result as R};\nimport std::ai::*;\n");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

#[test]
fn effect_declaration_parses() {
    let src = "module m;
export effect Ledger {
  get_history: String -> List<Txn> raises DbError,
}
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert!(dump.contains("EFFECT_DECL"), "{dump}");
    assert!(dump.contains("RAISES_CLAUSE"), "{dump}");
}

/// `with` on a signature belongs to the declaration; `with` after an arrow
/// belongs to the function type. Both must coexist in one signature.
#[test]
fn signature_and_function_type_clauses_coexist() {
    let src = "module m;
export fn map<A, B, 'e>(f: A -> B with 'e) -> List<B>
  with 'e
  raises DbError
{ f }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert_eq!(dump.matches("WITH_CLAUSE").count(), 2, "{dump}");
}

#[test]
fn return_type_does_not_swallow_the_with_clause() {
    let dump = parse("module m;
export fn f() -> Report with { ledger: Ledger } { g() }
").debug_tree();
    // The `with` is the declaration's, so it must not appear inside a FN_TYPE.
    assert!(dump.contains("WITH_CLAUSE"), "{dump}");
    assert!(!dump.contains("FN_TYPE"), "return type absorbed the clause:
{dump}");
}

#[test]
fn raise_and_try_parse() {
    let src = "module m;
fn f() { let x = g()!; raise DbError::Unavailable; }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert!(dump.contains("TRY_EXPR"), "{dump}");
    assert!(dump.contains("RAISE_EXPR"), "{dump}");
}

#[test]
fn handler_and_catch_parse() {
    let src = "module m;
const h = handler for Ledger { get_history: fn id => \"x\" };
fn f() { g()! catch { E::A(_) => 1, } }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert!(dump.contains("HANDLER_EXPR"), "{dump}");
    assert!(dump.contains("CATCH_EXPR"), "{dump}");
}

/// All three installation spellings, including a bare named context.
#[test]
fn handler_installation_forms_parse() {
    for src in [
        "module m;
fn f() { g() with { ledger: h } }
",
        "module m;
fn f() { g() with Mock }
",
        "module m;
fn f() { g() with Mock { ai: stub } }
",
        "module m;
fn f() { with { ledger: h } { g() } }
",
        "module m;
fn f() { with Mock { g() } }
",
    ] {
        let parse = parse(src);
        assert!(parse.errors().is_empty(), "{src:?} -> {:?}", parse.errors());
    }
}

/// A block body after a named context must not be read as an override record.
#[test]
fn with_block_body_is_not_mistaken_for_overrides() {
    let dump = parse("module m;
fn f() { with Mock { g() } }
").debug_tree();
    assert!(dump.contains("WITH_BLOCK"), "{dump}");
    assert!(!dump.contains("RECORD_EXPR"), "body read as overrides:
{dump}");
}

#[test]
fn context_test_and_bench_declarations_parse() {
    let src = "module m;
export context Mock { ledger: h }
test \"it works\" { assert(1 == 1); }
bench \"fast\" { f() }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert!(dump.contains("CONTEXT_DECL"), "{dump}");
    assert!(dump.contains("TEST_DECL"), "{dump}");
    assert!(dump.contains("BENCH_DECL"), "{dump}");
}

// --- contextual keywords -------------------------------------------------
//
// `handler`, `context`, `test` and `bench` are keywords in one position each
// and ordinary identifiers everywhere else. Both directions need proving: a
// regression in either one is silent until somebody's parameter stops
// compiling, or `test "name" { .. }` starts parsing as a call.

/// Every position a user is likely to want one of these words in.
#[test]
fn contextual_keywords_are_usable_as_identifiers() {
    for word in ["handler", "context", "test", "bench"] {
        let src = format!(
            "module m;
export type {word} = {{ {word}: Int }};
export fn f({word}: {word}) -> {word} {{
  let {word} = {word};
  g({word}, {{ {word}: 1 }})
}}
"
        );
        let parse = parse(&src);
        assert!(
            parse.errors().is_empty(),
            "`{word}` should be usable as an identifier: {:?}",
            parse.errors()
        );
        assert_eq!(parse.syntax().text().to_string(), src, "`{word}` lost source text");
    }
}

/// The case that motivated the change: `std/net/http.kh` had to rename a
/// parameter called `handler` to work around the reservation.
#[test]
fn a_parameter_named_handler_parses() {
    let src = "module m;
fn f(handler: Request -> Response) { let context = 1; test(context) }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());

    let dump = parse.debug_tree();
    assert!(
        !dump.contains("HANDLER_EXPR") && !dump.contains("HANDLER_KW"),
        "a parameter named `handler` was read as a handler expression:\n{dump}"
    );
    assert!(
        !dump.contains("CONTEXT_KW") && !dump.contains("TEST_KW"),
        "`context` or `test` was read as a keyword:\n{dump}"
    );
    assert_eq!(dump.matches("CALL_EXPR").count(), 1, "`test(context)` should be a call:\n{dump}");
}

/// Each word still parses as its keyword in the position that defines it, and
/// the token is recorded in the tree as the keyword rather than as an `IDENT`.
#[test]
fn contextual_keywords_still_parse_as_keywords() {
    let src = "module m;
export context Production { ledger: h }
const live = handler for Ledger { get_history: fn id => \"x\" };
test \"it works\" { assert(1 == 1); }
bench \"fast\" { f() }
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    assert_eq!(parse.syntax().text().to_string(), src, "lost source text");

    let dump = parse.debug_tree();
    for expected in [
        "CONTEXT_DECL",
        "CONTEXT_KW",
        "HANDLER_EXPR",
        "HANDLER_KW",
        "FOR_KW",
        "TEST_DECL",
        "TEST_KW",
        "BENCH_DECL",
        "BENCH_KW",
    ] {
        assert!(dump.contains(expected), "missing {expected}:\n{dump}");
    }
}

/// A record field, a local and a declaration keyword can all be the same word
/// in one file without interfering.
#[test]
fn a_contextual_keyword_can_be_both_in_one_file() {
    let src = "module m;
export context Mock { handler: h }
test \"the test names a test\" {
  let test = 1;
  let handler = handler for Ledger { get_history: fn id => test };
  assert(handler == handler);
}
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    assert!(dump.contains("CONTEXT_DECL"), "{dump}");
    assert!(dump.contains("TEST_DECL"), "{dump}");
    assert!(dump.contains("HANDLER_EXPR"), "{dump}");
}

/// `for` was contextual only because `handler for` needed it. The `for` loop
/// promoted it, as the test it replaces predicted.
#[test]
fn for_is_a_hard_keyword() {
    let installed = parse("module m;\nconst h = handler for Ledger { get: fn i => 1 };\n");
    assert!(installed.errors().is_empty(), "{:?}", installed.errors());
    assert!(installed.debug_tree().contains("FOR_KW"), "`for` was not read as a keyword");

    let identifier = parse("module m;\nfn f(for: Int) -> Int { for }\n");
    assert!(!identifier.errors().is_empty(), "`for` is reserved and cannot be a name");
}

/// `in`, by contrast, is contextual: reserving it would cost every program a
/// perfectly good name for one position that is never ambiguous.
#[test]
fn in_is_a_keyword_only_inside_a_for_loop() {
    let loop_ = parse("module m;\nfn f(xs: List) { for x in xs { g(x); } }\n");
    assert!(loop_.errors().is_empty(), "{:?}", loop_.errors());
    let dump = loop_.debug_tree();
    assert!(dump.contains("FOR_EXPR"), "{dump}");
    assert!(dump.contains("IN_KW"), "{dump}");

    let identifier = parse("module m;\nfn f(in: Int) -> Int { in }\n");
    assert!(identifier.errors().is_empty(), "{:?}", identifier.errors());
    assert!(!identifier.debug_tree().contains("IN_KW"), "`in` outside a `for` is a name");
}

/// A `for` loop destructures like a `let`, so the pattern is a full one.
#[test]
fn a_for_loop_takes_a_pattern() {
    let out = parse("module m;\nfn f(xs: List) { for Pair::Of(a, b) in xs { g(a); } }\n");
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert!(out.debug_tree().contains("TUPLE_STRUCT_PAT"), "{}", out.debug_tree());
}

/// The body's `{` must not be read as a record literal, the same hazard a
/// `while` condition has.
#[test]
fn a_for_loops_iterable_does_not_swallow_the_body() {
    let out = parse("module m;\nfn f(xs: List) { for x in xs { g(x); } }\n");
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let dump = out.debug_tree();
    assert!(!dump.contains("RECORD_EXPR"), "the body was read as a record\n{dump}");
}

#[test]
fn a_for_loop_without_in_says_so() {
    let out = parse("module m;\nfn f(xs: List) { for x xs { g(x); } }\n");
    let found: Vec<String> = out.errors().iter().map(|e| e.message.clone()).collect();
    assert!(found.iter().any(|e| e.contains("expected `in`")), "{found:?}");
}

/// Dropping `handler` mid-expression must still say what is missing, rather
/// than silently reading `handler` as a variable and failing somewhere else.
#[test]
fn handler_without_for_still_gets_a_pointed_diagnostic() {
    let parse = parse("module m;\nlet h = handler Ledger { get: fn i => 1 };\n");
    let msgs: Vec<_> = parse.errors().iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("expected `for` after `handler`")),
        "unhelpful diagnostics: {msgs:?}"
    );
}

/// Recovery has to resynchronise on a contextual declaration keyword too,
/// otherwise a broken declaration swallows the `context` that follows it.
#[test]
fn recovery_stops_at_a_contextual_declaration_keyword() {
    let src = "module m;\ntype = ;\ncontext Mock { ledger: h }\ntest \"t\" { f() }\n";
    let parse = parse(src);
    assert_eq!(parse.syntax().text().to_string(), src, "lost source text");
    let dump = parse.debug_tree();
    assert!(dump.contains("CONTEXT_DECL"), "recovery ate the context declaration:\n{dump}");
    assert!(dump.contains("TEST_DECL"), "recovery ate the test declaration:\n{dump}");
}

#[test]
fn list_literals_parse() {
    let parse = parse("module m;
fn f() { let xs = [1, 2, 3]; let empty = []; }
");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    assert!(parse.debug_tree().contains("LIST_EXPR"));
}

#[test]
fn assignment_parses_and_binds_loosest() {
    let assigned = parse("module m;
fn f() { let mut x = 1; x = 2; }
");
    assert!(assigned.errors().is_empty(), "{:?}", assigned.errors());
    assert!(assigned.debug_tree().contains("ASSIGN_EXPR"));

    // `x = a |> b` assigns the whole pipeline, not just `a`.
    let dump = parse("module m;
fn f() { x = a |> b; }
").debug_tree();
    let assign = dump.find("ASSIGN_EXPR").expect("no assignment");
    let pipe = dump.find("PIPE_EXPR").expect("no pipe");
    assert!(pipe > assign, "pipe should nest inside the assignment:
{dump}");
}

#[test]
fn loops_and_jumps_parse() {
    let src = "module m;
fn f() {
  let mut i = 0;
  while i < 10 { i = i + 1; }
  loop {
    if i > 5 { break i; }
    continue;
  }
}
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let dump = parse.debug_tree();
    for node in ["WHILE_EXPR", "LOOP_EXPR", "BREAK_EXPR", "CONTINUE_EXPR"] {
        assert!(dump.contains(node), "missing {node}:
{dump}");
    }
}

#[test]
fn early_return_parses_with_and_without_a_value() {
    let parse = parse("module m;
fn f(n: Int) -> Int {
  if n < 0 { return 0; }
  n
}
fn g() { return; }
");
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    assert_eq!(parse.debug_tree().matches("RETURN_EXPR").count(), 2);
}

/// Without this rule an `if` in the middle of a block is read as the block's
/// tail expression and everything after it is orphaned.
#[test]
fn block_like_expressions_are_statements_without_a_semicolon() {
    let src = "module m;
fn f(n: Int) -> Int {
  if n < 0 { print(\"neg\"); }
  while n > 0 { n = n - 1; }
  match n { _ => (), }
  n
}
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());

    // Assert on the typed tree rather than counting text: nested blocks contain
    // statements of their own, so a string count would be misleading.
    let file = parse.source_file();
    let body = file
        .decls()
        .find_map(|d| match d {
            Decl::Fn(f) => f.body(),
            _ => None,
        })
        .expect("no function body");

    assert_eq!(body.stmts().count(), 3, "the if, while and match should be statements");
    assert!(
        matches!(body.tail_expr(), Some(Expr::Path(_))),
        "`n` should remain the tail expression"
    );
}

/// The condition must not absorb the loop body as a record literal.
#[test]
fn while_condition_does_not_swallow_the_body() {
    let dump = parse("module m;
fn f() { while ready { step(); } }
").debug_tree();
    assert!(dump.contains("WHILE_EXPR"), "{dump}");
    assert!(!dump.contains("RECORD_EXPR"), "condition read as a record:
{dump}");
}

#[test]
fn an_imperative_function_parses_end_to_end() {
    let src = "module m;

export fn import_batch(rows: List<Row>) -> Summary
  with { ledger: Ledger }
  raises DbError
{
  let mut summary = Summary::empty();
  let mut i = 0;
  while i < rows.len() {
    let row = rows.at(i);
    i = i + 1;
    match Txn::parse(row) {
      Option::None => continue,
      Option::Some(txn) => {
        if txn.amount < 0 { return summary; }
        ledger.record(txn)!;
        summary = summary.record(txn);
      }
    }
  }
  summary
}
";
    let parse = parse(src);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    assert_eq!(parse.syntax().text().to_string(), src);
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
    let src = "module m;\ntype = ;\nexport fn good() = { 1 };\n";
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
