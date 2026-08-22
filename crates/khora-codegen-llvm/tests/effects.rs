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

// --- failures --------------------------------------------------------------

const FALLIBLE: &str = "module t;
fn print(value: Int);
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export type DbError = | Timeout | Refused;
export type Node = | Of(value: Int);

fn halve(n: Int) -> Int raises DbError {
  if n % 2 == 0 { n / 2 } else { raise DbError::Refused }
}
";

/// The ordinary path: a raise that never happens costs a branch.
#[test]
fn a_call_that_does_not_raise_returns_its_value() {
    let ran = run(
        "raises_ok",
        &format!(
            "{FALLIBLE}
fn twice(n: Int) -> Int raises DbError {{ halve(n)! + halve(n)! }}
fn main() -> Int raises DbError {{ print(twice(8)!); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "8\n");
    assert_eq!(ran.code, Some(0));
}

/// A raise leaves through every frame between it and the entry point, which
/// has nowhere to hand it — so an uncaught raise is a failing exit.
#[test]
fn a_raise_propagates_to_the_entry_point() {
    let ran = run(
        "raises_err",
        &format!(
            "{FALLIBLE}
fn twice(n: Int) -> Int raises DbError {{ halve(n)! + halve(n)! }}
fn main() -> Int raises DbError {{ print(twice(7)!); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "", "nothing after the raise runs");
    assert_eq!(ran.code, Some(1));
}

/// Both paths from one program, so the branch is really a branch.
#[test]
fn the_tag_chooses_between_the_two_paths() {
    let ran = run(
        "raises_both",
        &format!(
            "{FALLIBLE}
fn attempt(n: Int) -> Int raises DbError {{
  if n < 0 {{ raise DbError::Timeout }}
  halve(n)!
}}
fn main() -> Int raises DbError {{
  print(attempt(10)!);
  print(attempt(20)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n10\n");
    assert_eq!(ran.code, Some(0));
}

/// The whole of unwinding: the raising frame releases what it owns on the way
/// out. No tables, no personality routine — a raise is a return with a tag.
#[test]
fn a_raising_frame_releases_what_it_owns() {
    let ran = run(
        "raises_leaks",
        &format!(
            "{FALLIBLE}
/// Holds two boxed values, then raises past both.
fn holding(n: Int) -> Int raises DbError {{
  let node = Node::Of(n);
  let text = \"held\";
  if n == 0 {{ raise DbError::Refused }}
  n
}}

fn main() -> Int raises DbError {{
  khora_print_int(holding(3)!);
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n0\n", "the ok path leaves nothing behind either");
    assert_eq!(ran.code, Some(0));
}

// --- catch -----------------------------------------------------------------

/// A `catch` that names the only error type takes the row to empty, so the
/// function needs no `raises` clause of its own.
#[test]
fn a_catch_handles_the_whole_row() {
    let ran = run(
        "catch_all",
        &format!(
            "{FALLIBLE}
fn safe(n: Int) -> Int {{
  halve(n)! catch {{
    DbError::Timeout => 0 - 1,
    DbError::Refused => 0 - 2,
  }}
}}
fn main() -> Int {{ print(safe(8)); print(safe(7)); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "4
-2
");
    assert_eq!(ran.code, Some(0));
}

const TWO_ERRORS: &str = "module t;
fn print(value: Int);
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export type DbError = | Timeout | Refused;
export type ModelError = | RateLimited(ms: Int) | TooLong;

fn fetch(n: Int) -> Int raises DbError + ModelError {
  if n == 1 { raise DbError::Timeout }
  if n == 2 { raise ModelError::RateLimited(500) }
  if n == 3 { raise ModelError::TooLong }
  n
}
";

/// Half a row. `ModelError` is handled here and gone from the signature;
/// `DbError` is not named, so it leaves through the same `!` it always did.
#[test]
fn a_catch_handles_part_of_the_row() {
    let ran = run(
        "catch_partial",
        &format!(
            "{TWO_ERRORS}
fn attempt(n: Int) -> Int raises DbError {{
  fetch(n)! catch {{
    ModelError::RateLimited(ms) => ms,
    ModelError::TooLong => 0 - 1,
  }}
}}
fn main() -> Int raises DbError {{
  print(attempt(2)!);
  print(attempt(3)!);
  print(attempt(9)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "500
-1
9
");
    assert_eq!(ran.code, Some(0));
}

/// The unhandled half of the same row still leaves the function, and still
/// reaches the entry point as a failing exit.
#[test]
fn an_unnamed_error_type_passes_through_a_catch() {
    let ran = run(
        "catch_passthrough",
        &format!(
            "{TWO_ERRORS}
fn attempt(n: Int) -> Int raises DbError {{
  fetch(n)! catch {{
    ModelError::RateLimited(ms) => ms,
    ModelError::TooLong => 0 - 1,
  }}
}}
fn main() -> Int raises DbError {{ print(attempt(1)!); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "", "the `DbError` was not diverted into the arms");
    assert_eq!(ran.code, Some(1));
}

/// An arm reads the error's payload, so the error object has to survive until
/// the arm is done with it — and be released afterwards, since the raising
/// frame moved it here and nobody else will.
#[test]
fn a_caught_error_is_released_after_its_arm() {
    let ran = run(
        "catch_leaks",
        &format!(
            "{TWO_ERRORS}
fn attempt(n: Int) -> Int raises DbError {{
  fetch(n)! catch {{
    ModelError::RateLimited(ms) => ms,
    ModelError::TooLong => 0 - 1,
  }}
}}
fn main() -> Int raises DbError {{
  khora_print_int(attempt(2)!);
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "500
0
", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// Two `catch`es, one inside the other. What the inner one does not name goes
/// to the outer one rather than out of the function.
#[test]
fn an_outer_catch_takes_what_the_inner_one_left() {
    let ran = run(
        "catch_nested",
        &format!(
            "{TWO_ERRORS}
fn attempt(n: Int) -> Int {{
  (fetch(n)! catch {{
    ModelError::RateLimited(ms) => ms,
    ModelError::TooLong => 0 - 1,
  }}) catch {{
    DbError::Timeout => 0 - 100,
    DbError::Refused => 0 - 200,
  }}
}}
fn main() -> Int {{
  print(attempt(1));
  print(attempt(2));
  print(attempt(9));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-100
500
9
");
    assert_eq!(ran.code, Some(0));
}

/// A `raise` written directly inside a `catch`'s operand is caught by it, the
/// same as one that arrived through a `!`. There is no third mechanism.
#[test]
fn a_catch_takes_a_raise_from_its_own_operand() {
    let ran = run(
        "catch_direct",
        &format!(
            "{TWO_ERRORS}
fn attempt(n: Int) -> Int {{
  (if n < 0 {{ raise DbError::Refused }} else {{ n }}) catch {{
    DbError::Timeout => 0 - 1,
    DbError::Refused => 0 - 2,
  }}
}}
fn main() -> Int {{ print(attempt(5)); print(attempt(0 - 5)); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "5
-2
");
    assert_eq!(ran.code, Some(0));
}

// --- composition -----------------------------------------------------------

const LAYERED: &str = "module t;
fn print(value: Int);
fn khora_print_int(value: Int);
fn khora_live_count() -> Int;

export effect Config { rate: () -> Int, }
export effect Ledger { balance: (Int) -> Int, }
export effect Audit { note: (Int) -> Int, }

export fn report(id: Int) -> Int with { ledger: Ledger } { ledger.balance(id) }
";

/// A service built on another service. `live_ledger` needs `Config` to be
/// built, and what it returns needs nothing — which is the whole of
/// `Layer<RIn, ROut>` without a wrapper type: the handler is a value, so a
/// function that makes one is a function.
#[test]
fn a_handler_can_be_built_from_another_capability() {
    let ran = run(
        "layer",
        &format!(
            "{LAYERED}
export fn live_ledger() -> Ledger with {{ config: Config }} {{
  let rate = config.rate();
  handler for Ledger {{ balance: fn id => id * rate }}
}}

fn main() -> Int {{
  with {{ config: handler for Config {{ rate: fn () => 10 }} }} {{
    with {{ ledger: live_ledger() }} {{ print(report(4)); }}
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// Several capabilities installed by one `with`, which is what Effect calls
/// merging layers. Nothing special: a row with two labels.
#[test]
fn one_region_installs_several_capabilities() {
    let ran = run(
        "layer_merge",
        &format!(
            "{LAYERED}
export fn audited(id: Int) -> Int with {{ ledger: Ledger, audit: Audit }} {{
  audit.note(report(id))
}}

fn main() -> Int {{
  with {{
    ledger: handler for Ledger {{ balance: fn id => id * 10 }},
    audit: handler for Audit {{ note: fn n => n + 1 }},
  }} {{
    print(audited(4));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "41\n");
    assert_eq!(ran.code, Some(0));
}

/// A handler built from a capability is still just a value, so the region that
/// built it releases it and everything it captured.
#[test]
fn a_composed_handler_is_released_with_its_region() {
    let ran = run(
        "layer_leaks",
        &format!(
            "{LAYERED}
export fn live_ledger() -> Ledger with {{ config: Config }} {{
  let rate = config.rate();
  handler for Ledger {{ balance: fn id => id * rate }}
}}

fn run_it() -> Int {{
  with {{ config: handler for Config {{ rate: fn () => 10 }} }} {{
    with {{ ledger: live_ledger() }} {{ report(4) }}
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
    assert_eq!(ran.stdout, "40\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// Building a service can fail. The `raises` of the builder is the region's,
/// not the served computation's.
#[test]
fn building_a_handler_can_raise() {
    let ran = run(
        "layer_raises",
        &format!(
            "{LAYERED}
export type ConfigError = | Missing;

export fn live_ledger() -> Ledger with {{ config: Config }} raises ConfigError {{
  let rate = config.rate();
  if rate == 0 {{ raise ConfigError::Missing }}
  handler for Ledger {{ balance: fn id => id * rate }}
}}

fn attempt(rate: Int) -> Int raises ConfigError {{
  with {{ config: handler for Config {{ rate: fn () => rate }} }} {{
    with {{ ledger: live_ledger()! }} {{ report(4) }}
  }}
}}

fn main() -> Int raises ConfigError {{ print(attempt(10)!); print(attempt(0)!); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n", "the second build raises before it can serve");
    assert_eq!(ran.code, Some(1));
}

/// The type system allows an effectful function as a value and the backend
/// does not, so the gap has to be a message rather than a bad call. Pinned so
/// that when the backend catches up, this test is what says so.
#[test]
fn a_function_value_that_needs_capabilities_is_refused_by_the_backend() {
    let source = format!(
        "{LEDGER}
export fn apply_to<'r>(f: (Int) -> Int with 'r, n: Int) -> Int with 'r {{ f(n) }}

fn main() -> Int with {{ ledger: Ledger }} {{ apply_to(report, 4) }}
"
    );
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fnval_effect");
    std::fs::create_dir_all(&dir).expect("a workspace");

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source);
    let root = SourceRoot::new(&db, vec![file]);

    let errors = khora_codegen_llvm::compile(&db, root, &dir.join("program"))
        .expect_err("a function value with a requirement should be refused");
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("cannot build a value out of such a function")),
        "{messages:?}"
    );
}
