//! What a `catch` handles, and what it does not.
//!
//! **A `catch` handles what its operand raises, and nothing its own arms
//! raise.** The arms were inferred inside the window the `catch` subtracts
//! from, so `inv(n)! catch { e => raise e }` erased the `InventoryError` the
//! arm sends back out: a function declaring only `PaymentError` was accepted,
//! and at run time the error reached a caller's `catch` the checker had
//! called total, which stopped the program with a trap naming a compiler bug.
//!
//! **A row's rigid tail is kept wherever two rows meet.** A closure calling
//! both a function with an open row and a parameter raising `'er` kept only
//! the first tail, so `'er` left the closure's row, the function needed no
//! `+ 'er`, and an error the caller chose escaped into a `catch` the checker
//! had called total.

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

/// Asserts one error contains every needle, and that no error prints a row
/// the way the checker spells it internally.
fn assert_reports(text: &str, needles: &[&str]) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| needles.iter().all(|n| e.contains(n))),
        "expected an error containing all of {needles:?}, got {found:?}\n{text}"
    );
    for e in &found {
        assert!(!e.contains("{}") && !e.contains("| _") && !e.contains(": Pay"), "row syntax in {e:?}");
    }
}

const TYPES: &str = "module m;\n\
    pub type InventoryError = | OutOfStock(sku: String) | WarehouseOffline;\n\
    pub type PaymentError = | Declined(reason: String) | GatewayTimeout;\n\
    pub type Pay = | Refused(reason: String) | Timeout;\n\
    fn inv(n: Int) -> String raises InventoryError {\n\
      if n == 0 { raise InventoryError::WarehouseOffline };\n\
      \"ok\"\n\
    }\n\
    fn pay(n: Int) -> String raises Pay {\n\
      if n == 0 { raise Pay::Timeout };\n\
      \"ok\"\n\
    }\n";

/// The shape that reached the trap: a binding arm that re-raises what it
/// caught, in a function that does not declare it.
#[test]
fn a_raise_in_a_binding_arm_leaves_the_catch() {
    assert_reports(
        &format!(
            "{TYPES}fn narrow(n: Int) -> String raises PaymentError {{\n\
               inv(n)! catch {{ e => raise e }}\n\
             }}\n"
        ),
        &["InventoryError"],
    );
}

/// Named arms raising the type they handle: the same rule, without the
/// binding.
#[test]
fn a_raise_in_a_named_arm_leaves_the_catch() {
    assert_reports(
        &format!(
            "{TYPES}fn narrow(n: Int) -> String raises PaymentError {{\n\
               inv(n)! catch {{\n\
                 InventoryError::OutOfStock(s) => raise InventoryError::OutOfStock(s),\n\
                 InventoryError::WarehouseOffline => \"offline\",\n\
               }}\n\
             }}\n"
        ),
        &["InventoryError"],
    );
}

/// Declared, the same function is accepted: the arm's raise is the
/// function's, not refused outright.
#[test]
fn a_raise_in_an_arm_the_function_declares_is_accepted() {
    assert_clean(&format!(
        "{TYPES}fn passes(n: Int) -> String raises InventoryError {{\n\
           inv(n)! catch {{ e => raise e }}\n\
         }}\n"
    ));
}

/// An inner arm's raise is the outer `catch`'s to handle, because it leaves
/// the inner one inside the outer one's operand.
#[test]
fn a_raise_in_an_inner_arm_is_handled_by_the_outer_catch() {
    assert_clean(&format!(
        "{TYPES}fn total(n: Int) -> String {{\n\
           (inv(n)! catch {{ e => raise e }}) catch {{\n\
             InventoryError::OutOfStock(s) => s,\n\
             InventoryError::WarehouseOffline => \"offline\",\n\
           }}\n\
         }}\n"
    ));
}

/// A closure joins an open row with `'er`: the function has to say it
/// raises `'er`, and the message says how.
#[test]
fn a_rigid_tail_joined_with_an_open_one_is_required() {
    assert_reports(
        &format!(
            "{TYPES}fn joined<'er>(g: () -> String raises 'er) -> String raises Pay {{\n\
               let f = fn () => pay(1)!;\n\
               let k = fn () => if false {{ f()! }} else {{ g()! }};\n\
               k()!\n\
             }}\n"
        ),
        &["+ 'er"],
    );
}

/// The same join inside a `catch` operand, whose named arms take `Pay` and
/// leave `'er`.
#[test]
fn a_rigid_tail_joined_inside_a_catch_operand_is_required() {
    assert_reports(
        &format!(
            "{TYPES}fn joined<'er>(g: () -> String raises 'er) -> String {{\n\
               let f = fn () => pay(1)!;\n\
               let k = fn () => if false {{ f()! }} else {{ g()! }};\n\
               k()! catch {{ Pay::Refused(r) => r, Pay::Timeout => \"t\" }}\n\
             }}\n"
        ),
        &["add `raises 'er`"],
    );
}

/// Declared, it is accepted.
#[test]
fn a_rigid_tail_the_function_declares_is_accepted() {
    assert_clean(&format!(
        "{TYPES}fn joined<'er>(g: () -> String raises 'er) -> String raises Pay + 'er {{\n\
           let f = fn () => pay(1)!;\n\
           let k = fn () => if false {{ f()! }} else {{ g()! }};\n\
           k()!\n\
         }}\n"
    ));
}

/// `'er` and a named type both undeclared: the named type is reported in
/// the ordinary words, rather than hidden behind the `+ 'er` advice.
#[test]
fn an_undeclared_type_beside_a_rigid_tail_is_still_named() {
    assert_reports(
        &format!(
            "{TYPES}fn both<'er>(g: () -> String raises 'er) -> String raises Pay {{\n\
               let k = fn () => if false {{ inv(1)! }} else {{ g()! }};\n\
               k()!\n\
             }}\n"
        ),
        &["InventoryError"],
    );
}

/// `'er` reached directly is refused with the same advice.
#[test]
fn a_rigid_tail_reached_directly_is_required() {
    assert_reports(
        &format!(
            "{TYPES}fn direct<'er>(g: () -> String raises 'er) -> String raises Pay {{ g()! }}\n"
        ),
        &["+ 'er"],
    );
}

/// A binding arm gives what it caught one type, and `'er` is any type the
/// caller picks, so there is no one type to give it: the spec's shape,
/// `err => raise err` over `f()!` and `g()!`, read an `AuditError` as a
/// `Pay` and crashed.
#[test]
fn a_binding_arm_over_a_rigid_tail_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn bound<'er>(g: () -> String raises 'er) -> String raises Pay {{\n\
               (if false {{ pay(1)! }} else {{ g()! }}) catch {{ err => raise err }}\n\
             }}\n"
        ),
        &["binds the failure", "'er"],
    );
}

/// `_` reads nothing, so it may take `'er` along with everything else.
#[test]
fn a_wildcard_arm_over_a_rigid_tail_is_accepted() {
    assert_clean(&format!(
        "{TYPES}fn swallowed<'er>(g: () -> String raises 'er) -> String {{\n\
           (if false {{ pay(1)! }} else {{ g()! }}) catch {{ _ => \"any\" }}\n\
         }}\n"
    ));
}

/// All the errors `found` holds, for a test that wants exactly one.
fn assert_one(text: &str, needles: &[&str]) {
    assert_reports(text, needles);
    let found = errors(text);
    assert_eq!(found.len(), 1, "expected one error, got {found:?}\n{text}");
}

// ---- A closure joining `'er` leaves the closures it calls as they were.
//
// `join_tails` unified every open tail beside `'er` with it, so `f`, which
// `k` calls, was typed as raising `'er` too -- even `fn () => "pure"`.
// Each of these compiled and ran on 0.4.0 and was refused.

/// `f` raises only `Pay`, so it may be passed where `raises Pay` is wanted.
#[test]
fn a_closure_called_beside_a_rigid_tail_keeps_its_own_row() {
    assert_clean(&format!(
        "{TYPES}fn takes_pay(cb: () -> String raises Pay) -> String {{\n\
           cb()! catch {{ Pay::Refused(r) => r, Pay::Timeout => \"t\" }}\n\
         }}\n\
         fn widened<'er>(g: () -> String raises 'er) -> String raises Pay + 'er {{\n\
           let f = fn () => pay(1)!;\n\
           let k = fn () => if false {{ f()! }} else {{ g()! }};\n\
           let s1 = takes_pay(f);\n\
           let s2 = k()!;\n\
           s1 + s2\n\
         }}\n"
    ));
}

/// `k`'s errors are handled with `_`, and `f`'s with its own arms; the
/// function raises nothing.
#[test]
fn a_closure_called_beside_a_rigid_tail_can_be_caught_by_its_own_type() {
    assert_clean(&format!(
        "{TYPES}fn caught<'er>(g: () -> String raises 'er) -> String {{\n\
           let f = fn () => pay(1)!;\n\
           let k = fn () => if false {{ f()! }} else {{ g()! }};\n\
           let s1 = k()! catch {{ _ => \"k\" }};\n\
           let s2 = f()! catch {{ Pay::Refused(r) => r, Pay::Timeout => \"t\" }};\n\
           s1 + s2\n\
         }}\n"
    ));
}

/// A closure that cannot fail stays one that cannot fail.
#[test]
fn an_infallible_closure_called_beside_a_rigid_tail_stays_infallible() {
    assert_clean(&format!(
        "{TYPES}fn pure<'er>(g: () -> String raises 'er) -> String {{\n\
           let f = fn () => \"pure\";\n\
           let k = fn () => f() + g()!;\n\
           let s1 = k()! catch {{ _ => \"k\" }};\n\
           let s2 = f();\n\
           s1 + s2\n\
         }}\n"
    ));
}

// ---- A binding arm over a raise typed only later.

/// The operand calls a closure whose raise is typed by the call after it:
/// at the `catch` its row was a variable, the binding was typed `Pay`, and
/// the `Audit` it raised was read as one (exit 132).
#[test]
fn a_binding_arm_over_a_closure_raising_another_type_later_is_refused() {
    assert_one(
        &format!(
            "{TYPES}pub type Audit = | Tampered(a: String);\n\
             fn use_audit(a: Audit) -> String {{ match a {{ Audit::Tampered(x) => x }} }}\n\
             fn late() -> String {{\n\
               let wrapper = fn e => {{\n\
                 let k = fn () => raise e;\n\
                 let s = (if false {{ pay(0)! }} else {{ k()! }}) catch {{\n\
                   err => match err {{ Pay::Refused(r) => r, Pay::Timeout => \"t\" }},\n\
                 }};\n\
                 s + use_audit(e)\n\
               }};\n\
               wrapper(Audit::Tampered(\"a\"))\n\
             }}\n"
        ),
        &["binds the failure as `Pay`", "`Audit`"],
    );
}

/// The same closure raising the type the binding was given is accepted.
#[test]
fn a_binding_arm_over_a_closure_raising_the_same_type_later_is_accepted() {
    assert_clean(&format!(
        "{TYPES}fn late() -> String {{\n\
           let wrapper = fn e => {{\n\
             let k = fn () => raise e;\n\
             (if false {{ pay(0)! }} else {{ k()! }}) catch {{\n\
               err => match err {{ Pay::Refused(r) => r, Pay::Timeout => \"t\" }},\n\
             }}\n\
           }};\n\
           wrapper(Pay::Timeout)\n\
         }}\n"
    ));
}

// ---- The interim refusal says what to write, once.

/// One error, naming a case to write: refusing the arm left `pay`'s `Pay`
/// unhandled, and a second error blamed `pay(1)!` in row syntax.
#[test]
fn a_refused_binding_arm_is_the_only_error() {
    assert_one(
        &format!(
            "{TYPES}fn bound<'er>(f: () -> String raises 'er) -> String raises 'er {{\n\
               (if false {{ pay(1)! }} else {{ f()! }}) catch {{ err => \"caught\" }}\n\
             }}\n"
        ),
        &["binds the failure", "`Pay::Refused(_) => ..`", "use `_`"],
    );
}

/// Where the operand raises nothing but `'er`, the message does not say
/// "also".
#[test]
fn a_binding_arm_over_only_a_rigid_tail_says_so() {
    assert_one(
        &format!(
            "{TYPES}fn only<'er>(g: () -> String raises 'er) -> String raises 'er {{\n\
               g()! catch {{ err => raise err }}\n\
             }}\n"
        ),
        &["raises only `'er`", "Use `_`"],
    );
}

// ---- Rows in messages are written as a `raises` clause writes them.

/// A named type missing from a `raises` that ends in `'er`. Printed
/// "`'er` ... cannot be assumed to be `{ Pay: Pay, | _ }`".
#[test]
fn a_named_type_missing_beside_a_rigid_tail_says_what_to_add() {
    assert_one(
        &format!(
            "{TYPES}fn named<'er>(f: () -> String raises 'er) -> String raises 'er {{ pay(1)! }}\n"
        ),
        &["`Pay`", "add `+ Pay` to this function's `raises`"],
    );
}

/// A row ending in `'e1` against a function ending in `'e2`. Printed
/// "`'e2` ... cannot be assumed to be `{ Pay: Pay, | 'e1 }`".
#[test]
fn two_rigid_tails_are_named_in_clause_words() {
    assert_one(
        &format!(
            "{TYPES}fn two<'e1, 'e2>(g: () -> String raises 'e1) -> String raises 'e2 {{\n\
               let k = fn () => if false {{ pay(1)! }} else {{ g()! }};\n\
               k()!\n\
             }}\n"
        ),
        &["`Pay`", "`'e1`", "ends in `'e2`"],
    );
}

/// A closure raising `'er` passed where nothing may be raised. Printed
/// "`'er` ... cannot be assumed to be `{}`".
#[test]
fn a_rigid_tail_against_an_empty_row_does_not_print_braces() {
    assert_one(
        &format!(
            "{TYPES}fn closed(cb: () -> String) -> String {{ cb() }}\n\
             fn passes<'er>(g: () -> String raises 'er) -> String {{ closed(g) }}\n"
        ),
        &["`'er`", "empty"],
    );
}

// ---- A raise typed later, in an arm of an inner `catch`.

/// `raise x` in the middle `catch`'s arm, `x` typed by `use_e3(x)` after
/// all three: the outer `catch` was refused for naming `E3`, "nothing in
/// this expression raises `E3`".
#[test]
fn a_raise_typed_later_in_an_inner_arm_reaches_the_outer_catch() {
    assert_clean(
        "module m;\n\
         pub type E1 = | One(s: String) | OneB;\n\
         pub type E2 = | Two(s: String) | TwoB;\n\
         pub type E3 = | Three(s: String) | ThreeB;\n\
         fn do_e1(n: Int) -> Int raises E1 { if n == 0 { raise E1::OneB }; n }\n\
         fn use_e3(x: E3) -> Int { match x { E3::Three(_) => 1, E3::ThreeB => 0 } }\n\
         fn late(n: Int) -> Int {\n\
           let k = fn x => {\n\
             let r = ((do_e1(n)! catch {\n\
               E1::One(_) => raise E2::Two(\"mid\"),\n\
               E1::OneB => 10,\n\
             }) catch {\n\
               E2::Two(_) => raise x,\n\
               E2::TwoB => 20,\n\
             }) catch {\n\
               E3::Three(_) => 3,\n\
               E3::ThreeB => 30,\n\
             };\n\
             r + use_e3(x)\n\
           };\n\
           k(E3::ThreeB)\n\
         }\n",
    );
}

// ---- Arms after a catch-all, and a guard on one.

/// `_ => .., e if c => ..` ended at a join with nothing written (exit 139).
#[test]
fn an_arm_after_a_catch_all_is_unreachable() {
    assert_one(
        &format!(
            "{TYPES}fn two() -> String {{ pay(1)! catch {{ _ => \"a\", e if true => \"b\" }} }}\n"
        ),
        &["unreachable"],
    );
}

/// A named arm after `_` is unreachable as well: code generation tries the
/// named arms first, and ran it ahead of the `_`.
#[test]
fn a_named_arm_after_a_catch_all_is_unreachable() {
    assert_one(
        &format!(
            "{TYPES}fn two() -> String {{ pay(1)! catch {{ _ => \"a\", Pay::Timeout => \"b\" }} }}\n"
        ),
        &["unreachable"],
    );
}

// ---- A guard on a catch-all arm.
//
// The first review round refused every guard on `_` or a binding arm. That
// refused ordinary code: `e if c => .., _ => ..` sends what the guard turns
// away to the `_`, as `match` does with `k if k > 0 => .., _ => ..`. Only a
// guarded catch-all with no unguarded catch-all after it is refused.

/// The director's probe: a guarded named arm, a guarded binding, then `_`.
#[test]
fn a_guarded_catch_all_before_an_unguarded_one_is_accepted() {
    assert_clean(&format!(
        "{TYPES}fn three(n: Int) -> String {{\n\
           pay(n)! catch {{\n\
             Pay::Refused(r) if n > 3 => r,\n\
             e if n == 1 => \"b\",\n\
             _ => \"c\",\n\
           }}\n\
         }}\n"
    ));
}

/// A guarded `_` may be followed by another catch-all, guarded or not.
#[test]
fn guarded_catch_alls_in_a_row_are_accepted() {
    assert_clean(&format!(
        "{TYPES}fn four(n: Int) -> String {{\n\
           pay(n)! catch {{\n\
             _ if n == 7 => \"seven\",\n\
             e if n == 8 => \"eight\",\n\
             e => \"rest\",\n\
           }}\n\
         }}\n"
    ));
}

/// A named arm after a guarded catch-all is refused, as after an unguarded
/// one: named arms are tried before the catch-all arms, so written below
/// one it would run ahead of it.
#[test]
fn a_named_arm_after_a_guarded_catch_all_is_refused() {
    assert_one(
        &format!(
            "{TYPES}fn four(n: Int) -> String {{\n\
               pay(n)! catch {{ _ if n == 7 => \"seven\", Pay::Timeout => \"t\", e => \"rest\" }}\n\
             }}\n"
        ),
        &["`Pay::Timeout`", "above"],
    );
}

/// A guarded binding followed by `_` still binds: when the guard holds, the
/// binding reads the error, so a raise typed later of another type is
/// refused as it is without the `_`. Taking the `_` as the handler would
/// accept it, and the binding would read an `Audit` as a `Pay`.
#[test]
fn a_guarded_binding_before_a_wildcard_still_types_late_raises() {
    assert_reports(
        &format!(
            "{TYPES}pub type Audit = | Tampered(a: String);\n\
             fn use_audit(a: Audit) -> String {{ match a {{ Audit::Tampered(x) => x }} }}\n\
             fn late(n: Int) -> String {{\n\
               let wrapper = fn e => {{\n\
                 let k = fn () => raise e;\n\
                 let s = (if false {{ pay(0)! }} else {{ k()! }}) catch {{\n\
                   err if n == 1 => match err {{ Pay::Refused(r) => r, Pay::Timeout => \"t\" }},\n\
                   _ => \"other\",\n\
                 }};\n\
                 s + use_audit(e)\n\
               }};\n\
               wrapper(Audit::Tampered(\"a\"))\n\
             }}\n"
        ),
        &["binds the failure as `Pay`", "`Audit`"],
    );
}

/// The last arm handles every failure only when its guard holds; the
/// `catch` has nothing to send the rest to.
#[test]
fn a_guarded_catch_all_as_the_last_arm_is_refused() {
    assert_one(
        &format!(
            "{TYPES}fn one() -> String {{ pay(1)! catch {{ e if true => \"b\" }} }}\n"
        ),
        &["guard", "add a `_` arm after it", "inside the arm"],
    );
}

/// Named arms after the guarded catch-all do not make up for the missing
/// `_`: a failure of another type, or a case they do not name, would still
/// have nowhere to go. (The named arm is also refused, for following a
/// catch-all.)
#[test]
fn a_guarded_catch_all_followed_only_by_named_arms_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn two(n: Int) -> String {{\n\
               pay(n)! catch {{ e if n == 1 => \"b\", Pay::Timeout => \"t\" }}\n\
             }}\n"
        ),
        &["guard", "add a `_` arm after it"],
    );
}

/// An arm after an *unguarded* catch-all is still unreachable, guarded or
/// not.
#[test]
fn an_arm_after_an_unguarded_catch_all_stays_unreachable() {
    assert_one(
        &format!(
            "{TYPES}fn two(n: Int) -> String {{\n\
               pay(n)! catch {{ e if n == 1 => \"a\", _ => \"b\", _ if n == 2 => \"c\" }}\n\
             }}\n"
        ),
        &["unreachable"],
    );
}
