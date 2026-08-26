#![cfg(feature = "llvm")]

//! What a backtick literal is *worth*, end to end.
//!
//! `docs/design/multiline-strings.md`, D17. The lexer's tests say it is one
//! token; these say what value it takes, which is where the two decisions
//! live: the source's indentation comes off, and `${..}` interpolates the same
//! way it does in a quoted literal.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn run(name: &str, source: &str) -> String {
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
    let ran = Command::new(&exe).output().expect("the program should run");
    String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n")
}

const PRELUDE: &str = "module t;\nfn print(value: String);\n";

/// **The whole point.** A literal written inside a function is indented to
/// match the code around it, and those spaces are not part of the string.
#[test]
fn the_sources_indentation_comes_off() {
    let out = run(
        "backtick_dedent",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  print(`
    create table entries (
      id serial primary key
    )
  `);
  0
}}
"
        ),
    );
    assert_eq!(out, "create table entries (\n  id serial primary key\n)\n");
}

/// Relative indentation survives, because the *common* prefix is what comes
/// off rather than all of it.
#[test]
fn nesting_inside_the_string_is_kept() {
    let out = run(
        "backtick_nesting",
        &format!("{PRELUDE}\nfn main() -> Int {{\n  print(`\n    a\n      b\n    c\n  `);\n  0\n}}\n"),
    );
    assert_eq!(out, "a\n  b\nc\n");
}

/// A blank line has no content to indent, so counting its zero would make
/// every literal with a paragraph break in it strip nothing at all.
#[test]
fn a_blank_line_does_not_defeat_the_measurement() {
    let out = run(
        "backtick_blank",
        &format!("{PRELUDE}\nfn main() -> Int {{\n  print(`\n    a\n\n    b\n  `);\n  0\n}}\n"),
    );
    assert_eq!(out, "a\n\nb\n");
}

/// The same `${..}` a quoted literal has. One string with two escaping rules
/// would be worse than one rule.
#[test]
fn a_backtick_literal_interpolates() {
    let out = run(
        "backtick_interpolation",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let who = \"world\";
  print(`hello ${{who}}`);
  0
}}
"
        ),
    );
    assert_eq!(out, "hello world\n");
}

/// And interpolation and the dedent compose, which is the case where the
/// offsets could have gone wrong: the holes are positioned against the
/// *undedented* source.
#[test]
fn interpolation_and_the_dedent_compose() {
    let out = run(
        "backtick_both",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let name = \"entries\";
  print(`
    select * from ${{name}}
    where id = 1
  `);
  0
}}
"
        ),
    );
    assert_eq!(out, "select * from entries\nwhere id = 1\n");
}

/// A template meant for some other tool still fits, which is most of why
/// escaping a dollar exists.
#[test]
fn an_escaped_dollar_is_not_a_hole() {
    let out = run(
        "backtick_escaped_dollar",
        &format!("{PRELUDE}\nfn main() -> Int {{ print(`cost: \\${{USD}}`); 0 }}\n"),
    );
    assert_eq!(out, "cost: ${USD}\n");
}

#[test]
fn an_escaped_backtick_is_a_backtick() {
    let out = run(
        "backtick_escaped_tick",
        &format!("{PRELUDE}\nfn main() -> Int {{ print(`a \\` b`); 0 }}\n"),
    );
    assert_eq!(out, "a ` b\n");
}

/// A literal that opens on the same line as its content has nothing to strip,
/// because that line's indentation is zero and it is the minimum. That is the
/// documented behaviour rather than an accident: put the delimiter on its own
/// line to get the stripping.
#[test]
fn a_literal_that_starts_on_the_same_line_keeps_its_shape() {
    let out = run(
        "backtick_inline",
        &format!("{PRELUDE}\nfn main() -> Int {{ print(`one\ntwo`); 0 }}\n"),
    );
    assert_eq!(out, "one\ntwo\n");
}

/// The ordinary quoted literal is untouched.
#[test]
fn a_quoted_literal_still_means_what_it_did() {
    let out = run(
        "backtick_regression",
        &format!(
            "{PRELUDE}\nfn main() -> Int {{ let x = \"a\"; print(\"${{x}}\\tb\\n\"); 0 }}\n"
        ),
    );
    assert_eq!(out, "a\tb\n\n");
}
