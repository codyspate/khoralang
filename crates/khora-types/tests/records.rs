//! Record types, literals and field access.
//!
//! Records exist because effects need them: a capability is a record of
//! closures, and `ledger.get_history(id)` is a field read followed by a call.
//! `docs/design/effects.md` says the dependency-injection model is "a record of
//! function types", so this is that record.

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

const POINT: &str = "module m;\npub type Point = { x: Int, y: Int };\n";

#[test]
fn a_field_has_the_type_it_was_declared_with() {
    assert_clean(&format!("{POINT}fn f(p: Point) -> Int {{ p.x + p.y }}\n"));
    assert_reports(&format!("{POINT}fn f(p: Point) -> Bool {{ p.x }}\n"), "returns `Bool`");
}

#[test]
fn a_field_that_does_not_exist_is_reported() {
    assert_reports(&format!("{POINT}fn f(p: Point) -> Int {{ p.z }}\n"), "`Point` has no field `z`");
}

/// A literal is nominal, like everything else: it is *some declared record*,
/// found by its labels rather than being a structural type of its own.
#[test]
fn a_literal_is_found_by_its_labels() {
    assert_clean(&format!("{POINT}fn f() -> Point {{ {{ x: 1, y: 2 }} }}\n"));
}

/// Order written need not match order declared.
#[test]
fn the_order_written_does_not_matter() {
    assert_clean(&format!("{POINT}fn f() -> Point {{ {{ y: 2, x: 1 }} }}\n"));
}

#[test]
fn a_field_of_the_wrong_type_is_reported() {
    assert_reports(
        &format!("{POINT}fn f() -> Point {{ {{ x: true, y: 2 }} }}\n"),
        "field `x`: expected `Int`, found `Bool`",
    );
}

#[test]
fn a_missing_field_is_reported() {
    assert_reports(
        &format!("{POINT}fn f() -> Point {{ {{ x: 1 }} }}\n"),
        "this `Point` is missing `y`",
    );
}

#[test]
fn a_field_the_record_does_not_have_is_reported() {
    assert_reports(
        &format!("{POINT}fn f() -> Point {{ {{ x: 1, y: 2, z: 3 }} }}\n"),
        "no record type has exactly the fields",
    );
}

/// Two records with the same labels cannot be told apart from the literal
/// alone, and saying so beats guessing.
#[test]
fn an_ambiguous_literal_asks_which_type_it_is() {
    assert_reports(
        "module m;\n\
         pub type A = { v: Int };\n\
         pub type B = { v: Int };\n\
         fn f() -> A { { v: 1 } }\n",
        "say which",
    );
}

/// A record's fields are declared against its own parameters, so a literal
/// decides them.
#[test]
fn a_generic_record_takes_its_argument_from_the_literal() {
    assert_clean(
        "module m;\n\
         pub type Wrapper<A> = { value: A };\n\
         fn f() -> Wrapper<Int> { { value: 1 } }\n\
         fn g(w: Wrapper<Bool>) -> Bool { w.value }\n",
    );
    assert_reports(
        "module m;\n\
         pub type Wrapper<A> = { value: A };\n\
         fn f() -> Wrapper<Bool> { { value: 1 } }\n",
        "returns `Wrapper<Bool>`",
    );
}

/// A sum type's fields are not reachable without matching: which variant is
/// present is not known until it is.
#[test]
fn a_sum_types_payload_is_not_a_field() {
    assert_reports(
        "module m;\n\
         pub type Shape = | Circle(radius: Int) | Square(side: Int);\n\
         fn f(s: Shape) -> Int { s.radius }\n",
        "has no field `radius`",
    );
}

/// The shape effects need: a record whose fields are functions, read and
/// called.
#[test]
fn a_record_of_functions_is_callable_through_its_fields() {
    assert_clean(
        "module m;\n\
         pub type Ledger = { get: (Int) -> Int, flag: (Int) -> Bool };\n\
         fn use_it(l: Ledger) -> Int { l.get(1) }\n\
         fn make() -> Ledger { { get: fn i => i + 1, flag: fn i => i == 0 } }\n",
    );
}

// --- mutable fields --------------------------------------------------------

const MUT: &str = "module m;\n\
                   pub type Counter = { mut count: Int, name: String };\n\
                   pub type Frozen = { total: Int };\n\
                   pub type Fiber<A, 'r>;\n\
                   impl<A, 'r> Fiber<A, 'r> {\n\
                     fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;\n\
                   }\n\
                   pub fn nothing() -> () { }\n";

#[test]
fn a_mut_field_can_be_assigned() {
    assert_clean(&format!(
        "{MUT}pub fn f(c: Counter) -> () {{ c.count = 1; }}\n"
    ));
}

/// The default. A field is written only where the declaration says it may be,
/// which is what makes `is_shareable` mean anything.
#[test]
fn a_field_that_is_not_mut_cannot_be_assigned() {
    assert_reports(
        &format!("{MUT}pub fn f(c: Counter) -> () {{ c.name = \"x\"; }}\n"),
        "`Counter` does not declare `mut`",
    );
}

// --- what may cross into a fiber -------------------------------------------

/// The rule. Two fibers writing one value is a race, and atomic reference
/// counts do not help — they protect the count, not the fields.
#[test]
fn a_mutable_value_cannot_be_handed_to_a_fiber() {
    assert_reports(
        &format!(
            "{MUT}\
             pub fn bump(c: Counter) -> () {{ c.count = 1; }}\n\
             pub fn f(c: Counter) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => bump(c)) }}\n"
        ),
        "cannot be handed to another fiber",
    );
}

/// An immutable value crosses freely, which is what refcount atomicity bought.
#[test]
fn an_immutable_value_can_be_handed_to_a_fiber() {
    assert_clean(&format!(
        "{MUT}\
         pub fn look(v: Frozen) -> () {{ }}\n\
         pub fn f(v: Frozen) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(v)) }}\n"
    ));
}

/// A named function captures nothing, so there is nothing to check.
#[test]
fn a_named_function_can_be_handed_to_a_fiber() {
    assert_clean(&format!("{MUT}pub fn f() -> Fiber<(), {{}}> {{ Fiber::spawn(nothing) }}\n"));
}

/// Transitive: holding a mutable value is as unshareable as being one.
#[test]
fn holding_a_mutable_value_is_unshareable_too() {
    assert_reports(
        &format!(
            "{MUT}\
             pub type Holder = {{ inner: Counter }};\n\
             pub fn look(h: Holder) -> () {{ }}\n\
             pub fn f(h: Holder) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(h)) }}\n"
        ),
        "cannot be handed to another fiber",
    );
}

/// A thunk that cannot be seen cannot be checked, so it is refused rather than
/// waved through. That also makes the rule worth having on its own: a fiber's
/// body is written where it starts.
#[test]
fn a_forwarded_thunk_cannot_be_spawned() {
    assert_reports(
        &format!(
            "{MUT}pub fn f(body: () -> ()) -> Fiber<(), {{}}> {{ Fiber::spawn(body) }}\n"
        ),
        "a closure written here or a named function",
    );
}

// --- shareability ----------------------------------------------------------
//
// `docs/design/sharing.md`. Fibers are operating-system threads, so each of
// these is about a real data race rather than a future one.

/// A type declared with no body is refused by default, because nothing here can
/// see whether it can be written — and `Array` can, through `Array::set`.
///
/// This compiled and raced before the rule existed: two fibers writing one
/// array, no diagnostic anywhere.
#[test]
fn an_opaque_type_cannot_cross_without_saying_so() {
    assert_reports(
        &format!(
            "{MUT}\
             pub type Buffer;\n\
             pub fn look(b: Buffer) -> () {{ }}\n\
             pub fn f(b: Buffer) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(b)) }}\n"
        ),
        "declared without a body",
    );
}

/// And accepted once it does. The impl is the author asserting what the
/// compiler cannot check, which is why it may only be written here.
#[test]
fn an_opaque_type_crosses_once_it_says_so() {
    assert_clean(&format!(
        "{MUT}\
         pub type Buffer;\n\
         pub trait Share {{}}\n\
         impl Share for Buffer {{}}\n\
         pub fn look(b: Buffer) -> () {{ }}\n\
         pub fn f(b: Buffer) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(b)) }}\n"
    ));
}

/// `Share` asserts; it does not describe. For a type this compiler can see
/// into it decides for itself, and an impl would be a way to write down a lie
/// about a record with a `mut` field.
#[test]
fn share_cannot_be_claimed_for_a_type_the_compiler_can_see() {
    assert_reports(
        &format!("{MUT}pub trait Share {{}}\nimpl Share for Counter {{}}\n"),
        "cannot be implemented for `Counter`",
    );
}

/// A type the caller chooses could be anything, so a generic function that
/// spawns has to require it. Without this a two-line wrapper laundered a
/// caller's mutable record onto another fiber.
#[test]
fn a_type_parameter_has_to_be_required_to_be_shareable() {
    assert_reports(
        &format!(
            "{MUT}\
             pub fn sink<A>(a: A) -> () {{ }}\n\
             pub fn launder<A>(a: A) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => sink(a)) }}\n"
        ),
        "is a type the caller chooses",
    );
}

/// With the bound written it crosses, and the bound is what the caller then has
/// to satisfy.
#[test]
fn a_bounded_type_parameter_crosses() {
    assert_clean(&format!(
        "{MUT}\
         pub trait Share {{}}\n\
         pub fn sink<A>(a: A) -> () {{ }}\n\
         pub fn launder<A: Share>(a: A) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => sink(a)) }}\n"
    ));
}

/// Nobody writes `impl Share` for a record: a structure this compiler can see
/// is shareable exactly when everything in it is, so the bound is satisfied by
/// looking rather than by finding an impl.
#[test]
fn a_share_bound_is_satisfied_structurally() {
    assert_clean(&format!(
        "{MUT}\
         pub trait Share {{}}\n\
         pub fn sink<A>(a: A) -> () {{ }}\n\
         pub fn launder<A: Share>(a: A) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => sink(a)) }}\n\
         pub fn go(v: Frozen) -> Fiber<(), {{}}> {{ launder(v) }}\n"
    ));
}

#[test]
fn a_share_bound_is_not_satisfied_by_a_mutable_record() {
    assert_reports(
        &format!(
            "{MUT}\
             pub trait Share {{}}\n\
             pub fn sink<A>(a: A) -> () {{ }}\n\
             pub fn launder<A: Share>(a: A) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => sink(a)) }}\n\
             pub fn go(c: Counter) -> Fiber<(), {{}}> {{ launder(c) }}\n"
        ),
        "`Counter` does not implement `Share`",
    );
}

/// A generic container follows its argument. The declared field types speak in
/// the *type's* parameters, and reading those as the caller's made every
/// `List` unshareable.
#[test]
fn a_generic_container_follows_its_argument() {
    assert_clean(&format!(
        "{MUT}\
         pub type Stack<A> = | Empty | Push(A, Stack<A>);\n\
         pub fn look(s: Stack<Frozen>) -> () {{ }}\n\
         pub fn f(s: Stack<Frozen>) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(s)) }}\n"
    ));
}

#[test]
fn a_generic_container_of_something_mutable_does_not() {
    assert_reports(
        &format!(
            "{MUT}\
             pub type Stack<A> = | Empty | Push(A, Stack<A>);\n\
             pub fn look(s: Stack<Counter>) -> () {{ }}\n\
             pub fn f(s: Stack<Counter>) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(s)) }}\n"
        ),
        "cannot be handed to another fiber",
    );
}

// --- handlers --------------------------------------------------------------

const EFFECTS: &str = "module m;\n\
                       pub type Counter = { mut count: Int, name: String };\n\
                       pub type Frozen = { total: Int };\n\
                       pub type Fiber<A, 'r>;\n\
                       impl<A, 'r> Fiber<A, 'r> {\n\
                         fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;\n\
                       }\n\
                       pub effect Counting { tick: () -> (), }\n\
                       pub fn bump(c: Counter) -> () { c.count = 1; }\n\
                       pub fn peek(v: Frozen) -> () { }\n";

/// An effect is shareable, and this is what pays for it: the captures are
/// checked at the `handler for` literal, the one place they are visible.
#[test]
fn a_handler_capturing_something_mutable_is_refused() {
    assert_reports(
        &format!(
            "{EFFECTS}pub fn make(c: Counter) -> Counting {{ \
             handler for Counting {{ tick: fn () => bump(c) }} }}\n"
        ),
        "has to be safe to hand to another fiber",
    );
}

#[test]
fn a_handler_capturing_something_immutable_is_accepted() {
    assert_clean(&format!(
        "{EFFECTS}pub fn make(v: Frozen) -> Counting {{ \
         handler for Counting {{ tick: fn () => peek(v) }} }}\n"
    ));
}

/// A handler may capture a capability, which is the case the exception exists
/// for: a fiber answering a request needs the database its handler holds.
#[test]
fn a_handler_may_capture_another_handler() {
    assert_clean(&format!(
        "{EFFECTS}\
         pub effect Logging {{ note: () -> (), }}\n\
         pub fn make(inner: Counting) -> Logging {{ \
         handler for Logging {{ note: fn () => inner.tick() }} }}\n"
    ));
}

/// The laundering move: write the closure somewhere else, then name it. What it
/// captured went with it, so there is nothing at this line to look at — and the
/// whole exception rests on this line being the one that cannot be dodged.
#[test]
fn a_pre_bound_closure_cannot_be_a_handler_operation() {
    assert_reports(
        &format!(
            "{EFFECTS}pub fn make(c: Counter) -> Counting {{ \
             let leak = fn () => bump(c); \
             handler for Counting {{ tick: leak }} }}\n"
        ),
        "has to be a closure written here or a named function",
    );
}

/// And a capability crosses, which is the whole point of the exception.
#[test]
fn a_capability_can_be_handed_to_a_fiber() {
    assert_clean(&format!(
        "{EFFECTS}\
         pub fn use_it(c: Counting) -> () {{ c.tick() }}\n\
         pub fn f(c: Counting) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => use_it(c)) }}\n"
    ));
}

/// `Share` is a trusted assertion, so it has to be unforgeable.
///
/// It was not: the compiler recognised any trait spelled `Share`, and any file
/// could implement it for any opaque type. Declare a trait of your own, write
/// `impl<A> Share for Array<A>`, and an array — which `Array::set` writes —
/// became something two fibers may hold. It compiled, and it raced.
///
/// The author of a type is the only one who knows what the compiler cannot, so
/// they are the only one who may say.
#[test]
fn share_cannot_be_claimed_for_a_type_from_another_module() {
    let found = errors(
        "module m;
         import std::core::{Array};
         trait Share {}
         impl<A> Share for Array<A> {}
        ",
    );
    assert!(
        found.iter().any(|e| e.contains("only the module that declares a type")),
        "expected the orphan rule to refuse it, got {found:?}"
    );
}

/// And the module that does declare it may, which is what `std::core` does for
/// the handful of runtime types that take a lock.
#[test]
fn share_can_be_claimed_where_the_type_is_declared() {
    assert_clean(
        "module m;
         pub trait Share {}
         pub type Handle;
         impl Share for Handle {}
        ",
    );
}

// --- what may go in a cell -------------------------------------------------
//
// `docs/design/shared.md`. A `Shared<A>` is shareable because nothing
// unshareable can go in or come out — which is the whole soundness argument,
// and is enforced by the `A: Share` bound rather than by anything clever.

const CELLS: &str = "module m;
                     pub trait Share {}
                     pub type Counter = { mut count: Int };
                     pub type Frozen = { total: Int };
                     pub type Shared<A>;
                     impl<A> Share for Shared<A> {}
                     impl<A: Share> Shared<A> {
                       fn of(value: A) -> Shared<A>;
                       fn get(self) -> A;
                     }
";

#[test]
fn a_cell_holds_something_shareable() {
    assert_clean(&format!("{CELLS}pub fn go(v: Frozen) -> Shared<Frozen> {{ Shared::of(v) }}\n"));
}

/// The bound is the argument. A cell of something writable would hand two
/// fibers the same mutable record with the lock protecting nothing that
/// matters — the pointer swap, not the fields behind it.
#[test]
fn a_cell_cannot_hold_something_writable() {
    assert_reports(
        &format!("{CELLS}pub fn go(c: Counter) -> Shared<Counter> {{ Shared::of(c) }}\n"),
        "`Counter` does not implement `Share`",
    );
}

/// And the cell itself crosses, which is the point of the whole exercise.
#[test]
fn a_cell_can_be_handed_to_a_fiber() {
    assert_clean(&format!(
        "{CELLS}\
         pub type Fiber<A, 'r>;\n\
         impl<A, 'r> Fiber<A, 'r> {{ fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>; }}\n\
         pub fn look(c: Shared<Frozen>) -> () {{ }}\n\
         pub fn go(c: Shared<Frozen>) -> Fiber<(), {{}}> {{ Fiber::spawn(fn () => look(c)) }}\n"
    ));
}

/// A handler may capture one, which is what makes a stateful test double
/// writable again after the sharing rules refused a `mut` record.
#[test]
fn a_handler_may_capture_a_cell() {
    assert_clean(&format!(
        "{CELLS}\
         pub effect Counting {{ tick: () -> (), }}\n\
         pub fn peek(c: Shared<Frozen>) -> () {{ }}\n\
         pub fn make(c: Shared<Frozen>) -> Counting {{ \
         handler for Counting {{ tick: fn () => peek(c) }} }}\n"
    ));
}
