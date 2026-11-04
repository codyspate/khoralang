//! `assert` and `assert_that` outside a `test` block: refused when they are
//! the builtin, and only then.
//!
//! The builtin is the bodiless declaration code generation writes at each call
//! site. A function a program writes with a body under the same name is an
//! ordinary function, and code generation compiles a call to it as one -- so
//! refusing it was `khora check` failing a program `khora build` would build.

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

/// Every diagnostic for each of `program`'s files, compiled with `std`.
fn said(program: &[(&str, &str)]) -> Vec<String> {
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
    let mine: Vec<SourceFile> = program
        .iter()
        .map(|(path, text)| SourceFile::new(&db, PathBuf::from(path), text.to_string()))
        .collect();
    files.extend(mine.iter().copied());
    SourceRoot::new(&db, files);
    mine.iter()
        .flat_map(|file| khora_types::diagnostics(&db, *file).iter().map(|e| e.message.clone()))
        .collect()
}

const REFUSED: &str = "is only allowed inside a `test` block";

const OWN_ASSERT: &str = "module main;\n\
     type Oops = | Bad;\n\
     fn assert(b: Bool) -> Int raises Oops {\n  if b { 1 } else { raise Oops::Bad }\n}\n\
     pub fn main() -> Int {\n  assert(true)! catch { Oops::Bad => 0 }\n}\n";

/// **A program's own `assert` was refused as the test builtin.** The rule
/// matched the callee's text.
#[test]
fn a_programs_own_assert_may_be_called_outside_a_test() {
    let found = said(&[("main.kh", OWN_ASSERT)]);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn a_programs_own_assert_that_may_be_called_outside_a_test() {
    let found = said(&[(
        "main.kh",
        "module main;\n\
         fn assert_that(b: Bool, why: String) -> String {\n  if b { \"held\" } else { why }\n}\n\
         pub fn main() -> Int {\n  let s = assert_that(false, \"no\");\n  0\n}\n",
    )]);
    assert!(found.is_empty(), "{found:#?}");
}

/// Imported from another module of the program, and imported under another
/// name: still the program's function, wherever it is called.
#[test]
fn an_imported_assert_may_be_called_outside_a_test() {
    let helpers = "module helpers;\n\
         pub type Oops = | Bad;\n\
         pub fn assert(b: Bool) -> Int raises Oops {\n  if b { 1 } else { raise Oops::Bad }\n}\n";
    let found = said(&[
        ("helpers.kh", helpers),
        (
            "main.kh",
            "module main;\nimport helpers::{Oops, assert};\n\
             pub fn main() -> Int {\n  assert(true)! catch { Oops::Bad => 0 }\n}\n",
        ),
    ]);
    assert!(found.is_empty(), "{found:#?}");
}

/// The builtin itself, from `std::core`, is still refused outside a test --
/// in `main`, in another function and in a method -- and both of them are.
#[test]
fn the_builtin_assert_is_still_refused_outside_a_test() {
    for body in [
        "pub fn main() -> Int {\n  assert(1 == 1);\n  0\n}\n",
        "fn helper() -> Int {\n  assert(1 == 1);\n  0\n}\n",
        "type P = { x: Int };\nimpl P {\n  fn check(self) -> Int {\n    assert(self.x == 1);\n    0\n  }\n}\n",
    ] {
        let program = format!("module main;\nimport std::core::{{assert}};\n{body}");
        let found = said(&[("main.kh", &program)]);
        assert!(found.iter().any(|e| e.contains(&format!("`assert` {REFUSED}"))), "{body}: {found:#?}");
    }
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{assert_that};\n\
         pub fn main() -> Int {\n  assert_that(1 == 1, \"one\");\n  0\n}\n",
    )]);
    assert!(found.iter().any(|e| e.contains(&format!("`assert_that` {REFUSED}"))), "{found:#?}");
}

/// Inside a test both are what they are for.
#[test]
fn the_builtin_assert_is_quiet_inside_a_test() {
    let found = said(&[(
        "main.kh",
        "module main;\nimport std::core::{assert, assert_that};\n\
         test \"holds\" {\n  assert(1 == 1);\n  assert_that(2 == 2, \"two\");\n}\n",
    )]);
    assert!(found.is_empty(), "{found:#?}");
}
