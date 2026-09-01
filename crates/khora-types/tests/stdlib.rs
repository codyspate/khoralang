//! `std/` type checks.
//!
//! The corpus test in `khora-fmt` proves `std/` *parses*; nothing proved it
//! meant anything. Everything in `std::core` below the effect declarations is
//! ordinary phase 3 code — traits, generic impls, higher kinds, closures — so
//! it can be checked like any other program, and it is the largest single
//! piece of Khora that exists.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn repo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn std_dir() -> PathBuf {
    repo_dir().join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the source directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// Every diagnostic from every file under `dirs`, checked as one compilation
/// so that cross-module imports resolve.
fn errors_under(dirs: &[PathBuf]) -> Vec<String> {
    let mut paths = Vec::new();
    for dir in dirs {
        sources(dir, &mut paths);
    }
    paths.sort();
    assert!(!paths.is_empty(), "no .kh files under {dirs:?}");

    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("the sources should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    SourceRoot::new(&db, files.clone());

    files
        .iter()
        .flat_map(|f| {
            let path = f.path(&db).display().to_string();
            khora_types::diagnostics(&db, *f)
                .iter()
                .map(|e| format!("{path}: {}", e.message))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Every diagnostic from one program compiled together with `std`.
///
/// The messages below are about a capability, and a capability only exists
/// because `std::core` declares the effect — so unlike the rest of the
/// type-checker's tests these cannot be a single file with nothing behind it.
fn errors_with_std(program: &str) -> Vec<String> {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();

    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("the sources should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    let mine = SourceFile::new(&db, PathBuf::from("program.kh"), program.to_string());
    files.push(mine);
    SourceRoot::new(&db, files);

    khora_types::diagnostics(&db, mine).iter().map(|e| e.message.clone()).collect()
}

/// **A capability offered to a closure is not one it has to use.**
///
/// `nursery(fn () => 1)` was refused with ``nursery: Nursery is required here
/// but not provided`` — about a nursery that was being provided, to a body
/// that did not want it. Every parameter written `with { 'ef | cap: Cap }` had
/// the same effect: a body needing nothing could not be passed where something
/// was on offer.
///
/// The cause was that a lambda's capability row came out *closed*, so it could
/// not absorb a label nobody asked for. The error row beside it has been left
/// open since it was written, with a comment saying why — "a closed row here is
/// what made a mock that cannot fail unusable as an operation declared to
/// fail" — and the same sentence is true one field up.
#[test]
fn a_closure_need_not_use_the_capability_it_is_offered() {
    for body in ["nursery(fn () => 1)!", "bounded_nursery(4, fn () => 1)!", "scoped(fn () => 1)!"] {
        let found = errors_with_std(&format!(
            "module program;\n\
             import std::core::{{ChildFailed, Scope, bounded_nursery, nursery, scoped}};\n\
             pub fn main() -> Int raises ChildFailed {{\n  {body}\n}}\n"
        ));
        assert!(found.is_empty(), "`{body}` should compile: {found:?}");
    }
}

/// And the direction that must keep failing: a body that needs a capability
/// nobody offers is still an error.
///
/// This is what opening the row could have broken. An open tail absorbs what
/// the *caller* has; it does not invent one.
#[test]
fn a_closure_still_cannot_use_a_capability_nobody_has() {
    let found = errors_with_std(
        "module program;\n\
         import std::core::{Clock};\n\
         pub fn main() -> () {\n  let f = fn () => clock.sleep(1);\n  f()\n}\n",
    );
    assert!(!found.is_empty(), "a capability nobody installed is still an error");
}

/// **A capability's type arrives without its name, and the message says so.**
///
/// Importing `nursery` and not `Nursery` gave ``Nursery has no method
/// `adopt` ``, which is false twice: `Nursery` has that operation, and nothing
/// was misspelled. A trait's methods need the trait in scope — Rust's rule,
/// and a defensible one — but a capability is the case where the type can
/// reach a file without anybody naming it, so the reader has no reason to
/// suspect an import.
#[test]
fn a_capability_whose_type_is_unimported_says_to_import_it() {
    let found = errors_with_std(
        "module program;\n\
         import std::core::{ChildFailed, Fiber, nursery, print};\n\
         pub fn main() -> () raises ChildFailed {\n  \
           nursery(fn () => {\n    \
             nursery.adopt(Fiber::spawn(fn () => print(\"child\")));\n  \
           })!;\n\
         }\n",
    );
    assert!(
        found.iter().any(|m| m.contains("`Nursery` is not imported here")
            && m.contains("import std::core::{Nursery};")),
        "the fix is an import and the message should write it out: {found:?}"
    );
}

/// And the same program with the import checks, so the advice is true.
#[test]
fn the_import_the_message_names_is_the_one_that_works() {
    let found = errors_with_std(
        "module program;\n\
         import std::core::{ChildFailed, Fiber, Nursery, nursery, print};\n\
         pub fn main() -> () raises ChildFailed {\n  \
           nursery(fn () => {\n    \
             nursery.adopt(Fiber::spawn(fn () => print(\"child\")));\n  \
           })!;\n\
         }\n",
    );
    assert!(found.is_empty(), "the message's own advice must compile: {found:?}");
}

/// **The direction that must keep failing.** A real misspelling on a type that
/// *is* imported is not an import problem.
///
/// This caught the first version, which asked the trait table alone — an effect
/// is imported into `effects` and not into the traits, so it reported `Nursery`
/// as unimported when it was right there and `adpot` was the mistake.
#[test]
fn a_misspelled_operation_is_still_a_misspelling() {
    let found = errors_with_std(
        "module program;\n\
         import std::core::{ChildFailed, Fiber, Nursery, nursery, print};\n\
         pub fn main() -> () raises ChildFailed {\n  \
           nursery(fn () => {\n    \
             nursery.adpot(Fiber::spawn(fn () => print(\"child\")));\n  \
           })!;\n\
         }\n",
    );
    assert!(
        found.iter().any(|m| m.contains("`Nursery` has no method `adpot`")),
        "a typo is a typo: {found:?}"
    );
    assert!(
        !found.iter().any(|m| m.contains("is not imported here")),
        "and it must not be blamed on an import: {found:?}"
    );
}

/// **A capability shadows a function of the same name**, which is worth saying
/// because the names most likely to collide are exactly the ones that do: a
/// capability is usually called after the function that installs it.
///
/// ``Nursery is not a function`` was true, unhelpful, and about a type the
/// reader never wrote.
#[test]
fn a_capability_shadowing_a_function_says_which_is_which() {
    let found = errors_with_std(
        "module program;\n\
         import std::core::{Nursery, nursery, print};\n\
         fn f() -> () with { nursery: Nursery } {\n  \
           nursery(fn _n => print(\"inside\"));\n\
         }\n\
         pub fn main() -> () { print(\"hi\") }\n",
    );
    assert!(
        found.iter().any(|m| m.contains("`nursery` here is the capability")
            && m.contains("shadows any function of the same name")),
        "the message should name the binding, not just its type: {found:?}"
    );
}

#[test]
fn the_standard_library_type_checks() {
    let found = errors_under(&[std_dir()]);
    assert!(found.is_empty(), "std/ does not type check:\n  {}", found.join("\n  "));
}

/// The phase 4 exit criterion, minus serving a request.
///
/// `examples/risk_analyzer` is the program the whole design was written
/// against: capabilities, a fallible service, `catch` discharging half an error
/// row, a router carrying its handlers' requirements, a named context
/// installing three services at once. It type checking is the claim that the
/// pieces fit together, and it is worth a test precisely because every one of
/// those pieces has a unit test that passed while this did not.
///
/// **This reported clean for a long time while it was not.** `ai.extract` was
/// declared `forall <A: Extract> . (Prompt, A::Spec) -> A`, the checker had
/// nowhere to put the `A`, and the `Unknown` it produced agreed with everything
/// downstream — the same way entry 24's test was green. Errata 40 and 41 are
/// that story, the `Unknown` audit is what ended it, and
/// `docs/design/polymorphic-operations.md` is the decision that made the
/// program true rather than the test lenient.
///
/// `packages/` is in each source set because `examples/ledger_service` depends
/// on `postgres`. A real build resolves that through the manifest; this test
/// has no resolver, so it is handed the whole tree.
///
/// **One example at a time**, which it did not used to be. All four went into
/// a single compilation, and two of them declare `module main;` — so the test
/// was asserting that a file set no build would ever produce type checks, and
/// it only passed because a module declared in two files was an error nothing
/// reported. Checking them together also meant a name in one example could
/// satisfy a reference in another, which is the opposite of what this asserts.
#[test]
fn the_reference_application_type_checks() {
    let examples = repo_dir().join("examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(&examples).expect("examples/ should exist").flatten() {
        let example = entry.path();
        if !example.is_dir() {
            continue;
        }
        checked += 1;
        let found = errors_under(&[std_dir(), example.clone(), repo_dir().join("packages")]);
        assert!(
            found.is_empty(),
            "{} does not type check:\n  {}",
            example.display(),
            found.join("\n  ")
        );
    }
    assert!(checked >= 4, "expected every example to be checked, saw {checked}");
}

/// The pieces that make `std::core` worth having, so that losing one is a test
/// failure rather than a quiet regression in a file nobody reads.
#[test]
fn the_standard_library_declares_what_it_promises() {
    let text = std::fs::read_to_string(std_dir().join("core.kh")).expect("std/core.kh");
    for expected in [
        // Comparison, and the three-way answer that decides all six operators.
        "pub type Ordering",
        "pub trait Eq",
        "pub trait Ord: Eq",
        "pub trait Show",
        // Optional values and failures.
        "pub type Option<A>",
        "pub type Result<A, E>",
        "impl<A> Option<A>",
        "impl<A, E> Result<A, E>",
        // Containers and iteration.
        "pub type Array<A>",
        "pub type Map<K, V>",
        "pub type Chain<K, V>",
        // A map's key is any type with a `Hash`, which is what having bytes
        // was for: before them, a `String` could not be one.
        "pub trait Hash: Eq",
        "impl Hash for String",
        "pub type List<A>",
        "pub type Step<S, A>",
        "pub trait Iterator",
        "impl Iterator for Range",
        "impl<A> Iterator for List<A>",
        // The reason higher-kinded types are a non-negotiable.
        "pub trait Functor",
        "pub trait Applicative: Functor",
        "pub trait Traversable: Functor",
        "impl Traversable for Option",
        "impl Traversable for List",
        // The scalars show, which is what a derived `Show` on a record of them
        // calls. Without these `derive(Show)` was correctly refused everywhere.
        "impl Show for Int",
        "impl Show for Bool",
        "impl Show for String",
        // Shared state, and the ordered map that can go in it.
        "pub type Shared<A>",
        "pub type Dict<K, V>",
        "pub type Pair<K, V>",
        // The growable one. `Array::empty` is what made it possible to hold an
        // `Array<A>` rather than an `Array<Option<A>>`, and a hundred integers
        // two objects rather than a hundred and three.
        "pub type Vector<A>",
        "fn empty() -> Array<A>",
    ] {
        assert!(text.contains(expected), "std/core.kh no longer declares `{expected}`");
    }
}
