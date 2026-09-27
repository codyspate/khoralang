//! A bare name in a pattern that is the name of one of its value's cases.
//!
//! **A bare name in a pattern binds**, so `Red => "warm"` over a `Color`
//! matched every color and answered "warm" for green. The only thing said
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

const COLOR: &str = "module m;\npub type Color = | Red | Green | Blue;\n";

/// The headline: the arm after the bare name is not unreachable, because it
/// comes first, so nothing about coverage ever looked wrong.
#[test]
fn a_bare_case_name_in_a_match_is_refused() {
    let text = format!(
        "{COLOR}fn describe(c: Color) -> String {{ match c {{ Color::Blue => \"cool\", Red => \"warm\" }} }}\n"
    );
    assert_reports(
        &text,
        "`Red` is a case of `Color`, and a bare name in a pattern binds rather than matching \
         one -- this would match every `Color`. Write `Color::Red` to match the case, or pick \
         another name to bind the value",
    );
}

/// **One error, at the name.** The arm it swallows is not reported as well:
/// the pattern is marked broken, so coverage says nothing more about it.
#[test]
fn the_refusal_is_the_only_error() {
    let text = format!(
        "{COLOR}fn go(c: Color) -> Int {{ match c {{ Red => 1, Color::Green => 2, Color::Blue => 3 }} }}\n"
    );
    let found = errors(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("is a case of `Color`"), "{found:?}");
}

#[test]
fn a_bare_case_name_in_a_let_is_refused() {
    let text = format!("{COLOR}fn go() -> Int {{ let Red = Color::Green; 0 }}\n");
    assert_reports(&text, "`Red` is a case of `Color`");
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
        "{COLOR}fn f(pair: (Color, Int)) -> Int {{ match pair {{ (Color::Blue, _) => 0, (Red, n) => n }} }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Color`");
}

#[test]
fn a_bare_case_name_nested_in_a_payload_is_refused() {
    let text = format!(
        "{COLOR}pub type Maybe<A> = | Some(v: A) | None;\n\
         fn g(o: Maybe<Color>) -> Int {{ match o {{ Maybe::Some(Color::Blue) => 1, Maybe::Some(Red) => 2, Maybe::None => 3 }} }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Color`");
}

/// A record pattern's sub-pattern goes through the same lowering as the
/// rest; the `{ name }` shorthand does not, and names a field.
#[test]
fn a_bare_case_name_under_a_record_field_is_refused() {
    let text = format!(
        "{COLOR}pub type Pen = {{ color: Color, width: Int }};\n\
         fn f(p: Pen) -> Int {{ match p {{ Pen {{ color: Red, width }} => width }} }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Color`");
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
        "{COLOR}fn apply(c: Color, f: (Color) -> Int) -> Int {{ f(c) }}\n\
         fn go() -> Int {{ apply(Color::Green, fn c => match c {{ Color::Blue => 1, Red => 2 }}) }}\n"
    );
    assert_reports(&text, "`Red` is a case of `Color`");
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
         pub fn color(n: Int) -> Shade { if n == 0 { Shade::Dark } else { Shade::Light } }\n\
         pub fn load(n: Int) -> Int raises LoadError { if n == 0 { raise LoadError::Missing; } n }\n"
            .to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\n\
         import errs::{color, load};\n\
         fn shade(n: Int) -> Int { match color(n) { Light => 1 } }\n\
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
        "{COLOR}pub type Other = | stop | go;\n\
         pub type Maybe<A> = | Some(v: A) | None;\n\
         fn a(c: Color) -> Color {{ match c {{ Color::Red => c, stop => stop }} }}\n\
         fn b(o: Maybe<Int>) -> Int {{ match o {{ Maybe::Some(n) => n, Maybe::None => 0 }} }}\n\
         fn c(o: Maybe<Color>) -> Color {{ match o {{ Maybe::Some(Color::Red) => Color::Red, other => Color::Blue }} }}\n"
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

/// **A2, the owner's decision: a capitalized bare name that is no case.**
///
/// Kept in its own block so it goes with `refuse_capitalized_binding` if the
/// owner drops it. The rule above cannot see either of these: `Gren` is no
/// case of `Color`, and `FAVORITE` binds rather than compares, so both
/// were catch-alls with at most an `unused-binding` warning.
mod a2 {
    use super::*;

    /// Read like an undefined name, with the case it was one typo away from.
    #[test]
    fn a_misspelled_case_suggests_the_nearest_case() {
        let text = format!(
            "{COLOR}fn describe(c: Color) -> Int {{ match c {{ Color::Blue => 1, Color::Red => 2, Gren => 3 }} }}\n"
        );
        let found = errors(&text);
        assert_eq!(
            found,
            vec!["`Color` has no case `Gren`. Did you mean `Color::Green`?".to_string()],
            "{text}"
        );
    }

    /// The suggestion is built to compile as written: a payload case takes
    /// one `_` per field, as the bare-case message's does.
    #[test]
    fn the_suggestion_for_a_payload_case_carries_its_fields() {
        let text = "module m;\npub type FsError = | NotFound(String) | Denied(String, Int);\n\
            fn f(e: FsError) -> Int { match e { FsError::Denied(_, n) => n, NotFond => 0 } }\n";
        assert_reports(text, "`FsError` has no case `NotFond`. Did you mean `FsError::NotFound(_)`?");
    }

    /// A threshold of a third of the name would give a four-letter name one
    /// edit; the floor of two is what lets `Rde` find `Red`.
    #[test]
    fn two_edits_are_allowed_however_short_the_name() {
        let text = format!(
            "{COLOR}fn describe(c: Color) -> Int {{ match c {{ Color::Blue => 1, Color::Green => 2, Rde => 3 }} }}\n"
        );
        assert_reports(&text, "`Color` has no case `Rde`. Did you mean `Color::Red`?");
    }

    /// Nothing near: no guess, and the rule the name broke instead.
    #[test]
    fn a_name_near_no_case_is_refused_without_a_suggestion() {
        let text = format!(
            "{COLOR}fn describe(c: Color) -> Int {{ match c {{ Color::Blue => 1, Other => 3 }} }}\n"
        );
        let found = errors(&text);
        assert_eq!(
            found,
            vec![
                "`Color` has no case `Other`. A name in a pattern that starts with a capital \
                 letter must be a case; bind the value with a lower-case name."
                    .to_string()
            ],
            "{text}"
        );
    }

    /// A value with no cases to name: the rule, and -- because a `const` of
    /// that name is in scope -- the guard that compares against it.
    #[test]
    fn a_const_written_as_a_pattern_is_refused() {
        let text = "module m;\nconst FAVORITE: Int = 7;\n\
            fn lucky(n: Int) -> Int { match n { FAVORITE => 1 } }\n";
        let found = errors(text);
        assert_eq!(
            found,
            vec![
                "`FAVORITE` is not a case of `Int`. A name in a pattern that starts with a \
                 capital letter must be a case; bind the value with a lower-case name. A \
                 pattern can't compare against a `const`; use `n if n == FAVORITE` or an `if`."
                    .to_string()
            ],
            "{text}"
        );
    }

    /// No `const` of the name, so nothing about comparing against one.
    #[test]
    fn a_capitalized_name_over_an_int_is_refused_without_the_const_advice() {
        let text = "module m;\nfn lucky(n: Int) -> Int { match n { Seven => 1 } }\n";
        let found = errors(text);
        assert_eq!(
            found,
            vec![
                "`Seven` is not a case of `Int`. A name in a pattern that starts with a \
                 capital letter must be a case; bind the value with a lower-case name."
                    .to_string()
            ],
            "{text}"
        );
    }

    /// A lower-case binding is what binding looks like, and stays clean.
    #[test]
    fn a_lower_case_binding_is_not_refused() {
        assert_clean(&format!(
            "{COLOR}fn describe(c: Color) -> Int {{ match c {{ Color::Blue => 1, gren => 3 }} }}\n"
        ));
    }
}
