#![cfg(feature = "llvm")]

//! String interpolation, end to end.
//!
//! `"a ${e} b"` is `"a " + e + " b"`. That is the whole of the feature: `+` on
//! strings already exists, already requires both sides to be `String`, and
//! already says *"string concatenation: expected `String`, found `Int`"* — which
//! is the right thing to say about `"${count}"`, and it points at the
//! expression rather than at the string.
//!
//! Before this, `"hello ${name}"` compiled and printed `hello ${name}`. That is
//! what every language without interpolation does and it is still a trap for
//! anyone arriving from JavaScript, Kotlin or Swift, because it is wrong
//! silently rather than loudly.
//!
//! **Nothing was added to the grammar.** A string literal is still one token;
//! the holes are found in its text during HIR lowering and each is parsed on its
//! own. The lexer learned exactly one thing — that a `"` inside a hole opens a
//! nested string — because without it `"${f("x")}"` ends at the third quote and
//! the rest of the line lexes as code.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        code: output.status.code(),
    }
}

fn refused(name: &str, source: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("rejected.exe");
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    let parse = khora_db::parse(&db, file);
    if !parse.errors().is_empty() {
        return parse.errors().iter().map(|e| e.message.clone()).collect();
    }
    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => panic!("`{name}` should have been refused:\n\n{source}"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
}

const PRELUDE: &str = "module t;
fn print(value: String);

fn twice(s: String) -> String { s + s }
";

/// The shapes worth having: a name, several holes, an expression with a call,
/// an escaped dollar, and braces inside a hole.
#[test]
fn a_string_interpolates_its_holes() {
    let ran = run(
        "interp_basic",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let name = \"world\";
  print(\"hello ${{name}}!\");
  print(\"${{name}} and ${{name}}\");
  print(\"called: ${{twice(name)}}\");
  print(\"literal \\${{name}} stays\");
  print(\"branch: ${{if true {{ \"yes\" }} else {{ \"no\" }}}}\");
  print(\"plain, no holes\");
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout,
        "hello world!\nworld and world\ncalled: worldworld\nliteral ${name} stays\n\
         branch: yes\nplain, no holes\n"
    );
    assert_eq!(ran.code, Some(0));
}

/// **A `\"` inside a hole opens a nested string.** This is the one thing the
/// lexer had to learn: a string is still a single token, so without it the
/// literal ends at the quote inside `${..}` and everything after it lexes as
/// code — which arrives as `expected )` pointing at nothing to do with strings.
#[test]
fn a_hole_may_contain_a_string() {
    let ran = run(
        "interp_nested_string",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  print(\"a ${{twice(\"b\")}} c\");
  print(\"${{\"just a string\"}}\");
  // A brace inside a nested string must not close the hole.
  print(\"${{twice(\"}}\")}}\");
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "a bb c\njust a string\n}}\n");
    assert_eq!(ran.code, Some(0));
}

/// The type error is `+`'s, which is the right one — and it points at the
/// expression inside the hole rather than at the string, which is what
/// `Ctx::range_shift` is for: the hole is parsed as a little source file of its
/// own, so its ranges start again at zero and have to be moved back.
/// **A hole holds any value that can be shown**, which it did not.
///
/// `"there are ${count} of them"` where `count: Int` was `string
/// concatenation: expected String, found Int`, so every number in a message
/// needed an explicit `Int::to_string`. Three examples in the guide quietly
/// left that out, and the first person to print a table called it the single
/// biggest tax in the language.
///
/// This fixture declares its own tiny prelude and no `Show` at all, so a hole
/// holding an `Int` here has nothing to call — which is the *other* half of
/// the rule and the one this test pins: the requirement is real, and it is
/// reported at the hole rather than as a confusing complaint about `+`.
#[test]
fn a_hole_needs_a_show_for_what_it_holds() {
    let found = refused(
        "interp_wrong_type",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let count = 42;
  print(\"there are ${{count}} of them\");
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("`Int` has no `Show`")),
        "the error should name the type and the trait: {found:?}"
    );
    // And say what to do about it, since "no Show" is not an instruction.
    assert!(
        found.iter().any(|e| e.contains("derive(Show)")),
        "and how to get one: {found:?}"
    );
}

/// **A hole that already holds a `String` needs nothing at all** — no call,
/// and no `Show` in the program.
///
/// `impl Show for String` is the identity, so requiring it would cost a call
/// that gives back its argument and, worse, would mean a message made of text
/// did not work in a file that has never heard of the trait. Most files have
/// not. This one has no `Show` anywhere in it.
#[test]
fn a_string_hole_needs_no_show() {
    let ran = run(
        "interp_string_hole",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let who = \"world\";
  print(\"hello ${{who}}\");
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "hello world\n");
    assert_eq!(ran.code, Some(0));
}

/// `${}` holds no expression, which is a mistake rather than an empty string —
/// and it is said at the hole rather than becoming `""` quietly.
#[test]
fn an_empty_hole_is_refused() {
    let found = refused(
        "interp_empty_hole",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  print(\"empty:${{}}done\");
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("does not contain an expression")),
        "an empty hole should be refused: {found:?}"
    );
}

/// A name that is not in scope is reported where it is written, not at the top
/// of the file — the other half of what the range shift buys.
#[test]
fn an_unknown_name_in_a_hole_is_located() {
    let db = KhoraDatabase::new();
    let source = format!(
        "{PRELUDE}
fn main() -> Int {{
  print(\"and ${{missing}} too\");
  0
}}
"
    );
    let file = SourceFile::new(&db, "main.kh".into(), source.clone());
    SourceRoot::new(&db, vec![file]);

    let errors = khora_types::diagnostics(&db, file);
    let found = errors
        .iter()
        .find(|e| e.message.contains("missing"))
        .expect("the unknown name should be reported");

    let at: usize = u32::from(found.range.start()) as usize;
    let end: usize = u32::from(found.range.end()) as usize;
    assert_eq!(
        &source[at..end],
        "missing",
        "the range should cover the name inside the hole, not the whole literal"
    );
}
