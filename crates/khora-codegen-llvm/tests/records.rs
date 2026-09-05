#![cfg(feature = "llvm")]

//! Record update: `{ ..old, field: value }`.
//!
//! **Three people reached for this before it existed.** Each was writing their
//! first Khora program, none had seen the others' code, and all three wrote
//! `{ ..tally, lines: tally.lines + 1 }` unprompted — because it is what a
//! reader of Rust or JavaScript already knows. What they got was twenty parse
//! errors starting with "expected a statement or expression", none of which
//! said the form was not there.
//!
//! The cost was not the confusion, it was what they wrote instead. One
//! function — "add one to whichever counter this event names" — came out at
//! forty lines of five near-identical five-field literals. Another author
//! restructured a whole fold accumulator around `mut` fields after two failed
//! attempts at a pure one.
//!
//! The semantics are the boring ones and that is deliberate: a new record
//! every time, the base untouched, fields not named carried across, a field
//! named twice refused. Nothing here is written in place — `{ ..old, .. }` is
//! about not *repeating* the fields that do not change, not about changing
//! `old`.

use crate::harness;

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

/// Compiles `source` expecting it to be refused, and hands back the messages.
fn refused(name: &str, source: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    match khora_codegen_llvm::compile(&db, root, &dir.join("unused")) {
        Ok(()) => panic!("`{name}` compiled and should not have:\n{source}"),
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
}

/// The forty-line function, written the way it was reached for.
const TALLY: &str = "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

pub type Counts = { created: Int, deleted: Int, notified: Int, name: String };
pub type Event = | Created | Deleted;

fn applied(c: Counts, e: Event) -> Counts {
  match e {
    Event::Created => { ..c, created: c.created + 1 },
    Event::Deleted => { ..c, deleted: c.deleted + 1 },
  }
}

fn shown(c: Counts) -> () {
  print(c.created);
  print(c.deleted);
  print(c.notified);
}
";

/// **A record update carries the fields it does not name.**
#[test]
fn a_record_update_carries_the_fields_it_does_not_name() {
    let ran = run(
        "record_update",
        &format!(
            "{TALLY}
fn main() -> Int {{
  let start: Counts = {{ created: 0, deleted: 0, notified: 7, name: \"tally\" }};
  let one = applied(start, Event::Created);
  let two = applied(applied(one, Event::Created), Event::Deleted);
  shown(two);
  0
}}
"
        ),
    );

    assert_eq!(ran.stdout, "2\n1\n7\n");
    assert_eq!(ran.code, Some(0));
}

/// **The base is untouched**, which is the whole difference between this and
/// the `mut` fields two of the three authors fell back to.
#[test]
fn a_record_update_leaves_its_base_alone() {
    let ran = run(
        "record_update_pure",
        &format!(
            "{TALLY}
fn main() -> Int {{
  let start: Counts = {{ created: 0, deleted: 0, notified: 7, name: \"tally\" }};
  let after = applied(start, Event::Created);
  shown(after);
  shown(start);
  0
}}
"
        ),
    );

    assert_eq!(ran.stdout, "1\n0\n7\n0\n0\n7\n");
}

/// Several fields at once, a `String` carried across, and a base with no
/// fields after it — which is the record itself, legal and pointless.
#[test]
fn a_record_update_may_name_several_fields_or_none() {
    let ran = run(
        "record_update_several",
        &format!(
            "{TALLY}
fn main() -> Int {{
  let start: Counts = {{ created: 0, deleted: 0, notified: 7, name: \"tally\" }};
  shown({{ ..start, created: 100, deleted: 5 }});
  shown({{ ..start }});
  0
}}
"
        ),
    );

    assert_eq!(ran.stdout, "100\n5\n7\n0\n0\n7\n");
}

/// **The carried fields are counted, not aliased.**
///
/// Every field the literal does not name is loaded out of the base and stored
/// into the new record, so a boxed one is held twice and has to be retained.
/// Getting that wrong is a double free or a leak rather than a compile error,
/// which is why this counts objects instead of reading output.
#[test]
fn a_record_update_does_not_leak_or_double_free() {
    let ran = run(
        "record_update_counts",
        "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

pub type Holder = { name: String, n: Int };

fn churn(rounds: Int) -> Int {
  let mut h: Holder = { name: \"held\", n: 0 };
  let mut i = 0;
  while i < rounds {
    h = { ..h, n: h.n + 1 };
    i = i + 1;
  };
  h.n
}

fn main() -> Int {
  print(churn(1000));
  print(khora_live_count());
  0
}
",
    );

    assert_eq!(ran.stdout, "1000\n0\n", "a thousand updates, nothing left over");
    assert_eq!(ran.code, Some(0));
}

/// A field the base's type does not have.
#[test]
fn a_field_the_record_does_not_have_is_refused() {
    let found = refused(
        "record_update_no_field",
        &format!(
            "{TALLY}
fn main() -> Int {{
  let start: Counts = {{ created: 0, deleted: 0, notified: 7, name: \"tally\" }};
  shown({{ ..start, nosuch: 1 }});
  0
}}
"
        ),
    );
    assert!(found.iter().any(|e| e.contains("`Counts` has no field `nosuch`")), "{found:?}");
}

/// A base that is not a record at all.
#[test]
fn a_base_that_is_not_a_record_is_refused() {
    let found = refused(
        "record_update_bad_base",
        &format!(
            "{TALLY}
fn main() -> Int {{
  shown({{ ..5, created: 1 }});
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("is not a record, so there is nothing to take fields")),
        "{found:?}"
    );
}

/// **A field named twice is a mistake, not a last-one-wins.**
///
/// With a base it is the difference between overriding a field and overriding
/// it twice, and silently taking the second is how one of the two becomes dead
/// code nobody notices.
#[test]
fn a_field_given_twice_is_refused() {
    let found = refused(
        "record_update_twice",
        &format!(
            "{TALLY}
fn main() -> Int {{
  let start: Counts = {{ created: 0, deleted: 0, notified: 7, name: \"tally\" }};
  shown({{ ..start, created: 1, created: 2 }});
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("`created` is given twice in this record")),
        "{found:?}"
    );
}

/// A value of the wrong type for the field it is given to.
#[test]
fn a_field_of_the_wrong_type_is_refused() {
    let found = refused(
        "record_update_wrong_type",
        &format!(
            "{TALLY}
fn main() -> Int {{
  let start: Counts = {{ created: 0, deleted: 0, notified: 7, name: \"tally\" }};
  shown({{ ..start, created: \"text\" }});
  0
}}
"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("field `created`") && e.contains("found `String`")),
        "{found:?}"
    );
}

/// **A record literal takes its type from what was expected of it.**
///
/// A bare literal is found by its field set among the record types the module
/// can *name*, which is right when there is nothing to go on and wrong the
/// moment there is. `Shared::modify`'s closure returns `Changed<A, B>`, so
///
/// ```khora
/// Shared::modify(cell, fn n => { state: n * 2, result: n })
/// ```
///
/// was `no record type has exactly the fields `state`, `result``, and the fix
/// was to add `Changed` to the import list — a name the source never mentions
/// and never wants to.
///
/// Two things had to change. `Changed` exists only in the *signature* of
/// `Shared::modify`, and what travels with an import is walked through a
/// type's fields — `Shared` is opaque and holds nothing, so nothing was
/// reached. And the walker deliberately does not descend into a function type,
/// which is right for deciding what a value contains and wrong for deciding
/// what a caller must be able to produce.
///
/// This uses the real `std::core`, because the bug was about what an import
/// carries.
#[test]
fn a_record_literal_takes_its_type_from_what_expects_it() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("record_expected");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join("core.kh");
    let core = std::fs::read_to_string(&core).expect("std/core.kh");

    // No `Changed` in the import list, and the source never names it.
    let main = "module demo::main;
import std::core::{Shared, print};

fn main() -> () {
  let cell = Shared::of(1);
  let answer = Shared::modify(cell, fn n => { state: n * 2, result: n });
  print(Int::to_string(answer));
  print(Int::to_string(Shared::get(cell)));
}
";

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), core),
        SourceFile::new(&db, dir.join("main.kh"), main.to_string()),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling failed:\n  {}", messages.join("\n  "));
    }

    let out = Command::new(&exe).output().expect("the program should run");
    let printed = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(printed, "1\n2\n", "the answer, then the state it left behind");
}

/// **And a literal nothing expects is still found by its labels alone.**
///
/// `TypeMap::reachable`'s note is the rule this had to respect: a record
/// literal must not *infer* as a type the file cannot name. Taking the type
/// from an expectation is not inferring — the expected type already decided
/// which record it is, and the lookup only asks what that record holds. With
/// no expectation, nothing changes.
#[test]
fn a_literal_with_nothing_expecting_it_is_still_found_by_its_labels() {
    let found = refused(
        "record_no_expectation",
        "module t;
fn print(value: Int);
extern fn khora_print_int(value: Int);

fn main() -> Int {
  let r = { alpha: 1, beta: 2 };
  print(r.alpha);
  0
}
",
    );
    assert!(
        found.iter().any(|e| e.contains("no record type has exactly the fields")),
        "{found:?}"
    );
}
