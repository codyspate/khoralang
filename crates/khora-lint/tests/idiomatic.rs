//! The `idiomatic` group's lints: what each reports, what it leaves alone, and
//! the edit it offers.
//!
//! **Each fix is applied by somebody who did not read it**, so every fix here
//! meets the same four checks, in [`fixed`]:
//!
//! 1. the rewritten file parses;
//! 2. it type-checks with no error, the rewritten expression sitting where a
//!    declared type pins it, so it has the type the original had;
//! 3. linting and fixing it again changes nothing;
//! 4. the two forms print the same, which needs a backend and is
//!    `khora-cli/tests/fix.rs`.
//!
//! The not-firing cases are the exclusions, each one a rewrite that would
//! change what the program means.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_lint::idiomatic::{apply, Fix};
use khora_lint::{
    findings, Finding, BOOL_COMPARISON, CONCATENATED_STRING, MODULE_PATH, NEEDLESS_RETURN,
    PARENTHESIZED_PARAMETER, SUBTRACTION_FROM_ZERO,
};

fn lint_at(db: &dyn Db, path: &str, text: &str, lint: &str) -> Vec<Finding> {
    let file = SourceFile::new(db, path.into(), text.to_string());
    findings(db, file).iter().filter(|f| f.lint == lint).cloned().collect()
}

fn lint(text: &str, lint: &str) -> Vec<Finding> {
    let db = KhoraDatabase::new();
    lint_at(&db, "a.kh", text, lint)
}

/// A module with `body` inside it.
fn module(body: &str) -> String {
    format!("module m;\n\n{body}\n")
}

/// `text` with every fix `lint` offers applied, after the four checks the
/// module comment lists (the fourth is elsewhere).
fn fixed(text: &str, which: &str) -> String {
    let found = lint(text, which);
    let fixes: Vec<&Fix> = found.iter().filter_map(|f| f.fix.as_ref()).collect();
    assert!(!fixes.is_empty(), "expected a fix for `{which}` in:\n{text}");
    let (once, _) = apply(text, &fixes);

    let parse = khora_syntax::parse(&once);
    assert!(parse.errors().is_empty(), "the fixed program does not parse:\n{once}\n{:?}", parse.errors());

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), once.clone());
    let errors = khora_types::diagnostics(&db, file);
    assert!(errors.is_empty(), "the fixed program does not type-check:\n{once}\n{errors:?}");

    let again = lint(&once, which);
    let more: Vec<&Fix> = again.iter().filter_map(|f| f.fix.as_ref()).collect();
    let (twice, _) = apply(&once, &more);
    assert_eq!(twice, once, "applying the fix twice differs from once");
    once
}

/// The original must type-check too, or check 2 proves nothing.
fn checks(text: &str) {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    let errors = khora_types::diagnostics(&db, file);
    assert!(errors.is_empty(), "the fixture does not type-check:\n{text}\n{errors:?}");
}

// --- concatenated-string -----------------------------------------------------

#[test]
fn a_joined_message_is_one_interpolated_string() {
    let text = module("fn f(x: String) -> String { \"a \" + x + \"!\" }");
    checks(&text);
    assert_eq!(lint(&text, CONCATENATED_STRING).len(), 1, "one chain, one finding");
    assert!(fixed(&text, CONCATENATED_STRING).contains("\"a ${x}!\""));
}

#[test]
fn a_dollar_that_meets_a_brace_is_escaped() {
    let text = module("fn f() -> String { \"$\" + \"{a}\" }");
    checks(&text);
    assert!(fixed(&text, CONCATENATED_STRING).contains(r#""\${a}""#));
}

#[test]
fn a_dollar_before_a_hole_needs_no_escape() {
    let text = module("fn f(x: String) -> String { \"$\" + x }");
    assert!(fixed(&text, CONCATENATED_STRING).contains("\"$${x}\""));
}

#[test]
fn a_non_ascii_piece_is_carried_across() {
    let text = module("fn f(x: String) -> String { \"café \" + x + \"é\" }");
    checks(&text);
    let out = fixed(&text, CONCATENATED_STRING);
    assert!(out.contains("\"café ${x}é\""), "{out}");
}

/// A chain holding a call is reported with no fix, even a call whose type is
/// written (see `no_fix_that_removes_the_only_type_for_a_call`); the chain
/// inside the call's argument is its own finding, and is fixed.
#[test]
fn a_chain_holding_a_call_keeps_its_finding_without_a_fix() {
    let text = module(
        "fn g(s: String) -> String { s }\nfn f(x: String) -> String { \"café \" + g(\"}\" + x) + \"é\" }",
    );
    checks(&text);
    let found = lint(&text, CONCATENATED_STRING);
    assert_eq!(found.len(), 2, "{found:?}");
    let outer = found.iter().max_by_key(|f| f.range.len()).expect("the outer chain");
    assert!(outer.fix.is_none(), "{found:?}");
    assert!(fixed(&text, CONCATENATED_STRING).contains("g(\"}${x}\")"));
}

#[test]
fn an_interpolated_piece_is_left_alone() {
    let text = module("fn f(x: String) -> String { \"a ${x}\" + x }");
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
}

#[test]
fn a_chain_across_lines_is_left_alone() {
    let text = module("fn f(x: String) -> String {\n  \"a \"\n    + x\n}");
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
}

#[test]
fn a_backtick_piece_is_left_alone() {
    let text = module("fn f(x: String) -> String { \"a\" + `b` + x }");
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
}

#[test]
fn a_character_literal_inside_a_piece_is_left_alone() {
    let text = module("fn g(c: Char) -> String { \"\" }\nfn f() -> String { \"a\" + g('}') }");
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
}

#[test]
fn arithmetic_is_not_a_message() {
    let text = module("fn f(a: Int, b: Int) -> Int { a + b + 1 }");
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
}

// --- needless-return ---------------------------------------------------------

#[test]
fn a_last_return_becomes_the_tail() {
    let text = module("fn f(x: Int) -> Int {\n  let y = x + 1;\n  return y;\n}");
    checks(&text);
    let out = fixed(&text, NEEDLESS_RETURN);
    assert!(out.contains("  y\n}"), "{out}");
    assert!(!out.contains("return"), "{out}");
}

#[test]
fn a_last_bare_return_is_deleted() {
    let text = module("fn f(x: Int) -> () {\n  let _ = x;\n  return;\n}");
    checks(&text);
    let out = fixed(&text, NEEDLESS_RETURN);
    assert!(!out.contains("return"), "{out}");
}

#[test]
fn an_early_return_is_left_alone() {
    let text = module("fn f(x: Int) -> Int {\n  if x > 0 { return 1; }\n  2\n}");
    assert!(lint(&text, NEEDLESS_RETURN).is_empty());
}

#[test]
fn a_return_in_a_lambda_is_left_alone() {
    let text = module("fn f() -> Int {\n  let g = fn x => { return x; };\n  g(1)\n}");
    assert!(lint(&text, NEEDLESS_RETURN).is_empty());
}

/// `if c { .. }` with no `;` before the `return` would become the tail, so
/// the finding is made and the fix is not.
#[test]
fn no_fix_after_a_statement_that_would_become_the_value() {
    let text = module(
        "fn g() -> () {}\nfn f(c: Bool) -> Int {\n  if c { g() } else { g() }\n  return 1;\n}",
    );
    checks(&text);
    let found = lint(&text, NEEDLESS_RETURN);
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none());
}

// --- subtraction-from-zero ---------------------------------------------------

#[test]
fn zero_minus_a_literal_is_a_negative_literal() {
    let text = module("fn f() -> Int { 0 - 1 }");
    checks(&text);
    assert!(fixed(&text, SUBTRACTION_FROM_ZERO).contains("{ -1 }"));
}

/// `i64::MIN` spelled the only way a literal can: `-9223372036854775807 - 1`.
#[test]
fn the_smallest_int_keeps_its_value() {
    let text = module("fn f() -> Int { 0 - 9223372036854775807 - 1 }");
    checks(&text);
    let out = fixed(&text, SUBTRACTION_FROM_ZERO);
    assert!(out.contains("{ -9223372036854775807 - 1 }"), "{out}");
}

#[test]
fn a_float_is_left_alone() {
    let text = module("fn f() -> Float { 0.0 - 0.0 }");
    assert!(lint(&text, SUBTRACTION_FROM_ZERO).is_empty());
}

#[test]
fn a_fixed_width_integer_is_left_alone() {
    let text = module("fn f() -> I32 { 0 - 1 }");
    checks(&text);
    assert!(lint(&text, SUBTRACTION_FROM_ZERO).is_empty());
}

#[test]
fn a_variable_is_left_alone() {
    let text = module("fn f(x: Int) -> Int { 0 - x }");
    assert!(lint(&text, SUBTRACTION_FROM_ZERO).is_empty());
}

#[test]
fn a_zero_that_is_not_the_left_operand_is_left_alone() {
    let text = module("fn f(a: Int) -> Int { a - 0 - 1 }");
    assert!(lint(&text, SUBTRACTION_FROM_ZERO).is_empty());
}

// --- parenthesized-parameter -------------------------------------------------

#[test]
fn one_untyped_parameter_loses_its_brackets() {
    let text = module("fn f() -> Int {\n  let g = fn (x) => x + 1;\n  g(1)\n}");
    checks(&text);
    assert!(fixed(&text, PARENTHESIZED_PARAMETER).contains("fn x => x + 1"));
}

#[test]
fn a_typed_parameter_keeps_its_brackets() {
    let text = module("fn f() -> Int {\n  let g = fn (x: Int) => x + 1;\n  g(1)\n}");
    assert!(lint(&text, PARENTHESIZED_PARAMETER).is_empty());
}

#[test]
fn two_parameters_keep_their_brackets() {
    let text = module("fn f() -> Int {\n  let g = fn (x, y) => x + y;\n  g(1, 2)\n}");
    assert!(lint(&text, PARENTHESIZED_PARAMETER).is_empty());
}

// --- bool-comparison ---------------------------------------------------------

#[test]
fn equal_to_true_is_the_value() {
    let text = module("fn f(b: Bool) -> Bool { b == true }");
    checks(&text);
    assert!(fixed(&text, BOOL_COMPARISON).contains("{ b }"));
}

#[test]
fn equal_to_false_is_the_negation_and_brackets_an_operator() {
    let text = module("fn f(a: Int, b: Bool) -> Bool { a < 1 == false && b == false }");
    checks(&text);
    let out = fixed(&text, BOOL_COMPARISON);
    assert!(out.contains("!(a < 1) && !b"), "{out}");
}

#[test]
fn not_equal_is_left_alone() {
    let text = module("fn f(b: Bool) -> Bool { b != false }");
    assert!(lint(&text, BOOL_COMPARISON).is_empty());
}

// --- module-path -------------------------------------------------------------

/// A package on disk, because this lint reads the package's name from its
/// manifest.
fn package(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("idiomatic").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::write(dir.join("khora.toml"), "[package]\nname = \"shop\"\nversion = \"0.1.0\"\n")
        .expect("a manifest");
    dir
}

const MAIN: &str = "module main;\n\npub fn main() -> Int { 0 }\n";

#[test]
fn an_entry_file_gets_the_package_path() {
    let dir = package("entry");
    let db = KhoraDatabase::new();
    for (path, expected) in [("src/main.kh", "module shop::main;"), ("src/bin/tool.kh", "module shop::tool;")] {
        let at = dir.join(path).display().to_string();
        let found = lint_at(&db, &at, MAIN, MODULE_PATH);
        assert_eq!(found.len(), 1, "{path}");
        let fix = found[0].fix.as_ref().expect("an entry file gets a fix");
        let (out, _) = apply(MAIN, &[fix]);
        assert!(out.starts_with(expected), "{path}: {out}");
    }
}

#[test]
fn another_file_is_reported_without_a_fix() {
    let dir = package("other");
    let db = KhoraDatabase::new();
    let at = dir.join("src/util.kh").display().to_string();
    let found = lint_at(&db, &at, "module main;\n", MODULE_PATH);
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "renaming an imported module breaks its importers");
}

#[test]
fn a_script_and_a_qualified_path_are_left_alone() {
    let dir = package("script");
    let db = KhoraDatabase::new();
    assert!(lint_at(&db, "script.kh", MAIN, MODULE_PATH).is_empty());
    let at = dir.join("src/main.kh").display().to_string();
    assert!(lint_at(&db, &at, "module shop::main;\n", MODULE_PATH).is_empty());
}

// --- the group ---------------------------------------------------------------

/// Off until a project asks: every one of these fires on correct code.
#[test]
fn each_is_allow_until_the_group_is_on() {
    for lint in khora_lint::idiomatic::ALL {
        assert!(khora_lint::LINTS.contains(lint), "{lint} is a lint");
        assert_eq!(khora_lint::default_level(lint), khora_manifest::LintLevel::Allow, "{lint}");
    }
}

/// Two fixes over nested ranges: the second is computed against text the
/// first replaces, so it is left for the next pass rather than spliced in.
#[test]
fn overlapping_fixes_are_taken_one_per_pass() {
    let text = module("fn f(x: String) -> Bool { (\"a\" + x == \"a!\") == true }");
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.clone());
    let all: Vec<Finding> = findings(&db, file).to_vec();
    let fixes: Vec<&Fix> = all.iter().filter_map(|f| f.fix.as_ref()).collect();
    assert_eq!(fixes.len(), 2, "{all:?}");
    let (once, taken) = apply(&text, &fixes);
    assert_eq!(taken, 1, "{once}");
    let parse = khora_syntax::parse(&once);
    assert!(parse.errors().is_empty(), "{once}");
}

// --- a fix must not join the statement before it -------------------------------

/// Every fix any idiomatic lint offers for `text`, applied until none is left
/// or ten passes, as `khora check --fix` does; the result must still type-check.
fn fixed_everything(text: &str) -> String {
    let mut now = text.to_string();
    for _ in 0..10 {
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "a.kh".into(), now.clone());
        let all: Vec<Finding> = findings(&db, file).to_vec();
        let fixes: Vec<&Fix> = all.iter().filter_map(|f| f.fix.as_ref()).collect();
        if fixes.is_empty() {
            break;
        }
        now = apply(&now, &fixes).0;
    }
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), now.clone());
    assert!(khora_syntax::parse(&now).errors().is_empty(), "does not parse:\n{now}");
    let errors = khora_types::diagnostics(&db, file);
    assert!(errors.is_empty(), "no longer type-checks:\n{now}\n{errors:?}");
    now
}

/// The review's probes: a statement written after a block-like statement with
/// no `;`. `-1` or `(c)` there continues the block's value, so the finding is
/// made with no fix. Where the block's value is `()` the wrong fix fails to
/// type-check; where it is an `Int` or a function it is a silent wrong answer,
/// which `khora-cli/tests/fix.rs` runs.
#[test]
fn no_fix_that_would_continue_the_statement_before() {
    let cases = [
        // s_silent, s_after_if, s_after_match, s_after_while, s_after_block
        (SUBTRACTION_FROM_ZERO, "fn f(c: Bool) -> Int {\n  if c { 10 } else { 20 }\n  0 - 1\n}"),
        (SUBTRACTION_FROM_ZERO, "fn p(s: String) -> () {}\nfn f(c: Bool) -> Int {\n  if c { p(\"x\") } else { p(\"y\") }\n  0 - 1\n}"),
        (SUBTRACTION_FROM_ZERO, "fn p(s: String) -> () {}\nfn f(n: Int) -> Int {\n  match n { 1 => p(\"one\"), _ => p(\"other\") }\n  0 - 5\n}"),
        (SUBTRACTION_FROM_ZERO, "fn f(n: Int) -> Int {\n  let mut i = 0;\n  while i < n { i = i + 1; }\n  0 - 5\n}"),
        (SUBTRACTION_FROM_ZERO, "fn p(s: String) -> () {}\nfn f(n: Int) -> Int {\n  { p(\"blk\") }\n  0 - 5\n}"),
        // b_silent, b_paren_after_if, b_paren_lt_after_if
        (BOOL_COMPARISON, "fn f(c: Bool, h: (Bool) -> Bool) -> Bool {\n  if c { h } else { h }\n  true == (c)\n}"),
        (BOOL_COMPARISON, "fn p(s: String) -> () {}\nfn f(c: Bool) -> Bool {\n  if c { p(\"x\") } else { p(\"y\") }\n  true == (c)\n}"),
        (BOOL_COMPARISON, "fn p(s: String) -> () {}\nfn f(x: Int) -> Bool {\n  if x > 0 { p(\"x\") } else { p(\"y\") }\n  true == (x < 3)\n}"),
    ];
    for (which, body) in cases {
        let text = module(body);
        checks(&text);
        let found = lint(&text, which);
        assert_eq!(found.len(), 1, "{which} is still reported:\n{text}");
        assert!(found[0].fix.is_none(), "{which} offers a fix that joins the statement before:\n{text}");
        let out = fixed_everything(&text);
        assert!(out.contains("}\n  0 - ") || out.contains("}\n  true == "), "the statement is left as written:\n{out}");
    }
}

/// The same shapes where nothing comes before them keep their fix, and `!b`
/// after a block is safe, because `!` after a block-like expression starts a
/// new one.
#[test]
fn a_fix_that_starts_a_statement_safely_is_kept() {
    for (which, body) in [
        (SUBTRACTION_FROM_ZERO, "fn f(c: Bool) -> Int {\n  let x = 1;\n  0 - 1\n}"),
        (BOOL_COMPARISON, "fn p(s: String) -> () {}\nfn f(c: Bool, b: Bool) -> Bool {\n  if c { p(\"x\") } else { p(\"y\") }\n  b == false\n}"),
    ] {
        let text = module(body);
        assert!(lint(&text, which)[0].fix.is_some(), "{which}:\n{text}");
        fixed(&text, which);
    }
}

// --- concatenated-string: the review's F1 and F4 --------------------------------

/// `c_nest2`: a string inside a hole that is itself interpolated is two levels
/// deep once the piece moves into a hole, and the lexer reads one.
#[test]
fn a_piece_holding_an_interpolated_string_is_left_alone() {
    let text = module(
        "fn wrap(s: String) -> String { s }\nfn g(s: String) -> String { s }\nfn f() -> String { \"[\" + wrap(\"${g(\"}\")}\") + \"]\" }",
    );
    checks(&text);
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
    fixed_everything(&text);
}

/// `c_lambda_unused`: `s + "!"` is what pins `s` to `String`; `"${s}!"` does
/// not, and a closure nobody calls then has no type.
#[test]
fn a_piece_naming_an_unannotated_lambda_parameter_is_left_alone() {
    let text = module("fn f() -> Int {\n  let shout = fn s => s + \"!\";\n  0\n}");
    checks(&text);
    let found = lint(&text, CONCATENATED_STRING);
    assert_eq!(found.len(), 1, "still reported");
    assert!(found[0].fix.is_none(), "with no fix");
    // With the parameter annotated, the fix is safe and offered.
    let typed = module("fn f() -> Int {\n  let shout = fn (s: String) => s + \"!\";\n  0\n}");
    assert!(fixed(&typed, CONCATENATED_STRING).contains("\"${s}!\""));
}

// --- module-path: the review's F2 ----------------------------------------------

/// `m_importer`: a test file that imports `main` breaks when `main` is renamed,
/// so the entry file keeps its finding and loses its fix.
#[test]
fn an_entry_file_that_something_imports_gets_no_fix() {
    let dir = package("imported");
    let db = KhoraDatabase::new();
    let main = SourceFile::new(&db, dir.join("src/main.kh"), MAIN.to_string());
    let tests = SourceFile::new(
        &db,
        dir.join("src/main_test.kh"),
        "module shop::main_test;\nimport main::{main};\n".to_string(),
    );
    khora_db::SourceRoot::new(&db, vec![main, tests]);
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == MODULE_PATH).cloned().collect();
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "renaming `main` breaks `main_test.kh`");

    // Nothing importing it: the fix is back.
    let db = KhoraDatabase::new();
    let main = SourceFile::new(&db, dir.join("src/main.kh"), MAIN.to_string());
    let other = SourceFile::new(&db, dir.join("src/util.kh"), "module shop::util;\n".to_string());
    khora_db::SourceRoot::new(&db, vec![main, other]);
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == MODULE_PATH).cloned().collect();
    assert!(found[0].fix.is_some());
}

/// A piece the checker did not type as `String` never becomes a hole. In a
/// program that type-checks this cannot happen, because `+` refuses any other
/// operand; the lints still run over a file with that error in it, and a fix
/// offered there would turn a type error into a program that compiles and
/// prints something nobody wrote.
#[test]
fn a_piece_that_is_not_a_string_is_left_alone() {
    let text = module("fn f(n: Int) -> String { \"x\" + n }");
    assert!(lint(&text, CONCATENATED_STRING).is_empty());
}

// --- a fix must not remove the only thing deciding a type (review 2, N1 and N3) --

/// One case for each kind of local nobody wrote a type for, in both lints.
/// Where nothing else decides its type (a closure nobody calls, an arm that
/// never matches), the fixed program checks and does not build; so any use of
/// such a local keeps its finding and loses its fix.
#[test]
fn no_fix_that_removes_the_only_type_for_a_local() {
    let cases = [
        // c_alias: a `let` with no type
        (CONCATENATED_STRING, "fn f() -> Int {\n  let shout = fn s => {\n    let t = s;\n    t + \"!\"\n  };\n  0\n}"),
        // c_match_none: a pattern binding (`Maybe` stands in for `Option`; this
        // database has no standard library)
        (CONCATENATED_STRING, "fn f() -> Int {\n  let o = Maybe::None;\n  match o { Maybe::Some(s) => { s + \"!\"; 0 }, Maybe::None => 0 }\n}"),
        // a parameter (the first review's c_lambda_unused)
        (CONCATENATED_STRING, "fn f() -> Int {\n  let shout = fn s => s + \"!\";\n  0\n}"),
        // b_lambda_unused: a parameter
        (BOOL_COMPARISON, "fn f() -> Int {\n  let on = fn b => b == true;\n  0\n}"),
        // a `let`, through a lambda
        (BOOL_COMPARISON, "fn f() -> Int {\n  let on = fn b => {\n    let c = b;\n    true == c\n  };\n  0\n}"),
        // a pattern binding
        (BOOL_COMPARISON, "fn f() -> Int {\n  let o = Maybe::None;\n  match o { Maybe::Some(b) => { b == true; 0 }, Maybe::None => 0 }\n}"),
    ];
    for (which, body) in cases {
        let text = format!("module m;\n\ntype Maybe<A> = | Some(v: A) | None;\n\n{body}\n");
        checks(&text);
        let found = lint(&text, which);
        assert_eq!(found.len(), 1, "{which} is still reported, for a person to rewrite:\n{text}");
        assert!(found.iter().all(|f| f.fix.is_none()), "{which} offers a fix that unpins a local:\n{text}\n{found:?}");
    }
}

/// `b == false` becomes `!b`, and `!` still says `b` is a `Bool`, so
/// `b_lambda_unused_false` keeps its fix. And a local with its type written
/// keeps its fix in both lints.
#[test]
fn a_fix_that_still_decides_the_type_is_kept() {
    let text = module("fn f() -> Int {\n  let off = fn b => b == false;\n  0\n}");
    assert!(fixed(&text, BOOL_COMPARISON).contains("fn b => !b"));
    let text = module("fn f() -> Int {\n  let on = fn (b: Bool) => b == true;\n  let t: String = \"x\";\n  let u = t + \"!\";\n  0\n}");
    assert!(fixed(&text, BOOL_COMPARISON).contains("fn (b: Bool) => b;"));
    assert!(fixed(&text, CONCATENATED_STRING).contains("\"${t}!\""));
}

// --- a call's result may be what the operator decides (review 3, R1) -----------

/// `make<A>() -> A` returns whatever its caller needs, so in `make() + "!"`
/// the `+` is what makes it a `String`, and in `make() == true` the `==` is
/// what makes it a `Bool`. `"${make()}!"` and a bare `make()` decide nothing,
/// and in code that never runs nothing else does either: the fixed program
/// checks and does not build. `k_todo_concat` is the same with std's `todo`.
/// Deciding which callee is generic needs name resolution, so any call is
/// refused, and the finding stays.
#[test]
fn no_fix_that_removes_the_only_type_for_a_call() {
    let cases = [
        // k_todo_concat, with a local stand-in for `todo`
        (CONCATENATED_STRING, "fn f() -> Int {\n  if false { make() + \"!\"; }\n  0\n}"),
        // a call holding the generic one
        (CONCATENATED_STRING, "fn f() -> Int {\n  if false { \"<\" + id(make()) + \"!\"; }\n  0\n}"),
        // k_generic_call_bool
        (BOOL_COMPARISON, "fn f() -> Int {\n  if false { let on = make() == true; }\n  0\n}"),
    ];
    for (which, body) in cases {
        let text = module(&format!("fn make<A>() -> A {{ make() }}\n\nfn id<A>(x: A) -> A {{ x }}\n\n{body}"));
        checks(&text);
        let found = lint(&text, which);
        assert_eq!(found.len(), 1, "{which} is still reported, for a person to rewrite:\n{text}");
        assert!(found[0].fix.is_none(), "{which} offers a fix that unpins a call's result:\n{text}");
    }
}

/// `k_false_generic`: `make() == false` becomes `!make()`, and `!` still says
/// the call returns a `Bool`, so the fix is made.
#[test]
fn a_call_under_not_keeps_its_fix() {
    let text = module("fn make<A>() -> A { make() }\n\nfn f() -> Int {\n  if false { let off = make() == false; }\n  0\n}");
    assert!(fixed(&text, BOOL_COMPARISON).contains("let off = !make();"));
}
