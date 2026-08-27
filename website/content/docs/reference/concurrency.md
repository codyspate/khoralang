---
title: Concurrency
sidebar:
  order: 22
---

Khora concurrency is structured around fibers and nurseries. A fiber may suspend without blocking the scheduler worker, and child work remains owned by a lexical lifetime rather than becoming detached by default.

Source functions are not colored `async`; suspension-capable operations remain ordinary direct-style calls.

## Fibers

A fiber handle has three core operations:

```khora
pub type Fiber;

impl Fiber {
  pub fn spawn<'e>(body: () -> () raises 'e) -> Fiber;
  pub fn join(self) -> ();
  pub fn cancel(self) -> ();
}
```

Spawn and wait explicitly:

```khora
fn run() -> () {
  let child = Fiber::spawn(fn () => work());
  Fiber::join(child);
}
```

A spawned closure may capture values that satisfy the sharing rules:

```khora
fn print_later(value: Int) -> () {
  let child = Fiber::spawn(fn () => print(value));
  Fiber::join(child);
}
```

Releasing the final `Fiber` handle also waits for the child. This means a fiber cannot silently outlive the scope that still owns its handle.

## Nurseries

A nursery owns a set of fibers. The capability installed in a nursery body is:

```khora
pub effect Nursery {
  adopt: (Fiber) -> (),
}
```

A body that starts children declares the requirement and adopts each handle:

```khora
fn fan_out() -> ()
  with { nursery: Nursery }
{
  nursery.adopt(Fiber::spawn(fn () => first()));
  nursery.adopt(Fiber::spawn(fn () => second()));
}
```

`nursery` installs that capability and waits for the children on the normal path:

```khora
pub fn nursery<A, 'e, 'r>(
  body: () -> A with { 'e | nursery: Nursery } raises 'r
) -> A
  with 'e
  raises 'r
```

Example:

```khora
fn run() -> () {
  nursery(fan_out)
}
```

When the body completes normally, `nursery` waits until every adopted child is finished. If the body leaves by failure or cancellation, releasing the nursery cancels children that are still running and waits for them before the scope is gone.

Pass the named function that requires `nursery`. Named functions receive capability rows when called; a lambda captures the capabilities available where the lambda is created and does not become a receiver for a capability installed later.

## Bounded nurseries

The bounded form has the same row behavior plus a concurrency limit:

```khora
pub fn bounded_nursery<A, 'e, 'r>(
  limit: Int,
  body: () -> A with { 'e | nursery: Nursery } raises 'r
) -> A
  with 'e
  raises 'r
```

```khora
fn serve() -> ()
  with { nursery: Nursery }
{
  loop {
    let request = next_request();
    nursery.adopt(Fiber::spawn(fn () => handle(request)));
  }
}

bounded_nursery(128, serve)
```

Adopting past a bounded nursery's limit waits for older work to finish. Use this for work whose arrival rate is controlled externally so overload becomes backpressure instead of unbounded growth.

Use unbounded `nursery` when the fan-out is already bounded by data the program holds, such as a known handful of independent tasks.

## Cancellation

Cancellation belongs to the target fiber:

```khora
let child = Fiber::spawn(fn () => work());
Fiber::cancel(child);
Fiber::join(child);
continue_parent();
```

Cancelling a child does not cancel its parent.

Cancellation is observed at cancellation points rather than between arbitrary source instructions. The same `!` sites that mark propagation/suspension boundaries are where a pending cancellation can cause control to leave. A blocked or suspended operation is made runnable so that the fiber can unwind its structured scopes.

Cancellation is **not** a member of a `raises` row. A `catch` that handles every declared failure does not consume cancellation.

During cancellation, intervening regions are released and their finalizers run before the fiber terminates. See [Memory and resources](/docs/reference/memory-and-resources/).

## Suspension and workers

Waiting for nonblocking I/O, a timer, a channel, a join, or scheduler capacity suspends the fiber so the worker can execute other runnable work. Application code continues to look like ordinary calls:

```khora
let bytes = receive(socket)!;
let message = decode(bytes)!;
handle(message)
```

Suspension is distinct from scheduler safepoints used for fairness. Fairness may move execution between workers without being a cancellation event.

A fiber may resume on a different operating-system thread after suspension. Foreign code must therefore not carry a thread-local address, borrowed errno-like state, native-thread identity, or another thread-affine value across a Khora suspension unless the foreign API explicitly permits that migration.

## Sharing boundary

Values captured by or handed to concurrent work must satisfy Khora's sharing rules. Use `Shared<A>` for synchronized shared state, `Channel<A>` for hand-off/backpressure, and `SharedFn` for callbacks stored in shareable data.

See [Sharing](/docs/reference/sharing/) for those exact APIs and constraints.
