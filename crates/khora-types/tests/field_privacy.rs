//! Field privacy: a record field is private to the module that declares its
//! type unless it is marked `pub`.
//!
//! **What this guards is that a constructor means something.** Before the
//! rule, any module could write `{ hi: 0, lo: 12345, scale: 0 - 4 }` and have
//! a `Decimal` with a negative scale -- the one thing `Decimal`'s constructors
//! exist to prevent -- or assign `v.len = 40` and have a `Vector` that reads
//! past its own array. Every refusal below is one way outside code could build
//! or change a value without going through the functions that keep it valid,
//! and every "clean" twin is the same program inside the declaring module,
//! where nothing changes.
//!
//! Two modules in-process for the rule itself, because privacy is invisible
//! in a program of one module. `std` is compiled alongside only for the tests
//! that are about `std`'s own types or about `derive(Decode)`, which expands
//! against `std::schema`.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// The diagnostics of `user`, with `library` alongside it in one program.
fn errors_in_user(library: &str, user: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let library = SourceFile::new(&db, "library.kh".into(), library.to_string());
    let user = SourceFile::new(&db, "user.kh".into(), user.to_string());
    SourceRoot::new(&db, vec![library, user]);
    khora_types::diagnostics(&db, user).iter().map(|e| e.message.clone()).collect()
}

/// The diagnostics of `library` itself, for the inside-the-module twins.
fn errors_in_library(library: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let library = SourceFile::new(&db, "library.kh".into(), library.to_string());
    SourceRoot::new(&db, vec![library]);
    khora_types::diagnostics(&db, library).iter().map(|e| e.message.clone()).collect()
}

fn std_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("std should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            std_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// The diagnostics of `program`, compiled with the real `std` and `others`.
fn errors_with_std(others: &[(&str, &str)], program: &str) -> Vec<String> {
    let mut paths = Vec::new();
    std_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| SourceFile::new(&db, p.clone(), std::fs::read_to_string(p).expect("readable")))
        .collect();
    for (name, text) in others {
        files.push(SourceFile::new(&db, PathBuf::from(name), text.to_string()));
    }
    let mine = SourceFile::new(&db, PathBuf::from("program.kh"), program.to_string());
    files.push(mine);
    SourceRoot::new(&db, files);
    khora_types::diagnostics(&db, mine).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(found: &[String]) {
    assert!(found.is_empty(), "expected no errors, got {found:?}");
}

/// Exactly one error, and it is `expected`. Exact, because the message is
/// the fix: which module, which field, and what to write instead.
fn assert_only(found: &[String], expected: &str) {
    assert_eq!(found, [expected.to_string()], "expected exactly one error");
}

/// `Email` has one private field and a getter of the same name; `Tally` has
/// one private field and one `pub mut`; `Port` is a closed newtype and
/// `UserId` an open one; `Point` is all `pub`.
const LIBRARY: &str = "\
module library;
pub type Email = { text: String };
pub type Tally = { pub name: String, mut hidden: Int, pub mut count: Int };
pub type Point = { pub x: Int, pub y: Int };
pub type Fixed = { pub x: Int };
pub type Port = Int;
pub type UserId = pub Int;
type Quiet = pub Int;
impl Email {
  pub fn parse(text: String) -> Email { { text: text } }
  pub fn text(self) -> String { self.text }
}
impl Tally {
  pub fn new(name: String) -> Tally { { name: name, hidden: 0, count: 0 } }
}
impl Port {
  pub fn of(n: Int) -> Port { Port(n) }
}
";

const OUTSIDE: &str = "module user;\nimport library::{Email, Tally, Point, Fixed, Port, UserId};\n";

fn outside(body: &str) -> Vec<String> {
    errors_in_user(LIBRARY, &format!("{OUTSIDE}{body}\n"))
}

fn inside(body: &str) -> Vec<String> {
    errors_in_library(&format!("{LIBRARY}{body}\n"))
}

// --- reading ------------------------------------------------------------------

/// A private field read from outside names the getter when there is one:
/// the fix that needs no edit anywhere else.
#[test]
fn reading_a_private_field_outside_is_refused_and_names_the_getter() {
    assert_only(
        &outside("pub fn f(e: Email) -> String { e.text }"),
        "cannot read `text`: it is private to `library`, which declares `Email`. Call \
         `.text()` instead, which `Email` offers publicly",
    );
}

/// Without a getter the message offers the declaration's `pub`, because
/// `library` is not `std` and may be the reader's own.
#[test]
fn reading_a_private_field_with_no_getter_names_the_declaration() {
    assert_only(
        &outside("pub fn f(t: Tally) -> Int { t.hidden }"),
        "cannot read `hidden`: it is private to `library`, which declares `Tally`. Use the \
         functions `library` offers for `Tally`, or, if `library` is yours, write `pub \
         hidden` in its declaration",
    );
}

#[test]
fn reading_a_private_field_inside_is_fine() {
    assert_clean(&inside("fn f(e: Email) -> String { e.text }"));
}

#[test]
fn reading_a_pub_field_outside_is_fine() {
    assert_clean(&outside("pub fn f(t: Tally) -> String { t.name }"));
}

/// **A private field does not hide a method of the same name.** A field
/// shadows a method (decision D2), and with the field private that made the
/// getter this rule sends readers to unreachable.
#[test]
fn a_private_field_beside_a_getter_of_its_name_calls_the_getter() {
    assert_clean(&outside("pub fn f(e: Email) -> String { e.text() }"));
}

// --- writing ------------------------------------------------------------------

#[test]
fn assigning_a_private_field_outside_is_refused() {
    assert_only(
        &outside("pub fn f(t: Tally) -> () { let mut u = t; u.hidden = 40; }"),
        "cannot assign to `hidden`: it is private to `library`, which declares `Tally`. Use \
         the functions `library` offers for `Tally`, or, if `library` is yours, write `pub \
         hidden` in its declaration",
    );
}

#[test]
fn assigning_a_private_field_inside_is_fine() {
    assert_clean(&inside("fn f(t: Tally) -> () { let mut u = t; u.hidden = 40; }"));
}

/// `pub mut` is written from anywhere, and `pub` alone is still refused --
/// by the `mut` rule, which privacy does not replace.
#[test]
fn pub_mut_is_written_and_plain_pub_is_refused_by_mut() {
    assert_clean(&outside("pub fn f(t: Tally) -> () { let mut u = t; u.count = 3; }"));
    assert_only(
        &outside("pub fn f(t: Tally) -> () { let mut u = t; u.name = \"x\"; }"),
        "cannot assign to `name`, which `Tally` does not declare `mut`",
    );
}

// --- literals -----------------------------------------------------------------

/// Every way a literal finds its type is refused: an annotation, a hint
/// through a call, a `let` annotation, and inside a lambda.
#[test]
fn a_literal_of_a_type_with_a_private_field_is_refused_every_way_it_is_typed() {
    let expected = "cannot build `Email` here: `text` is private to `library`, so only \
                    `library` can make one. Call one of its functions that returns `Email`, \
                    or, if `library` is yours, mark every field `pub`";
    for body in [
        "pub fn f() -> Email { { text: \"x\" } }",
        "fn take(e: Email) -> Int { 1 }\npub fn f() -> Int { take({ text: \"x\" }) }",
        "pub fn f() -> Email { let e: Email = { text: \"x\" }; e }",
        "pub fn f() -> (String) -> Email { fn s => { text: s } }",
    ] {
        assert_only(&outside(body), expected);
    }
}

/// **All the private fields, not the first.** Naming one sent the reader to
/// open it and come back to be told about the next.
#[test]
fn a_refused_literal_names_every_private_field() {
    let found = outside("pub fn f() -> Tally { { name: \"a\", hidden: 1, count: 2 } }");
    assert_only(
        &found,
        "cannot build `Tally` here: `hidden` is private to `library`, so only `library` can \
         make one. Call one of its functions that returns `Tally`, or, if `library` is yours, \
         mark every field `pub`",
    );
    let two = errors_in_user(
        "module library;\npub type Two = { a: Int, pub b: Int, c: Int };\n",
        "module user;\nimport library::{Two};\npub fn f() -> Two { { a: 1, b: 2, c: 3 } }\n",
    );
    assert!(two[0].contains("`a` and `c` are private"), "{two:?}");
}

/// A literal with nothing to say what it is finds its record by its labels,
/// and that search sees `Email` too. It is refused the same way.
#[test]
fn a_literal_found_by_its_labels_alone_is_refused() {
    assert_only(
        &outside("pub fn f() -> Int { let _e = { text: \"x\" }; 1 }"),
        "cannot build `Email` here: `text` is private to `library`, so only `library` can \
         make one. Call one of its functions that returns `Email`, or, if `library` is yours, \
         mark every field `pub`",
    );
}

/// **One error, not the refusal and a list of missing fields behind it.**
/// \"this `Tally` is missing `hidden`\" would be advice to write a field the
/// reader cannot name.
///
/// Both ways a literal finds its type: from what is expected of it, and from
/// its labels alone.
#[test]
fn a_refused_literal_does_not_go_on_to_list_missing_fields() {
    for body in [
        "pub fn f() -> Tally { { name: \"a\" } }",
        "pub fn f() -> Int { let _t = { name: \"a\", hidden: 1 }; 1 }",
    ] {
        let found = outside(body);
        assert_eq!(found.len(), 1, "{body}: {found:?}");
        assert!(found[0].starts_with("cannot build `Tally` here"), "{body}: {found:?}");
    }
}

#[test]
fn a_literal_inside_is_fine() {
    assert_clean(&inside("fn f() -> Email { { text: \"x\" } }"));
}

#[test]
fn a_literal_of_an_all_pub_record_outside_is_fine() {
    assert_clean(&outside("pub fn f() -> Point { { x: 1, y: 2 } }"));
}

// --- updates ------------------------------------------------------------------

/// **An update is construction, even naming only `pub` fields.** The result
/// carries the private fields next to a public one the module's functions
/// never saw; with `Vector`'s `len` public and `items` private, that is how
/// a length gets ahead of its array. And one error, not the privacy refusal
/// plus a type error behind it.
#[test]
fn an_update_naming_only_pub_fields_is_refused_once() {
    assert_only(
        &outside("pub fn f(t: Tally) -> Tally { { ..t, count: 9 } }"),
        "cannot build `Tally` here: `hidden` is private to `library`, so only `library` can \
         make one. Call one of its functions that returns `Tally`, or, if `library` is yours, \
         mark every field `pub`",
    );
}

/// And a refused update does not go on to check its values against fields
/// it may not build, which would put a type error beside the refusal.
#[test]
fn a_refused_update_does_not_go_on_to_check_its_values() {
    let found = outside("pub fn f(t: Tally) -> Tally { { ..t, count: \"nine\" } }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("cannot build `Tally` here"), "{found:?}");
}

#[test]
fn an_update_inside_is_fine() {
    assert_clean(&inside("fn f(t: Tally) -> Tally { { ..t, hidden: 9 } }"));
}

#[test]
fn an_update_of_an_all_pub_record_outside_is_fine() {
    assert_clean(&outside("pub fn f(p: Point) -> Point { { ..p, x: 9 } }"));
}

// --- an update whose base is not known when it is checked ---------------------

/// The refusal an outside update of `Email` gets, however its type was found.
const EMAIL_UPDATE_REFUSED: &str = "cannot build `Email` here: `text` is private to `library`, \
     so only `library` can make one. Call one of its functions that returns `Email`, or, if \
     `library` is yours, mark every field `pub`";

const DECIMAL_UPDATE_REFUSED: &str = "cannot build `Decimal` here: `hi`, `lo` and `scale` are \
     private to `std::decimal`, so only `std::decimal` can make one. Call one of its functions \
     that returns `Decimal`";

const VECTOR_UPDATE_REFUSED: &str = "cannot build `Vector` here: `items`, `len` and `wanted` \
     are private to `std::core`, so only `std::core` can make one. Call one of its functions \
     that returns `Vector`";

/// **A type parameter is never a record**, whatever it is instantiated at,
/// so there are no fields to take from it. Checked as if it might be one
/// later, `forge` built a `Decimal` with a negative scale out of any
/// `Decimal` it was handed, from outside `std`.
#[test]
fn an_update_of_a_type_parameter_is_refused_for_a_decimal() {
    let found = errors_with_std(
        &[],
        "module program;\nimport std::decimal::{Decimal};\n\
         fn forge<A>(d: A) -> A { { ..d, scale: 0 - 4 } }\n\
         pub fn f() -> Decimal { forge(Decimal::of_int(12345)) }\n",
    );
    assert_only(&found, "`A` is not a record, so there is nothing to take fields from with `..`");
}

/// The other wrong answer, through the same door: a `Vector` whose `len`
/// runs past its array, which `get` then reads out of bounds.
#[test]
fn an_update_of_a_type_parameter_is_refused_for_a_vector() {
    let found = errors_with_std(
        &[],
        "module program;\nimport std::core::{Vector};\n\
         fn forge<A>(v: A) -> A { { ..v, len: 40 } }\n\
         pub fn f() -> Int { let v: Vector<Int> = Vector::new(); forge(v).length() }\n",
    );
    assert_only(&found, "`A` is not a record, so there is nothing to take fields from with `..`");
}

#[test]
fn an_update_of_a_type_parameter_is_refused_for_a_record_of_another_module() {
    assert_only(
        &outside(
            "fn forge<A>(e: A) -> A { { ..e, text: \"forged\" } }\n\
             pub fn f() -> String { forge(Email::parse(\"a\")).text() }",
        ),
        "`A` is not a record, so there is nothing to take fields from with `..`",
    );
}

/// Inside the declaring module too: the rule is about `A`, not about who
/// may build an `Email`. The twin that works names the type.
#[test]
fn an_update_of_a_type_parameter_is_refused_inside_the_module_as_well() {
    assert_only(
        &inside("fn forge<A>(e: A) -> A { { ..e, text: \"x\" } }"),
        "`A` is not a record, so there is nothing to take fields from with `..`",
    );
    assert_clean(&inside("fn forge(e: Email) -> Email { { ..e, text: \"x\" } }"));
}

/// **A lambda's parameter is found by its call, after the update in its
/// body was checked.** The update is checked again once it is, and refused
/// exactly as the annotated one is.
#[test]
fn a_lambda_update_of_a_decimal_is_refused_once_its_type_is_known() {
    let late = errors_with_std(
        &[],
        "module program;\nimport std::decimal::{Decimal};\n\
         pub fn f() -> Decimal {\n\
           let g = fn d => { ..d, scale: 0 - 4 };\n\
           g(Decimal::of_int(12345))\n\
         }\n",
    );
    assert_only(&late, DECIMAL_UPDATE_REFUSED);
    let annotated = errors_with_std(
        &[],
        "module program;\nimport std::decimal::{Decimal};\n\
         pub fn f() -> Decimal {\n\
           let g = fn (d: Decimal) => { ..d, scale: 0 - 4 };\n\
           g(Decimal::of_int(12345))\n\
         }\n",
    );
    assert_only(&annotated, DECIMAL_UPDATE_REFUSED);
}

#[test]
fn a_lambda_update_of_a_vector_is_refused_once_its_type_is_known() {
    let found = errors_with_std(
        &[],
        "module program;\nimport std::core::{Vector};\n\
         pub fn f() -> Int {\n\
           let v: Vector<Int> = Vector::new();\n\
           let g = fn x => { ..x, len: 40 };\n\
           let w: Vector<Int> = g(v);\n\
           w.length()\n\
         }\n",
    );
    assert_only(&found, VECTOR_UPDATE_REFUSED);
}

#[test]
fn a_lambda_update_of_a_record_of_another_module_is_refused_once_its_type_is_known() {
    assert_only(
        &outside(
            "pub fn f() -> Email {\n\
               let g = fn e => { ..e, text: \"forged\" };\n\
               g(Email::parse(\"a\"))\n\
             }",
        ),
        EMAIL_UPDATE_REFUSED,
    );
}

/// The twins: the same late-typed update, inside the declaring module and
/// on a record whose fields are all `pub`, still checks.
#[test]
fn a_lambda_update_whose_type_is_known_later_is_fine_where_it_may_build() {
    assert_clean(&inside(
        "fn f() -> Email {\n\
           let g = fn e => { ..e, text: \"x\" };\n\
           g(Email::parse(\"a\"))\n\
         }",
    ));
    assert_clean(&outside(
        "pub fn f(p: Point) -> Point {\n\
           let g = fn q => { ..q, x: 9 };\n\
           g(p)\n\
         }",
    ));
}

/// **Every field check was skipped, not only privacy.** A field the record
/// does not have, and a value of the wrong type, both checked clean, and
/// the second built a program that hung.
#[test]
fn a_lambda_update_whose_type_is_known_later_checks_its_fields() {
    assert_only(
        &outside(
            "pub fn f(p: Point) -> Point {\n\
               let g = fn q => { ..q, nosuch: 1 };\n\
               g(p)\n\
             }",
        ),
        "`Point` has no field `nosuch`",
    );
    assert_only(
        &outside(
            "pub fn f(p: Point) -> Point {\n\
               let g = fn q => { ..q, x: \"text\" };\n\
               g(p)\n\
             }",
        ),
        "field `x`: expected `Int`, found `String`",
    );
}

/// And the same pair through a type parameter, which the refusal above
/// covers: one error, not three.
#[test]
fn an_update_of_a_type_parameter_with_a_wrong_field_is_one_error() {
    assert_only(
        &outside(
            "fn forge<A>(d: A) -> A { { ..d, nosuch: 1 } }\n\
             pub fn f(p: Point) -> Point { forge(p) }",
        ),
        "`A` is not a record, so there is nothing to take fields from with `..`",
    );
    assert_only(
        &outside(
            "fn forge<A>(d: A) -> A { { ..d, x: \"text\" } }\n\
             pub fn f(p: Point) -> Point { forge(p) }",
        ),
        "`A` is not a record, so there is nothing to take fields from with `..`",
    );
}

/// A base that nothing ever settles cannot be checked at all, and says so
/// rather than passing.
#[test]
fn an_update_whose_base_is_never_known_asks_for_an_annotation() {
    assert_only(
        &outside(
            "pub fn f() -> () {\n\
               let g = fn q => { ..q, x: 9 };\n\
               ()\n\
             }",
        ),
        "the type of `..q` has to be known here to check the fields given with it; annotate \
         `q` where it is bound",
    );
}

/// **`{}` is construction too.** With an expected type from outside the
/// module it names no field to anchor the refusal on, and was let through
/// to `khora build`, which then reported a missing field at the
/// declaration.
#[test]
fn an_empty_literal_of_a_record_with_private_fields_is_refused() {
    assert_only(&outside("pub fn f() -> Email { {} }"), EMAIL_UPDATE_REFUSED);
}

// --- labeled arguments ------------------------------------------------------

/// **A case's payload is public, so labels on it work from anywhere.** A
/// labeled argument names a parameter, and a named payload's fields are its
/// parameters: privacy, which is about records, must not reach them.
#[test]
fn a_named_payload_takes_labels_from_another_module() {
    let library = "module library;\npub type Ng = | A(v: Int, w: Bool) | B(Int);\n";
    let found = errors_in_user(
        library,
        "module user;\nimport library::{Ng};\npub fn go() -> Ng { Ng::A(v: 1, w: true) }\n",
    );
    assert_clean(&found);
}

/// A labeled argument is not a way round a private field: a checking
/// constructor that takes labels builds the record itself, and a literal
/// with the same names is still refused.
#[test]
fn a_labeled_constructor_call_is_how_a_private_record_is_built_from_outside() {
    let library = "module library;\npub type Span = { start: Int, end: Int };\n\
                   impl Span {\n  pub fn of(start: Int, end: Int) -> Span {\n    \
                   { start: if start < end { start } else { end }, end: end }\n  }\n}\n";
    assert_clean(&errors_in_user(
        library,
        "module user;\nimport library::{Span};\npub fn go() -> Span { Span::of(start: 1, end: 2) }\n",
    ));
    assert_eq!(
        errors_in_user(
            library,
            "module user;\nimport library::{Span};\npub fn go() -> Span { { start: 2, end: 1 } }\n",
        )
        .len(),
        1
    );
}

// --- patterns -----------------------------------------------------------------

/// Binding a private field is reading it, in a `match` and in a `let`.
#[test]
fn binding_a_private_field_in_a_pattern_is_refused() {
    let expected = "cannot bind `text`: it is private to `library`, which declares `Email`. \
                    Call `.text()` instead, which `Email` offers publicly";
    assert_only(&outside("pub fn f(e: Email) -> String { match e { Email { text } => text } }"), expected);
    assert_only(&outside("pub fn f(e: Email) -> String { let Email { text: t } = e; t }"), expected);
}

#[test]
fn binding_a_private_field_inside_is_fine() {
    assert_clean(&inside("fn f(e: Email) -> String { let Email { text: t } = e; t }"));
}

/// A pattern naming only `pub` fields reads only what anyone may read.
#[test]
fn a_pattern_on_pub_fields_only_is_fine_outside() {
    assert_clean(&outside("pub fn f(t: Tally) -> String { let Tally { name } = t; name }"));
}

// --- newtypes -----------------------------------------------------------------

/// **A newtype is private by default.** `Port(0)`, `Port::Port(0)` and
/// `match p { Port(n) => .. }` are refused outside, so `Port::of` is the one
/// way to make one. This reversed the rule that `UserId(1)` works from
/// anywhere: a newtype is a record with one unnamed field, and two defaults
/// for one idea is what the language avoids. `type UserId = pub Int;` is the
/// spelling for an identifier anyone may make.
#[test]
fn a_closed_newtype_cannot_be_built_or_opened_outside() {
    let build = "cannot build `Port` here: its value is private to `library`, so only \
                 `library` can make one. Call one of its functions that returns `Port`, or, if \
                 `library` is yours, declare it `type Port = pub ..`";
    assert_only(&outside("pub fn f() -> Port { Port(0) }"), build);
    assert_only(&outside("pub fn f() -> Port { Port::Port(0) }"), build);
    assert_only(
        &outside("pub fn f(p: Port) -> Int { match p { Port(n) => n } }"),
        "cannot match on `Port`'s value: it is private to `library`. Use the functions \
         `library` offers for `Port`, or, if `library` is yours, declare it `type Port = pub ..`",
    );
}

#[test]
fn a_closed_newtype_is_built_and_opened_inside() {
    assert_clean(&inside("fn f(p: Port) -> Port { match p { Port(n) => Port(n + 1) } }"));
}

#[test]
fn an_open_newtype_is_built_and_opened_outside() {
    assert_clean(&outside("pub fn f(u: UserId) -> UserId { match u { UserId(n) => UserId(n + 1) } }"));
}

/// **The `pub` after `=` is not the declaration's.** `type Quiet = pub Int;`
/// is a private type with an open value; reading the second `pub` as an
/// export made every open newtype public.
#[test]
fn an_open_newtype_without_a_leading_pub_is_not_exported() {
    let found = errors_in_user(LIBRARY, "module user;\nimport library::{Quiet};\n");
    assert!(found.iter().any(|e| e.contains("Quiet")), "`Quiet` must not be importable: {found:?}");
    // And `pub type Port = Int;` is exported and closed, which the two
    // tests above already show from the importing side.
}

/// The audit's leak: a public newtype over a private record. Building the
/// wrapper from outside is refused for the wrapper and for the literal
/// inside it, each for its own reason.
#[test]
fn a_newtype_over_a_private_record_is_refused_twice() {
    let found = errors_in_user(
        "module library;\ntype Repr = { n: Int };\npub type Counter = Repr;\n\
         pub fn zero() -> Counter { Counter({ n: 0 }) }\n",
        "module user;\nimport library::{Counter};\npub fn f() -> Counter { Counter({ n: 41 }) }\n",
    );
    assert!(found.iter().any(|e| e.starts_with("cannot build `Counter` here")), "{found:?}");
}

// --- variant payloads ---------------------------------------------------------

/// A case's payload is public: matching is how a variant is used. Hiding a
/// variant's shape is a newtype over it, which the test above covers.
#[test]
fn a_case_payload_is_built_and_matched_outside() {
    let found = errors_in_user(
        "module library;\npub type Shape = | Circle(radius: Int) | Dot;\n",
        "module user;\nimport library::{Shape};\n\
         pub fn f() -> Int { match Shape::Circle(2) { Shape::Circle(r) => r, Shape::Dot => 0 } }\n",
    );
    assert_clean(&found);
}

// --- tests --------------------------------------------------------------------

/// A `test` block is inside the module it is written in.
#[test]
fn a_test_block_in_the_declaring_module_sees_private_fields() {
    assert_clean(&inside(
        "fn assert(condition: Bool);\ntest \"reads\" { assert(Email::parse(\"a\").text == \"a\"); }",
    ));
}

/// A separate `module library_test;` is outside `library`, exactly as it is
/// for `library`'s private functions.
#[test]
fn a_separate_test_module_is_outside() {
    let found = errors_in_user(
        LIBRARY,
        "module library_test;\nimport library::{Email};\n\
         fn assert(condition: Bool);\n\
         test \"reads\" { assert(Email::parse(\"a\").text == \"a\"); }\n",
    );
    assert!(found.iter().any(|e| e.starts_with("cannot read `text`")), "{found:?}");
}

// --- std ----------------------------------------------------------------------

/// **The wrong answer this closes:** a `Decimal` with a negative scale,
/// written as a literal. Every constructor clamps the scale; a literal did
/// not go through one.
#[test]
fn a_decimal_with_a_negative_scale_cannot_be_written_outside_std() {
    let found = errors_with_std(
        &[],
        "module program;\nimport std::decimal::{Decimal};\n\
         pub fn f() -> Decimal { { hi: 0, lo: 12345, scale: 0 - 4 } }\n",
    );
    assert_only(
        &found,
        "cannot build `Decimal` here: `hi`, `lo` and `scale` are private to `std::decimal`, \
         so only `std::decimal` can make one. Call one of its functions that returns `Decimal`",
    );
}

/// **And the other:** a `Vector` whose `len` runs past its array, by
/// assignment. `std` is never told "if it is yours".
#[test]
fn a_vectors_length_cannot_be_assigned_outside_std() {
    let found = errors_with_std(
        &[],
        "module program;\nimport std::core::{Vector};\n\
         pub fn f() -> Int { let mut v: Vector<Int> = Vector::new(); v.len = 40; v.length() }\n",
    );
    assert_only(
        &found,
        "cannot assign to `len`: it is private to `std::core`, which declares `Vector`. Use \
         the functions `std::core` offers for `Vector`",
    );
}

/// The getters `std` added in place of the fields read the same numbers.
#[test]
fn stds_getters_read_what_the_fields_held() {
    let found = errors_with_std(
        &[],
        "module program;\nimport std::core::{Option};\nimport std::time::{Date, Time, Offset};\n\
         import std::decimal::{Decimal};\n\
         pub fn f(d: Date, t: Time, o: Offset, x: Decimal) -> Int {\n\
           d.year() + d.month() + d.day() + t.hour() + t.minute() + t.second() + t.milli()\n\
             + o.minutes() + x.scale()\n\
         }\n",
    );
    assert_clean(&found);
}

/// **The spoof.** A module in somebody else's package that calls itself
/// `std::forged` is still not `std::decimal`: the unit is the module, and a
/// shared first segment grants nothing. The package unit the design rejected
/// would have let this through.
#[test]
fn a_module_claiming_a_std_path_cannot_build_a_decimal() {
    let found = errors_with_std(
        &[],
        "module std::forged;\nimport std::decimal::{Decimal};\n\
         pub fn bad() -> Decimal { { hi: 0, lo: 12345, scale: 0 - 4 } }\n",
    );
    assert!(found.iter().any(|e| e.starts_with("cannot build `Decimal` here")), "{found:?}");
}

/// `std::schema::struct` takes a literal, so it cannot build a record with
/// private fields either.
#[test]
fn schema_struct_over_a_private_record_is_a_refused_literal() {
    let found = errors_with_std(
        &[("library.kh", "module library;\npub type Email = { text: String };\n")],
        "module program;\nimport std::schema::{Schema, string, struct};\nimport library::{Email};\n\
         pub fn f() -> Schema<Email> { struct({ text: string() }) }\n",
    );
    assert!(found.iter().any(|e| e.starts_with("cannot build `Email` here")), "{found:?}");
}

/// **A derive that reads is allowed on private fields**: it is written in the
/// declaring module, and the author opted in by writing `derive`. Outside,
/// `show` and `==` work on a value whose fields cannot be named.
#[test]
fn derived_show_and_eq_read_private_fields_and_work_from_outside() {
    let found = errors_with_std(
        &[(
            "library.kh",
            "module library;\nimport std::core::{Eq, Show, String};\n\
             derive(Eq, Show)\npub type Email = { text: String };\n\
             impl Email { pub fn parse(text: String) -> Email { { text: text } } }\n",
        )],
        "module program;\nimport std::core::{Eq, Show, String};\nimport library::{Email};\n\
         pub fn f() -> String { let a = Email::parse(\"x\"); if a == Email::parse(\"x\") { a.show() } else { \"\" } }\n",
    );
    assert_clean(&found);
}

/// An impl written in another module for somebody else's type is ordinary
/// code there, and gets no access the rest of that module does not have.
#[test]
fn an_impl_in_another_module_cannot_read_private_fields() {
    let found = errors_in_user(
        LIBRARY,
        "module user;\nimport library::{Email};\n\
         pub trait Loud { fn loud(self) -> String; }\n\
         impl Loud for Email { fn loud(self) -> String { self.text } }\n",
    );
    assert!(found.iter().any(|e| e.starts_with("cannot read `text`")), "{found:?}");
}

/// `a.b.c = x` assigns only `c`: a private `b` is refused as a read.
#[test]
fn only_the_outermost_projection_of_an_assignment_is_the_write() {
    let found = errors_in_user(
        "module library;\npub type Inner = { pub mut n: Int };\n\
         pub type Outer = { inner: Inner };\n",
        "module user;\nimport library::{Inner, Outer};\n\
         pub fn f(o: Outer) -> () { o.inner.n = 1; }\n",
    );
    assert_only(
        &found,
        "cannot read `inner`: it is private to `library`, which declares `Outer`. Use the \
         functions `library` offers for `Outer`, or, if `library` is yours, write `pub inner` \
         in its declaration",
    );
}

// --- derive(Decode) -----------------------------------------------------------

const DECODED: &str = "\
module library;
import std::core::{Option, String};
import std::schema::{Decode, Schema, string};
";

/// **The decoder bypass.** A derived decoder writes a literal inside the
/// declaring module, where privacy allows it, so it would build from input a
/// `Checked` that `Checked::of` refuses. Refused, with the fix named.
#[test]
fn derive_decode_on_a_pub_type_with_a_private_field_is_refused() {
    let found = errors_with_std(
        &[],
        &format!("{DECODED}derive(Decode)\npub type Checked = {{ text: String }};\n")
            .replace("module library;", "module program;"),
    );
    assert_only(
        &found,
        "`derive(Decode)` would build `Checked` without calling any of its functions, and \
         `text` is private, so input could skip whatever they check. Write `impl Decode for \
         Checked` with `Schema::try_map` over the function that checks, or mark every field \
         `pub` if nothing needs checking",
    );
}

/// The same refusal for a closed newtype, which has no field to name.
#[test]
fn derive_decode_on_a_closed_pub_newtype_is_refused() {
    let found = errors_with_std(
        &[],
        &format!("{DECODED}derive(Decode)\npub type Port = Int;\n")
            .replace("module library;", "module program;"),
    );
    assert!(
        found.iter().any(|e| e.contains("`Port`'s value is private")
            && e.contains("declare it `type Port = pub ..`")),
        "{found:?}"
    );
}

/// A private type's fields protect nothing its own module cannot write, and
/// an all-`pub` record has nothing to protect: both keep their derive.
#[test]
fn derive_decode_is_allowed_on_a_private_type_and_on_an_all_pub_one() {
    for decl in ["type Local = { text: String };", "pub type Dto = { pub text: String };"] {
        let found = errors_with_std(
            &[],
            &format!("{DECODED}derive(Decode)\n{decl}\n").replace("module library;", "module program;"),
        );
        assert_clean(&found);
    }
}

/// **The fix the message names compiles**, and a type *holding* a checked
/// one decodes through the checked one's own `impl Decode`.
#[test]
fn a_hand_written_decode_over_try_map_is_the_way_through() {
    let library = "\
module library;
import std::core::{Option, String};
import std::schema::{Decode, Schema, string};
pub type Checked = { text: String };
impl Checked {
  pub fn of(text: String) -> Option<Checked> {
    if text == \"\" { Option::None } else { Option::Some({ text: text }) }
  }
}
impl Decode for Checked {
  fn schema() -> Schema<Checked> { string().try_map(\"a non-empty string\", fn s => Checked::of(s)) }
}
";
    let found = errors_with_std(
        &[("library.kh", library)],
        "module program;\nimport std::schema::{Decode};\nimport library::{Checked};\n\
         derive(Decode)\npub type Holder = { pub inner: Checked };\n",
    );
    assert_clean(&found);
}
