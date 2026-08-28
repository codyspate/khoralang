---
title: Concurrency
sidebar:
  order: 22
---

Khora concurrency is structured around fibers and nurseries. A fiber may suspend without blocking the scheduler worker, and child work remains owned by a lexical lifetime rather than becoming detached by default.

Source functions are not colored `async`; suspension-capable operations remain ordinary direct-style calls.

## Fibers

A fiber handle carries both the answer type and the failure row:

```khora
pub type Fiber<A, 'er>;

impl<A: Share, 'er> Fiber<A, 'er> {
  pub fn spawn(body: () -> A raises 'er) -> Fiber<A, 'er>;
  pub fn join(self) -> A raises 'er;
  pub fn cancel(self) -> ();
  pub fn detach(self) -> ();
}
```

`A` must be `Share`: the value is computed on one fiber and read on another.

`join` waits and answers what the body answered. If the body raised, `join` re-raises with the same type, so the caller catches by name:

```khora
fn run(id: Int) -> () {
  let child = Fiber::spawn(fn () => load(id)!);
  let row = Fiber::join(child)! catch {
    DbError::Timeout => Row::empty(),
    DbError::Missing(_id) => Row::empty(),
  };
  print(Int::to_string(row.total))
}
```

A body with an empty failure row needs no `!` on the join. Joining twice is joining once, from either side, and answers twice.

A spawned closure may capture values that satisfy the sharing rules:

```khora
fn print_later(value: Int) -> () {
  let child = Fiber::spawn(fn () => print(Int::to_string(value)));
  Fiber::join(child)
}
```

Releasing the final `Fiber` handle also waits for the child. This means a fiber cannot silently outlive the scope that still owns its handle.

`Fiber::detach` is the exception, and the only one: it stops waiting and asks the fiber to stop. The fiber keeps running, its answer is discarded, and a later failure is silent. Without it, a bounded wait over a body with an uninterruptible tail could not be honored.

## Tasks

A `Task` is a running fiber under a handle that has given up its answer and its error type. It is what a nursery adopts:

```khora
pub type Task;

impl Share for Task {}

impl Task {
  pub fn spawn<'er>(body: () -> () raises 'er) -> Task;
  pub fn cancel(self) -> ();
}
```

The thunk keeps its `raises` row; the handle does not. That split is deliberate and is the reason `Task` exists rather than `Fiber<(), {}>`:

- An effect operation cannot be generic, so `adopt` must name one type with no parameters. `Fiber<(), 'er>` is refused — `'er` is a type the caller chooses.
- A cancellation travels out on the same tagged return an error does. A fiber whose row is empty therefore has no channel to be stopped on, and a nursery whose children cannot be cancelled is not a nursery. The row stays at the runtime, where cancellation reads it, and leaves the type, where the operation needs one shape.

The consequence is that a child's failure is a runtime report rather than a compile-time one. That is the price of children that can be stopped.

`Task::cancel` is rarely called directly. Leaving the nursery's block cancels every child still running, which is what a nursery is.

## Nurseries

A nursery owns a set of fibers. The capability installed in a nursery body is:

```khora
pub effect Nursery {
  adopt: (Task) -> (),
}
```

A body that starts children declares the requirement and adopts each handle. The body is written at the adoption site, and it may raise:

```khora
fn fan_out() -> ()
  with { nursery: Nursery }
{
  nursery.adopt(Task::spawn(fn () => first()));
  nursery.adopt(Task::spawn(fn () => second()!));
}
```

`adopt` takes a running fiber rather than a thunk. A handler's fields are closures and a closure cannot be generic, so an operation could not be generic in what a thunk raises; and a thunk cannot be forwarded to `spawn` at all, because a fiber's body must be written where it starts so its captures can be checked against the sharing rules.

`nursery` installs that capability and waits for the children on the normal path:

```khora
pub fn nursery<A, 'ef, 'er>(
  body: () -> A with { 'ef | nursery: Nursery } raises 'er
) -> A
  with 'ef
  raises 'er
```

Example:

```khora
fn run() -> () {
  nursery(fan_out)
}
```

When the body completes normally, `nursery` waits until every adopted child is finished. If the body leaves by failure or cancellation, releasing the nursery cancels children that are still running and waits for them before the scope is gone.

The body may be a named function or a lambda. A lambda resolves its capabilities where it is written, and as the argument to `nursery` that is inside the row `nursery` installs, so `nursery(fan_out)` and `nursery(fn () => fan_out())` mean the same thing.

## Bounded nurseries

The bounded form has the same row behavior plus a concurrency limit:

```khora
pub fn bounded_nursery<A, 'ef, 'er>(
  limit: Int,
  body: () -> A with { 'ef | nursery: Nursery } raises 'er
) -> A
  with 'ef
  raises 'er
```

```khora
fn serve() -> ()
  with { nursery: Nursery }
{
  loop {
    let request = next_request();
    nursery.adopt(Task::spawn(fn () => handle(request)));
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
