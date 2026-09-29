//! A `Region` or `Scope` stays on the fiber that opened it.
//!
//! **What these guard: a finalizer run on a fiber other than the one that
//! deferred it.** A finalizer's captures need not be `Share` -- `acquire` of a
//! connection with `mut` fields is what one is for -- so a region that reached
//! a second fiber let that fiber run and release a record the first was still
//! writing. Each program below is a route a region or scope could take to
//! another fiber, and each must be refused with a message that names the
//! rewrite, `scoped`, inside the child.
//!
//! The programs are the design round's probes (`race_*`, `implicit`, and the
//! routes of `routes`), each of which compiled and ran on 1bd3ce4.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

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

/// Every diagnostic from one program compiled together with `std`, which is
/// where `Region`, `Scope` and `scoped` are declared.
fn errors_with_std(program: &str) -> Vec<String> {
    let std_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut paths = Vec::new();
    sources(&std_dir, &mut paths);
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

/// Requires a refusal that mentions `about` and names `scoped` as the fix.
fn refused(program: &str, about: &str) {
    let found = errors_with_std(program);
    assert!(
        found.iter().any(|m| m.contains(about) && m.contains("scoped")),
        "expected a refusal about {about} naming `scoped`, got {found:#?}"
    );
}

/// **The implicit route.** A spawn lambda written inside a function that has
/// `scope` picks it up without naming it, and the child acquires into its
/// parent's region.
#[test]
fn a_spawn_lambda_cannot_pick_up_its_parents_scope() {
    refused(
        "module main;

import std::core::{print, Fiber, Scope, scoped, acquire};

type Conn = { name: String, mut uses: Int };

fn work() -> Int with { scope: Scope } {
  let c = acquire({ name: \"c\", uses: 0 }, fn c => print(\"closed ${c.name} ${c.uses}\"));
  c.uses = c.uses + 1;
  c.uses
}

fn implicit() -> Int with { scope: Scope } {
  let f = Fiber::spawn(fn () => work());
  Fiber::join(f)
}

pub fn main() -> Int {
  let a = scoped(fn () => implicit());
  print(\"a ${a}\");
  0
}
",
        "`scope` cannot be handed to another fiber",
    );
}

/// **The documented pattern, with a nursery outside the `scoped`.** The
/// scope ends while the child still writes the connection it deferred.
#[test]
fn a_child_adopted_outside_scoped_cannot_defer_into_its_scope() {
    refused(
        "module main;

import std::core::{print, Fiber, Scope, List, scoped, nursery, Nursery, ChildFailed, Channel};

type Conn = { name: String, mut uses: Int };

fn child(ready: Channel<Int>, go: Channel<Int>) -> () with { scope: Scope } {
  let c: Conn = { name: \"c\", uses: 0 };
  scope.defer(fn () => print(\"closing ${c.name} after ${c.uses} uses\"));
  Channel::send(ready, 1);
  let _ = Channel::receive(go);
  c.uses = c.uses + 1;
}

fn opens(ready: Channel<Int>, go: Channel<Int>) -> () with { scope: Scope, nursery: Nursery } {
  nursery.adopt(Fiber::spawn(fn () => child(ready, go)));
  let _ = Channel::receive(ready);
}

fn outer(ready: Channel<Int>, go: Channel<Int>) -> () with { nursery: Nursery } {
  scoped(fn () => opens(ready, go));
  Channel::send(go, 1);
  ()
}

pub fn main() -> Int raises ChildFailed {
  let ready: Channel<Int> = Channel::bounded(1);
  let go: Channel<Int> = Channel::bounded(1);
  nursery(fn () => outer(ready, go))!;
  0
}
",
        "`scope` cannot be handed to another fiber",
    );
}

/// **A scope bound to a local and installed in the child** with `with`.
#[test]
fn a_scope_bound_to_a_local_cannot_be_installed_in_a_child() {
    refused(
        "module main;

import std::core::{print, Fiber, Scope, scoped, acquire};

type Conn = { name: String, mut uses: Int };

fn take() -> Conn with { scope: Scope } {
  acquire({ name: \"c\", uses: 0 }, fn c => print(\"closing ${c.name} after ${c.uses} uses\"))
}

fn opens() -> Fiber<Int, Never> with { scope: Scope } {
  let s = scope;
  Fiber::spawn(fn () => {
    let c = with { scope: s } { take() };
    c.uses = c.uses + 1;
    c.uses
  })
}

pub fn main() -> Int {
  let f = scoped(fn () => opens());
  0
}
",
        "`s` cannot be handed to another fiber",
    );
}

/// **A region sent over a channel.** The generic bound message, which has to
/// say why a `Region` in particular is refused.
#[test]
fn a_region_cannot_travel_through_a_channel() {
    refused(
        "module main;

import std::core::{print, Fiber, Region, Channel, Option};

fn child(regions: Channel<Region>) -> Int {
  match Channel::receive(regions) {
    Option::Some(r) => Region::defer(r, fn () => print(\"finalizer\")),
    Option::None => (),
  };
  0
}

pub fn main() -> Int {
  let regions: Channel<Region> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => child(regions));
  let r = Region::open();
  Channel::send(regions, r);
  let _ = Fiber::join(f);
  0
}
",
        "`Region` does not implement `Share`",
    );
}

/// **A region put in a `Shared` cell**, which any fiber could read out.
#[test]
fn a_region_cannot_go_in_a_shared_cell() {
    refused(
        "module main;

import std::core::{Region, Shared};

pub fn main() -> Int {
  let r = Region::open();
  let c = Shared::of(r);
  0
}
",
        "`Region` does not implement `Share`, which `Shared::of` requires",
    );
}

/// **A certified closure capturing a region.**
#[test]
fn a_certified_closure_cannot_capture_a_region() {
    refused(
        "module main;

import std::core::{print, Region, SharedFn};

pub fn main() -> Int {
  let r = Region::open();
  let f = SharedFn::of(fn (x: Int) => Region::defer(r, fn () => print(\"x\")));
  0
}
",
        "`r` cannot be handed to another fiber",
    );
}

/// **A fiber answering a record that holds a region.** The record is the
/// type the bound names, so the sentence has to find the region inside it.
#[test]
fn a_fiber_cannot_answer_a_record_holding_a_region() {
    refused(
        "module main;

import std::core::{Fiber, Region};

type Holds = { r: Region };

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => { let h: Holds = { r: Region::open() }; h });
  0
}
",
        "`Holds` does not implement `Share`",
    );
}

/// **A region in a fiber's raised error.** The child raises a record holding
/// a region it deferred into; the parent catches it, and its drop runs the
/// child's finalizer on the parent. A fiber's error crosses as its answer
/// does. `raise_region.kh` from the review.
#[test]
fn a_fiber_cannot_raise_a_region() {
    refused(
        "module main;

import std::core::{print, Fiber, Region, List};

type H = { mut n: Int, mut xs: List<String> };
type Oops = { why: String, r: Region };

fn child() -> Int raises Oops {
  let h: H = { n: 0, xs: List::Nil };
  let r = Region::open();
  Region::defer(r, fn () => print(\"finalizer sees n=${h.n}\"));
  h.n = 1;
  raise { why: \"carrying the region\", r: r }
}

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => child()!);
  let got = Fiber::join(f)! catch {
    Oops { why, r } => { print(\"parent caught: ${why}\"); 7 },
  };
  print(\"parent after catch ${got}\");
  0
}
",
        "`Oops`, which this fiber can raise, cannot be handed to another fiber",
    );
}

/// **The same through `Fiber::outcome`.** `outcome_region.kh`.
#[test]
fn a_fiber_cannot_raise_a_region_to_outcome() {
    refused(
        "module main;

import std::core::{print, Fiber, Region, Outcome};

type Oops = { why: String, r: Region };

fn child() -> Int raises Oops {
  let r = Region::open();
  Region::defer(r, fn () => print(\"finalizer\"));
  raise { why: \"out\", r: r }
}

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => child()!);
  let o = Fiber::outcome(f)! catch { Oops { why, r } => Outcome::Stopped };
  match o { Outcome::Answered(v) => print(\"answered ${v}\"), Outcome::Stopped => print(\"stopped\") };
  0
}
",
        "`Oops`, which this fiber can raise, cannot be handed to another fiber",
    );
}

/// **A caught region deferred into by the parent**, which the runtime would
/// otherwise stop at the defer. `raise_then_defer.kh`.
#[test]
fn a_parent_cannot_defer_into_a_region_its_child_raised() {
    refused(
        "module main;

import std::core::{print, Fiber, Region};

type H = { mut n: Int };
type Oops = { r: Region };

fn child() -> Int raises Oops {
  raise { r: Region::open() }
}

pub fn main() -> Int {
  let h: H = { n: 0 };
  let f = Fiber::spawn(fn () => child()!);
  let got = Fiber::join(f)! catch {
    Oops { r } => { Region::defer(r, fn () => print(\"parent's finalizer n=${h.n}\")); 1 },
  };
  h.n = 3;
  print(\"got ${got}\");
  0
}
",
        "`Oops`, which this fiber can raise, cannot be handed to another fiber",
    );
}

/// **A `mut` record in a fiber's error is not refused here.** The runtime
/// marks an error at the handover, so only a region or scope, whose
/// finalizers would move with it, is this rule's business.
#[test]
fn a_fiber_may_still_raise_a_mutable_record() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Fiber};

type Oops = { mut n: Int };

fn child() -> Int raises Oops { raise { n: 1 } }

pub fn main() -> Int {
  let f = Fiber::spawn(fn () => child()!);
  Fiber::join(f)! catch { Oops { n } => n }
}
",
    );
    assert!(found.is_empty(), "a `mut` error is refused: {found:#?}");
}

/// **The same through a named function** handed to `spawn`, which captures
/// nothing but raises what it raises.
#[test]
fn a_named_function_cannot_raise_a_region_across() {
    refused(
        "module main;

import std::core::{Fiber, Region};

type Oops = { r: Region };

fn child() -> Int raises Oops { raise { r: Region::open() } }

pub fn main() -> Int {
  let f = Fiber::spawn(child);
  Fiber::join(f)! catch { Oops { r } => 1 }
}
",
        "`Oops`, which this fiber can raise, cannot be handed to another fiber",
    );
}

/// **The lambda spelling of the rewrite compiles inside a function that has
/// `scope`**, joined and adopted, and so does a lambda inside that lambda.
/// The `scope` that `scoped` hands its lambda shadows the enclosing
/// function's, so the child uses its own and nothing crosses. Resolved to
/// the enclosing one, the child captured its parent's scope, and this was
/// refused.
#[test]
fn the_lambda_rewrite_compiles_inside_a_scope() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Fiber, Scope, Nursery, scoped, nursery, acquire, ChildFailed};

fn work(name: String) -> Int with { scope: Scope } {
  let _ = acquire(1, fn n => print(\"released ${name}\"));
  1
}

fn twice(f: () -> Int with { scope: Scope }) -> Int with { scope: Scope } { f() + f() }

fn run() -> Int with { scope: Scope, nursery: Nursery } {
  let f = Fiber::spawn(fn () => scoped(fn () => work(\"joined\")));
  let g = Fiber::spawn(fn () => scoped(fn () => twice(fn () => work(\"nested\"))));
  nursery.adopt(Fiber::spawn(fn () => scoped(fn () => { let _ = work(\"adopted\"); () })));
  Fiber::join(f) + Fiber::join(g)
}

pub fn main() -> Int raises ChildFailed {
  scoped(fn () => nursery(fn () => run())!)!
}
",
    );
    assert!(found.is_empty(), "the lambda spelling is refused inside a scope: {found:#?}");
}

/// **A lambda that names its parent's scope is still refused**, inside
/// `scoped` or not. A name written in the lambda is the binding of that name
/// where the lambda is written: `outer`, and `scope` itself, are `run`'s
/// scope, so the child would defer into its parent's region. What `scoped`
/// hands its lambda shadows only the capability the body leaves unnamed.
#[test]
fn a_lambda_naming_its_parents_scope_is_refused_inside_scoped() {
    refused(
        "module main;

import std::core::{print, Fiber, Scope, scoped};

fn run() -> Int with { scope: Scope } {
  let outer = scope;
  let f = Fiber::spawn(fn () => scoped(fn () => { outer.defer(fn () => print(\"outer\")); 1 }));
  Fiber::join(f)
}

pub fn main() -> Int { scoped(run) }
",
        "`outer` cannot be handed to another fiber",
    );
    refused(
        "module main;

import std::core::{print, Fiber, Scope, scoped};

fn run() -> Int with { scope: Scope } {
  let f = Fiber::spawn(fn () => scoped(fn () => { scope.defer(fn () => print(\"named\")); 1 }));
  Fiber::join(f)
}

pub fn main() -> Int { scoped(run) }
",
        "`scope` cannot be handed to another fiber",
    );
}

/// **The spellings the refusals name compile inside a function that has
/// `scope`**, joined and adopted: `scoped` handed a named function, and a
/// named function whose body opens `scoped`.
#[test]
fn the_named_rewrites_compile_inside_a_scope() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Fiber, Scope, Nursery, scoped, nursery, acquire, ChildFailed};

fn work(name: String) -> Int with { scope: Scope } {
  let _ = acquire(1, fn n => print(\"released ${name}\"));
  1
}

fn joined_work() -> Int with { scope: Scope } { work(\"joined\") }
fn adopted_work() -> () with { scope: Scope } { let _ = work(\"adopted\"); () }
fn opens_its_own() -> Int { scoped(fn () => work(\"helper\")) }

fn run() -> Int with { scope: Scope, nursery: Nursery } {
  let f = Fiber::spawn(fn () => scoped(joined_work));
  let g = Fiber::spawn(fn () => opens_its_own());
  nursery.adopt(Fiber::spawn(fn () => scoped(adopted_work)));
  Fiber::join(f) + Fiber::join(g)
}

pub fn main() -> Int raises ChildFailed {
  scoped(fn () => nursery(fn () => run())!)!
}
",
    );
    assert!(found.is_empty(), "a spelling the refusals name is refused: {found:#?}");
}

/// **A user's own type called `Region` or `Scope` is held to the rule by
/// name**, and the refusal says so, so its author can see why a record of
/// theirs is said to stay on a fiber.
#[test]
fn a_users_own_region_is_refused_as_stds() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Fiber};

type Region = { held: String, count: Int };

pub effect Scope { note: (String) -> () }

fn show(r: Region) -> Int { r.count }

pub fn main() -> Int {
  let r: Region = { held: \"kept\", count: 41 };
  let f = Fiber::spawn(fn () => show(r));
  let h = handler for Scope { note: fn m => print(m) };
  let g = Fiber::spawn(fn () => { let _ = h; 1 });
  Fiber::join(f) + Fiber::join(g)
}
",
    );
    for name in ["Region", "Scope"] {
        assert!(
            found.iter().any(|m| m.contains(&format!("std's `{name}`"))),
            "the refusal of a user's own `{name}` does not say it was taken for std's: {found:#?}"
        );
    }
}

/// **std's own `Region` is not called \"std's\"**: the qualifier is for the
/// case where the name is all that matched.
#[test]
fn stds_region_is_refused_plainly() {
    let found = errors_with_std(
        "module main;

import std::core::{Fiber, Region};

pub fn main() -> Int {
  let r = Region::open();
  let f = Fiber::spawn(fn () => { let _ = r; 1 });
  Fiber::join(f)
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("a `Region` stays on the fiber")) && !found.iter().any(|m| m.contains("std's")),
        "{found:#?}"
    );
}

/// **A handler for another effect that captures `scope`.** A handler is
/// shareable because what its operations capture is checked where it is
/// written, and a `Scope` is not something it may capture.
#[test]
fn a_handler_capturing_a_scope_is_refused() {
    refused(
        "module main;

import std::core::{print, Scope};

pub effect Log { write: (String) -> () }

fn logs() -> () with { scope: Scope } {
  let h = handler for Log { write: fn m => scope.defer(fn () => print(m)) };
  ()
}

pub fn main() -> Int { 0 }
",
        "`Log`'s `write` captures `scope`",
    );
}

/// **A generic `A: Share` bound** asked about a region.
#[test]
fn a_share_bound_refuses_a_region() {
    refused(
        "module main;

import std::core::{Region, Share};

fn keep<A: Share>(a: A) -> Int { 0 }

pub fn main() -> Int {
  keep(Region::open())
}
",
        "`Region` does not implement `Share`",
    );
}

/// **A `Scope` handler may capture the region it defers into.** It never
/// crosses, so what it holds need not either. This is what `scoped` and
/// `Scope::root` themselves write, and what a program writing its own
/// scope over a region it opened writes.
#[test]
fn a_scope_handler_may_capture_its_region() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Region, Scope, acquire};

fn uses() -> Int with { scope: Scope } {
  let n = acquire(7, fn n => print(\"released ${n}\"));
  n
}

pub fn main() -> Int {
  let region = Region::open();
  let own = handler for Scope { defer: fn f => Region::defer(region, f) };
  uses() with { scope: own }
}
",
    );
    assert!(found.is_empty(), "a scope over this fiber's own region is refused: {found:#?}");
}

/// **The rewrite each refusal names compiles:** the child opens its own
/// `scoped` and acquires into that.
#[test]
fn a_child_with_its_own_scoped_compiles() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Fiber, Scope, scoped, acquire};

type Conn = { name: String, mut uses: Int };

fn work(n: Int) -> Int with { scope: Scope } {
  let c = acquire({ name: \"c${n}\", uses: 0 }, fn c => print(\"closed ${c.name} ${c.uses}\"));
  c.uses = c.uses + 1;
  c.uses
}

pub fn main() -> Int {
  let a = Fiber::spawn(fn () => scoped(fn () => work(1)));
  let b = Fiber::spawn(fn () => scoped(fn () => work(2)));
  Fiber::join(a) + Fiber::join(b)
}
",
    );
    assert!(found.is_empty(), "the rewrite the refusals name is refused: {found:#?}");
}

/// A program whose `go` spawns over a capture `x` that is still a type
/// variable at the spawn, and is solved by the call `go(..)` written after.
fn solved_after(declarations: &str, call: &str) -> String {
    format!(
        "module main;

import std::core::{{print, Fiber, Region, SharedFn, Channel, Share, List}};

type H = {{ mut n: Int }};
{declarations}
pub fn main() -> Int {{
  let go = fn x => Fiber::spawn(fn () => {{ let _k = x; 3 }});
  {call}
}}
"
    )
}

/// A handler for `Log` whose `record` keeps `x`, built by a lambda whose
/// parameter `x` is settled only by the call that follows it.
fn handler_late(arg_type: &str, value: &str) -> String {
    format!(
        "module main;

import std::core::{{Fiber, List, Channel}};
import std::log::{{Log, info}};

type Holder = {{ mut xs: List<String> }};
type Frozen = {{ xs: List<String> }};

fn logs(n: Int) -> Int with {{ log: Log }} {{
  let mut i = 0;
  while i < n {{ info(\"tick\"); i = i + 1; }};
  n
}}

pub fn main() -> Int {{
  let wrap = fn x => handler for Log {{
    record: fn (_level, _message, _attributes) => {{ let _k = x; () }},
  }};
  let gate: Channel<Int> = Channel::bounded(1);
  let h: {arg_type} = {value};
  let lg = wrap(h);
  let f = Fiber::spawn(fn () => {{
    let _ = Channel::receive(gate);
    logs(10) with {{ log: lg }}
  }});
  Channel::send(gate, 1);
  Fiber::join(f)
}}
"
    )
}

/// **A handler's capture whose type is solved after the handler is written
/// is still asked about `Share`.** The RC Stage 2 review's `handler_late.kh`:
/// `x` is a variable where `handler for Log` is written, a variable answers
/// "shareable", and `wrap(h)` solves it to a record with a `mut` field only
/// afterwards. A handler is shareable because its captures were checked, so
/// the child that logs through it held the parent's `h`: a race on `xs`,
/// and a list released while the parent was still counting it. So the
/// capture is asked again once the body's types are settled.
#[test]
fn a_handler_capture_solved_later_to_a_mut_record_is_refused() {
    let found = errors_with_std(&handler_late("Holder", "{ xs: List::Nil }"));
    assert!(
        found.iter().any(|m| m.contains("`Log`'s `record` captures `x`")
            && m.contains("`Holder` can be written")),
        "{found:#?}"
    );
}

/// **The same handler capturing an annotated `mut` record is refused where
/// it is written, with the message it always had.** `handler_direct.kh`: the
/// type is known at the handler, so the refusal is the one written there.
#[test]
fn a_handler_capturing_a_known_mut_record_is_refused_as_before() {
    let found = errors_with_std(
        "module main;

import std::core::{print, Fiber, List};
import std::log::{Log, info};

type Holder = { mut xs: List<String> };

pub fn main() -> Int {
  let h: Holder = { xs: List::Nil };
  let lg = handler for Log {
    record: fn (_level, _message, _attributes) => { let _k = h; () },
  };
  let f = Fiber::spawn(fn () => { info(\"tick\") with { log: lg }; 1 });
  print(\"${Fiber::join(f)}\");
  0
}
",
    );
    assert!(
        found.iter().any(|m| m.contains(
            "`Log`'s `record` captures `h`, and a handler has to be safe to hand to another fiber"
        ) && m.contains("`Holder` can be written")),
        "{found:#?}"
    );
}

/// **A handler's capture solved later to a shareable value is accepted.**
/// The re-check refuses what is writable, not what was unknown.
#[test]
fn a_handler_capture_solved_later_to_a_shareable_value_compiles() {
    let found = errors_with_std(&handler_late("Frozen", "{ xs: List::Nil }"));
    assert!(found.is_empty(), "a shareable late capture is refused: {found:#?}");
}

/// **A capture whose type is solved after the spawn is still asked about
/// `Share`.** At the spawn, `x` is a variable, and a variable answers
/// "shareable"; `go(h)` solves it to a record with a `mut` field only
/// afterwards. Asked only at the spawn, this compiled, and the child held
/// the parent's `h` -- a race on `n` that nothing traps. So the capture is
/// asked again once the body's types are settled. `var_mut` in the review's
/// route table.
#[test]
fn a_capture_solved_after_the_spawn_to_a_mut_record_is_refused() {
    let found = errors_with_std(&solved_after("", "let h: H = { n: 0 };\n  Fiber::join(go(h))"));
    assert!(
        found.iter().any(|m| m.contains("`x` cannot be handed to another fiber")
            && m.contains("`H` can be written")),
        "{found:#?}"
    );
}

/// **The same route with a `Region`** is refused at compile time: the
/// review's `var_capture.kh`. The runtime's release check stays behind it,
/// for a route no type can see.
#[test]
fn a_capture_solved_after_the_spawn_to_a_region_is_refused() {
    refused(
        &solved_after("", "Fiber::join(go(Region::open()))"),
        "`x` cannot be handed to another fiber: a `Region` stays on the fiber",
    );
}

/// **A capture solved after the spawn to something shareable is accepted**,
/// including a container of one, so the second question refuses only what
/// the first would have refused had it known.
#[test]
fn a_capture_solved_after_the_spawn_to_a_shareable_value_compiles() {
    let found = errors_with_std(&solved_after(
        "",
        "Fiber::join(go(List::Cons(\"a\", List::Nil)))",
    ));
    assert!(found.is_empty(), "{found:#?}");
    let found = errors_with_std(&solved_after("", "Fiber::join(go(41))"));
    assert!(found.is_empty(), "{found:#?}");
}

/// **A container settled after the spawn**: a `mut` binding captured while
/// it is still `List::Nil`, whose element type the next line decides.
#[test]
fn a_capture_whose_element_type_is_settled_after_the_spawn_is_refused() {
    let found = errors_with_std(
        "module main;

import std::core::{Fiber, List};

type H = { mut n: Int };

pub fn main() -> Int {
  let mut xs = List::Nil;
  let f = Fiber::spawn(fn () => { let _k = xs; 3 });
  let h: H = { n: 0 };
  xs = List::Cons(h, List::Nil);
  Fiber::join(f)
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("`xs` cannot be handed to another fiber")
            && m.contains("`List<H>` can be written")),
        "{found:#?}"
    );
}

/// **The same route through `SharedFn::of`**, which certifies its closure's
/// captures as `Fiber::spawn` does.
#[test]
fn a_shared_fn_capture_solved_after_the_call_is_refused() {
    let found = errors_with_std(
        "module main;

import std::core::{SharedFn};

type H = { mut n: Int };

pub fn main() -> Int {
  let go = fn x => SharedFn::of(fn (n: Int) => { let _k = x; n });
  let h: H = { n: 0 };
  let _s = go(h);
  0
}
",
    );
    assert!(
        found.iter().any(|m| m.contains("`x` cannot be handed to another fiber")
            && m.contains("`H` can be written")),
        "{found:#?}"
    );
}

/// **The same route through `Channel::send` and a generic `A: Share`
/// parameter** is refused by the bound on the channel and on the function,
/// which are asked once the types are settled. Pinned beside the spawn
/// rows so the three stay one rule.
#[test]
fn a_send_or_share_bound_solved_after_the_call_is_refused() {
    let send = errors_with_std(
        "module main;

import std::core::{Channel};

type H = { mut n: Int };

pub fn main() -> Int {
  let ch = Channel::bounded(1);
  let go = fn x => Channel::send(ch, x);
  let h: H = { n: 0 };
  go(h);
  0
}
",
    );
    assert!(send.iter().any(|m| m.contains("`H` does not implement `Share`")), "{send:#?}");
    let bound = errors_with_std(
        "module main;

import std::core::{Share};

type H = { mut n: Int };

fn keep<A: Share>(a: A) -> Int { 0 }

pub fn main() -> Int {
  let go = fn x => keep(x);
  let h: H = { n: 0 };
  go(h)
}
",
    );
    assert!(bound.iter().any(|m| m.contains("`H` does not implement `Share`")), "{bound:#?}");
}

/// **A capture that stays unsolved to the end is not refused by the
/// checker.** Nothing ever pins `x`, so nothing is ever passed in: `go` is
/// never called, and no value of that type exists to cross. `khora check`
/// accepts the program; building it is refused, because the backend cannot
/// lay out a closure whose type was never pinned down (LLVM
/// `sharing::a_capture_never_solved_is_refused_at_build`).
#[test]
fn a_capture_never_solved_is_not_a_sharing_error() {
    let found = errors_with_std(&solved_after("", "0"));
    assert!(
        !found.iter().any(|m| m.contains("cannot be handed to another fiber")),
        "{found:#?}"
    );
}
