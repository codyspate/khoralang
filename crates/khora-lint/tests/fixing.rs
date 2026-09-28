//! The backstop behind every fix: a pass that would break the compilation is
//! refused. Each test here offers fixes of its own -- deliberately wrong ones
//! -- rather than a real lint's, so the backstop is tested apart from the
//! lints' own refusals, which are `idiomatic.rs`'s.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_lint::fixing::{pass, Offered};
use khora_lint::idiomatic::{Edit, Fix};
use text_size::{TextRange, TextSize};

/// A fix replacing the first `from` in `text` with `to`.
fn replacing(text: &str, from: &str, to: &str) -> Fix {
    let at = text.find(from).expect("the text to replace") as u32;
    Fix {
        edits: vec![Edit {
            range: TextRange::new(TextSize::new(at), TextSize::new(at + from.len() as u32)),
            replacement: to.to_string(),
        }],
    }
}

/// A compilation of `files`, with `offer` choosing each file's fixes.
fn run(files: &[(&str, &str)], offer: &dyn Fn(&str, &str) -> Vec<Offered>) -> (khora_lint::fixing::Pass, Vec<String>) {
    let mut db = KhoraDatabase::new();
    let inputs: Vec<SourceFile> =
        files.iter().map(|(path, text)| SourceFile::new(&db, PathBuf::from(path), text.to_string())).collect();
    SourceRoot::new(&db, inputs.clone());
    let offered = |db: &KhoraDatabase, file: SourceFile| offer(&file.path(db).display().to_string(), file.text(db));
    let everything = |_: &Path| true;
    let out = pass(&mut db, &inputs, &everything, &offered);
    // The database is handed back as it was found.
    let texts = inputs.iter().map(|file| file.text(&db).to_string()).collect();
    (out, texts)
}

const MAIN: &str = "module main;\n\npub fn helper() -> Int { 41 }\n";
const IMPORTER: &str = "module shop::main_test;\n\nimport main::{helper};\n\nfn f() -> Int { helper() }\n";

#[test]
fn a_fix_that_would_not_parse_is_refused() {
    let text = "module m;\n\nfn f() -> String { \"a\" }\n";
    let (out, texts) = run(&[("a.kh", text)], &|_, text| vec![("test-lint", replacing(text, "\"a\"", "\"a"))]);
    assert!(out.changed.is_empty(), "{out:?}");
    assert_eq!(out.refused.len(), 1, "{out:?}");
    assert!(out.refused[0].why.contains("would not parse"), "{out:?}");
    assert_eq!(texts, vec![text.to_string()], "the database is left as it was");
}

/// The break is in another file: renaming `main` leaves its importer with
/// nothing to import.
#[test]
fn a_fix_that_breaks_another_file_is_refused_and_named() {
    let (out, texts) = run(&[("src/main.kh", MAIN), ("src/main_test.kh", IMPORTER)], &|path, text| {
        if path.ends_with("main.kh") { vec![("test-lint", replacing(text, "module main;", "module shop::main;"))] } else { vec![] }
    });
    assert!(out.changed.is_empty(), "{out:?}");
    assert_eq!(out.refused.len(), 1, "{out:?}");
    assert_eq!(out.refused[0].lints, vec!["test-lint"]);
    assert!(out.refused[0].why.contains("main_test.kh"), "names the file it breaks: {out:?}");
    assert_eq!(texts, vec![MAIN.to_string(), IMPORTER.to_string()]);
}

/// A good fix beside a bad one in the same file is still made.
#[test]
fn only_the_fix_that_breaks_something_is_refused() {
    let text = "module m;\n\nfn f() -> Int { 1 }\n\nfn g() -> Int { f() }\n";
    let (out, _) = run(&[("a.kh", text)], &|_, text| {
        vec![("renames-f", replacing(text, "fn f()", "fn h()")), ("changes-one", replacing(text, "{ 1 }", "{ 2 }"))]
    });
    assert_eq!(out.changed.len(), 1, "{out:?}");
    assert_eq!(out.changed[0].text, "module m;\n\nfn f() -> Int { 2 }\n\nfn g() -> Int { f() }\n");
    assert_eq!(out.refused.len(), 1, "{out:?}");
    assert_eq!(out.refused[0].lints, vec!["renames-f"]);
}

/// A file that already has an error is not fixed at all: the check afterwards
/// could not tell a new error from the old one.
#[test]
fn a_file_with_an_error_is_not_fixed() {
    let text = "module m;\n\nfn f() -> Int { \"not an int\" }\n\nfn g() -> Int { 1 }\n";
    let (out, _) = run(&[("a.kh", text)], &|_, text| vec![("changes-one", replacing(text, "{ 1 }", "{ 2 }"))]);
    assert!(out.changed.is_empty() && out.refused.is_empty(), "{out:?}");
}
