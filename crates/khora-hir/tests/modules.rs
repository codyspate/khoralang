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
     pub type Option<A> = | Some(value: A) | None;\n\
     pub fn double(x: Int) -> Int { x * 2 }\n\
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
             pub fn run() -> Int { double(21) }\n",
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
                 pub fn run() -> Int { double(true) }\n",
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
             pub fn unwrap(o: Option<Int>) -> Int {\n\
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
                 pub fn unwrap(o: Option<Int>) -> Int {\n\
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
                 pub fn run() -> Int { secret(1) }\n",
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
                 pub fn run() -> Int { 1 }\n",
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
                 pub fn run() -> Int { 1 }\n",
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
             pub fn run() -> Int { twice(21) }\n",
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
             pub fn run() -> Int { double(21) }\n",
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
             pub fn run() -> Bool { double(true) }\n",
        ),
    ]);
}

/// Nothing arrives unasked: without an import the name is simply absent.
#[test]
fn a_module_sees_nothing_it_did_not_import() {
    assert_reports(
        &[
            LIB,
            ("main.kh", "module demo::main;\npub fn run() -> Int { double(21) }\n"),
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
             pub type Params = | Of(one: String);\n\
             impl Params { pub fn one(self) -> String { match self { Params::Of(s) => s } } }\n\
             pub type Request = { params: Params };\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import net::{Request};\n\
             pub fn handle(req: Request) -> String { req.params.one() }\n",
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
             pub type Params = | Of(one: String);\n\
             impl Params { pub fn empty() -> Params { Params::Of(\"\") } }\n\
             pub type Request = { params: Params };\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import net::{Params, Request};\n\
             pub fn blank() -> Params { Params::empty() }\n",
        ),
    ]);
}

/// An imported `effect` has to arrive with its operations. Without them the
/// type read as `Unknown`, and every `ai.extract(..)` on it read as a call to
/// a method that was not there.
#[test]
fn an_imported_effect_brings_its_operations() {
    assert_clean(&[
        (
            "svc.kh",
            "module svc;\n\
             pub type Row = | Of(n: Int);\n\
             pub effect Db { query: (String) -> Row, }\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import svc::{Db, Row};\n\
             pub fn load(id: String) -> Row with { db: Db } { db.query(id) }\n",
        ),
    ]);
}

/// And its requirement has to be satisfiable from another module, which is the
/// same fact from the caller's side.
#[test]
fn an_imported_effect_can_be_installed() {
    assert_clean(&[
        (
            "svc.kh",
            "module svc;\n\
             pub type Row = | Of(n: Int);\n\
             pub effect Db { query: (String) -> Row, }\n\
             pub fn load(id: String) -> Row with { db: Db } { db.query(id) }\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import svc::{Db, Row, load};\n\
             pub fn go() -> Row {\n\
               with { db: handler for Db { query: fn _ => Row::Of(1) } } { load(\"x\") }\n\
             }\n",
        ),
    ]);
}

// --- `pub` on a method ---------------------------------------------------

/// A method without `pub` belongs to its module.
///
/// Without this the keyword is parsed and read by nothing, so every method of
/// an exported type is reachable from everywhere and `Map::rehash` and
/// `List::take_first` are promises. `docs/design/std-surface.md`.
#[test]
fn a_method_without_export_is_not_callable_from_another_module() {
    assert_reports(
        &[
            (
                "lib.kh",
                "module lib;\n\
                 pub type Counter = { n: Int };\n\
                 impl Counter { fn secret(self) -> Int { self.n * 2 } }\n",
            ),
            (
                "app.kh",
                "module app;\n\
                 import lib::{Counter};\n\
                 pub fn use_it(c: Counter) -> Int { Counter::secret(c) }\n",
            ),
        ],
        "is not exported",
    );
}

/// The same through method syntax, which resolves by a different path.
#[test]
fn the_method_form_is_refused_too() {
    assert_reports(
        &[
            (
                "lib.kh",
                "module lib;\n\
                 pub type Counter = { n: Int };\n\
                 impl Counter { fn secret(self) -> Int { self.n * 2 } }\n",
            ),
            (
                "app.kh",
                "module app;\n\
                 import lib::{Counter};\n\
                 pub fn use_it(c: Counter) -> Int { c.secret() }\n",
            ),
        ],
        "is not exported",
    );
}

/// **The message names the fix**, because it is one word in a file the reader
/// may not have thought to open.
#[test]
fn the_refusal_says_what_to_write_and_where() {
    assert_reports(
        &[
            (
                "lib.kh",
                "module lib;\n\
                 pub type Counter = { n: Int };\n\
                 impl Counter { fn secret(self) -> Int { self.n * 2 } }\n",
            ),
            (
                "app.kh",
                "module app;\n\
                 import lib::{Counter};\n\
                 pub fn use_it(c: Counter) -> Int { Counter::secret(c) }\n",
            ),
        ],
        "pub fn secret",
    );
}

/// A module may always call its own, keyword or not: `pub` is a statement about
/// other modules.
#[test]
fn a_module_may_call_its_own_unexported_methods() {
    assert_clean(&[(
        "lib.kh",
        "module lib;\n\
         pub type Counter = { n: Int };\n\
         impl Counter {\n\
           pub fn doubled(self) -> Int { Counter::secret(self) }\n\
           fn secret(self) -> Int { self.n * 2 }\n\
         }\n",
    )]);
}

/// And an exported one crosses, which is the whole point of the keyword.
#[test]
fn an_exported_method_crosses_the_boundary() {
    assert_clean(&[
        (
            "lib.kh",
            "module lib;\n\
             pub type Counter = { n: Int };\n\
             impl Counter { pub fn doubled(self) -> Int { self.n * 2 } }\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import lib::{Counter};\n\
             pub fn use_it(c: Counter) -> Int { c.doubled() + Counter::doubled(c) }\n",
        ),
    ]);
}

/// **A trait impl is not filtered.** Its methods are the trait's, reachable
/// wherever the trait is, and the keyword is not read there — writing it on
/// one would suggest the others were hidden.
#[test]
fn a_trait_method_needs_no_export_on_the_impl() {
    assert_clean(&[
        (
            "lib.kh",
            "module lib;\n\
             pub trait Show { fn show(self) -> String; }\n\
             pub type Counter = { n: Int };\n\
             impl Show for Counter { fn show(self) -> String { \"c\" } }\n",
        ),
        (
            "app.kh",
            "module app;\n\
             import lib::{Counter, Show};\n\
             pub fn render(c: Counter) -> String { c.show() }\n",
        ),
    ]);
}

// --- import cycles ---------------------------------------------------------
//
// A cycle used to be a **panic**, not a diagnostic: `type_map` resolves an
// imported name by asking the exporting file for its `type_map`, so two
// modules importing each other asked for each other for ever and Salsa gave
// up with `dependency graph cycle when querying type_map`. Errata 55.

/// Two modules importing each other is an error with a message, and the
/// message draws the cycle.
#[test]
fn a_two_module_cycle_is_reported() {
    assert_reports(
        &[
            ("a.kh", "module demo::a;\nimport demo::b::{b};\npub fn a() -> Int { 1 }\n"),
            ("b.kh", "module demo::b;\nimport demo::a::{a};\npub fn b() -> Int { a() }\n"),
        ],
        "import each other",
    );
}

/// And it does not crash, which is the whole point. `assert_reports` would
/// panic with the query stack rather than fail, so this says it plainly.
#[test]
fn a_cycle_does_not_panic() {
    let found = errors(&[
        ("a.kh", "module demo::a;\nimport demo::b::{b};\npub fn a() -> Int { 1 }\n"),
        ("b.kh", "module demo::b;\nimport demo::a::{a};\npub fn b() -> Int { a() }\n"),
    ]);
    assert!(!found.is_empty(), "a cycle has to be reported, not ignored");
}

/// Three modules round a ring, which the two-module check would miss.
#[test]
fn a_longer_cycle_is_reported() {
    assert_reports(
        &[
            ("a.kh", "module demo::a;\nimport demo::b::{b};\npub fn a() -> Int { b() }\n"),
            ("b.kh", "module demo::b;\nimport demo::c::{c};\npub fn b() -> Int { c() }\n"),
            ("c.kh", "module demo::c;\nimport demo::a::{a};\npub fn c() -> Int { a() }\n"),
        ],
        "import each other",
    );
}

/// **A diamond is not a cycle**, and this is the case a careless reachability
/// check turns into one: two modules importing a third, and a fourth importing
/// both. Nothing here can reach itself.
#[test]
fn a_diamond_is_not_a_cycle() {
    assert_clean(&[
        ("base.kh", "module demo::base;\npub fn base() -> Int { 1 }\n"),
        (
            "left.kh",
            "module demo::left;\nimport demo::base::{base};\npub fn left() -> Int { base() }\n",
        ),
        (
            "right.kh",
            "module demo::right;\nimport demo::base::{base};\npub fn right() -> Int { base() }\n",
        ),
        (
            "top.kh",
            "module demo::top;\n\
             import demo::left::{left};\n\
             import demo::right::{right};\n\
             pub fn top() -> Int { left() + right() }\n",
        ),
    ]);
}

/// A chain is not a cycle either, however long.
#[test]
fn a_chain_is_not_a_cycle() {
    assert_clean(&[
        ("a.kh", "module demo::a;\npub fn a() -> Int { 1 }\n"),
        ("b.kh", "module demo::b;\nimport demo::a::{a};\npub fn b() -> Int { a() }\n"),
        ("c.kh", "module demo::c;\nimport demo::b::{b};\npub fn c() -> Int { b() }\n"),
    ]);
}
