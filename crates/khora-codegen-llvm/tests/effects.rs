#![cfg(feature = "llvm")]

//! Capabilities, end to end.
//!
//! `docs/design/effect-runtime.md` §2 decides what these compile to: an effect
//! is a record of closures, a `with` clause is extra parameters, and installing
//! a handler is a block of `let`s. Nothing here needs a handler stack, a
//! dynamic lookup, or a stack map — which is the whole point of the decision,
//! and what these tests are really pinning.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

/// Compiles `source` expecting it to be refused, and hands back the messages.
fn refused(name: &str, source: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(()) => panic!("`{name}` should have been refused:

{source}"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
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

const LEDGER: &str = "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

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
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

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
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

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
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

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

// --- effectful function values ---------------------------------------------

/// A function that needs a capability, passed as a value and called somewhere
/// else. The requirement is part of its type, so it is supplied at the *call*
/// — the value itself carries nothing but a code pointer.
#[test]
fn a_function_value_can_need_a_capability() {
    let ran = run(
        "fnval_capability",
        &format!(
            "{LEDGER}
export fn apply_to<'r>(f: (Int) -> Int with 'r, n: Int) -> Int with 'r {{ f(n) }}

fn main() -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => id * 10 }} }} {{
    print(apply_to(report, 4));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// The same value called under two different handlers. This is what a
/// requirement travelling in the type buys over capturing it at the mention:
/// the function is mounted once and served differently.
#[test]
fn one_function_value_serves_two_handlers() {
    let ran = run(
        "fnval_two_handlers",
        &format!(
            "{LEDGER}
export fn apply_to<'r>(f: (Int) -> Int with 'r, n: Int) -> Int with 'r {{ f(n) }}

export fn twice_over<'r>(f: (Int) -> Int with 'r) -> Int with 'r {{
  apply_to(f, 1) + apply_to(f, 2)
}}

fn main() -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => id * 10 }} }} {{
    print(twice_over(report));
  }}
  with {{ ledger: handler for Ledger {{ balance: fn id => id }} }} {{
    print(twice_over(report));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "30\n3\n");
    assert_eq!(ran.code, Some(0));
}

/// A fallible function as a value. The tagged return is part of the closure's
/// convention too, so `!` on a call through a value is the same branch it is
/// on a direct call.
#[test]
fn a_function_value_can_raise() {
    let ran = run(
        "fnval_raises",
        &format!(
            "{FALLIBLE}
export fn apply_to<'e>(f: (Int) -> Int raises 'e, n: Int) -> Int raises 'e {{ f(n)! }}

fn main() -> Int raises DbError {{
  print(apply_to(halve, 8)!);
  print(apply_to(halve, 7)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n", "the second call raises before it can print");
    assert_eq!(ran.code, Some(1));
}

/// Both at once, which is the shape `Router::post` mounts: a handler that
/// needs services and can fail.
#[test]
fn a_function_value_can_need_a_capability_and_raise() {
    let ran = run(
        "fnval_both",
        "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

export type DbError = | Timeout | Refused;
export effect Ledger { balance: (Int) -> Int, }

export fn report(id: Int) -> Int with { ledger: Ledger } raises DbError {
  if id < 0 { raise DbError::Refused }
  ledger.balance(id)
}

export fn apply_to<'r, 'e>(f: (Int) -> Int with 'r raises 'e, n: Int) -> Int
  with 'r
  raises 'e
{ f(n)! }

fn main() -> Int raises DbError {
  with { ledger: handler for Ledger { balance: fn id => id * 10 } } {
    print(apply_to(report, 4)!);
  }
  0
}
",
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// Nothing leaks along the way: the handler, the closure object and whatever
/// the evidence held are all released.
#[test]
fn an_effectful_function_value_leaves_nothing_behind() {
    let ran = run(
        "fnval_leaks",
        &format!(
            "{LEDGER}
export fn apply_to<'r>(f: (Int) -> Int with 'r, n: Int) -> Int with 'r {{ f(n) }}

fn run_it() -> Int {{
  let bonus = 100;
  with {{ ledger: handler for Ledger {{ balance: fn id => id + bonus }} }} {{
    apply_to(report, 1)
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
    assert_eq!(ran.stdout, "101\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

// --- capabilities inside a closure -----------------------------------------

/// A `with` block lowers to a block of `let`s, so a capability is an ordinary
/// binding and a closure that uses one captures it — the same as any other
/// name it reads. Except that this one is never written down: `report(n)`
/// needs `ledger` without saying so, and the capture scan watches names.
///
/// It compiled and segfaulted before the checker's answer was published to the
/// scan.
#[test]
fn a_closure_captures_a_capability_it_uses_without_naming() {
    let ran = run(
        "closure_capability",
        &format!(
            "{LEDGER}
export fn apply(f: (Int) -> Int, n: Int) -> Int {{ f(n) }}

fn main() -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => id * 10 }} }} {{
    print(apply(fn n => report(n), 4));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// And it is still the handler that was installed where the closure was
/// *written*, not one installed where it is called. A capability is captured
/// lexically because it is a binding, and a binding does not change meaning by
/// being read somewhere else.
#[test]
fn a_captured_capability_is_the_one_in_scope_where_the_closure_was_written() {
    let ran = run(
        "closure_capability_lexical",
        &format!(
            "{LEDGER}
export fn apply(f: (Int) -> Int, n: Int) -> Int {{ f(n) }}

fn make() -> (Int) -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => id * 10 }} }} {{
    fn n => report(n)
  }}
}}

fn main() -> Int {{
  let f = make();
  with {{ ledger: handler for Ledger {{ balance: fn id => 0 }} }} {{
    print(apply(f, 4));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n", "the closure kept the handler it was made with");
    assert_eq!(ran.code, Some(0));
}

/// A closure inside a closure reads the binding out of the outer one's frame,
/// so the outer one has to have captured it too.
#[test]
fn a_nested_closure_captures_through_the_one_around_it() {
    let ran = run(
        "closure_capability_nested",
        &format!(
            "{LEDGER}
export fn apply(f: (Int) -> Int, n: Int) -> Int {{ f(n) }}

fn main() -> Int {{
  with {{ ledger: handler for Ledger {{ balance: fn id => id * 10 }} }} {{
    print(apply(fn n => apply(fn m => report(m), n), 4));
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// Nothing leaks: the closure holds a reference to the handler and gives it
/// back.
#[test]
fn a_closure_holding_a_capability_leaves_nothing_behind() {
    let ran = run(
        "closure_capability_leaks",
        &format!(
            "{LEDGER}
export fn apply(f: (Int) -> Int, n: Int) -> Int {{ f(n) }}

fn run_it() -> Int {{
  let bonus = 100;
  with {{ ledger: handler for Ledger {{ balance: fn id => id + bonus }} }} {{
    apply(fn n => report(n), 1)
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
    assert_eq!(ran.stdout, "101\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

// --- a closure that can fail -----------------------------------------------

/// A closure cannot charge its failures to whoever wrote it — by the time it
/// is called that function has returned — so the error row is part of its
/// type, and the lifted function returns the tagged pair like any other
/// fallible one.
#[test]
fn a_closure_can_raise() {
    let ran = run(
        "closure_raises",
        &format!(
            "{FALLIBLE}
export fn apply<'e>(f: (Int) -> Int raises 'e, n: Int) -> Int raises 'e {{ f(n)! }}

fn main() -> Int raises DbError {{
  print(apply(fn n => halve(n)!, 8)!);
  print(apply(fn n => halve(n)!, 7)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "4\n", "the second call raises before it can print");
    assert_eq!(ran.code, Some(1));
}

/// The row is inferred, not declared, so a closure that cannot fail is not
/// charged for the possibility.
#[test]
fn a_closure_that_cannot_fail_has_an_empty_row() {
    let ran = run(
        "closure_infallible",
        &format!(
            "{FALLIBLE}
export fn apply(f: (Int) -> Int, n: Int) -> Int {{ f(n) }}

fn main() -> Int {{ print(apply(fn n => n * 2, 21)); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.code, Some(0));
}

/// A `raise` written straight into a closure counts the same as one that
/// arrived through a `!`.
#[test]
fn a_closure_can_raise_directly() {
    let ran = run(
        "closure_raise_direct",
        &format!(
            "{FALLIBLE}
export fn apply<'e>(f: (Int) -> Int raises 'e, n: Int) -> Int raises 'e {{ f(n)! }}

fn main() -> Int raises DbError {{
  print(apply(fn n => if n < 0 {{ raise DbError::Timeout }} else {{ n }}, 5)!);
  print(apply(fn n => if n < 0 {{ raise DbError::Timeout }} else {{ n }}, 0 - 5)!);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n");
    assert_eq!(ran.code, Some(1));
}

/// A raising closure releases what its frame owns on the way out, the same as
/// any other frame does.
#[test]
fn a_raising_closure_leaves_nothing_behind() {
    let ran = run(
        "closure_raises_leaks",
        &format!(
            "{FALLIBLE}
export fn apply<'e>(f: (Int) -> Int raises 'e, n: Int) -> Int raises 'e {{ f(n)! }}

fn attempt(n: Int) -> Int raises DbError {{
  apply(fn m => {{ let text = \"held\"; halve(m)! }}, n)!
}}

fn main() -> Int {{
  let value = attempt(7)! catch {{ DbError::Timeout => 0 - 1, DbError::Refused => 0 - 2 }};
  khora_print_int(value);
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "-2\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

// --- a `let` at module level ------------------------------------------------

const CONST: &str = "module t;
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

export effect Ledger { balance: (Int) -> Int, }

export fn report(id: Int) -> Int with { ledger: Ledger } { ledger.balance(id) }
";

/// A `let` at module level is a **constant**: a named expression, lowered
/// wherever it is mentioned.
///
/// It used to be a name with no type and no value at all. Nothing complained —
/// the reference to it typed as `Unknown`, which is compatible with everything
/// — and the first sign was the code generator saying it could not represent a
/// binding whose type nobody had worked out. Errata 40.
#[test]
fn a_module_level_let_is_a_constant() {
    let ran = run(
        "const_let",
        &format!(
            "{CONST}
let mock = handler for Ledger {{ balance: fn id => id * 10 }};

fn main() -> Int {{
  with {{ ledger: mock }} {{ khora_print_int(report(4)); }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// And a `context` may name one, which is what the whole thing was for: a
/// composed set of handlers written once and installed by name.
#[test]
fn a_context_can_name_a_constant() {
    let ran = run(
        "const_context",
        &format!(
            "{CONST}
let mock = handler for Ledger {{ balance: fn id => id * 10 }};

export context Mock {{ ledger: mock, }}

fn main() -> Int {{
  with Mock {{ khora_print_int(report(4)); }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// **Inlined, not shared.** Two mentions are two handlers, which is the right
/// answer for the thing this exists to name — and the reason there is no
/// initialization order to get wrong and nothing to release at exit.
#[test]
fn two_mentions_are_two_values() {
    let ran = run(
        "const_twice",
        &format!(
            "{CONST}
let mock = handler for Ledger {{ balance: fn id => id * 10 }};

fn main() -> Int {{
  with {{ ledger: mock }} {{ khora_print_int(report(1)); }}
  with {{ ledger: mock }} {{ khora_print_int(report(2)); }}
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "10\n20\n0\n", "two handlers, and both released");
    assert_eq!(ran.code, Some(0));
}

/// A constant may be built from an ordinary expression, not only a handler.
#[test]
fn a_constant_can_be_any_expression() {
    let ran = run(
        "const_value",
        &format!(
            "{CONST}
let answer = 6 * 7;
let greeting = \"kh\" + \"ora\";

impl String {{ fn byte_length(self) -> Int; }}

fn main() -> Int {{
  khora_print_int(answer);
  khora_print_int(String::byte_length(greeting));
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n5\n");
    assert_eq!(ran.code, Some(0));
}

/// A constant defined in terms of itself would be a stack overflow while
/// inlining, and a stack overflow is not a diagnostic.
#[test]
fn a_constant_defined_in_terms_of_itself_is_refused() {
    let found = refused(
        "const_cycle",
        &format!(
            "{CONST}
let a = b;
let b = a;

fn main() -> Int {{ khora_print_int(a); 0 }}
"
        ),
    );
    assert!(
        found.iter().any(|m| m.contains("defined in terms of itself")),
        "expected the loop to be named, got {found:?}"
    );
}

/// A mutable global is shared state two fibers could reach, which is the one
/// thing `docs/design/memory.md` §5a does not let cross. A constant has no
/// such question to answer.
#[test]
fn a_mutable_global_is_refused() {
    let found = refused(
        "const_mut",
        &format!(
            "{CONST}
let mut counter = 0;

fn main() -> Int {{ khora_print_int(counter); 0 }}
"
        ),
    );
    assert!(
        found.iter().any(|m| m.contains("mutable global")),
        "expected the reason to be named, got {found:?}"
    );
}

// --- `==` on something with a shape -----------------------------------------

const EQ: &str = "module t;
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

export trait Eq { fn eq(self, other: Self) -> Bool; }

export type Colour = | Red | Green | Blue(shade: Int);

impl Eq for Colour {
  fn eq(self, other: Colour) -> Bool {
    match self {
      Colour::Red => match other {
        Colour::Red => true,
        Colour::Green => false,
        Colour::Blue(s) => false,
      },
      Colour::Green => match other {
        Colour::Red => false,
        Colour::Green => true,
        Colour::Blue(s) => false,
      },
      Colour::Blue(mine) => match other {
        Colour::Red => false,
        Colour::Green => false,
        Colour::Blue(theirs) => mine == theirs,
      },
    }
  }
}
";

/// **`==` on a scalar is an instruction; on anything with a shape it is
/// `Eq::eq`.** One meaning for the operator, and the type gets to say what
/// equality means for it — in Khora, in a function a reader can go and look at.
#[test]
fn equality_on_an_adt_calls_its_eq_impl() {
    let ran = run(
        "eq_adt",
        &format!(
            "{EQ}
fn main() -> Int {{
  khora_print_int(if Colour::Red == Colour::Red {{ 1 }} else {{ 0 }});
  khora_print_int(if Colour::Red == Colour::Green {{ 1 }} else {{ 0 }});
  khora_print_int(if Colour::Blue(3) == Colour::Blue(3) {{ 1 }} else {{ 0 }});
  khora_print_int(if Colour::Blue(3) == Colour::Blue(4) {{ 1 }} else {{ 0 }});
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n1\n0\n0\n", "and nothing left alive");
    assert_eq!(ran.code, Some(0));
}

/// `!=` is `==` negated. Asking a type for both would be asking it to be
/// consistent about something it cannot get wrong.
#[test]
fn inequality_is_equality_negated() {
    let ran = run(
        "ne_adt",
        &format!(
            "{EQ}
fn main() -> Int {{
  khora_print_int(if Colour::Blue(3) != Colour::Blue(4) {{ 1 }} else {{ 0 }});
  khora_print_int(if Colour::Green != Colour::Green {{ 1 }} else {{ 0 }});
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n");
    assert_eq!(ran.code, Some(0));
}

/// A record has a shape too, and the same rule.
#[test]
fn equality_on_a_record_calls_its_eq_impl() {
    let ran = run(
        "eq_record",
        &format!(
            "{EQ}
export type Point = {{ x: Int, y: Int }};

impl Eq for Point {{
  fn eq(self, other: Point) -> Bool {{ self.x == other.x }}
}}

fn main() -> Int {{
  // Bound first: a record literal at the start of an `if` condition reads as
  // the start of a block, which is a grammar wrinkle rather than anything to
  // do with equality.
  let here: Point = {{ x: 1, y: 2 }};
  let same_x: Point = {{ x: 1, y: 9 }};
  let other: Point = {{ x: 2, y: 2 }};
  khora_print_int(if here == same_x {{ 1 }} else {{ 0 }});
  khora_print_int(if here == other {{ 1 }} else {{ 0 }});
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n", "the impl only looks at `x`, and that is its business");
    assert_eq!(ran.code, Some(0));
}

/// Without an impl there is nothing to call, and the checker says so where the
/// comparison is rather than letting the code generator find out.
#[test]
fn equality_without_an_impl_is_refused() {
    let found = refused(
        "eq_missing",
        "module t;
extern fn khora_print_int(value: Int);

export trait Eq { fn eq(self, other: Self) -> Bool; }

export type Colour = | Red | Green;

fn main() -> Int {
  khora_print_int(if Colour::Red == Colour::Green { 1 } else { 0 });
  0
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("no `Eq` impl") && m.contains("impl Eq for Colour")),
        "expected the missing impl to be named, got {found:?}"
    );
}

/// The scalars stay primitive, which is what keeps the rule from being
/// circular: `impl Eq for Int` is written *in terms of* `==`.
#[test]
fn scalars_still_compare_primitively() {
    let ran = run(
        "eq_scalars",
        &format!(
            "{EQ}
fn main() -> Int {{
  khora_print_int(if 1 == 1 {{ 1 }} else {{ 0 }});
  khora_print_int(if true == false {{ 1 }} else {{ 0 }});
  khora_print_int(if 1.5 == 1.5 {{ 1 }} else {{ 0 }});
  khora_print_int(if \"a\" == \"a\" {{ 1 }} else {{ 0 }});
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n1\n1\n");
    assert_eq!(ran.code, Some(0));
}

// --- ordering -----------------------------------------------------------

const ORD: &str = "module t;
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

export type Ordering = | Less | Equal | Greater;
export trait Eq { fn eq(self, other: Self) -> Bool; }
export trait Ord: Eq { fn cmp(self, other: Self) -> Ordering; }

export type Version = { major: Int, minor: Int };

impl Eq for Version {
  fn eq(self, other: Version) -> Bool { self.major == other.major && self.minor == other.minor }
}

impl Ord for Version {
  fn cmp(self, other: Version) -> Ordering {
    if self.major < other.major { Ordering::Less }
    else if self.major > other.major { Ordering::Greater }
    else if self.minor < other.minor { Ordering::Less }
    else if self.minor > other.minor { Ordering::Greater }
    else { Ordering::Equal }
  }
}

fn show(flag: Bool) -> () { khora_print_int(if flag { 1 } else { 0 }); }
";

/// **`<` on a type with a shape is `Ord::cmp`**, the same bargain `==` makes
/// with `Eq`. What "less than" means for a type is the type's answer, and
/// `Ord: Eq` is the trait saying the two have to agree.
///
/// One call decides all four operators, which is what the three-way `Ordering`
/// is for.
#[test]
fn ordering_an_adt_calls_its_ord_impl() {
    let ran = run(
        "ord_adt",
        &format!(
            "{ORD}
fn main() -> Int {{
  let early: Version = {{ major: 1, minor: 2 }};
  let later: Version = {{ major: 1, minor: 9 }};
  show(early < later);
  show(later < early);
  show(early > later);
  show(later > early);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n0\n0\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// `<=` is *not* "less or equal" spelled out; it is "not greater". Two tests
/// rather than one, the same answer, so the cheaper one wins — and equality is
/// where it shows.
#[test]
fn the_inclusive_operators_are_the_others_negated() {
    let ran = run(
        "ord_inclusive",
        &format!(
            "{ORD}
fn main() -> Int {{
  let same: Version = {{ major: 2, minor: 0 }};
  let also: Version = {{ major: 2, minor: 0 }};
  show(same <= also);
  show(same >= also);
  show(same < also);
  show(same > also);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n1\n0\n0\n", "equal is both inclusive and neither strict");
    assert_eq!(ran.code, Some(0));
}

/// The `Ordering` a comparison produces is a heap object, and it is released.
/// One allocation per comparison is what phase 9's reuse analysis exists to
/// remove; leaking one is a different problem and not this one.
#[test]
fn comparing_leaves_nothing_behind() {
    let ran = run(
        "ord_leaks",
        &format!(
            "{ORD}
fn count_below(limit: Version, at: Int, seen: Int) -> Int {{
  if at >= 20 {{
    seen
  }} else {{
    let here: Version = {{ major: 1, minor: at }};
    count_below(limit, at + 1, if here < limit {{ seen + 1 }} else {{ seen }})
  }}
}}

fn main() -> Int {{
  let limit: Version = {{ major: 1, minor: 7 }};
  khora_print_int(count_below(limit, 0, 0));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "7\n1\n", "twenty comparisons, and only `limit` still alive");
    assert_eq!(ran.code, Some(0));
}

/// Without an impl there is nothing to call, and the checker says so where the
/// comparison is.
#[test]
fn ordering_without_an_impl_is_refused() {
    let found = refused(
        "ord_missing",
        "module t;
extern fn khora_print_int(value: Int);

export type Ordering = | Less | Equal | Greater;
export trait Eq { fn eq(self, other: Self) -> Bool; }
export trait Ord: Eq { fn cmp(self, other: Self) -> Ordering; }

export type Version = { major: Int };

fn main() -> Int {
  let a: Version = { major: 1 };
  let b: Version = { major: 2 };
  khora_print_int(if a < b { 1 } else { 0 });
  0
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("no `Ord` impl") && m.contains("impl Ord for Version")),
        "expected the missing impl to be named, got {found:?}"
    );
}

// --- a wildcard arm --------------------------------------------------------
//
// `_` subtracts the whole row, tail included. It was type-checked before it
// was code-generated: `lower_catch` grouped the arms by the error type they
// name, a wildcard names none, so the switch's default still propagated while
// the checker said the function could not fail — and a program with no
// `raises` clause then walked into `unreachable`. These pin it.

/// Every concrete error in the row, taken by the one arm.
#[test]
fn a_wildcard_arm_catches_a_concrete_error() {
    let ran = run(
        "catch_wild_concrete",
        &format!(
            "{TWO_ERRORS}
fn safe(n: Int) -> Int {{ fetch(n)! catch {{ _ => 0 - 1, }} }}
fn main() -> Int {{
  print(safe(1));
  print(safe(2));
  print(safe(3));
  print(safe(9));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-1\n-1\n-1\n9\n",
        "both error types went to the one arm, and success still came back"
    );
    assert_eq!(ran.code, Some(0), "the function really cannot fail now");
}

/// The row the *caller* chose. `'e` is rigid inside `supervise`, so there is no
/// constructor to name and no set of error types to enumerate — the case a
/// wildcard exists for, and the one with no static type to take drop glue
/// from.
#[test]
fn a_wildcard_arm_catches_a_row_the_caller_chose() {
    let ran = run(
        "catch_wild_generic",
        &format!(
            "{TWO_ERRORS}
fn supervise<'e>(work: () -> Int raises 'e) -> Int {{ work()! catch {{ _ => 0 - 1, }} }}
fn main() -> Int {{
  print(supervise(fn () => fetch(1)!));
  print(supervise(fn () => fetch(2)!));
  print(supervise(fn () => fetch(9)!));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-1\n-1\n9\n",
        "a supervisor recovered from failures it cannot name"
    );
    assert_eq!(ran.code, Some(0));
}

/// An error type carrying something boxed.
const BOXED_ERRORS: &str = "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

export type Node = | Of(value: Int);
export type Heavy = | Detail(node: Node) | Plain;
export type DbError = | Timeout | Refused;

fn fetch(n: Int) -> Int raises Heavy + DbError {
  if n == 1 { raise Heavy::Detail(Node::Of(7)) }
  if n == 2 { raise Heavy::Plain }
  if n == 3 { raise DbError::Timeout }
  n
}
";

/// The hard half. A wildcard arm binds nothing — there is no name to bind
/// under — so the arm is the only place the error can be let go of, and it has
/// no static type to select drop glue from. Releasing the object with a null
/// callback would free it and leak every boxed field inside, once per caught
/// error, which on a server's failure path is a leak per request.
#[test]
fn a_wildcard_arm_releases_what_it_caught() {
    let ran = run(
        "catch_wild_release",
        &format!(
            "{BOXED_ERRORS}
fn safe(n: Int) -> Int {{ fetch(n)! catch {{ _ => 0 - 1, }} }}
fn main() -> Int {{
  khora_print_int(safe(1));
  khora_print_int(safe(2));
  khora_print_int(safe(3));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-1\n-1\n-1\n0\n",
        "the trailing 0 is the live-object count: the `Node` inside the caught \
         `Heavy::Detail` went with it"
    );
    assert_eq!(ran.code, Some(0));
}

/// A named arm and a wildcard in one `catch`: the named one still gets its
/// own error, and the wildcard takes what is left including the row's tail.
#[test]
fn a_named_arm_and_a_wildcard_share_one_catch() {
    let ran = run(
        "catch_wild_mixed",
        &format!(
            "{TWO_ERRORS}
fn safe(n: Int) -> Int {{
  fetch(n)! catch {{
    ModelError::RateLimited(ms) => ms,
    ModelError::TooLong => 0 - 2,
    _ => 0 - 1,
  }}
}}
fn main() -> Int {{
  print(safe(1));
  print(safe(2));
  print(safe(3));
  print(safe(9));
  0
}}
"
        ),
    );
    assert_eq!(
        ran.stdout, "-1\n500\n-2\n9\n",
        "the `DbError` fell to `_` while `ModelError` kept its own arms"
    );
    assert_eq!(ran.code, Some(0));
}
