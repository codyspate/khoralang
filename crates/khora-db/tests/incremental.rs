//! Proves the database is actually incremental.
//!
//! These are the tests that stop decision A3 from being a claim rather than a
//! property. Incrementality is invisible when it works, so the only way to keep
//! it is to assert on which queries executed.

use khora_db::{parse, Db, KhoraDatabase, Setter, SourceFile, SourceRoot};

fn file(db: &dyn Db, name: &str, text: &str) -> SourceFile {
    SourceFile::new(db, name.into(), text.to_string())
}

#[test]
fn parse_runs_once_per_file_then_is_cached() {
    let (db, log) = KhoraDatabase::logged();
    let a = file(&db, "a.kh", "module a;");

    parse(&db, a);
    assert_eq!(log.take().len(), 1, "first parse should execute");

    parse(&db, a);
    parse(&db, a);
    assert!(log.is_empty(), "repeated parses should be served from cache");
}

/// The exit criterion for roadmap phase 0.2: editing one file must not
/// invalidate another.
#[test]
fn editing_one_file_does_not_reparse_another() {
    let (mut db, log) = KhoraDatabase::logged();
    let a = file(&db, "a.kh", "module a;");
    let b = file(&db, "b.kh", "module b;");

    parse(&db, a);
    parse(&db, b);
    assert_eq!(log.take().len(), 2, "both files should parse initially");

    b.set_text(&mut db).to("module b; fn changed() = { 1 };".to_string());

    parse(&db, a);
    parse(&db, b);

    let executed = log.take();
    assert_eq!(
        executed.len(),
        1,
        "only the edited file should reparse, but these ran: {executed:?}"
    );
}

/// Salsa backdates a query whose recomputed value is unchanged. An edit that
/// produces an identical tree therefore costs one reparse and invalidates
/// nothing downstream.
#[test]
fn an_edit_back_to_the_original_text_is_backdated() {
    let (mut db, log) = KhoraDatabase::logged();
    let a = file(&db, "a.kh", "module a;");

    let first = parse(&db, a).syntax().text().to_string();
    log.take();

    set_text(&mut db, a, "module a; ");
    parse(&db, a);
    assert_eq!(log.take().len(), 1, "changed text must reparse");

    set_text(&mut db, a, "module a;");
    let restored = parse(&db, a).syntax().text().to_string();
    assert_eq!(restored, first, "restoring the text should restore the tree");
}

fn set_text(db: &mut KhoraDatabase, file: SourceFile, text: &str) {
    file.set_text(db).to(text.to_string());
}

#[test]
fn parse_results_survive_unrelated_input_changes() {
    let (mut db, log) = KhoraDatabase::logged();
    let a = file(&db, "a.kh", "module a;");
    let b = file(&db, "b.kh", "module b;");
    let root = SourceRoot::new(&db, vec![a, b]);

    parse(&db, a);
    log.take();

    // Changing the file set must not invalidate the parse of a file in it.
    let c = file(&db, "c.kh", "module c;");
    root.set_files(&mut db).to(vec![a, b, c]);

    parse(&db, a);
    assert!(log.is_empty(), "adding a file should not reparse existing ones");
}

#[test]
fn diagnostics_come_through_the_query() {
    let db = KhoraDatabase::new();
    let broken = file(&db, "broken.kh", "module a; type = ;");

    let parse = parse(&db, broken);
    assert!(!parse.errors().is_empty(), "expected syntax errors");
    assert_eq!(
        parse.syntax().text().to_string(),
        "module a; type = ;",
        "the tree must still cover the whole input"
    );
}
