//! Who may assert that two fibers can hold a value, and for what.
//!
//! `Share` asserts rather than provides: everything downstream trusts it
//! without being able to check it. `docs/design/sharing.md` therefore allows
//! the impl in exactly two places — a type declared with no body, and one whose
//! only obstacle is a `Ptr` — and only in the module that declares the type.
//!
//! The pointer case is the newer half and the reason is worth keeping straight.
//! A `mut` field is something the compiler can *see*, so an assertion over it
//! would be overriding knowledge. Foreign memory is the opposite: the refusal
//! is a conservative default rather than a finding, and the module that put the
//! pointer across the ABI is the only thing that knows what is behind it.
//! `std::net::tls` is the case that needed it.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

const HEAD: &str = "module m;\npub trait Share {}\n";

/// A type with no body: nothing to look at, so the module says.
#[test]
fn an_opaque_type_may_be_vouched_for() {
    let found = errors(&format!("{HEAD}pub type Handle;\nimpl Share for Handle {{}}\n"));
    assert!(found.is_empty(), "an opaque type should be vouchable: {found:?}");
}

/// **The pointer case.** Foreign memory the compiler cannot judge.
#[test]
fn a_type_blocked_only_by_a_pointer_may_be_vouched_for() {
    let found = errors(&format!(
        "{HEAD}pub type Settings = {{ inner: Ptr }};\nimpl Share for Settings {{}}\n"
    ));
    assert!(found.is_empty(), "a pointer is what the module is vouching for: {found:?}");
}

/// **A `mut` field still refuses**, and this is the case the rule exists for:
/// an assertion here would hand two fibers a value they can both write.
#[test]
fn a_mutable_field_may_not_be_vouched_for() {
    let found = errors(&format!(
        "{HEAD}pub type Counter = {{ mut count: Int, inner: Ptr }};\n\
         impl Share for Counter {{}}\n"
    ));
    assert!(
        found.iter().any(|m| m.contains("cannot be implemented")),
        "a `mut` field must still refuse the assertion: {found:?}"
    );
}

/// A closure field refuses too: it can have captured anything.
#[test]
fn a_function_field_may_not_be_vouched_for() {
    let found = errors(&format!(
        "{HEAD}pub type Callback = {{ run: (Int) -> Int }};\nimpl Share for Callback {{}}\n"
    ));
    assert!(
        found.iter().any(|m| m.contains("cannot be implemented")),
        "a closure field must still refuse the assertion: {found:?}"
    );
}

/// A type that needs no assertion does not get one. The compiler can see that
/// two `Int`s are fine, so an impl is either redundant or a lie.
#[test]
fn an_ordinary_record_may_not_be_vouched_for() {
    let found = errors(&format!(
        "{HEAD}pub type Point = {{ x: Int, y: Int }};\nimpl Share for Point {{}}\n"
    ));
    assert!(
        found.iter().any(|m| m.contains("cannot be implemented")),
        "an ordinary record decides for itself: {found:?}"
    );
}

/// **The vouch is honoured**, not merely permitted: a value of a vouched type
/// crosses into a fiber.
///
/// Without this the impl would be accepted and then ignored, which is the shape
/// the pointer case had before `shareable` learned to consult it.
#[test]
fn a_vouched_value_may_cross_into_a_fiber() {
    let found = errors(
        "module m;
pub trait Share {}
pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> { fn spawn(body: () -> ()) -> Fiber; }

pub type Settings = { inner: Ptr };
impl Share for Settings {}

fn use_it(s: Settings) -> () {}

fn go(settings: Settings) -> Fiber {
  Fiber::spawn(fn () => use_it(settings))
}
",
    );
    assert!(found.is_empty(), "a vouched value should be spawnable: {found:?}");
}

/// And one that is not vouched for still cannot.
#[test]
fn an_unvouched_pointer_may_not_cross_into_a_fiber() {
    let found = errors(
        "module m;
pub trait Share {}
pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> { fn spawn(body: () -> ()) -> Fiber; }

pub type Settings = { inner: Ptr };

fn use_it(s: Settings) -> () {}

fn go(settings: Settings) -> Fiber {
  Fiber::spawn(fn () => use_it(settings))
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("cannot be handed to another fiber")),
        "an unvouched pointer must still be refused: {found:?}"
    );
}
