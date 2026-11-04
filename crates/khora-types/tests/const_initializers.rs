//! A `const` is inlined at each mention, and its initializer has to mean the
//! same thing wherever it is mentioned.
//!
//! The initializer is lowered in the middle of whatever body names the
//! constant, so every piece of state that body has open -- its locals, the
//! closure it is inside, the loop around it, its type parameters, the `${..}`
//! it is written in -- is state the initializer must not see. Each test here
//! names a constant from inside one of those and asks for exactly what the
//! same constant says named from nowhere in particular.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the std directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic for `program`, compiled with `std`, as `(message, the
/// source text it underlines)`, in the order `khora check` prints them.
fn said(program: &str) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("std should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    let mine = SourceFile::new(&db, PathBuf::from("program.kh"), program.to_string());
    files.push(mine);
    SourceRoot::new(&db, files);
    khora_types::diagnostics(&db, mine)
        .iter()
        .map(|e| {
            let span = program[usize::from(e.range.start())..usize::from(e.range.end())].to_string();
            (e.message.clone(), span)
        })
        .collect()
}

fn one(message: &str, span: &str) -> Vec<(String, String)> {
    vec![(message.to_string(), span.to_string())]
}

/// **A closure around the mention made the compiler panic.** Lowering an
/// unresolved name inside a lambda invents a capability binding for it and
/// defers the error to the checker; the initializer was lowered with the
/// mention's lambda still open and its scopes taken away, so the invented
/// binding was declared into a scope stack that was empty.
#[test]
fn a_const_with_an_unresolved_name_named_inside_a_closure_reports_the_name() {
    let found = said(
        "module main;\n\
         const CA = CA_PEM;\n\
         fn round() -> Int {\n  let g = fn () => CA;\n  0\n}\n",
    );
    assert_eq!(found, one("cannot find `CA_PEM` in this scope", "CA_PEM"));
}

/// The same constant named outside any closure, which was always right: the
/// sentence the closure case has to match.
#[test]
fn a_const_with_an_unresolved_name_named_at_the_top_of_a_function_reports_the_name() {
    let found = said(
        "module main;\n\
         const CA = CA_PEM;\n\
         fn round() -> Int {\n  let x = CA;\n  0\n}\n",
    );
    assert_eq!(found, one("cannot find `CA_PEM` in this scope", "CA_PEM"));
}

/// **A `break` in a constant broke out of the loop around its mention.** It
/// is not in a loop where it is written, and that is where it is judged.
#[test]
fn a_break_in_a_const_is_outside_a_loop_wherever_it_is_named() {
    let found = said(
        "module main;\n\
         const X = break;\n\
         fn spin() -> Int {\n  loop { X; };\n  0\n}\n",
    );
    assert_eq!(found, one("`break` outside a loop", "break"));
}

/// **A constant's free name meant the closure being defined around its
/// mention.** `let g = fn () => C;` with `const C = g;` made `C` the closure
/// itself, and the checker refused an infinite type nobody wrote.
#[test]
fn a_const_cannot_see_the_closure_it_is_named_in() {
    let found = said(
        "module main;\n\
         const C = g;\n\
         fn named() -> Int {\n  let g = fn () => C;\n  0\n}\n",
    );
    assert_eq!(found, one("cannot find `g` in this scope", "g"));
}

/// Nor the mention's locals: `x` in an initializer is not whichever `x` the
/// function naming it happens to have. The declaration says it cannot find
/// `x` either way, so what shows a leak is the mention's `x` being used: a
/// `String` named `x` made `K + 1` a second error, about a type the
/// constant never had.
#[test]
fn a_const_cannot_see_the_locals_of_the_function_naming_it() {
    let found = said(
        "module main;\n\
         const K = x + 1;\n\
         fn named() -> Int {\n  let x = \"s\";\n  K\n}\n",
    );
    assert_eq!(found, one("cannot find `x` in this scope", "x"));
}

/// **A constant saw the type parameters of the function naming it.** `T` in
/// an initializer is not the `T` of whichever generic function happens to
/// mention it; at module level there is no `T` at all.
#[test]
fn a_const_cannot_see_the_type_parameters_of_the_function_naming_it() {
    let found = said(
        "module main;\n\
         const D = T::zero();\n\
         fn generic<T>() -> Int {\n  let d = D;\n  0\n}\n",
    );
    assert_eq!(found, one("cannot resolve `T::zero` in this scope", "T::zero"));
}

/// **A constant named in `${..}` had its error moved by the hole's offset.**
/// A hole is parsed on its own and its ranges shifted back into the string;
/// the initializer is not in the hole, and was shifted anyway, so the error
/// pointed past the end of the file.
#[test]
fn a_const_named_in_an_interpolation_reports_where_the_const_is() {
    let found = said(
        "module main;\n\
         const E = NOPE;\n\
         fn shown() -> String {\n  \"abc ${E}\"\n}\n",
    );
    assert_eq!(found, one("cannot find `NOPE` in this scope", "NOPE"));
}

/// **A lower-case name a constant cannot resolve was explained as a
/// capability its mention's function forgot to ask for.** `clock` is a label
/// `std` uses, so the message offered a `with` clause -- for the function
/// naming the constant, which is not where the name is written, and a
/// constant has no `with` clause to add.
#[test]
fn a_const_naming_a_capability_label_is_not_told_to_add_a_with_clause() {
    let found = said(
        "module main;\n\
         const C = clock;\n\
         fn timed() -> Int {\n  let c = C;\n  0\n}\n",
    );
    assert_eq!(found, one("cannot find `clock` in this scope", "clock"));
}

// --- every initializer is checked once, at its declaration -----------------

/// **A constant nobody named was never looked at.** Constants are inlined at
/// each mention, so one without a mention was never lowered, and
/// `const A = UNDEFINED_NAME;` passed `khora check`.
#[test]
fn an_unmentioned_const_with_an_unresolved_name_is_reported() {
    let found = said("module main;\nconst A = UNDEFINED_NAME;\npub fn main() -> Int { 0 }\n");
    assert_eq!(found, one("cannot find `UNDEFINED_NAME` in this scope", "UNDEFINED_NAME"));
}

/// Type errors are left to a mention: an initializer named nowhere is not
/// asked what type it has. See `Checker::kept` for why this is narrow.
#[test]
fn an_unmentioned_const_with_a_type_error_is_left_to_its_mention() {
    let found = said("module main;\nconst A = 1 + true;\npub fn main() -> Int { 0 }\n");
    assert!(found.is_empty(), "{found:#?}");
    let found = said("module main;\nconst A = 1 + true;\npub fn main() -> Int { A }\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].1, "true", "{found:#?}");
}

/// An annotation is a claim about the initializer, and is checked against it.
#[test]
fn a_const_annotation_is_checked_against_its_initializer() {
    let found = said("module main;\nconst A: Int = \"s\";\npub fn main() -> Int { 0 }\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].1, "\"s\"", "{found:#?}");
}

/// **One mistake, one error, however often the constant is named.**
#[test]
fn a_const_named_twice_reports_its_unresolved_name_once() {
    let found = said(
        "module main;\n\
         const A = UNDEFINED_NAME;\n\
         fn one() -> Int { let a = A; 0 }\n\
         fn two() -> Int { let a = A; let b = A; 0 }\n",
    );
    assert_eq!(found, one("cannot find `UNDEFINED_NAME` in this scope", "UNDEFINED_NAME"));
}

#[test]
fn a_const_named_twice_reports_its_type_error_once() {
    let found = said(
        "module main;\n\
         const A = 1 + true;\n\
         fn one() -> Int { A }\n\
         fn two() -> Int { A + A }\n",
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].1, "true", "{found:#?}");
}

/// A cycle is one mistake whichever of its constants is named, and whether
/// either is.
#[test]
fn a_const_cycle_is_reported_once() {
    for named in ["", "fn f() -> Int { a }\n", "fn f() -> Int { b }\n", "fn f() -> Int { a + b }\n"] {
        let found = said(&format!("module main;\nconst a = b;\nconst b = a;\n{named}"));
        assert_eq!(found.len(), 1, "{named}: {found:#?}");
        assert!(found[0].0.contains("is defined in terms of itself"), "{named}: {found:#?}");
    }
}

/// What checking an initializer on its own must not refuse: what only a
/// mention decides. An empty list's element type, a generic constructor's
/// type arguments, a closure's parameter, a literal's width under an
/// annotation, and a call that can fail -- whose `!` leaves the function the
/// constant is named in, not the constant. A closure naming a capability
/// label, which the row it is handed to supplies, whether a program's own
/// effect or `std`'s `log`, named or not. A record literal two record types
/// fit, which the mention's expected type chooses between. And `return`,
/// which returns from the function the constant is named in.
#[test]
fn checking_an_initializer_alone_refuses_nothing_a_mention_would_accept() {
    let found = said(
        "module main;\n\
         import std::core::{List, Map, Option, print};\n\
         import std::log::{Log, Severity};\n\
         type Oops = | Bad;\n\
         fn may(n: Int) -> Int raises Oops { if n > 0 { n } else { raise Oops::Bad } }\n\
         const EMPTY = [];\n\
         const TABLE = Map::new();\n\
         const NOTHING = Option::None;\n\
         const ID = fn x => x;\n\
         const SMALL: U8 = 200;\n\
         const RISKY = may(1)!;\n\
         const UNUSED_RISKY = may(2)!;\n\
         fn f() -> Int raises Oops {\n\
           let xs: List<Int> = EMPTY;\n\
           let t: Map<String, Int> = TABLE;\n\
           let n: Option<Int> = NOTHING;\n\
           ID(RISKY)\n\
         }\n\
         effect Name { get: () -> String }\n\
         const SAY = fn () => name.get();\n\
         const UNUSED_SAY = fn () => name.get();\n\
         fn call(k: () -> String with { name: Name }) -> String with { name: Name } { k() }\n\
         fn said_it() -> () {\n\
           with { name: handler for Name { get: fn () => \"N\" } } { print(call(SAY)) }\n\
         }\n\
         const LOGS = fn () => log.record(Severity::Info, \"x\", []);\n\
         const UNUSED_LOGS = fn () => log.record(Severity::Info, \"y\", []);\n\
         fn run(k: () -> () with { log: Log }) -> () with { log: Log } { k() }\n\
         fn logged() -> () with { log: Log } { run(LOGS) }\n\
         type P = { x: Int, y: Int };\n\
         type Q = { x: Int, y: Int };\n\
         const ANON = { x: 1, y: 2 };\n\
         fn picked() -> Int { let q: Q = ANON; q.x }\n\
         const R = return 5;\n\
         fn returned() -> Int { R }\n\
         const UNUSED_R = return 6;\n",
    );
    assert!(found.is_empty(), "{found:#?}");
}

/// **An annotation naming no type is one mistake.** `cannot find type
/// `Nope`` is the error; checking the initializer against a type that does
/// not exist added "expected `Nope`, found `Int`" after it.
#[test]
fn a_const_annotated_with_an_unknown_type_reports_the_type_once() {
    let found = said("module main;\nconst A: Nope = 3;\npub fn main() -> Int { let a = A; 0 }\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].0.starts_with("cannot find type `Nope`"), "{found:#?}");
}

/// **The loop around a mention stays outside the constant**, where the
/// constant's own check cannot see it. In an element `for`, the mention's
/// loop would say `break` cannot leave it early -- a second sentence about a
/// `break` that is outside any loop where it is written. A written loop is no
/// test of this: the mention would say nothing, and the constant's own
/// check says "outside a loop" alone.
#[test]
fn a_break_in_a_const_named_in_an_element_for_is_outside_a_loop() {
    let found = said(
        "module main;\n\
         import std::core::{List, Step, Iterator};\n\
         const X = break;\n\
         fn f(rows: List<Int>) -> List<Int> { [for r in rows => X] }\n",
    );
    assert_eq!(found, one("`break` outside a loop", "break"));
}
