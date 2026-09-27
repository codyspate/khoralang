//! A bare name in a pattern that is the name of one of its value's cases.
//!
//! **A bare name in a pattern binds**, so `Red => "warm"` over a `Colour`
//! matched every colour and answered "warm" for green. The only thing said
//! was that `Red` was never read -- a warning, gone as soon as the arm used
//! the name. A `catch` of the same shape built a binary that died with
//! `Illegal instruction`. Every case here checked clean before the rule.
//!
//! The programs are the design round's probes, cut down to one file (or two,
//! for the type the file never imports) and without `std`.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

const COLOUR: &str = "module m;\npub type Colour = | Red | Green | Blue;\n";

/// The headline: the arm after the bare name is not unreachable, because it
/// comes first, so nothing about coverage ever looked wrong.
#[test]
fn a_bare_case_name_in_a_match_is_refused() {
    let text = format!(
        "{COLOUR}fn describe(c: Colour) -> String {{ match c {{ Colour::Blue => \"cool\", Red => \"warm\" }} }}\n"
    );
    assert_reports(
        &text,
        "`Red` is a case of `Colour`, and a bare name in a pattern binds rather than matching \
         one -- this would match every `Colour`. Write `Colour::Red` to match the case, or pick \
         another name to bind the value",
    );
}

/// **One error, at the name.** The arm it swallows is not reported as well:
/// the pattern is marked broken, so coverage says nothing more about it.
#[test]
fn the_refusal_is_the_only_error() {
    let text = format!(
        "{COLOUR}fn go(c: Colour) -> Int {{ match c {{ Red => 1, Colour::Green => 2, Colour::Blue => 3 }} }}\n"
    );
    let found = errors(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("is a case of `Colour`"), "{found:?}");
}

#[test]
fn a_bare_case_name_in_a_let_is_refused() {
    let text = format!("{COLOUR}fn go() -> Int {{ let Red = Colour::Green; 0 }}\n");
    assert_reports(&text, "`Red` is a case of `Colour`");
}

#[test]
fn a_bare_case_name_in_a_catch_is_refused() {
    let text = "module m;\n\
        pub type LoadError = | Missing | Broken(String);\n\
        fn load(n: Int) -> Int raises LoadError { if n == 0 { raise LoadError::Missing; } n }\n\
        fn go(n: Int) -> Int { load(n)! catch { LoadError::Broken(_) => 1, Missing => 2 } }\n";
    assert_reports(text, "`Missing` is a case of `LoadError`");
}

#[test]
fn a_bare_case_name_nested_in_a_tuple_is_refused() {
    let text = format!(
        "{COLOUR}fn f(pair: (Colour, Int)) -> Int {{ match pair {{ (Colour::Blue, _) => 0, (Red, n) => n }} }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Colour`");
}

#[test]
fn a_bare_case_name_nested_in_a_payload_is_refused() {
    let text = format!(
        "{COLOUR}pub type Maybe<A> = | Some(v: A) | None;\n\
         fn g(o: Maybe<Colour>) -> Int {{ match o {{ Maybe::Some(Colour::Blue) => 1, Maybe::Some(Red) => 2, Maybe::None => 3 }} }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Colour`");
}

/// A record pattern's sub-pattern goes through the same lowering as the
/// rest; the `{ name }` shorthand does not, and names a field.
#[test]
fn a_bare_case_name_under_a_record_field_is_refused() {
    let text = format!(
        "{COLOUR}pub type Pen = {{ colour: Colour, width: Int }};\n\
         fn f(p: Pen) -> Int {{ match p {{ Pen {{ colour: Red, width }} => width }} }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Colour`");
}

/// A `None` after `Option::Some(n)`: the Rust habit, which binds here.
#[test]
fn the_rust_habit_is_refused() {
    let text = "module m;\npub type Maybe<A> = | Some(v: A) | None;\n\
        fn f(o: Maybe<Int>) -> Int { match o { Maybe::Some(n) => n, None => 0 } }\n";
    assert_reports(text, "`None` is a case of `Maybe`");
}

/// **The value's type is a variable when the pattern is bound**, and only
/// settles when the call around the lambda is inferred. Asked at bind time,
/// this passes.
#[test]
fn a_bare_case_name_in_an_inferred_lambda_is_refused() {
    let text = format!(
        "{COLOUR}fn apply(c: Colour, f: (Colour) -> Int) -> Int {{ f(c) }}\n\
         fn go() -> Int {{ apply(Colour::Green, fn c => match c {{ Colour::Blue => 1, Red => 2 }}) }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Colour`");
}

/// Written with its payload the name is a constructor already refused; this
/// is the name alone, which binds. The suggestion carries one `_` per field
/// so that it compiles as written.
#[test]
fn a_payload_case_written_bare_is_refused() {
    let text = "module m;\npub type FsError = | NotFound(String) | Denied(String, Int);\n\
        fn f(e: FsError) -> Int { match e { FsError::Denied(_, n) => n, NotFound => 0 } }\n";
    assert_reports(text, "Write `FsError::NotFound(_)` to match the case");
}

/// The rule is about names, not capital letters.
#[test]
fn a_lower_case_case_name_is_refused() {
    let text = "module m;\npub type Mode = | fast | slow;\n\
        fn f(m: Mode) -> Int { match m { Mode::fast => 1, slow => 2 } }\n";
    assert_reports(text, "`slow` is a case of `Mode`");
}

/// **A type's only case.** The binding matched exactly what the case would
/// have, so the answer was right; only what it looked like was wrong. The
/// message says so, and offers `_` rather than a qualified name.
#[test]
fn a_single_case_types_own_name_is_refused_as_misleading() {
    let text = "module m;\n\
        pub type Stop = { why: String };\n\
        fn worker() -> () raises Stop { () }\n\
        fn run() -> () { worker()! catch { Stop => () }; }\n";
    assert_reports(
        text,
        "`Stop` is the name of `Stop`'s only case, and a bare name in a pattern binds rather \
         than matching it. Write `_` to match any `Stop`, or a lower-case name to bind it",
    );
}

#[test]
fn a_wrapper_types_own_name_is_refused_as_misleading() {
    let text = "module m;\npub type UserId = Int;\n\
        fn g(u: UserId) -> Int { match u { UserId => 2 } }\n";
    assert_reports(text, "`UserId` is the name of `UserId`'s only case");
}

/// **The value's type comes from a call, and this file never names it.**
/// Checked against the scope, the name would be nothing and bind in
/// silence; the check is against the value's own type, and the message says
/// what to import.
#[test]
fn a_case_of_a_type_the_file_never_imports_is_refused() {
    let db = KhoraDatabase::new();
    let errs = SourceFile::new(
        &db,
        "errs.kh".into(),
        "module errs;\n\
         pub type Shade = | Dark | Light;\n\
         pub type LoadError = | Missing | Broken(String);\n\
         pub fn colour(n: Int) -> Shade { if n == 0 { Shade::Dark } else { Shade::Light } }\n\
         pub fn load(n: Int) -> Int raises LoadError { if n == 0 { raise LoadError::Missing; } n }\n"
            .to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\n\
         import errs::{colour, load};\n\
         fn shade(n: Int) -> Int { match colour(n) { Light => 1 } }\n\
         fn go(n: Int) -> Int { load(n)! catch { Missing => 2 } }\n"
            .to_string(),
    );
    SourceRoot::new(&db, vec![errs, app]);
    let found: Vec<String> =
        diagnostics(&db, app).iter().map(|e| e.message.clone()).collect();
    assert!(
        found.iter().any(|e| e.contains(
            "`Light` is a case of `Shade`, and a bare name in a pattern binds rather than \
             matching one -- this would match every `Shade`. Write `Shade::Light` (with `Shade` \
             imported from `errs`) to match the case"
        )),
        "{found:?}"
    );
    assert!(found.iter().any(|e| e.contains("`Missing` is a case of `LoadError`")), "{found:?}");
}

/// **What the rule leaves alone.** A binding whose name is a case of some
/// *other* type, a lower-case binding, a payload binding and the written
/// forms of every case: none of these is a case of the value it binds.
/// Lower-case throughout, so that this says nothing about A2.
#[test]
fn an_ordinary_binding_is_not_refused() {
    assert_clean(&format!(
        "{COLOUR}pub type Other = | stop | go;\n\
         pub type Maybe<A> = | Some(v: A) | None;\n\
         fn a(c: Colour) -> Colour {{ match c {{ Colour::Red => c, stop => stop }} }}\n\
         fn b(o: Maybe<Int>) -> Int {{ match o {{ Maybe::Some(n) => n, Maybe::None => 0 }} }}\n\
         fn c(o: Maybe<Colour>) -> Colour {{ match o {{ Maybe::Some(Colour::Red) => Colour::Red, other => Colour::Blue }} }}\n"
    ));
}

/// The record shorthand binds a *field*, and a field may share a case's
/// name without anybody having misread anything: `{ on }` here binds a
/// `Flag` called `on`, which is also a case of `Flag`, and is left alone.
#[test]
fn the_record_shorthand_is_not_asked() {
    assert_clean(
        "module m;\npub type Flag = | on | off;\npub type Box = { on: Flag };\n\
         fn f(b: Box) -> Flag { match b { Box { on } => on } }\n",
    );
}

/// The qualified and `_` spellings the messages suggest check clean.
#[test]
fn the_suggested_spellings_are_accepted() {
    assert_clean(
        "module m;\n\
         pub type Stop = { why: String };\n\
         pub type FsError = | NotFound(String) | Denied(String, Int);\n\
         fn worker() -> () raises Stop { () }\n\
         fn run() -> () { worker()! catch { _ => () }; }\n\
         fn f(e: FsError) -> Int { match e { FsError::Denied(_, n) => n, FsError::NotFound(_) => 0 } }\n\
         fn g(s: Stop) -> Int { match s { Stop {} => 1 } }\n",
    );
}

/// **A2, the owner's decision: a capitalised bare name that is no case.**
///
/// Kept in its own block so it goes with `refuse_capitalised_binding` if the
/// owner drops it. The rule above cannot see either of these: `Gren` is no
/// case of `Colour`, and `FAVOURITE` binds rather than compares, so both
/// were catch-alls with at most an `unused-binding` warning.
mod a2 {
    use super::*;

    #[test]
    fn a_misspelt_case_is_refused() {
        let text = format!(
            "{COLOUR}fn describe(c: Colour) -> Int {{ match c {{ Colour::Blue => 1, Colour::Red => 2, Gren => 3 }} }}\n"
        );
        assert_reports(
            &text,
            "`Gren` binds the value, because it is no case of `Colour` -- and a capitalised name \
             in a pattern reads as a case",
        );
    }

    #[test]
    fn a_const_written_as_a_pattern_is_refused() {
        let text = "module m;\nconst FAVOURITE: Int = 7;\n\
            fn lucky(n: Int) -> Int { match n { FAVOURITE => 1 } }\n";
        assert_reports(text, "`FAVOURITE` binds the value, because it is no case of `Int`");
    }

    /// A lower-case binding is what binding looks like, and stays clean.
    #[test]
    fn a_lower_case_binding_is_not_refused() {
        assert_clean(&format!(
            "{COLOUR}fn describe(c: Colour) -> Int {{ match c {{ Colour::Blue => 1, gren => 3 }} }}\n"
        ));
    }
}
