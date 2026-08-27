---
title: Fibers and nurseries
sidebar:
  order: 9
---

Khora uses structured concurrency. Concurrent work runs in fibers, and the normal way to own several fibers is a nursery whose lifetime is part of the surrounding scope.

There is no `async`/`await` version of the language. A fiber may suspend on I/O, a timer, a channel, or another fiber while source code remains direct style.

## Spawn and join a fiber

`Fiber::spawn` starts a child from a closure and returns a handle. `Fiber::join` waits for it:

```khora
fn run_one() -> () {
  let worker = Fiber::spawn(fn () => do_work());
  Fiber::join(worker);
}
```

A spawned closure may capture ordinary shareable values from its environment:

```khora
fn print_double(value: Int) -> () {
  let worker = Fiber::spawn(fn () => print(value * 2));
  Fiber::join(worker);
}
```

The core handle API is:

```khora
pub type Fiber;

impl Fiber {
  pub fn spawn<'e>(body: () -> () raises 'e) -> Fiber;
  pub fn join(self) -> ();
  pub fn cancel(self) -> ();
}
```

A fiber handle is itself a lifetime boundary. Releasing the final handle waits for the fiber, so a child cannot silently outlive the scope that still owns its handle.

## Prefer a nursery for a group of children

A nursery makes ownership explicit for fan-out work. The `Nursery` capability contains the operation that adopts a spawned fiber:

```khora
pub effect Nursery {
  adopt: (Fiber) -> (),
}
```

A function that starts children asks for that capability:

```khora
fn children() -> ()
  with { nursery: Nursery }
{
  nursery.adopt(Fiber::spawn(fn () => first_job()));
  nursery.adopt(Fiber::spawn(fn () => second_job()));
}
```

Run it with `nursery`:

```khora
fn run() -> () {
  nursery(children)
}
```

`nursery(children)` does not return normally until every adopted child is finished. If the body leaves through failure or cancellation instead, the nursery cancels the children that are still running and waits for them before the scope is released.

The generic helper is effect-polymorphic:

```khora
pub fn nursery<A, 'e, 'r>(
  body: () -> A with { 'e | nursery: Nursery } raises 'r
) -> A
  with 'e
  raises 'r
```

Pass the named function that requires `nursery`; do not wrap it in an unnecessary lambda. Named functions receive capability rows as parameters, while lambdas capture the capabilities available where the lambda is created.

## Bound work that comes from outside

Use an unbounded nursery when the amount of fan-out is already bounded by data you hold, such as a known handful of independent lookups.

When the work rate is controlled by the outside world, use `bounded_nursery`:

```khora
fn serve() -> ()
  with { nursery: Nursery }
{
  loop {
    let connection = accept_next();
    nursery.adopt(Fiber::spawn(fn () => handle(connection)));
  }
}

fn main() -> Int {
  bounded_nursery(128, serve);
  0
}
```

Its signature is:

```khora
pub fn bounded_nursery<A, 'e, 'r>(
  limit: Int,
  body: () -> A with { 'e | nursery: Nursery } raises 'r
) -> A
  with 'e
  raises 'r
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

## Values crossing into a fiber must be shareable

Spawning concurrent work is also a sharing boundary. Immutable structural values are shareable when everything they contain is shareable. Writable fiber-local structures are not silently allowed to cross.

For coordinated mutable state, use `Shared<A>`. For handing work or ownership from one fiber to another, use `Channel<A>`. For a closure stored in a shareable data structure, use `SharedFn`.

See [Shared state](/docs/guide/shared-state/) for those patterns and the [Concurrency reference](/docs/reference/concurrency/) for the exact fiber and nursery rules.
