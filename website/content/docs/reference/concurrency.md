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

## Nurseries

A nursery owns a set of fibers. The capability installed in a nursery body is:

```khora
pub effect Nursery {
  adopt: (Fiber<(), 'er>) -> (),
}
```

The answer is fixed at `()` and the row is not, and each half has its own reason.

The answer is fixed because a nursery has nothing to do with a result it cannot hand back: it holds children as bare handles and waits for them. A fiber whose result matters is one whose handle you keep and `join`.

The row stays because a cancellation travels out on the same tagged return an error does. A child whose row is empty has no channel to be stopped on, and a nursery whose children cannot be stopped is not a nursery.

`'er` is quantified per call rather than per handler, so children raising unrelated failures are adopted by one nursery. A body that starts children declares the requirement and adopts each handle; the child's body may raise, and no `catch` is needed at the adoption site:

```khora
fn fan_out() -> ()
  with { nursery: Nursery }
{
  nursery.adopt(Fiber::spawn(fn () => first()));
  nursery.adopt(Fiber::spawn(fn () => second()!));
}
```

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

### An operation can be generic in a row, but not in a type

`adopt` binds `'er` and cannot bind an answer type. The asymmetry follows from how each one is represented.

A capability crosses as evidence and an error as a tag, so a handler's closure is the same code for every row: nothing in it depends on which failures a child can raise. A type parameter decides a layout and must be monomorphized, and a handler's fields are closures, which have nowhere to put a per-layout instantiation.

### Why `adopt` takes a fiber and not a thunk

A fiber's body must be written where it starts, so that what it closes over can be checked against the sharing rules. A thunk built somewhere else and forwarded to `spawn` inside the handler would move that check away from the code it is about.

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
    nursery.adopt(Fiber::spawn(fn () => handle(request)!));
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

Cancellation is observed at cancellation points rather than between arbitrary source instructions. There are two:

- a `!` site, which is also where propagation and suspension are marked; and
- a **loop back-edge** — the point where `loop` or `while` goes round again.

Both only exist in a function that can raise, because a `raises` row is the channel a cancellation travels on.

The back-edge is why an ordinary background worker can be stopped:

```khora
fn reaper() -> () with { clock: Clock } raises Stop {
  loop {
    clock.sleep(1000);
    sweep();
  }
}
```

There is no `!` in that body. Without the back-edge it could not be cancelled, and a nursery that had to unwind past it would wait for ever.

A blocked or suspended operation is made runnable so that the fiber can unwind its structured scopes. A *straight-line* blocking call is not itself a cancellation point: the fiber wakes, finishes the call, and stops at the next `!` or back-edge after it.

### A fiber with no error row runs to its end

A cancellation leaves a function the same way an error does, on the same tagged return. A function declared without `raises` has no such return, so it has nothing to travel on — it has no cancellation points, and neither `!` nor a loop back-edge puts one there.

That is a language rule rather than a gap. It also means a background worker that genuinely cannot fail still needs an error row to be stoppable:

```khora
// Cannot be cancelled: nothing to carry the cancellation out.
fn reporter() -> () with { clock: Clock } {
  loop { clock.sleep(5000); report(); }
}
```

Give it a `raises` row — even one whose error it never raises — and the loop becomes cancellable.

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
