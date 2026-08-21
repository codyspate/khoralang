//! Snapshot tests over rendered diagnostics.
//!
//! Decision A7 makes diagnostic quality a product requirement rather than
//! end-of-project polish. That only means anything if regressions are visible,
//! so every message the compiler can emit gets pinned here and any change to
//! one shows up as a reviewable diff.
//!
//! Snapshots live beside this file. To accept a change after reading it:
//!
//! ```text
//! KHORA_UPDATE_SNAPSHOTS=1 cargo test -p khora-diagnostics
//! ```

use std::path::{Path, PathBuf};

use khora_diagnostics::render_parse_errors;

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("snapshots")
}

/// Compares rendered output against a stored snapshot, or writes it when
/// `KHORA_UPDATE_SNAPSHOTS` is set.
fn assert_snapshot(name: &str, actual: &str) {
    let path = snapshot_dir().join(format!("{name}.txt"));
    let actual = format!("{}\n", actual.trim_end());

    if std::env::var_os("KHORA_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(snapshot_dir()).expect("creating snapshot dir");
        std::fs::write(&path, &actual).expect("writing snapshot");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no snapshot for `{name}`.\n\
             Run with KHORA_UPDATE_SNAPSHOTS=1 to create it. Rendered output was:\n\n{actual}"
        )
    });

    assert_eq!(
        actual,
        expected.replace("\r\n", "\n"),
        "\ndiagnostic output for `{name}` changed. Read the diff; if it is an \
         improvement, re-run with KHORA_UPDATE_SNAPSHOTS=1."
    );
}

fn render(source: &str) -> String {
    let parse = khora_syntax::parse(source);
    assert!(!parse.errors().is_empty(), "expected this to fail parsing:\n{source}");
    render_parse_errors(Path::new("src/main.kh"), source, parse.errors())
}

#[test]
fn missing_semicolon_after_module() {
    assert_snapshot("missing_semicolon", &render("module app::main\n\nfn f() { 1 }\n"));
}

#[test]
fn type_declaration_without_a_name() {
    assert_snapshot("type_without_name", &render("module m;\ntype = ;\n"));
}

/// The old `fn f() = { .. };` spelling should name its own fix.
#[test]
fn the_removed_equals_form() {
    assert_snapshot("equals_function_body", &render("module m;\nexport fn f() -> Int = { 1 };\n"));
}

/// The end of the file is the one place in the program the reader cannot use:
/// the mistake is wherever the brace was opened, often far above. Reporting the
/// opener also collapses the cascade — one error rather than every construct
/// the parser then failed to finish.
#[test]
fn unclosed_brace_at_end_of_file() {
    assert_snapshot("unclosed_brace", &render("module m;\nfn f() {\n  let x = 1;\n"));
}

#[test]
fn handler_installation_without_a_row() {
    assert_snapshot("with_without_row", &render("module m;\nfn f() { g() with }\n"));
}

#[test]
fn effect_declaration_missing_its_body() {
    assert_snapshot("effect_without_body", &render("module m;\nexport effect Ledger\n"));
}

#[test]
fn import_without_a_selection() {
    assert_snapshot("import_without_selection", &render("module m;\nimport std::core;\n"));
}

/// Column counting is by character, so a caret must still land correctly when
/// the line contains multi-byte text.
#[test]
fn caret_lands_correctly_after_non_ascii_text() {
    // The offending token must sit *after* multi-byte characters on the same
    // line, or this proves nothing: the two accented letters are two bytes
    // each, so byte counting would report column 25 where character counting
    // correctly reports 23.
    let source = "module m;\nlet s = \"héllo wörld\" ~ 1;\n";
    let rendered = render(source);
    assert!(rendered.contains(":2:23"), "column counted in bytes, not chars:\n{rendered}");
    assert_snapshot("non_ascii_line", &rendered);
}

#[test]
fn several_errors_in_one_file_are_all_reported() {
    assert_snapshot("multiple_errors", &render("module m;\ntype = ;\nfn = ;\n"));
}
