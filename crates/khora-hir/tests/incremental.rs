//! What a body edit is allowed to invalidate.
//!
//! `khora-db`'s incremental tests stop at the parse layer: they prove an edit to
//! one file does not reparse another. That is the cheap half. The half a
//! language server rests on is one layer up — a keystroke inside a function
//! body must not make the compiler re-collect items, re-resolve names, or
//! re-check types in files that merely *import* the edited one. Nothing
//! measured that, and `docs/design/testing.md` names it as the promise most
//! likely to be quietly false.
//!
//! Salsa gives it to us if, and only if, the queries in between are *stable*:
//! `item_map` re-executes whenever the tree changes, which is unavoidable
//! because it reads the tree, but its **result** must compare equal so that
//! backdating stops the invalidation there. Anything in an `ItemMap` that moves
//! when an unrelated body is edited breaks that, and breaks it silently — the
//! answers stay correct and the work grows with the size of the file.
//!
//! So these tests assert on which queries ran, not on what they returned.

use khora_db::{Db, KhoraDatabase, QueryLog, Setter, SourceFile, SourceRoot};
use khora_hir::body::bodies;
use khora_hir::{file_scope, item_map, module_graph};

fn file(db: &dyn Db, name: &str, text: &str) -> SourceFile {
    SourceFile::new(db, name.into(), text.to_string())
}

/// Which of the logged executions were of `query`.
///
/// The log holds `{database_key:?}`, which starts with the query's name.
fn ran(log: &QueryLog, query: &str) -> usize {
    log.peek().iter().filter(|entry| entry.starts_with(query)).count()
}

/// `library.kh` declares two functions, and the *second* is what matters: the
/// edits below change the body of the first, which shifts every span after it.
fn library(body: &str) -> String {
    format!(
        "module app::library;\n\
         export fn first() -> Int = {{ {body} }}\n\
         export fn second() -> Int = {{ 2 }}\n"
    )
}

/// A consumer that imports the library and calls the function whose span moves.
const CONSUMER: &str = "module app::consumer;\n\
                        import app::library::{second};\n\
                        export fn use_it() -> Int = { second() }\n";

struct World {
    db: KhoraDatabase,
    log: QueryLog,
    library: SourceFile,
    consumer: SourceFile,
    root: SourceRoot,
}

/// Builds both files and runs everything once, so the log starts empty and
/// every later execution is a genuine invalidation.
fn world() -> World {
    let (db, log) = KhoraDatabase::logged();
    let library = file(&db, "library.kh", &library("1"));
    let consumer = file(&db, "consumer.kh", CONSUMER);
    let root = SourceRoot::new(&db, vec![library, consumer]);

    let w = World { db, log, library, consumer, root };
    w.everything();
    w.log.take();
    w
}

impl World {
    /// Asks for every query a language server would ask for after a keystroke.
    fn everything(&self) {
        module_graph(&self.db, self.root);
        for f in [self.library, self.consumer] {
            item_map(&self.db, f);
            file_scope(&self.db, f);
            bodies(&self.db, f);
        }
    }

    fn edit_library_body(&mut self, body: &str) {
        self.library.set_text(&mut self.db).to(library(body));
    }
}

/// The property the language server rests on.
///
/// Editing a body in `library.kh` re-collects `library.kh`'s own items and
/// re-lowers its own bodies, both of which are unavoidable: the tree changed.
/// It must not reach `consumer.kh`, which imports from it and cannot tell the
/// difference.
///
/// Note which half of this is trivial. `item_map` reads one file, so an
/// importer's item collection was never at risk and asserting on it alone
/// would prove nothing -- `docs/design/testing.md` asked for the promise to be
/// *discriminating*, and the discriminating query is `file_scope`, which is
/// where a cross-file dependency actually lives.
#[test]
fn a_body_edit_does_not_reach_an_importing_file() {
    let mut w = world();

    w.edit_library_body("1 + 0");
    w.everything();

    let log = &w.log;
    assert_eq!(
        ran(log, "item_map"),
        1,
        "only the edited file's items should be recollected, but these ran: {:?}",
        log.peek()
    );
    // One, not zero: the edited file's own scope re-executes because it reads
    // its own `item_map`. That is local and it backdates. The consumer's must
    // not run at all.
    assert_eq!(
        ran(log, "file_scope"),
        1,
        "the consumer's scope did not change; these ran: {:?}",
        log.peek()
    );
    assert_eq!(
        ran(log, "bodies"),
        1,
        "only the edited file has a changed body; these ran: {:?}",
        log.peek()
    );
}

/// The same edit must not disturb the module graph, which is what every
/// cross-file question goes through. If this re-executes, *nothing* downstream
/// of it can be cached, and the incrementality is decorative.
#[test]
fn a_body_edit_does_not_rebuild_the_module_graph() {
    let mut w = world();

    w.edit_library_body("1 + 0");
    w.everything();

    assert_eq!(
        ran(&w.log, "module_graph"),
        0,
        "a body edit changed no module's path or file; these ran: {:?}",
        w.log.peek()
    );
}

/// The other half, and the one that stops the fix from being a blanket
/// suppression: a change that really does alter what the module offers must
/// still reach the importer.
///
/// Without this, deleting the dependency edge entirely would pass the test
/// above.
#[test]
fn a_new_declaration_does_reach_the_importing_file() {
    let mut w = world();

    w.library
        .set_text(&mut w.db)
        .to(format!("{}export fn third() -> Int = {{ 3 }}\n", library("1")));
    w.everything();

    assert_eq!(
        ran(&w.log, "file_scope"),
        2,
        "a new export must reach the importer's scope; these ran: {:?}",
        w.log.peek()
    );
}

/// Whitespace is not free here, and it is worth being precise about why.
///
/// Rowan's tree is lossless, so a space inside a body is a real change to the
/// green tree and `Parse` cannot backdate it. What must hold is that the change
/// stops at the file that contains it.
#[test]
fn whitespace_in_a_body_does_not_leave_its_file() {
    let mut w = world();

    w.edit_library_body("1  ");
    w.everything();

    assert_eq!(
        ran(&w.log, "module_graph"),
        0,
        "whitespace changed no module's identity; these ran: {:?}",
        w.log.peek()
    );
    assert_eq!(
        ran(&w.log, "file_scope"),
        1,
        "only the edited file's own scope may re-run; these ran: {:?}",
        w.log.peek()
    );
}
