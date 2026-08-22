#![cfg(feature = "llvm")]

//! Regions and finalizers, end to end.
//!
//! The phase 5 promise is that a resource acquired in a region is released
//! when the region ends, *however* it ends. What makes that cheap is that a
//! region is an ordinary reference-counted value: releasing the last reference
//! runs its finalizers, and the paths that release a binding — falling off the
//! end of a block, an early `return`, a raise passing through — are paths code
//! generation already emitted. Nothing here needed a new rule about unwinding.

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

/// A region declared here rather than imported, so these stay one file. `std`
/// declares the same three operations against the same runtime.
const REGIONS: &str = "module t;
fn print(value: Int);
fn khora_live_count() -> Int;

export type Region;

impl Region {
  fn open() -> Region;
  fn root() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}
";

/// The ordinary path: the region ends with the block that opened it.
#[test]
fn a_finalizer_runs_when_the_region_ends() {
    let ran = run(
        "region_end",
        &format!(
            "{REGIONS}
fn work() -> Int {{
  let region = Region::open();
  Region::defer(region, fn () => print(1));
  print(0);
  9
}}

fn main() -> Int {{ print(work()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n1\n9\n", "the finalizer runs before the caller sees the value");
    assert_eq!(ran.code, Some(0));
}

/// Reverse order: a finalizer deferred later may depend on one deferred
/// earlier, so the last acquired is the first released.
#[test]
fn finalizers_run_in_reverse() {
    let ran = run(
        "region_order",
        &format!(
            "{REGIONS}
fn work() -> Int {{
  let region = Region::open();
  Region::defer(region, fn () => print(1));
  Region::defer(region, fn () => print(2));
  Region::defer(region, fn () => print(3));
  0
}}

fn main() -> Int {{ work() }}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n2\n1\n");
    assert_eq!(ran.code, Some(0));
}

/// An early `return` leaves the region the same way falling off the end does,
/// because it is the same release of the same binding.
#[test]
fn a_finalizer_runs_on_an_early_return() {
    let ran = run(
        "region_return",
        &format!(
            "{REGIONS}
fn work(n: Int) -> Int {{
  let region = Region::open();
  Region::defer(region, fn () => print(1));
  if n > 0 {{ return 7 }}
  8
}}

fn main() -> Int {{ print(work(1)); print(work(0)); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n7\n1\n8\n");
    assert_eq!(ran.code, Some(0));
}

/// The one that matters: an error passing through runs the finalizers on its
/// way out. This is `unwind_to`, which raises have used since 4.3b — the
/// region needed nothing of its own.
#[test]
fn a_finalizer_runs_when_a_raise_passes_through() {
    let ran = run(
        "region_raise",
        &format!(
            "{REGIONS}
export type Oops = | Bad;

fn work(n: Int) -> Int raises Oops {{
  let region = Region::open();
  Region::defer(region, fn () => print(1));
  if n < 0 {{ raise Oops::Bad }}
  n
}}

fn main() -> Int raises Oops {{ print(work(5)!); print(work(0 - 1)!); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n5\n1\n", "the second call's finalizer runs before it leaves");
    assert_eq!(ran.code, Some(1));
}

/// A finalizer closes over what it releases, which is the whole point of it
/// being a closure rather than a function pointer.
#[test]
fn a_finalizer_closes_over_what_it_releases() {
    let ran = run(
        "region_capture",
        &format!(
            "{REGIONS}
fn work(n: Int) -> Int {{
  let region = Region::open();
  let held = n * 10;
  Region::defer(region, fn () => print(held));
  0
}}

fn main() -> Int {{ work(4) }}
"
        ),
    );
    assert_eq!(ran.stdout, "40\n");
    assert_eq!(ran.code, Some(0));
}

/// Nothing leaks: not the region, not the finalizer closures, not what they
/// captured.
#[test]
fn a_region_leaves_nothing_behind() {
    let ran = run(
        "region_leaks",
        &format!(
            "{REGIONS}
fn work() -> Int {{
  let region = Region::open();
  let text = \"held\";
  Region::defer(region, fn () => print(1));
  Region::defer(region, fn () => print(2));
  0
}}

fn main() -> Int {{ work(); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(ran.stdout, "2\n1\n0\n", "the trailing 0 is the live-object count");
    assert_eq!(ran.code, Some(0));
}

/// The root region ends when the program does, and the entry point is what
/// ends it — after `main` has returned, so a finalizer deferred to it runs
/// last rather than not at all.
#[test]
fn the_root_region_ends_with_the_program() {
    let ran = run(
        "region_root",
        &format!(
            "{REGIONS}
fn main() -> Int {{
  let region = Region::root();
  Region::defer(region, fn () => print(99));
  print(1);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n99\n");
    assert_eq!(ran.code, Some(0));
}

/// And on the failing path too: a finalizer that only runs when nothing went
/// wrong is not a finalizer.
#[test]
fn the_root_region_ends_after_an_uncaught_raise() {
    let ran = run(
        "region_root_raise",
        &format!(
            "{REGIONS}
export type Oops = | Bad;

fn main() -> Int raises Oops {{
  let region = Region::root();
  Region::defer(region, fn () => print(99));
  raise Oops::Bad
}}
"
        ),
    );
    assert_eq!(ran.stdout, "99\n");
    assert_eq!(ran.code, Some(1));
}

// --- scope, as std spells it -----------------------------------------------

/// `Scope` and `scoped` written out here rather than imported, so this stays
/// one file. They are the same three lines `std::core` has, against the same
/// runtime — what is being pinned is that a region *composes* with the effect
/// system: a handler holds it, a `with` block installs it, and row subtraction
/// takes `scope` out of the caller's requirement.
const SCOPE: &str = "module t;
fn print(value: Int);
fn khora_live_count() -> Int;

export type Region;
impl Region {
  fn open() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}

export effect Scope { defer: (() -> ()) -> (), }

export fn acquire<A, 'e>(value: A, release: (A) -> ()) -> A
  with { 'e | scope: Scope }
{
  scope.defer(fn () => release(value));
  value
}
";

/// The shape a caller writes: acquire inside a region, and the release is
/// somebody else's problem.
#[test]
fn an_acquired_value_is_released_when_the_region_ends() {
    let ran = run(
        "scope_acquire",
        &format!(
            "{SCOPE}
export fn use_it() -> Int with {{ scope: Scope }} {{
  let handle = acquire(7, fn h => print(h));
  handle + 1
}}

fn main() -> Int {{
  let region = Region::open();
  with {{ scope: handler for Scope {{ defer: fn f => Region::defer(region, f) }} }} {{
    print(use_it());
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "8\n7\n", "the value is used, then released");
    assert_eq!(ran.code, Some(0));
}

/// Row subtraction is what makes this readable: `use_it` requires `scope`, and
/// the function that installs one does not.
#[test]
fn a_region_discharges_the_scope_requirement() {
    let ran = run(
        "scope_subtract",
        &format!(
            "{SCOPE}
export fn use_it() -> Int with {{ scope: Scope }} {{
  acquire(7, fn h => print(h))
}}

export fn scoped_use() -> Int {{
  let region = Region::open();
  with {{ scope: handler for Scope {{ defer: fn f => Region::defer(region, f) }} }} {{
    use_it()
  }}
}}

fn main() -> Int {{ print(scoped_use()); print(khora_live_count()); 0 }}
"
        ),
    );
    assert_eq!(
        ran.stdout, "7\n7\n0\n",
        "released as the region ends, then the value, then nothing left over"
    );
    assert_eq!(ran.code, Some(0));
}

/// Two resources, released in the reverse of the order they were acquired.
#[test]
fn acquired_values_are_released_in_reverse() {
    let ran = run(
        "scope_reverse",
        &format!(
            "{SCOPE}
export fn use_them() -> Int with {{ scope: Scope }} {{
  let first = acquire(1, fn h => print(h));
  let second = acquire(2, fn h => print(h));
  first + second
}}

fn main() -> Int {{
  let region = Region::open();
  with {{ scope: handler for Scope {{ defer: fn f => Region::defer(region, f) }} }} {{
    print(use_them());
  }}
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "3\n2\n1\n");
    assert_eq!(ran.code, Some(0));
}
