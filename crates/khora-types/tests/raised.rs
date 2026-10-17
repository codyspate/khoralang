//! Failures as values: `Raised<'er>`, `Opaque`, and what a `catch` binding
//! arm is bound as.
//!
//! **A binding arm over a row is a `Raised`.** Before it, a binding over two
//! types, a rigid `'er`, or a raise typed later was refused ("there is no
//! single type to give the binding"), and a function raising `A + B` could
//! not go through `attempt` or `retry` at all.
//!
//! **A `Raised` is never its error type**, at any row: it is a box that says
//! which error it holds, and an error is laid out as itself. `Raised::only`
//! converts, for a row of one.
//!
//! **`Opaque` is only ever inside a `Raised`.** It has no cases and so no
//! drop glue; a bare one leaked what it carried and was matched under the
//! wrong id.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

/// The declarations `std::core` makes, in a file standing in for it: a test
/// file is one module, and `Raised` and `Opaque` are only std's in
/// `std::core`.
const CORE: &str = "module std::core;\n\
                    pub type Result<A, E> = | Ok(A) | Err(E);\n\
                    pub type Option<A> = | Some(A) | None;\n\
                    pub type Raised<'er>;\n\
                    pub type Opaque;\n\
                    impl<'er> Raised<'er> {\n\
                      pub fn type_name(self) -> String;\n\
                      pub fn note(self) -> String;\n\
                      pub fn with_note(self, note: String) -> Raised<'er>;\n\
                      pub fn forget(self) -> Raised<Opaque>;\n\
                    }\n\
                    impl<E> Raised<E> { pub fn only(self) -> E; }\n\
                    pub fn attempt<A, 'er, 'ef>(body: () -> A with 'ef raises 'er) -> Result<A, Raised<'er>> with 'ef;\n\
                    pub type Inv = | Out | Offline;\n\
                    pub type Pay = | Declined | Timeout;\n\
                    pub type Audit = | Tampered;\n\
                    fn two(n: Int) -> Int raises Inv + Pay {\n\
                      if n == 0 { raise Inv::Out } else if n == 1 { raise Pay::Timeout } else { n }\n\
                    }\n\
                    fn one(n: Int) -> Int raises Pay { if n == 0 { raise Pay::Declined } else { n } }\n";

// ---- `attempt`, and the box at every row.

/// `attempt` over a body raising two types answers `Raised` of both. On
/// 0.4.0 `attempt` took one `E`, and this was refused.
#[test]
fn attempt_over_two_types_answers_raised_of_both() {
    assert_clean(&format!(
        "{CORE}pub fn f() -> Result<Int, Raised<Inv + Pay>> {{ attempt(fn () => two(3)!) }}\n"
    ));
}

/// **Over one type it is still a `Raised`.** Letting `Raised<Pay>` stand for
/// `Pay` when a row happened to close chose the layout at a moment code
/// generation did not see, and read a `Pay`'s fields as a box.
#[test]
fn attempt_over_one_type_answers_raised_of_it() {
    assert_clean(&format!("{CORE}pub fn f() -> Result<Int, Raised<Pay>> {{ attempt(fn () => one(3)!) }}\n"));
    assert_reports(
        &format!("{CORE}pub fn f() -> Result<Int, Pay> {{ attempt(fn () => one(3)!) }}\n"),
        "`Pay` does not match `Raised<",
    );
}

/// `only` turns a one-type failure into its error, and refuses two.
#[test]
fn only_takes_a_failure_of_one_type() {
    assert_clean(&format!(
        "{CORE}pub fn f(r: Raised<Pay>) -> Int {{ match Raised::only(r) {{ Pay::Declined => 0, Pay::Timeout => 1 }} }}\n"
    ));
    assert_reports(
        &format!(
            "{CORE}pub fn f(r: Raised<Inv + Pay>) -> Int {{ match Raised::only(r) {{ Pay::Declined => 0, Pay::Timeout => 1 }} }}\n"
        ),
        "is not accounted for here",
    );
}

/// **`match` on a `Raised` binding with an error type's cases says what to
/// write** (p3_rigid). The binding over `Pay + 'er` is a `Raised`; `Pay`
/// patterns there ran on 0.4.0 only because every caller chose an empty
/// `'er`, and read any other type's error as a `Pay`. The refusal names the
/// form that matches the error inside.
#[test]
fn a_match_on_a_raised_binding_says_to_raise_it_into_a_catch() {
    let text = format!(
        "{CORE}pub fn f<'er>(g: () -> Int raises 'er) -> Int raises 'er {{\n\
           (if false {{ one(0)! }} else {{ g()! }}) catch {{\n\
             err => match err {{ Pay::Declined => 0, Pay::Timeout => 1 }},\n\
           }}\n\
         }}\n"
    );
    assert_reports(&text, "the value here is a `Raised<Pay + 'er>`");
    assert_reports(&text, "`(raise err) catch { Pay::Declined => .., _ => .. }`");
}

/// **A later-typed re-raise of a `Raised` raises its whole row, tail and all**
/// (review probe `p_late_raised_rigid_abort`). `re`'s `raise f` is typed only
/// when `re(e)` is called, with `e: Raised<Pay + 'er>`; charging only `Pay`
/// let `launder` declare `raises Pay` and hand a caller's `Boom` to a `catch`
/// with no arm for it (status 134, "this is a compiler bug").
#[test]
fn a_late_typed_reraise_charges_the_rows_tail() {
    let launder = |declares: &str| {
        format!(
            "{CORE}pub fn launder<'er>(k: () -> Int raises Pay + 'er) -> Int raises {declares} {{\n\
               let re = fn f => raise f;\n\
               match attempt(k) {{ Result::Ok(n) => n, Result::Err(e) => re(e)! }}\n\
             }}\n"
        )
    };
    assert_reports(&launder("Pay"), "'er");
    assert_clean(&launder("Pay + 'er"));
    // With no `raises` at all (`p_late_raised_rigid_ice`), the refusal says
    // what to add.
    assert_reports(
        &format!(
            "{CORE}pub fn swallow<'er>(k: () -> Int raises 'er) -> Int {{\n\
               let re = fn f => raise f;\n\
               match attempt(k) {{ Result::Ok(n) => n, Result::Err(e) => re(e)! }}\n\
             }}\n"
        ),
        "add `raises 'er` to this function",
    );
}

/// **A late-typed `raise f` of a forgotten row is checked as one** (review
/// probes `p_late_opaque_generic_bypass`, `p_late_opaque_false_refusal`).
///
/// `f` is a `Raised<Opaque>` only once `step(forgotten)` is checked, after the
/// `catch`. Settled late, `Gx::X(Option::Some(_))` was compiled at an
/// instantiation nothing decided and missed the `Gx<Option<Int>>` that
/// arrived (`got=99`), and `Pay::Declined` was refused as raised by nothing.
#[test]
fn a_late_forgotten_row_is_checked_as_one() {
    let gx = format!(
        "{CORE}pub type Gx<A> = | X(a: A) | Y;\n\
         fn throw_gx() -> Int raises Gx<Option<Int>> {{ raise Gx::X(Option::Some(42)) }}\n\
         pub fn f() -> Int {{\n\
           let step = fn f => (if false {{ raise Gx::Y }} else {{ raise f }}) catch {{\n\
             Gx::X(Option::Some(_)) => 1,\n\
             _ => 99,\n\
           }};\n\
           match attempt(throw_gx) {{ Result::Ok(n) => n, Result::Err(e) => step(Raised::forget(e)) }}\n\
         }}\n"
    );
    assert_reports(&gx, "from a failure that forgot its row, and nothing says which `Gx` it is");
}

/// The other half: `Pay::Declined` over a late-typed forgotten row is not
/// refused as raised by nothing, as it is not over a known one.
#[test]
fn a_named_arm_over_a_late_forgotten_row_is_accepted() {
    let pay = format!(
        "{CORE}pub fn f() -> Int {{\n\
           let step = fn f => (raise f) catch {{ Pay::Declined => 1, _ => 2 }};\n\
           match attempt(fn () => one(0)!) {{ Result::Ok(n) => n, Result::Err(e) => step(Raised::forget(e)) }}\n\
         }}\n"
    );
    assert_clean(&pay);
}

/// **A binding after named arms that take every case of the one type is
/// unreachable** (review probe `p_unreachable_bind_one_type`), as an arm after
/// a catch-all is. `Pay::Declined => 1, Pay::Timeout => 2, err => 3` was
/// accepted and `err` never ran.
#[test]
fn a_binding_after_arms_that_take_everything_is_unreachable() {
    assert_reports(
        &format!(
            "{CORE}pub fn f() -> Int {{ one(0)! catch {{ Pay::Declined => 1, Pay::Timeout => 2, err => 3 }} }}\n"
        ),
        "this arm is unreachable",
    );
    // One case left over, or a guard on a case: the binding is reachable.
    assert_clean(&format!(
        "{CORE}pub fn f() -> Int {{ one(0)! catch {{ Pay::Declined => 1, err => 3 }} }}\n"
    ));
    assert_clean(&format!(
        "{CORE}pub fn f(b: Bool) -> Int {{ one(0)! catch {{ Pay::Declined if b => 1, Pay::Timeout => 2, err => 3 }} }}\n"
    ));
}

/// An error value is not a `Raised`, and a `Raised` is not its error.
#[test]
fn a_raised_and_its_error_type_never_meet() {
    assert_reports(&format!("{CORE}pub fn f(p: Pay) -> String {{ Raised::type_name(p) }}\n"), "found `Pay`");
    assert_reports(
        &format!("{CORE}fn take(p: Pay) -> Int {{ 0 }}\npub fn f(r: Raised<Pay>) -> Int {{ take(r) }}\n"),
        "found `Raised<Pay>`",
    );
}

/// **An open row stays a `Raised`**: a binding typed from a closure's open
/// row was read as the one type in it before the row closed, and code
/// generation panicked.
#[test]
fn a_failure_from_an_open_row_is_not_its_error() {
    assert_reports(
        &format!(
            "{CORE}fn take(p: Pay) -> Int {{ 0 }}\n\
             pub fn f() -> Int {{ match attempt(fn () => one(0)!) {{ Result::Ok(n) => n, Result::Err(e) => take(e) }} }}\n"
        ),
        "found `Raised<Pay",
    );
}

/// A generic function over `'er` instantiated at one type keeps the box.
#[test]
fn a_generic_failure_stays_a_raised_at_one_type() {
    assert_clean(&format!(
        "{CORE}fn name<'er>(r: Raised<'er>) -> String {{ Raised::type_name(r) }}\n\
         pub fn f(r: Raised<Pay>) -> String {{ name(r) }}\n"
    ));
}

/// Annotating a two-type failure as one of its types is refused, naming
/// both.
#[test]
fn a_two_type_failure_is_not_one_of_them() {
    assert_reports(
        &format!("{CORE}pub fn f() -> Result<Int, Pay> {{ attempt(fn () => two(3)!) }}\n"),
        "`Pay` does not match `Raised<Inv + Pay",
    );
}

/// `std::core::Raised<A + B>`, qualified, is not refused for its `+`. (The
/// review found the `+` check matched the bare name only.)
#[test]
fn a_qualified_raised_takes_a_row() {
    let found = errors(&format!(
        "{CORE}pub fn f() -> Result<Int, std::core::Raised<Inv + Pay>> {{ attempt(fn () => two(3)!) }}\n"
    ));
    assert!(!found.iter().any(|e| e.contains("`+` builds")), "got {found:?}");
}

// ---- A binding arm over a row.

/// A binding arm over two types binds a `Raised`, with no arm per case.
///
/// It was refused on 0.4.0: "there is no single type to give the binding".
#[test]
fn a_binding_arm_over_two_types_binds_raised() {
    assert_clean(&format!(
        "{CORE}fn log(failure: Raised<Inv + Pay>) -> Int {{ 0 }}\n\
         pub fn f() -> Int {{ two(0)! catch {{ err => log(err) }} }}\n"
    ));
}

/// Raising a `Raised` on raises the row it holds, so a function declaring
/// less is refused at the `raise`.
#[test]
fn raising_a_failure_on_needs_its_whole_row() {
    assert_reports(
        &format!("{CORE}pub fn f() -> Int raises Pay {{ two(0)! catch {{ err => raise err }} }}\n"),
        "`raise` needs `Inv`, which this function does not raise",
    );
    assert_clean(&format!(
        "{CORE}pub fn f() -> Int raises Inv + Pay {{ two(0)! catch {{ err => raise err }} }}\n"
    ));
}

/// **A binding over a rigid `'er` is a `Raised` that ends in it**, so
/// `err => raise err` needs `+ 'er` on the function. This is the shape the
/// interim refusal named ("`'er`, an error type the caller chooses, so there
/// is no single type to give the binding").
#[test]
fn a_binding_over_a_rigid_tail_keeps_it() {
    assert_reports(
        &format!(
            "{CORE}pub fn f<'er>(g: () -> Int raises 'er) -> Int raises Pay {{\n\
               (if false {{ one(0)! }} else {{ g()! }}) catch {{ err => raise err }}\n\
             }}\n"
        ),
        "'er",
    );
    assert_clean(&format!(
        "{CORE}pub fn f<'er>(g: () -> Int raises 'er) -> Int raises Pay + 'er {{\n\
           (if false {{ one(0)! }} else {{ g()! }}) catch {{ err => raise err }}\n\
         }}\n"
    ));
}

/// **The same with a closure's open tail beside `'er`**, which `join_tails`
/// once dropped the `'er` in favor of (on 0.4.0 the program segfaulted).
#[test]
fn a_binding_keeps_a_rigid_tail_beside_an_open_one() {
    assert_reports(
        &format!(
            "{CORE}pub fn f<'er>(g: () -> Int raises 'er) -> Int raises Pay {{\n\
               let h = fn () => one(0)!;\n\
               (if false {{ h()! }} else {{ g()! }}) catch {{ err => raise err }}\n\
             }}\n"
        ),
        "'er",
    );
    assert_clean(&format!(
        "{CORE}pub fn f<'er>(g: () -> Int raises 'er) -> Int raises Pay + 'er {{\n\
           let h = fn () => one(0)!;\n\
           (if false {{ h()! }} else {{ g()! }}) catch {{ err => raise err }}\n\
         }}\n"
    ));
}

/// A binding over only `'er` is a `Raised<'er>`, and may be inspected.
#[test]
fn a_binding_over_only_a_rigid_tail_is_a_failure_value() {
    assert_clean(&format!(
        "{CORE}pub fn f<'er>(g: () -> Int raises 'er) -> String {{\n\
           let _ = g()! catch {{ err => {{ let s: String = Raised::type_name(err); 0 }} }};\n\
           \"ok\"\n\
         }}\n"
    ));
}

/// **A raise typed late still reaches past a binding arm.** The operand
/// raises `x`, typed only at the call; `err => raise err` must still carry
/// the `Audit` out, and the function does not declare it.
#[test]
fn a_late_raise_in_a_catch_operand_reaches_the_binding() {
    assert_reports(
        "module a;\n\
         pub type Inv = | Out;\n\
         pub type Pay = | Declined;\n\
         pub type Audit = | Tampered;\n\
         fn checkout(n: Int) -> Int raises Inv + Pay { if n == 1 { raise Inv::Out } else { raise Pay::Declined } }\n\
         pub fn sneak(n: Int) -> Int raises Inv + Pay {\n\
           let step = fn (x) => { (if n == 0 { raise x } else { checkout(n)! }) catch { err => raise err } };\n\
           step(Audit::Tampered)!\n\
         }\n",
        "`Audit`, which this function does not raise",
    );
}

/// **A binding at one type closes the row.** The binding was typed `Pay`
/// and the closure's open tail left open; a later assignment widened the
/// closure to `Inv + Pay`, and `err => raise err` read an `Inv` as a `Pay`
/// (exit 139 on 0.4.0).
#[test]
fn a_binding_at_one_type_closes_the_row() {
    assert_reports(
        "module a;\n\
         pub type Inv = | Out(n: Int);\n\
         pub type Pay = | Declined(why: String);\n\
         fn pay(n: Int) -> Int raises Pay { raise Pay::Declined(\"card\") }\n\
         fn checkout(n: Int) -> Int raises Inv + Pay { if n == 0 { raise Inv::Out(1) } else { pay(n)! } }\n\
         pub fn confuse() -> Int raises Pay {\n\
           let mut f = fn () => pay(0)!;\n\
           let _ = f()! catch { err => raise err };\n\
           f = fn () => checkout(0)!;\n\
           0\n\
         }\n",
        "is not accounted for here",
    );
}

/// **A binding after named arms that cover a whole type does not see that
/// type.** `Pay` taken case by case, `err => raise err` raises only `Inv`.
#[test]
fn a_binding_after_named_arms_gets_what_is_left() {
    assert_clean(&format!(
        "{CORE}pub fn f() -> Int raises Inv {{\n\
           two(0)! catch {{ Pay::Declined => 0, Pay::Timeout => 1, err => raise err }}\n\
         }}\n"
    ));
}

/// **Narrowed to one type, it is still a `Raised`**, so a failure's context
/// survives the narrowing (review 3, `p10_context_stress` #5): bound bare,
/// the note was dropped on the floor.
#[test]
fn a_binding_narrowed_to_one_type_keeps_the_failure_value() {
    assert_clean(&format!(
        "{CORE}pub fn f() -> Int {{\n\
           two(0)! catch {{ Pay::Declined => 0, Pay::Timeout => 1, err => {{ let n: String = Raised::note(err); 2 }} }}\n\
         }}\n"
    ));
}

/// A type merely named, with a case left over, is still in the binding's
/// row: its remaining case arrives there.
#[test]
fn a_type_named_in_part_stays_in_the_binding() {
    assert_clean(&format!(
        "{CORE}fn log(failure: Raised<Inv + Pay>) -> Int {{ 0 }}\n\
         pub fn f() -> Int {{ two(0)! catch {{ Pay::Declined => 0, err => log(err) }} }}\n"
    ));
}

/// **One type, raised directly, binds the error itself** -- no box, and no
/// cost over 0.4.0.
#[test]
fn a_binding_over_one_type_is_the_error() {
    assert_clean(&format!(
        "{CORE}pub fn f() -> Int {{ one(0)! catch {{ err => match err {{ Pay::Declined => 0, Pay::Timeout => 1 }} }} }}\n"
    ));
}

/// **`(raise f) catch` keeps `f`'s context**: the binding is a `Raised` even
/// over one type, however the `raise` is wrapped, and a named arm matches
/// the error inside.
#[test]
fn a_reraised_failure_binds_a_raised() {
    assert_clean(&format!(
        "{CORE}pub fn f(r: Raised<Pay>) -> String {{ (raise r) catch {{ Pay::Declined => \"d\", other => Raised::note(other) }} }}\n"
    ));
    assert_clean(&format!(
        "{CORE}pub fn f(r: Raised<Pay>) -> String {{ ({{ raise r }}) catch {{ Pay::Declined => \"d\", other => Raised::note(other) }} }}\n"
    ));
}

/// **A binding over two instantiations of one type is refused**, as a row
/// holds one: the second arrived unhandled at run time.
#[test]
fn a_binding_over_two_instantiations_is_refused() {
    assert_reports(
        "module m;\n\
         pub type Gx<A> = | X(s: String, v: A);\n\
         pub type Pay = | Declined;\n\
         fn fi() -> Int raises Gx<Int>;\n\
         fn fs() -> Int raises Gx<String>;\n\
         fn fp() -> Int raises Pay { raise Pay::Declined }\n\
         pub fn f(b: Bool) -> Int { (if b { fi()! } else if !b { fs()! } else { fp()! }) catch { err => 0 } }\n",
        "raises `Gx` at two",
    );
}

/// Two different rigid tails have no one rest to share.
#[test]
fn a_binding_over_two_rigid_tails_is_refused() {
    assert_reports(
        &format!(
            "{CORE}pub fn f<'e1, 'e2>(g: () -> Int raises 'e1, h: () -> Int raises 'e2) -> Int {{\n\
               (if false {{ g()! }} else {{ h()! }}) catch {{ err => 0 }}\n\
             }}\n"
        ),
        "can follow only one of them",
    );
}

// ---- `Opaque`.

/// **`Opaque` is never a value of its own.** `Raised::only` at
/// `E := Opaque` handed back a bare `Opaque`, which has no drop glue.
#[test]
fn only_on_a_forgotten_failure_is_refused() {
    assert_reports(
        &format!("{CORE}pub fn f(r: Raised<Opaque>) -> Int {{ let _bare = Raised::only(r); 0 }}\n"),
        "`Opaque` is only ever held inside a `Raised`",
    );
}

/// The refusal points at the first place a bare `Opaque` was made, not at
/// the shortest expression that has one (review 3, `p9a`'s span).
#[test]
fn a_bare_opaque_is_reported_where_it_was_made() {
    let text = format!(
        "{CORE}pub fn f(r: Raised<Opaque>) -> Int {{\n\
           let bare = Raised::only(r);\n\
           let again = bare;\n\
           0\n\
         }}\n"
    );
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.clone());
    let found = diagnostics(&db, file);
    let hit = found
        .iter()
        .find(|e| e.message.contains("`Opaque` is only ever held"))
        .unwrap_or_else(|| panic!("no refusal: {found:?}"));
    let at = &text[usize::from(hit.range.start())..usize::from(hit.range.end())];
    assert!(at.starts_with("Raised::only"), "reported at {at:?}");
}

/// **A `raises Opaque` body does not reach a generic `E` bare.**
#[test]
fn opaque_as_a_generic_error_type_is_refused() {
    assert_reports(
        &format!(
            "{CORE}fn run(g: Raised<Opaque>) -> Int raises Opaque {{ raise g }}\n\
             fn rethrow<E>(body: () -> Int raises E) -> Int raises E {{ body()! catch {{ err => raise err }} }}\n\
             pub fn f(g: Raised<Opaque>) -> Int raises Opaque {{ rethrow(fn () => run(g)!)! }}\n"
        ),
        "`Opaque` is only ever held inside a `Raised`",
    );
}

/// **A named arm at a generic type over a forgotten row is refused when
/// nothing says which instantiation it is.**
#[test]
fn a_generic_arm_over_a_forgotten_row_must_be_decided() {
    assert_reports(
        &format!(
            "{CORE}pub type Gx<A> = | X(v: A) | Y(msg: String);\n\
             pub fn f(g: Raised<Opaque>) -> Int {{ (raise g) catch {{ Gx::Y(_m) => 1, _ => 0 }} }}\n"
        ),
        "nothing says which `Gx` it is",
    );
}

/// **Decided all the way down** (review 3, `p10b`): `Gx::X(Option::Some(_))`
/// left `Gx<Option<?>>`, which the shallow test passed, and the arm missed
/// the `Gx<Option<Int>>` that arrived.
#[test]
fn a_generic_arm_over_a_forgotten_row_is_decided_in_its_arguments_too() {
    assert_reports(
        &format!(
            "{CORE}pub type Gx<A> = | X(v: A);\n\
             pub fn f(g: Raised<Opaque>) -> Int {{\n\
               (raise g) catch {{ Gx::X(Option::None) => 0, Gx::X(Option::Some(_)) => 1, _ => 2 }}\n\
             }}\n"
        ),
        "nothing says which `Gx` it is",
    );
}

/// **The arms of one type share one instantiation** (review 3, `p10c`):
/// `Gx::X(v)` used as an `Int` and `Gx::Y(w)` used as a `String` were two
/// types over a forgotten row, and a `Gx<Int>::Y` met the arm compiled for
/// `Gx<String>` (exit 132). As one type, the two uses disagree.
#[test]
fn arms_of_one_type_over_a_forgotten_row_share_an_instantiation() {
    let program = |arms: &str| {
        format!(
            "{CORE}pub type Gx<A> = | X(v: A) | Y(w: A);\n\
             fn int(n: Int) -> Int {{ n }}\n\
             fn text(s: String) -> Int {{ 0 }}\n\
             pub fn f(g: Raised<Opaque>) -> Int {{\n\
               (raise g) catch {{ {arms}, _ => 2 }}\n\
             }}\n"
        )
    };
    // The rule is named, with what each arm makes the type and the spelling
    // that catches both, and the caret is in the same place whichever arm
    // comes first (review 2, `c1` and `c5`): the unifier's own sentence
    // blamed whichever field use it met second.
    let mut places = Vec::new();
    for arms in ["Gx::X(v) => int(v), Gx::Y(w) => text(w)", "Gx::Y(w) => text(w), Gx::X(v) => int(v)"] {
        let text = program(arms);
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "a.kh".into(), text.clone());
        let found: Vec<_> = diagnostics(&db, file).iter().map(|e| (e.message.clone(), e.range)).collect();
        let rule = found
            .iter()
            .find(|(m, _)| m.contains("the arms for `Gx` in this `catch` are one `Gx<..>`"))
            .unwrap_or_else(|| panic!("no rule message for {arms}: {found:?}"));
        for needle in ["`Gx<Int>`", "`Gx<String>`", "because the failure forgot its row", "(raise other) catch {"] {
            assert!(rule.0.contains(needle), "{needle:?} missing from {:?}", rule.0);
        }
        assert_eq!(found.len(), 1, "the rule should be the only error: {found:?}");
        places.push(rule.1);
    }
    assert_eq!(places[0], places[1], "the caret moved with the arm order");
    // And one decided arm decides its sibling.
    assert_clean(&format!(
        "{CORE}pub type Gx<A> = | X(v: A) | Y(msg: String);\n\
         fn int(n: Int) -> Int {{ n }}\n\
         pub fn f(g: Raised<Opaque>) -> Int {{\n\
           (raise g) catch {{ Gx::X(v) => int(v), Gx::Y(_m) => 1, _ => 2 }}\n\
         }}\n"
    ));
}

/// The spelling the rule's message gives (review 2, `c4`) checks: one
/// instantiation per `catch`, the rest raised into a second one.
#[test]
fn a_second_instantiation_is_caught_by_a_second_catch() {
    assert_clean(&format!(
        "{CORE}pub type Gx<A> = | X(v: A) | Y(w: A);\n\
         fn int(n: Int) -> Int {{ n }}\n\
         fn text(s: String) -> Int {{ 0 }}\n\
         pub fn f(g: Raised<Opaque>) -> Int {{\n\
           (raise g) catch {{ Gx::X(v) => int(v), other => (raise other) catch {{ Gx::Y(w) => text(w), _ => 2 }} }}\n\
         }}\n"
    ));
}

/// A `catch` over `Opaque` is never total, so it needs a catch-all, and a
/// named arm needs nothing in the operand to name its type.
#[test]
fn a_catch_over_a_forgotten_row_takes_any_named_type() {
    assert_clean(&format!(
        "{CORE}pub fn f(g: Raised<Opaque>) -> Int {{ (raise g) catch {{ Audit::Tampered => 1, _ => 0 }} }}\n"
    ));
}

/// **A user's own `Opaque` is an ordinary error type**: matched by name, it
/// turned off the `catch` checks.
#[test]
fn a_users_opaque_is_not_stds() {
    assert_reports(
        "module app;\n\
         pub type Opaque = | Bad;\n\
         pub type Pay = | Declined;\n\
         fn f() -> Int raises Opaque { raise Opaque::Bad }\n\
         pub fn g() -> Int { f()! catch { Pay::Declined => 0, _ => 1 } }\n",
        "nothing in this expression raises `Pay`",
    );
}
