#![cfg(feature = "llvm")]

//! The flow operator, end to end.
//!
//! `docs/design/flow-operator.md`. `||> a |> b` is sugar for
//! `fn x => x |> a |> b`, so the claim these have to establish is not that it
//! works but that it is **the same program**: the same answer, the same
//! inferred effect row, the same failure row, the same closure behaviour.
//!
//! Several tests therefore run both spellings in one binary and compare their
//! output rather than asserting on a constant. A test that only checked the
//! flow's answer would pass just as happily if the desugaring quietly meant
//! something slightly different.

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

const PRELUDE: &str = "module t;
fn print(value: Int);

fn inc(n: Int) -> Int { n + 1 }
fn double(n: Int) -> Int { n * 2 }
fn add(n: Int, m: Int) -> Int { n + m }
fn apply(f: (Int) -> Int, v: Int) -> Int { f(v) }
";

#[test]
fn one_stage_is_the_function_applied_once() {
    let ran = run(
        "flow_one",
        &format!("{PRELUDE}\nfn main() -> Int {{ print(apply(||> inc, 1)); 0 }}\n"),
    );
    assert_eq!(ran.stdout, "2\n");
    assert_eq!(ran.code, Some(0));
}

/// The design test, and the reason the others compare rather than assert: the
/// two spellings must be the same program.
#[test]
fn a_flow_and_its_explicit_lambda_agree() {
    let ran = run(
        "flow_same",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let flowed = apply(||> inc |> double |> add(3), 1);
  let written = apply(fn x => x |> inc |> double |> add(3), 1);
  print(flowed);
  print(written);
  if flowed == written {{ print(1) }} else {{ print(0) }};
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "7\n7\n1\n", "double(inc(1)) is 4, add(4, 3) is 7");
    assert_eq!(ran.code, Some(0));
}

/// A stage with arguments of its own: the piped value goes in front, which is
/// what `|>` already does and is not decided again here.
#[test]
fn a_stage_may_carry_its_own_arguments() {
    let ran = run(
        "flow_args",
        &format!("{PRELUDE}\nfn main() -> Int {{ print(apply(||> add(10) |> double, 1)); 0 }}\n"),
    );
    assert_eq!(ran.stdout, "22\n", "add(1, 10) is 11, doubled is 22");
    assert_eq!(ran.code, Some(0));
}

/// `_` in a stage still means what it meant. The flow operator did not add a
/// placeholder system; it reuses the one `|>` has.
#[test]
fn a_placeholder_stage_still_chooses_the_position() {
    let ran = run(
        "flow_placeholder",
        &format!("{PRELUDE}\nfn main() -> Int {{ print(apply(||> add(10, _), 1)); 0 }}\n"),
    );
    assert_eq!(ran.stdout, "11\n", "the piped value went second");
    assert_eq!(ran.code, Some(0));
}

/// A flow captures what it reads, exactly as the lambda it becomes would.
#[test]
fn a_flow_captures_from_around_it() {
    let ran = run(
        "flow_capture",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let bump = 100;
  print(apply(||> add(bump) |> double, 1));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "202\n");
    assert_eq!(ran.code, Some(0));
}

/// Nested inside another flow's stage, which is the case where an invented
/// parameter name would collide if it were an ordinary identifier.
#[test]
fn a_flow_inside_a_flow_keeps_its_own_argument() {
    let ran = run(
        "flow_nested",
        &format!(
            "{PRELUDE}
fn twice(f: (Int) -> Int, v: Int) -> Int {{ f(f(v)) }}
fn main() -> Int {{
  print(apply(||> twice(||> inc |> double, _) |> inc, 1));
  0
}}
"
        ),
    );
    // inner: double(inc(x)). twice over 1 -> double(inc(1)) = 4,
    // then double(inc(4)) = 10. Then the outer stage: inc(10) = 11.
    assert_eq!(ran.stdout, "11\n");
    assert_eq!(ran.code, Some(0));
}

/// The failure row is the lambda's, and `!` is not special-cased inside a
/// flow: it marks the stage's call and the raise leaves the flow's body.
#[test]
fn a_fallible_stage_raises_out_of_the_flow() {
    let ran = run(
        "flow_raises",
        &format!(
            "{PRELUDE}
pub type Oops = | Bad;
fn checked(n: Int) -> Int raises Oops {{ if n > 5 {{ raise Oops::Bad }} else {{ n }} }}
fn attempt<'e>(f: (Int) -> Int raises 'e, v: Int) -> Int raises 'e {{ f(v)! }}

fn main() -> Int {{
  print(attempt(||> inc |> checked! |> double, 1)! catch {{ Oops::Bad => 0 - 1 }});
  print(attempt(||> inc |> checked! |> double, 9)! catch {{ Oops::Bad => 0 - 1 }});
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n-1\n", "inc(1) passes and doubles; inc(9) raises");
    assert_eq!(ran.code, Some(0));
}

/// And the row it infers is the explicit lambda's row, which is the half of
/// "same program" that a passing answer alone would not show.
#[test]
fn the_failure_row_matches_the_explicit_lambda() {
    let ran = run(
        "flow_row_same",
        &format!(
            "{PRELUDE}
pub type Oops = | Bad;
fn checked(n: Int) -> Int raises Oops {{ if n > 5 {{ raise Oops::Bad }} else {{ n }} }}
fn attempt<'e>(f: (Int) -> Int raises 'e, v: Int) -> Int raises 'e {{ f(v)! }}

fn main() -> Int {{
  let a = attempt(||> checked! |> double, 9)! catch {{ Oops::Bad => 0 - 1 }};
  let b = attempt(fn x => x |> checked! |> double, 9)! catch {{ Oops::Bad => 0 - 1 }};
  print(a);
  print(b);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-1\n-1\n");
    assert_eq!(ran.code, Some(0));
}

/// A named function still needs nothing, and gets the same answer. This is the
/// case the documentation tells people to prefer.
#[test]
fn a_named_function_needs_no_flow() {
    let ran = run(
        "flow_named",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  print(apply(inc, 1));
  print(apply(||> inc, 1));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "2\n2\n");
    assert_eq!(ran.code, Some(0));
}

/// **`break` carries a value.**
///
/// The checker has always been able to type this: a `loop` gets a fresh
/// variable, every `break` value unifies with it, and a loop nobody breaks out
/// of with something settles on `()`. Only the lowering had not caught up, and
/// it refused with "a `loop`'s type is not inferred in phase 2, so there is
/// nothing for the value to flow into" -- which had stopped being true.
///
/// So `khora check` accepted a program `khora build` refused, for a form with
/// a worked example in `reference/control-flow.md`. That is the worst thing a
/// toolchain can do, because `check` is the fast loop and the language server
/// shares it.
#[test]
fn a_loop_can_break_with_a_value() {
    let out = run(
        "break_value",
        "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);

fn main() -> Int {
  let mut n = 0;
  let found = loop {
    n = n + 1;
    if n > 3 { break n; }
  };
  print(found);

  // Several breaks, which is what the slot is for: more than one block
  // writes it and one reads it.
  let mut k = 0;
  let pick = loop {
    k = k + 1;
    if k == 1 { break 10; };
    if k == 2 { break 20; };
  };
  print(pick);

  // And a `loop` nobody breaks with a value is still `()`, so the block
  // after it is reached with nothing to load.
  let mut m = 0;
  loop { m = m + 1; if m > 2 { break; } };
  print(m);
  0
}
",
    );

    assert_eq!(out.stdout, "4\n10\n3\n");
    assert_eq!(out.code, Some(0));
}
