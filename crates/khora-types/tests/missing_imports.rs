//! A missing import is one error, not one per use.
//!
//! `import std::nothere::{Thing};` and four uses of `Thing::..` were six
//! errors: the import, a "cannot resolve" per use, and a closure "never
//! pinned down" because the call in it named nothing. Only the first has
//! anything to fix. These pin that the import's error is the only one, for
//! a module that is not there and for a name a module does not have, and that
//! a name nobody imported is still reported wherever it is used.

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

/// Every diagnostic for `program`, compiled with `std`.
fn said(program: &str) -> Vec<String> {
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
    khora_types::diagnostics(&db, mine).iter().map(|e| e.message.clone()).collect()
}

/// Uses of `Thing` in every position a use can take: a path call, a value
/// passed on, an interpolation, a closure's body, a parameter's type, an
/// annotation, a pattern and a bare value -- and what depends on one only: a
/// method called on it, a field read from it, and an arithmetic use of that.
fn uses_of(import: &str) -> String {
    format!(
        "module main;\n\
         import std::core::{{print, Fiber, List}};\n\
         {import}\n\
         fn takes(t: Thing) -> Int {{ 0 }}\n\
         fn matches(t: Thing) -> Int {{\n  match t {{\n    Thing::A => 1,\n    _ => 0,\n  }}\n}}\n\
         pub fn main() -> Int {{\n\
           let t = Thing::new();\n\
           let u = Thing::make(t);\n\
           print(\"${{Thing::show(u)}}\");\n\
           let f = Fiber::spawn(fn () => Thing::go(t));\n\
           Fiber::wait(f);\n\
           let v = Thing;\n\
           let w: List<Thing> = [t];\n\
           let n = t.size() + u.count;\n\
           0\n\
         }}\n"
    )
}

/// **The reported program: a module that is not there.**
#[test]
fn a_missing_module_is_one_error_however_often_its_names_are_used() {
    let found = said(&uses_of("import std::nothere::{Thing};"));
    assert_eq!(found, vec!["cannot find module `std.nothere`".to_string()], "{found:#?}");
}

/// A module that is there and does not have the name.
#[test]
fn a_name_the_module_does_not_have_is_one_error_however_often_it_is_used() {
    let found = said(&uses_of("import std::core::{Nope as Thing};"));
    assert_eq!(found, vec!["`std.core` does not declare `Nope`".to_string()], "{found:#?}");
    let found = said(&uses_of("import std::core::{Thing};"));
    assert_eq!(found, vec!["`std.core` does not declare `Thing`".to_string()], "{found:#?}");
}

/// **A name nobody imported is not poisoned**, beside one that is: the
/// missing import silences its own names and no others.
#[test]
fn a_name_never_imported_is_still_reported() {
    let found = said(
        "module main;\n\
         import std::nothere::{Thing};\n\
         pub fn main() -> Int {\n  let t = Thing::new();\n  let o = Other::new();\n  let w = wibble;\n  0\n}\n",
    );
    assert_eq!(
        found,
        vec![
            "cannot find module `std.nothere`".to_string(),
            "cannot resolve `Other::new` in this scope".to_string(),
            "cannot find `wibble` in this scope".to_string(),
        ],
        "{found:#?}"
    );
}

/// A name a failed import asked for and the file then declares itself is
/// the file's own, and works as one: its type is what decides the error,
/// which an unknown would agree with.
#[test]
fn a_declaration_of_the_same_name_is_not_poisoned() {
    let found = said(
        "module main;\n\
         import std::nothere::{helper};\n\
         fn helper() -> Int { 1 }\n\
         pub fn main() -> Int { let s: String = helper(); 0 }\n",
    );
    assert!(found.iter().any(|e| e.contains("cannot find module")), "{found:#?}");
    assert!(
        found.iter().any(|e| e.contains("expected `String`, found `Int`")),
        "the local `helper` is checked, so its misuse is reported: {found:#?}"
    );
}

/// **A receiver inside a closure** is lowered by a path of its own, which
/// took any name it could not find for a capability the closure needs, and
/// the checker then said it could not find it.
#[test]
fn a_poisoned_receiver_inside_a_closure_says_nothing_more() {
    let found = said(
        "module main;\n\
         import std::nothere::{thing};\n\
         pub fn main() -> Int { let g = fn () => thing.go(); 0 }\n",
    );
    assert_eq!(found, vec!["cannot find module `std.nothere`".to_string()], "{found:#?}");
}
