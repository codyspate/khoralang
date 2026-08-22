//! Resolution across module boundaries.
//!
//! Imports existed at the item level long before a function body could use
//! one. These tests are about the second half: what a body may name.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_types::diagnostics;

/// Builds a two-module program and returns every diagnostic, from both files.
fn errors(sources: &[(&str, &str)]) -> Vec<String> {
    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|(path, text)| SourceFile::new(&db, path.into(), text.to_string()))
        .collect();
    SourceRoot::new(&db, files.clone());

    files
        .iter()
        .flat_map(|f| diagnostics(&db, *f).iter().map(|e| e.message.clone()).collect::<Vec<_>>())
        .collect()
}

fn assert_clean(sources: &[(&str, &str)]) {
    let found = errors(sources);
    assert!(found.is_empty(), "expected no errors, got {found:?}");
}

fn assert_reports(sources: &[(&str, &str)], needle: &str) {
    let found = errors(sources);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}"
    );
}

const LIB: (&str, &str) = (
    "lib.kh",
    "module demo::lib;\n\
     export type Option<A> = | Some(value: A) | None;\n\
     export fn double(x: Int) -> Int { x * 2 }\n\
     fn secret(x: Int) -> Int { x }\n",
);

#[test]
fn a_named_import_is_callable_from_a_body() {
    assert_clean(&[
        LIB,
        (
            "main.kh",
            "module demo::main;\n\
             import demo::lib::{double};\n\
             export fn run() -> Int { double(21) }\n",
        ),
    ]);
}

/// The point of importing a signature rather than only a name: the call is
/// checked. Resolving without checking is a false pass, which is worse than
/// the unresolved-name error it replaces.
#[test]
fn an_imported_call_is_type_checked() {
    assert_reports(
        &[
            LIB,
            (
                "main.kh",
                "module demo::main;\n\
                 import demo::lib::{double};\n\
                 export fn run() -> Int { double(true) }\n",
            ),
        ],
        "expected `Int`, found `Bool`",
    );
}

/// A type arrives with its constructors. Importing one without them would be
/// ceremony for no decision — the type is not usable on its own.
#[test]
fn an_imported_type_brings_its_constructors() {
    assert_clean(&[
        LIB,
        (
            "main.kh",
            "module demo::main;\n\
             import demo::lib::{Option};\n\
             export fn unwrap(o: Option<Int>) -> Int {\n\
               match o { Option::Some(v) => v, Option::None => 0 }\n\
             }\n",
        ),
    ]);
}

/// And exhaustiveness still applies to it.
#[test]
fn an_imported_types_matches_are_still_checked() {
    assert_reports(
        &[
            LIB,
            (
                "main.kh",
                "module demo::main;\n\
                 import demo::lib::{Option};\n\
                 export fn unwrap(o: Option<Int>) -> Int {\n\
                   match o { Option::Some(v) => v }\n\
                 }\n",
            ),
        ],
        "not covered",
    );
}

#[test]
fn a_private_item_cannot_be_imported() {
    assert_reports(
        &[
            LIB,
            (
                "main.kh",
                "module demo::main;\n\
                 import demo::lib::{secret};\n\
                 export fn run() -> Int { secret(1) }\n",
            ),
        ],
        "`secret` is not exported from `demo.lib`",
    );
}

#[test]
fn importing_something_that_does_not_exist_is_reported() {
    assert_reports(
        &[
            LIB,
            (
                "main.kh",
                "module demo::main;\n\
                 import demo::lib::{nope};\n\
                 export fn run() -> Int { 1 }\n",
            ),
        ],
        "does not declare `nope`",
    );
}

#[test]
fn importing_from_a_module_that_does_not_exist_is_reported() {
    assert_reports(
        &[
            LIB,
            (
                "main.kh",
                "module demo::main;\n\
                 import demo::missing::{thing};\n\
                 export fn run() -> Int { 1 }\n",
            ),
        ],
        "cannot find module `demo.missing`",
    );
}

#[test]
fn an_alias_renames_what_it_imports() {
    assert_clean(&[
        LIB,
        (
            "main.kh",
            "module demo::main;\n\
             import demo::lib::{double as twice};\n\
             export fn run() -> Int { twice(21) }\n",
        ),
    ]);
}

#[test]
fn a_glob_import_brings_every_exported_item() {
    assert_clean(&[
        LIB,
        (
            "main.kh",
            "module demo::main;\n\
             import demo::lib::*;\n\
             export fn run() -> Int { double(21) }\n",
        ),
    ]);
}

/// A file's own declaration wins over an import of the same name, which is
/// what shadowing means everywhere else in the language.
#[test]
fn a_local_declaration_shadows_an_import() {
    assert_clean(&[
        LIB,
        (
            "main.kh",
            "module demo::main;\n\
             import demo::lib::{double};\n\
             fn double(x: Bool) -> Bool { x }\n\
             export fn run() -> Bool { double(true) }\n",
        ),
    ]);
}

/// Nothing arrives unasked: without an import the name is simply absent.
#[test]
fn a_module_sees_nothing_it_did_not_import() {
    assert_reports(
        &[
            LIB,
            ("main.kh", "module demo::main;\nexport fn run() -> Int { double(21) }\n"),
        ],
        "cannot find `double` in this scope",
    );
}

/// A method on a type the file never named. `req.params` has type `Params`,
/// and `req.params.get(..)` has to work whether or not `Params` was imported —
/// a value can arrive without its type being written down anywhere.
#[test]
fn a_method_arrives_on_a_type_that_was_never_imported() {
    assert_clean(&[
        (
            "net.kh",
            "module net;\n\
             export type Params = | Of(one: String);\n\
             impl Params { fn one(self) -> String { match self { Params::Of(s) => s } } }\n\
             export type Request = { params: Params };\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import net::{Request};\n\
             export fn handle(req: Request) -> String { req.params.one() }\n",
        ),
    ]);
}

/// The same rule for a function reached by path, which is how a constructor of
/// such a type is written.
#[test]
fn a_function_arrives_on_a_type_that_was_never_imported() {
    assert_clean(&[
        (
            "net.kh",
            "module net;\n\
             export type Params = | Of(one: String);\n\
             impl Params { fn empty() -> Params { Params::Of(\"\") } }\n\
             export type Request = { params: Params };\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import net::{Params, Request};\n\
             export fn blank() -> Params { Params::empty() }\n",
        ),
    ]);
}
