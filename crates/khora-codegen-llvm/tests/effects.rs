#![cfg(feature = "llvm")]

//! Capabilities, end to end.
//!
//! `docs/design/effect-runtime.md` §2 decides what these compile to: an effect
//! is a record of closures, a `with` clause is extra parameters, and installing
//! a handler is a block of `let`s. Nothing here needs a handler stack, a
//! dynamic lookup, or a stack map — which is the whole point of the decision,
//! and what these tests are really pinning.

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
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

const LEDGER: &str = "module t;
fn print(value: Int);
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export effect Ledger { balance: (Int) -> Int, }

export fn report(id: Int) -> Int with { ledger: Ledger } { ledger.balance(id) }
";

/// The whole shape: a function that requires a capability and never says how it
/// is provided, and a caller that provides one.
#[test]
fn a_capability_is_supplied_by_the_caller() {
    let ran = run(
        "capability",
        &format!(
            "{LEDGER}
fn main() -> Int {{
  let live = handler for Ledger {{ balance: fn id => id * 10 }};
  with {{ ledger: live }} {{ print(report(4)); }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// A function that requires a capability passes it on without naming it at the
/// call — that is the point of evidence being a parameter rather than a lookup.
#[test]
fn a_capability_passes_through_an_intermediate() {
    let ran = run(
        "capability_through",
        &format!(
            "{LEDGER}
export fn twice(id: Int) -> Int with {{ ledger: Ledger }} {{ report(id) + report(id) }}

fn main() -> Int {{
  let live = handler for Ledger {{ balance: fn id => id * 10 }};
  with {{ ledger: live }} {{ print(twice(1)); }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "20\n");
    assert_eq!(ran.code, Some(0));
}

/// Two regions, two handlers, and the same function serving both. The bug this
/// pins: the two blocks bind the same label, and resolving it by name after
/// lowering picks whichever was declared last for *both* call sites.
#[test]
fn two_regions_get_the_handler_each_installed() {
    let ran = run(
        "capability_regions",
        &format!(
            "{LEDGER}
fn main() -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => id * 10 }} }} {{
    print(report(4));
  }}
  with {{ ledger: handler for Ledger {{ balance: fn id => 0 }} }} {{
    print(report(4));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n0\n", "each region uses the handler it installed");
    assert_eq!(ran.code, Some(0));
}

/// A handler is a record of closures, so it captures like any closure does.
#[test]
fn a_handler_closes_over_its_environment() {
    let ran = run(
        "capability_capture",
        &format!(
            "{LEDGER}
fn main() -> Int {{
  let bonus = 100;
  with {{ ledger: handler for Ledger {{ balance: fn id => id + bonus }} }} {{
    print(report(5));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "105\n");
    assert_eq!(ran.code, Some(0));
}

/// An inner region shadows an outer one for the calls inside it, and the outer
/// one is back afterwards — installation is lexical, which is what makes it a
/// block of `let`s.
#[test]
fn an_inner_region_shadows_an_outer_one() {
    let ran = run(
        "capability_nested",
        &format!(
            "{LEDGER}
fn main() -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => 1 }} }} {{
    print(report(0));
    with {{ ledger: handler for Ledger {{ balance: fn id => 2 }} }} {{
      print(report(0));
    }}
    print(report(0));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// Everything a region owns is released when it ends: the handler record, the
/// closures in it, and whatever they captured.
#[test]
fn a_region_releases_its_handler() {
    let ran = run(
        "capability_leaks",
        &format!(
            "{LEDGER}
export fn twice(id: Int) -> Int with {{ ledger: Ledger }} {{ report(id) + report(id) }}

fn run_it() -> Int {{
  let bonus = 100;
  with {{ ledger: handler for Ledger {{ balance: fn id => id + bonus }} }} {{
    twice(1)
  }}
}}

fn main() -> Int {{
  khora_print_int(run_it());
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "202\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}
