---
title: Fibers and nurseries
sidebar:
  order: 9
---

Khora uses structured concurrency. Concurrent work runs in fibers, and the normal way to own several fibers is a nursery whose lifetime is part of the surrounding scope.

There is no `async`/`await` version of the language. A fiber may suspend on I/O, a timer, a channel, or another fiber while source code remains direct style.

## Spawn and join a fiber

`Fiber::spawn` starts a child from a closure and returns a handle. `Fiber::join` waits for it and gives back what it computed:

```khora
fn run_one() -> () {
  let count = Fiber::join(Fiber::spawn(fn () => tally(rows)));
  print("counted ${count}")
}
```

No `!` there, because `tally` cannot fail. When the body *can* fail, `join` re-raises — with the failure's own type, so you can catch it by name:

```khora
fn run_query(id: Int) -> () {
  let worker = Fiber::spawn(fn () => load(id)!);
  let row = Fiber::join(worker)! catch {
    DbError::Timeout => Row::empty(),
    DbError::Missing(_id) => Row::empty(),
  };
  print("total ${row.total}")
}
```

The failure row rides on the handle, which is what makes this pleasant rather than merely possible. A handle that erased it would need a `!` on every join, including on a fiber that provably cannot fail, and `catch { _ => .. }` as the only arm.

A spawned closure may capture ordinary shareable values from its environment:

```khora
fn print_double(value: Int) -> () {
  let worker = Fiber::spawn(fn () => print("${value * 2}"));
  Fiber::join(worker)
}
```

### Fan out and collect the answers

A nursery runs children for their effects and gives back nothing. When you want
the *answers*, spawn the fibers and `join_all` them:

```khora
fn totals(ids: List<Int>) -> List<Row> raises DbError {
  let workers = List::map(ids, fn id => Fiber::spawn(fn () => load(id)!));
  join_all(workers)!
}
```

Answers arrive in handle order rather than completion order, which is what
lines them up against the requests that produced them. If one child fails,
`join_all` raises its failure and the rest are cancelled and waited for before
it returns — so a fan-out where one branch fails is a raise, not a leak.

If you want the fastest answer first rather than all of them in order, you
wanted a channel.

The core handle API is:

```khora
pub type Fiber<A, 'er>;

impl<A: Share, 'er> Fiber<A, 'er> {
  pub fn spawn(body: () -> A raises 'er) -> Fiber<A, 'er>;
  pub fn join(self) -> A raises 'er;
  pub fn cancel(self) -> ();
  pub fn detach(self) -> ();
}
```

`A` must be `Share` for the reason every value crossing a fiber must be: it is computed on one fiber and read on another, so a thing that cannot be held twice cannot be an answer.

A fiber handle is itself a lifetime boundary. Releasing the final handle waits for the fiber, so a child cannot silently outlive the scope that still owns its handle. Joining twice is joining once, from either side, and gets the answer twice.

## Prefer a nursery for a group of children

A nursery makes ownership explicit for fan-out work. The `Nursery` capability contains the operation that adopts a running fiber:

```khora
pub effect Nursery {
  adopt: (Fiber<(), 'er>) -> (),
}
```

**The answer is fixed at `()`; the row is not.** The answer is fixed because a nursery has nothing to do with a result it cannot hand back — it holds children as bare handles and waits for them. The row stays because a cancellation travels out on the same tagged return an error does, so a child whose row is empty has no channel to be stopped on, and a nursery that cannot stop its children is not a nursery.

That means no `catch` at the adoption site. Spawn the fiber, let it raise, hand it over:

```khora
fn children() -> ()
  with { nursery: Nursery }
{
  nursery.adopt(Fiber::spawn(fn () => first_job()));
  nursery.adopt(Fiber::spawn(fn () => second_job()!));
}
```

`'er` is quantified per call, not per handler, so two children raising two different things are adopted by the same nursery. Keep a child's answer by holding its handle yourself and joining it instead.

Run it with `nursery`:

```khora
fn run() -> () {
  nursery(children)
}
```

`nursery(children)` does not return normally until every adopted child is finished. If the body leaves through failure or cancellation instead, the nursery cancels the children that are still running and waits for them before the scope is released.

### A child that fails is the nursery's failure

A nursery is a unit. The block asked for these fibers together, so one of them failing means the answer the group was computing is not coming — and the siblings are working on a question nobody will ask.

So the first child to fail cancels the rest, every child is still waited for, and the nursery raises `ChildFailed`:

```khora
pub type ChildFailed = { children: Int };
```

The count, and not the child's own error, because there is no such single thing to hand back: `adopt` binds the row per adoption, so two children may fail with two unrelated types. What survives the handles is how many of them did.

A child a nursery *cancelled* is not counted. Cancellation is what a nursery does to its children on the way out, and counting it would make every early exit look like a fault.

Catch it where you can do something about it:

```khora
match attempt(fn () => nursery(children)!) {
  Result::Ok(answer) => answer,
  Result::Err(_failure) => fallback(),
}
```

The generic helper is effect-polymorphic, and carries `ChildFailed` on top of whatever the body raises:

```khora
pub fn nursery<A, 'ef, 'er>(
  body: () -> A with { 'ef | nursery: Nursery } raises 'er
) -> A
  with 'ef
  raises 'er + ChildFailed
```

A named function or a lambda, whichever reads better. `nursery(children)` and `nursery(fn () => children())` are the same thing: a lambda resolves its capabilities where it is written, and as the argument to `nursery` that is inside the row `nursery` installs.

The lambda may also **name** the nursery, so a whole group of children can be written where it is started rather than in a function of its own:

```khora
nursery(fn () => {
  nursery.adopt(Fiber::spawn(fetch));
  nursery.adopt(Fiber::spawn(index));
})!
```

`nursery` here is the capability the lambda requires, not the function being called — a capability a lambda needs and cannot find is its parameter in all but spelling, and shadows an outer name the way a parameter does. The same holds for `bounded_nursery`, and for `scope` inside a `scoped` body.

### An operation can be generic in a row, but not in a type

`adopt` binds `'er` and cannot bind an answer type, and that asymmetry is a real property of the design rather than a hole nobody got to. A capability crosses as evidence and an error as a tag, so a handler's closure is the same machine code for every row; a type parameter decides a layout and would have to be monomorphized, and a closure has nowhere to put that. [Effects and rows](/docs/reference/effects/#an-operation-may-be-generic-in-a-row-but-not-in-a-type) has the rule.

So a nursery can hold children that fail in unrelated ways, and cannot hold children that answer in unrelated ways. Which is the shape a nursery wants anyway.

## Bound work that comes from outside

Use an unbounded nursery when the amount of fan-out is already bounded by data you hold, such as a known handful of independent lookups.

When the work rate is controlled by the outside world, use `bounded_nursery`:

```khora
fn serve() -> ()
  with { nursery: Nursery }
{
  loop {
    let connection = accept_next();
    nursery.adopt(Fiber::spawn(fn () => handle(connection)!));
  }
}

fn main() -> Int {
  bounded_nursery(128, serve);
  0
}
```

Its signature is:

```khora
pub fn bounded_nursery<A, 'ef, 'er>(
  limit: Int,
  body: () -> A with { 'ef | nursery: Nursery } raises 'er
) -> A
  with 'ef
  raises 'er
```

Adopting beyond the limit waits for older work to finish. That turns an external concurrency ceiling into backpressure instead of allowing a service to grow until it exhausts memory.

## Cancellation is per fiber

Cancel one child without cancelling its parent:

```khora
let worker = Fiber::spawn(fn () => work());
Fiber::cancel(worker);
Fiber::join(worker);
continue_parent_work();
```

Cancellation is observed at cancellation points rather than arriving between arbitrary source statements. When a cancelled fiber unwinds, its regions and finalizers run before the fiber finishes.

A `catch` handles failures declared in a `raises` row. Cancellation is separate and cannot be accidentally swallowed by matching every declared failure.

## `detach` is the valve

Every other way out of a handle waits. `join` waits, and so does letting the binding go — that is where structured concurrency comes from, and it is also how a program hangs: one finalizer that never returns holds its nursery, which holds its parent, up to `main`.

`Fiber::detach` signals and goes:

```khora
Fiber::detach(worker);
```

The fiber keeps running, its answer is dropped when it arrives, and a failure it reports afterwards is silent — the program said it was no longer listening. It cancels as well as detaching, because a detached fiber nobody asked to stop is a leak with a nicer name.

Reach for it when a bounded wait matters more than a clean one, and not otherwise.

## Values crossing into a fiber must be shareable

Spawning concurrent work is also a sharing boundary. Immutable structural values are shareable when everything they contain is shareable. Writable fiber-local structures are not silently allowed to cross.

For coordinated mutable state, use `Shared<A>`. For handing work or ownership from one fiber to another, use `Channel<A>`. For a closure stored in a shareable data structure, use `SharedFn`.

See [Shared state](/docs/guide/shared-state/) for those patterns and the [Concurrency reference](/docs/reference/concurrency/) for the exact fiber and nursery rules.
