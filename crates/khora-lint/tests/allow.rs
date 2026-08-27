//! `// @klint allow <lint>`.
//!
//! The failure mode of a suppression is that it silently does not suppress, or
//! silently suppresses more than it was meant to. Both are invisible in a
//! passing build, so most of these are about the edges rather than the happy
//! path. Roadmap 14.22's prerequisite; `docs/design/lint-hatch.md`.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_lint::{Finding, DANGLING_EXPRESSION, UNKNOWN_ALLOW, USELESS_ALLOW};

/// What the lints say about one file.
fn findings(source: &str) -> Vec<Finding> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "t.kh".into(), source.to_string());
    SourceRoot::new(&db, vec![file]);
    khora_lint::findings(&db, file).clone()
}

fn names(found: &[Finding]) -> Vec<&str> {
    found.iter().map(|f| f.lint).collect()
}

/// A body whose one statement computes a value and drops it.
fn dangling(pragma_before: &str, trailing: &str) -> String {
    format!(
        "module t;\n\npub fn main() -> Int {{\n{pragma_before}  1 + 1;{trailing}\n  0\n}}\n"
    )
}

#[test]
fn without_a_pragma_the_lint_fires() {
    // The control. Every test below is only meaningful against this.
    assert_eq!(names(&findings(&dangling("", ""))), [DANGLING_EXPRESSION]);
}

#[test]
fn a_pragma_on_the_line_before_suppresses() {
    let found = findings(&dangling("  // @klint allow dangling-expression\n", ""));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_trailing_pragma_suppresses() {
    let found = findings(&dangling("", " // @klint allow dangling-expression"));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_blank_line_between_does_not_break_it() {
    // A pragma on its own line governs the next line that has code, not
    // literally the next line -- otherwise a stray blank silently unsuppresses.
    let source = "module t;\n\npub fn main() -> Int {\n  \
                  // @klint allow dangling-expression\n\n  1 + 1;\n  0\n}\n";
    let found = findings(source);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn it_governs_one_statement_and_not_the_next() {
    // The whole reason this is per-line rather than per-block: a suppression
    // written for one line must not quietly cover the one after it.
    let source = "module t;\n\npub fn main() -> Int {\n  \
                  // @klint allow dangling-expression\n  1 + 1;\n  2 + 2;\n  0\n}\n";
    assert_eq!(names(&findings(source)), [DANGLING_EXPRESSION]);
}

#[test]
fn a_pragma_for_a_different_lint_does_not_suppress() {
    let found = findings(&dangling("  // @klint allow reference-cycle\n", ""));
    assert!(
        names(&found).contains(&DANGLING_EXPRESSION),
        "a pragma naming another lint must not suppress this one: {found:?}"
    );
}

#[test]
fn a_lint_that_does_not_exist_is_reported() {
    // **The reason a comment pragma is defensible at all.** Misspell the name
    // and the old objection applies: nothing suppresses, nothing complains,
    // and the reader believes the line is handled.
    let found = findings(&dangling("  // @klint allow dangling-expresion\n", ""));
    let names = names(&found);
    assert!(names.contains(&UNKNOWN_ALLOW), "{found:?}");
    assert!(names.contains(&DANGLING_EXPRESSION), "and it still fires: {found:?}");

    let message = &found.iter().find(|f| f.lint == UNKNOWN_ALLOW).expect("the report").message;
    assert!(message.contains("dangling-expresion"), "{message}");
    assert!(message.contains("dangling-expression"), "it should list what exists: {message}");
}

#[test]
fn a_pragma_that_suppressed_nothing_is_reported() {
    let source = "module t;\n\npub fn main() -> Int {\n  \
                  // @klint allow dangling-expression\n  let a = 1;\n  a\n}\n";
    assert_eq!(names(&findings(source)), [USELESS_ALLOW]);
}

#[test]
fn useless_allow_is_off_unless_asked_for() {
    // It fires on exactly the lines somebody is editing to satisfy a new lint,
    // so it must not be on while lints are still being added.
    assert_eq!(khora_lint::default_level(USELESS_ALLOW), khora_manifest::LintLevel::Allow);
    assert_eq!(khora_lint::default_level(UNKNOWN_ALLOW), khora_manifest::LintLevel::Warn);
    assert_eq!(khora_lint::default_level(DANGLING_EXPRESSION), khora_manifest::LintLevel::Warn);
}

#[test]
fn the_marker_inside_a_string_is_a_string() {
    // Read from the token stream, not the text, so this cannot suppress.
    let source = "module t;\n\npub fn main() -> Int {\n  \
                  let s = \"// @klint allow dangling-expression\";\n  1 + 1;\n  0\n}\n";
    assert!(
        names(&findings(source)).contains(&DANGLING_EXPRESSION),
        "a pragma in a string literal suppressed something"
    );
}

#[test]
fn a_doc_comment_showing_the_syntax_is_documentation() {
    // `docs/design/lint-hatch.md` and this crate's own module comment both
    // contain the pragma as an example. An example is not a use.
    let source = "module t;\n\n/// // @klint allow dangling-expression\npub fn main() -> Int {\n  \
                  1 + 1;\n  0\n}\n";
    assert!(
        names(&findings(source)).contains(&DANGLING_EXPRESSION),
        "a pragma inside a doc comment suppressed something"
    );
}

#[test]
fn the_shape_is_strict() {
    // No verb but `allow`, and one lint per pragma. Refusing to guess now
    // means `deny` can be added later without an older toolchain having
    // silently accepted something it did not understand.
    for pragma in [
        "  // @klint dangling-expression\n",
        "  // @klint deny dangling-expression\n",
        "  // @klint allow dangling-expression reference-cycle\n",
        "  // klint allow dangling-expression\n",
        "  // @klint allow\n",
    ] {
        let found = findings(&dangling(pragma, ""));
        assert!(
            names(&found).contains(&DANGLING_EXPRESSION),
            "`{}` should not be read as a pragma: {found:?}",
            pragma.trim()
        );
    }
}

#[test]
fn two_pragmas_for_two_lines_each_govern_their_own() {
    let source = "module t;\n\npub fn main() -> Int {\n  \
                  // @klint allow dangling-expression\n  1 + 1;\n  \
                  // @klint allow dangling-expression\n  2 + 2;\n  0\n}\n";
    let found = findings(source);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_pragma_at_the_end_of_a_file_governs_nothing() {
    // It has no statement to attach to, which `useless-allow` exists to say
    // rather than having it silently attach to something above it.
    let source = "module t;\n\npub fn main() -> Int {\n  0\n}\n\
                  // @klint allow dangling-expression\n";
    assert_eq!(names(&findings(source)), [USELESS_ALLOW]);
}
