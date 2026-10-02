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

/// Nothing is held inline in these tests: they are about which *shapes* carry
/// a count, and an empty answer is the representation this crate had before
/// unboxing existed.
fn nothing_unboxed() -> khora_types::unboxed::Unboxed {
    khora_types::unboxed::Unboxed::default()
}

const ADT: &str = "module m;\npub type R = | A | B(n: Int);\n";

#[test]
fn machine_words_are_not_counted() {
    assert!(!is_boxed(&Type::Int, &nothing_unboxed()));
    assert!(!is_boxed(&Type::Bool, &nothing_unboxed()));
    assert!(!is_boxed(&Type::Unit, &nothing_unboxed()));

    let db = KhoraDatabase::new();
    let p = plan(&db, "module m;\nfn f(a: Int) -> Int { let b = a; b }\n", "f");
    assert!(p.boxed.is_empty(), "an Int should not be reference counted: {p:?}");
    assert!(p.dups.is_empty(), "no dups for machine words");
}

#[test]
fn strings_and_adts_are_counted() {
    assert!(is_boxed(&Type::Str, &nothing_unboxed()));
    assert!(is_boxed(&Type::adt("R"), &nothing_unboxed()));
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
        "module m;\npub type R = | A | B;\n\
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
         pub effect Clock { now: () -> Int, }\n\
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
         pub type Shared = { label: String };\n\
         impl Shared {\n  \
         pub fn get(self) -> String { self.label }\n\
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
         pub type Array = { label: String };\n\
         impl Array {\n  \
         pub fn length(self) -> Int { 1 }\n\
         }\n\
         fn use_it(xs: Array) -> Int { xs.length() }\n",
        "use_it",
    );
    assert!(
        p.borrowed.is_empty(),
        "`Array` here is this module's, not `std`'s: {p:?}"
    );
}

/// **A call that cannot stop costs a moved binding nothing; one that can
/// costs it its strike.** `held` is taken by `consume` after a call to
/// `step`. Where the code generator says `step` can stop -- tagged, or
/// fallible -- a cancellation leaving there finds `held` still owned, so its
/// block keeps the release and the take clears the slot. Where `step` was
/// pruned, the binding is struck from the block like any other move.
///
/// This is the question per call site that replaced a per-body answer.
/// `unwinds` is false in both, because neither body can leave on an error.
#[test]
fn a_moved_binding_keeps_its_release_only_across_a_call_that_can_stop() {
    use khora_hir::body::Expr;
    let db = KhoraDatabase::new();
    let text = "module m;\n\
                fn step() -> Int { 1 }\n\
                fn consume(s: String) -> Int { 0 }\n\
                fn f(s: String) -> Int {\n  let held = s;\n  let n = step();\n  n + consume(held)\n}\n";
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    let checked = khora_types::checked(&db, file);
    let bodies = khora_hir::body::bodies(&db, file);
    let body = &bodies.iter().find(|(n, _)| n == "f").expect("f").1;
    let types = &checked.bodies.iter().find(|(n, _)| n == "f").expect("types").1;
    let defined = khora_perceus::Defined::default();
    let unboxed = nothing_unboxed();
    let step_call = body
        .exprs()
        .find(|(_, e)| match e {
            Expr::Call { callee, .. } => matches!(
                body.expr(*callee),
                Expr::Path(khora_hir::Resolution::Item { name, .. }) if name == "step"
            ),
            _ => false,
        })
        .map(|(id, _)| id)
        .expect("the call to step");
    let released = |p: &RcPlan| p.drops.values().flatten().copied().collect::<Vec<_>>();

    let pruned = khora_perceus::plan(body, types, &defined, &unboxed, &|_| false, false);
    let tagged =
        khora_perceus::plan(body, types, &defined, &unboxed, &|site| site == step_call, false);

    assert!(!pruned.unwinds && !tagged.unwinds, "neither body leaves on an error");
    let held: Vec<_> =
        tagged.moved.iter().copied().filter(|l| tagged.held_across.contains(l)).collect();
    assert_eq!(held.len(), 1, "one moved binding is held across the call: {tagged:?}");
    assert!(released(&tagged).contains(&held[0]), "and its block releases it: {tagged:?}");
    assert!(pruned.held_across.is_empty(), "nothing is held across no stop: {pruned:?}");
    assert!(
        pruned.moved.iter().all(|l| !released(&pruned).contains(l)),
        "every moved binding is struck when nothing can stop: {pruned:?}"
    );
}

// --- a field read or write through a binding --------------------------------

/// The plan for `function` in `text`, with the body it was made from, for a
/// test that has to find an expression by its shape.
fn plan_with_body(
    db: &dyn Db,
    text: &str,
    function: &str,
) -> (RcPlan, khora_hir::body::Body) {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    let bodies = khora_hir::body::bodies(db, file);
    let body = bodies.iter().find(|(n, _)| n == function).expect("the function").1.clone();
    (plan(db, text, function), body)
}

/// The base of every `base.f = v` in `body`, in source order.
fn written_bases(body: &khora_hir::body::Body) -> Vec<khora_hir::body::ExprId> {
    use khora_hir::body::Expr;
    let mut found: Vec<_> = body
        .exprs()
        .filter_map(|(_, e)| match e {
            Expr::Assign { target, .. } => match body.expr(*target) {
                Expr::Field { base, .. } => Some(*base),
                _ => None,
            },
            _ => None,
        })
        .collect();
    found.sort();
    found
}

const SLOT: &str = "module m;\npub type Slot = { mut held: String };\n";

/// **`r.x` borrows `r`**: the binding holds a reference for the whole read,
/// so the read makes none of its own and the lowering gives none back.
#[test]
fn a_field_read_through_a_binding_borrows_it() {
    use khora_hir::body::Expr;
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!(
            "{SLOT}fn f(s: Slot) -> Int {{\n  \
             String::byte_length(s.held) + String::byte_length(s.held)\n}}\n"
        ),
        "f",
    );
    let bases: Vec<_> = body
        .exprs()
        .filter_map(|(_, e)| match e {
            Expr::Field { base, .. } if matches!(body.expr(*base), Expr::Local(_)) => Some(*base),
            _ => None,
        })
        .collect();
    assert_eq!(bases.len(), 2, "two field reads: {p:?}");
    for base in &bases {
        assert!(p.borrowed.contains(base), "`s` in `s.held` should be borrowed: {p:?}");
        assert!(
            !p.dups.contains(base) && !p.takes.contains(base),
            "and neither copied nor taken: {p:?}"
        );
    }
    // Borrowing is not taking, so the parameter is still the block's to release.
    let released: Vec<_> = p.drops.values().flatten().collect();
    assert_eq!(released.len(), 1, "the parameter is released once, by the block: {p:?}");
}

/// **A write whose value cannot touch the binding borrows it too.**
#[test]
fn a_field_write_with_a_quiet_value_borrows_the_binding() {
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!("{SLOT}fn f(s: Slot, t: String) -> Int {{ s.held = t + \"!\"; 0 }}\n"),
        "f",
    );
    let bases = written_bases(&body);
    assert_eq!(bases.len(), 1);
    assert!(p.borrowed.contains(&bases[0]), "`s.held = t + ..` should borrow `s`: {p:?}");
}

/// **`r.x = v` where `v` assigns anything takes the ordinary path.** An
/// assignment in `v` could replace `r` before the store, and a borrowed `r`
/// holds no reference that would keep the old record alive until then.
#[test]
fn a_field_write_whose_value_assigns_does_not_borrow() {
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!(
            "{SLOT}fn f(s: Slot, t: Slot) -> Int {{\n  \
             s.held = {{ t.held = \"a\"; \"b\" }};\n  0\n}}\n"
        ),
        "f",
    );
    let bases = written_bases(&body);
    assert_eq!(bases.len(), 2, "the outer write and the one inside its value");
    let (outer, inner) = (bases[0], bases[1]);
    assert!(
        !p.borrowed.contains(&outer),
        "`s` must not be borrowed across a value that assigns: {p:?}"
    );
    assert!(p.dups.contains(&outer), "it is copied instead: {p:?}");
    assert!(p.borrowed.contains(&inner), "the inner write's value is quiet, so it borrows: {p:?}");
}

/// **`r.x = v` where `v` reads `r` takes the ordinary path**, because that
/// read could be the take that hands `r`'s reference away before the store.
#[test]
fn a_field_write_whose_value_reads_the_binding_does_not_borrow() {
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!("{SLOT}fn f(s: Slot) -> Int {{ s.held = s.held + \"!\"; 0 }}\n"),
        "f",
    );
    let bases = written_bases(&body);
    assert_eq!(bases.len(), 1);
    assert!(!p.borrowed.contains(&bases[0]), "`s` is read by the value: {p:?}");
}

// --- an arm handing its binding on ------------------------------------------

/// The reads of the local called `name` in `function`, in source order.
fn reads_of(body: &khora_hir::body::Body, name: &str) -> Vec<khora_hir::body::ExprId> {
    use khora_hir::body::Expr;
    let mut found: Vec<_> = body
        .exprs()
        .filter_map(|(id, e)| match e {
            Expr::Local(local) if body.local(*local).name == name => Some(id),
            _ => None,
        })
        .collect();
    found.sort();
    found
}

const HAND_ON: &str = "module m;\n\
     pub type Bad = | Bad(Int);\n\
     pub type Conn = | Open(Int, Conn) | End;\n\
     pub type Opt = | Some(Conn) | None;\n\
     fn consume(c: Conn) -> Int { 1 }\n\
     fn receive(n: Int) -> Opt { Opt::None }\n\
     fn check(n: Int) -> Int raises Bad { n }\n";

/// **An arm hands its binding to a consuming call in a body that can
/// unwind.** The arm copied `c` out of the payload at its head and released
/// the scrutinee, so the arm's reference is the only one, and the call can
/// take it. Copying it instead left the call a count of 2, which a hand-off's
/// uniqueness test refuses.
///
/// Because a `!` can leave before the take, the arm keeps its release of `c`
/// (`held_across`) and the take clears the slot, as every other binding in an
/// unwinding body does.
#[test]
fn an_arm_binding_is_taken_in_a_body_that_can_unwind() {
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!(
            "{HAND_ON}fn f(o: Opt) -> Int raises Bad {{\n  \
             let k = check(0)!;\n  \
             match o {{ Opt::Some(c) => consume(c) + k, Opt::None => 0 }}\n}}\n"
        ),
        "f",
    );
    assert!(p.unwinds, "the `!` makes this an unwinding body: {p:?}");
    let reads = reads_of(&body, "c");
    assert_eq!(reads.len(), 1);
    assert!(p.takes.contains(&reads[0]), "the call takes the arm's `c`: {p:?}");
    assert!(!p.dups.contains(&reads[0]), "and does not copy it: {p:?}");
    let c = p.moved.iter().copied().find(|l| body.local(*l).name == "c").expect("`c` moved");
    assert!(p.held_across.contains(&c), "the arm keeps its release for a `!`: {p:?}");
}

/// **An arm binding is dead before its `match`**, as a `let` binding is
/// before its `let`. Left live, a `match` in a loop carried the arm's `c` to
/// the back edge, so the read at the call found it "needed later" and copied.
#[test]
fn an_arm_binding_is_taken_in_a_loop() {
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!(
            "{HAND_ON}fn f(n: Int) -> Int {{\n  \
             let mut i = 0;\n  \
             let mut total = 0;\n  \
             while i < n {{\n    \
             match receive(1) {{ Opt::Some(c) => total = total + consume(c), Opt::None => () }};\n    \
             i = i + 1\n  \
             }};\n  \
             total\n}}\n"
        ),
        "f",
    );
    let reads = reads_of(&body, "c");
    assert_eq!(reads.len(), 1);
    assert!(p.takes.contains(&reads[0]), "the call takes the arm's `c`: {p:?}");
    assert!(!p.dups.contains(&reads[0]), "and does not copy it: {p:?}");
}

/// **A binding from outside the branch is still the branch's to settle, and
/// in an unwinding body it settles nothing.** Taking `s` in one arm would need
/// a release at the head of the other, and the block also keeps its release
/// there, so the read copies as before. Pins that the arm-binding rule above
/// did not widen to this.
#[test]
fn an_outside_binding_is_not_taken_in_an_arm_of_an_unwinding_body() {
    let db = KhoraDatabase::new();
    let (p, body) = plan_with_body(
        &db,
        &format!(
            "{HAND_ON}fn eat(s: String) -> Int {{ 1 }}\n\
             fn f(o: Opt, s: String) -> Int raises Bad {{\n  \
             let k = check(0)!;\n  \
             match o {{ Opt::Some(_) => eat(s) + k, Opt::None => 0 }}\n}}\n"
        ),
        "f",
    );
    let reads = reads_of(&body, "s");
    assert_eq!(reads.len(), 1);
    assert!(p.dups.contains(&reads[0]), "`s` is copied: {p:?}");
    assert!(p.arm_drops.is_empty(), "no arm releases it: {p:?}");
}
