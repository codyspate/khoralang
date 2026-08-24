//! Where reference counting lands.
//!
//! These assert the *shape* of the plan rather than exact counts: phase 6 will
//! remove pairs that cancel, and a test pinning "exactly two dups" would fail
//! for a good reason. What must not change is which values are counted at all,
//! and that every owned reference is released.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_perceus::{is_boxed, rc_plans, RcPlan};
use khora_types::Type;

fn plan(db: &dyn Db, text: &str, function: &str) -> RcPlan {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    rc_plans(db, file)
        .iter()
        .find(|(name, _)| name == function)
        .map(|(_, plan)| plan.clone())
        .unwrap_or_else(|| panic!("no function `{function}`"))
}

const ADT: &str = "module m;\nexport type R = | A | B(n: Int);\n";

#[test]
fn machine_words_are_not_counted() {
    assert!(!is_boxed(&Type::Int));
    assert!(!is_boxed(&Type::Bool));
    assert!(!is_boxed(&Type::Unit));

    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(a: Int) -> Int { let b = a; b }\n", "f");
    assert!(p.boxed.is_empty(), "an Int should not be reference counted: {p:?}");
    assert!(p.dups.is_empty(), "no dups for machine words");
}

#[test]
fn strings_and_adts_are_counted() {
    assert!(is_boxed(&Type::Str));
    assert!(is_boxed(&Type::adt("R")));
}

/// An owned parameter is released — unless the body hands its reference on,
/// which `fn f(s) { s }` does. The one read *is* the last use, so `s` moves
/// into the result and there is nothing left for the block to release.
///
/// This asserted a release when it was written, because every read copied and
/// every block released: two reference-count operations to return an argument
/// unchanged. `docs/design/reuse.md`.
#[test]
fn a_parameter_returned_unchanged_is_moved_not_copied() {
    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(s: String) -> String { s }\n", "f");

    assert_eq!(p.boxed.len(), 1, "the parameter should be counted: {p:?}");
    assert!(p.dups.is_empty(), "the last read should move, not copy: {p:?}");
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert!(released.is_empty(), "nothing is left to release: {p:?}");
}

/// A read that is *not* the last still copies, because the value has to outlive
/// it. Only the last one takes the binding's own reference.
#[test]
fn a_read_that_is_not_the_last_still_dups() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\nfn f(s: String) -> String { s + s }\n",
        "f",
    );
    // `String::byte_length` would not do here: it only borrows, so neither read
    // would copy. `+` genuinely consumes both sides.
    assert_eq!(p.dups.len(), 1, "the first read copies, the second moves: {p:?}");
}

/// `let t = s; t` moves twice and copies nothing: each binding is read exactly
/// once, unconditionally, and hands its reference straight on. Four
/// reference-count operations before this, none now.
#[test]
fn a_chain_of_single_uses_costs_nothing() {
    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(s: String) -> String { let t = s; t }\n", "f");
    assert!(p.dups.is_empty(), "nothing needs copying: {p:?}");
    assert!(p.drops.is_empty(), "nothing is left to release: {p:?}");
    assert_eq!(p.moved.len(), 2, "both bindings moved: {p:?}");
}

/// A branch where one arm takes the binding and the other never mentions it
/// consumes it on every path: the read moves, and the arm that did not take it
/// releases at its head.
#[test]
fn a_branch_that_takes_on_one_path_releases_on_the_other() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\nfn f(s: String, yes: Bool) -> String { if yes { s } else { \"\" } }\n",
        "f",
    );
    assert!(p.dups.is_empty(), "the taken read needs no copy: {p:?}");
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert!(released.is_empty(), "the block no longer releases it: {p:?}");
    let at_arms: Vec<_> = p.arm_drops.values().flatten().collect();
    assert_eq!(at_arms.len(), 1, "the other arm releases instead: {p:?}");
}

/// An arm that *borrows* the binding without taking it blocks the whole branch
/// from consuming it. An arm release goes at the arm's head, which is before
/// the borrow, so granting one here would free a value the arm is about to
/// read. The conservative plan stands: the taking read copies after all, and
/// the block releases.
#[test]
fn a_branch_with_a_borrowing_arm_keeps_its_dups() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\nfn f(s: String, yes: Bool) -> Int {\n  \
         if yes { String::byte_length(s) } else { String::byte_length(s + \"!\") }\n}\n",
        "f",
    );
    assert_eq!(p.dups.len(), 1, "a branch it cannot consume still copies: {p:?}");
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert_eq!(released.len(), 1, "and the block still releases: {p:?}");
    assert!(p.arm_drops.is_empty(), "no arm releases it: {p:?}");
}

#[test]
fn a_boxed_let_is_released_by_its_block() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!("{ADT}fn f() -> Int {{\n  let r = R::B(1);\n  0\n}}\n"),
        "f",
    );

    assert_eq!(p.boxed.len(), 1, "the ADT local should be counted: {p:?}");
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert_eq!(released.len(), 1, "the block must release what it declared: {p:?}");
}

/// Every counted local has to be released exactly once somewhere, or the
/// runtime's live counter will not return to zero.
#[test]
fn every_counted_local_is_released_once() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!(
            "{ADT}fn f(s: String) -> Int {{\n  let a = R::B(1);\n  let b = R::A;\n  let c = s;\n  0\n}}\n"
        ),
        "f",
    );

    let mut released: Vec<_> = p.drops.values().flatten().copied().collect();
    let before = released.len();
    released.sort();
    released.dedup();
    assert_eq!(released.len(), before, "a local was released twice: {p:?}");

    // Released *or* moved. A binding whose last read took its reference has
    // nothing left to release, and that is the optimization rather than an
    // omission — but it must be exactly one of the two, or the count does not
    // return to zero in one direction or the other.
    for local in &p.boxed {
        assert!(
            released.contains(local) || p.moved.contains(local),
            "local {local:?} is counted but neither released nor moved: {p:?}"
        );
        assert!(
            !(released.contains(local) && p.moved.contains(local)),
            "local {local:?} is both released and moved: {p:?}"
        );
    }
}

/// A binding from a match arm borrows out of the scrutinee, which the arm does
/// not own. Dropping it would free a value the scrutinee still holds.
#[test]
fn match_arm_bindings_are_not_released_by_the_arm() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!(
            "{ADT}fn f(r: R) -> Int {{\n  match r {{\n    R::B(n) => n,\n    R::A => 0,\n  }}\n}}\n"
        ),
        "f",
    );

    // The arm binding is never released — it borrows out of the scrutinee,
    // which the arm does not own. `r` itself is the function's, and its one read
    // is unconditional and consuming, so it moves into the `match` rather than
    // being released at the end. Either way the arm accounts for nothing.
    let released: Vec<_> = p.drops.values().flatten().copied().collect();
    assert!(released.is_empty(), "the arm should release nothing: {p:?}");
    assert_eq!(p.moved.len(), 1, "the scrutinee moved into the match: {p:?}");
}

#[test]
fn nested_blocks_release_what_they_declared() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        &format!("{ADT}fn f() -> Int {{\n  let outer = R::A;\n  {{\n    let inner = R::A;\n    0\n  }}\n}}\n"),
        "f",
    );

    assert_eq!(p.boxed.len(), 2, "both locals should be counted: {p:?}");
    assert_eq!(p.drops.len(), 2, "each block should release its own: {p:?}");
    for locals in p.drops.values() {
        assert_eq!(locals.len(), 1, "a block released someone else's local: {p:?}");
    }
}

#[test]
fn a_function_with_nothing_boxed_needs_no_plan() {
    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(a: Int, b: Int) -> Int { a + b }\n", "f");
    assert!(p.boxed.is_empty() && p.dups.is_empty() && p.drops.is_empty(), "{p:?}");
}

/// A guard runs before its arm, and the backward pass does not walk into one —
/// its reads keep their copies. They are still reads, though, and something
/// earlier must not hand the binding away underneath one.
#[test]
fn a_read_in_a_guard_keeps_the_binding_alive() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\nexport type R = | A | B;\n\
         fn f(s: String, r: R) -> Int {\n  \
         let t = s + \"\";\n  \
         match r { R::A if String::byte_length(s) > 0 => 1, _ => 0 }\n}\n",
        "f",
    );
    // Locals bind in written order, so  is the first.
    assert!(
        !p.moved.iter().any(|local| local.index() == 0),
        "`s` is read again in the guard and must not be handed away: {p:?}"
    );
}

/// **A capability is read where nothing mentions it**, so a mention can never
/// be its last use.
///
/// `with { clock: Clock }` puts `clock` in scope, and a call to anything that
/// also wants a `Clock` is handed the evidence by code generation. There is no
/// expression for that read, so a backward pass over the expressions cannot see
/// it: `twice` mentions `clock` once and forwards it once, and taking it at the
/// mention leaves the forward reading a binding that was handed away.
///
/// This was wrong before the last-use pass reached bodies that can unwind, and
/// it survived — the binding kept pointing at a handler the enclosing `with`
/// block still held, so the count was one short rather than the pointer being
/// wrong. Clearing the slot at a take is what turned it into a crash.
#[test]
fn a_capability_is_never_handed_to_its_last_mention() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\n\
         export effect Clock { now: () -> Int, }\n\
         fn tick() -> Int with { clock: Clock } { clock.now() }\n\
         fn twice() -> Int with { clock: Clock } { clock.now() + tick() }\n",
        "twice",
    );

    assert!(!p.boxed.is_empty(), "the capability should be counted at all: {p:?}");
    assert!(p.moved.is_empty(), "a capability must not be handed to a mention: {p:?}");
    assert!(p.takes.is_empty(), "and so no read of one is a take: {p:?}");
}

// --- the borrow table's key ------------------------------------------------

/// A package may declare a type called `Shared`, and its methods are ordinary
/// Khora functions that own their receiver.
///
/// `borrowed_arguments` is keyed by a type *name*, and while every program was
/// one source root that name could only ever be `std`'s. Packages ended that.
/// Under a name-only key this program's `get` would be told its caller was
/// lending: the caller would not make a reference, the callee would release one
/// anyway, and the receiver would be freed while somebody still held it.
///
/// So the plan must borrow nothing here. `owner_of` declines a type `std` did
/// not declare — `docs/design/reuse.md` §1.
#[test]
fn a_packages_own_shared_is_not_borrowed() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\n\
         export type Shared = { label: String };\n\
         impl Shared {\n  \
         export fn get(self) -> String { self.label }\n\
         }\n\
         fn use_it(cell: Shared) -> String { Shared::get(cell) }\n",
        "use_it",
    );
    assert!(
        p.borrowed.is_empty(),
        "a method this module wrote owns its receiver, so nothing may be lent to it: {p:?}"
    );
}

/// The same for the method-call spelling, which is the one people write and
/// the one the table reads through `owner_of`.
#[test]
fn a_packages_own_array_method_is_not_borrowed() {
    let db = KhoraDatabase::new();
    let p = plan(
        &db,
        "module m;\n\
         export type Array = { label: String };\n\
         impl Array {\n  \
         export fn length(self) -> Int { 1 }\n\
         }\n\
         fn use_it(xs: Array) -> Int { xs.length() }\n",
        "use_it",
    );
    assert!(
        p.borrowed.is_empty(),
        "`Array` here is this module's, not `std`'s: {p:?}"
    );
}
