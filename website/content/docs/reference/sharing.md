---
title: Sharing
sidebar:
  order: 21
---

Khora keeps ordinary mutation fiber-local. A value may cross a fiber boundary only when its type is shareable, and coordinated mutation is expressed through synchronization types rather than by sharing an ordinary writable record or container.

## `Share`

`Share` is the marker trait for values that two fibers may hold at the same time:

```khora
pub trait Share {}
```

Structural values are shareable when all of their contents are shareable. For example, an immutable record containing only shareable fields needs no handwritten marker implementation.

Opaque types are different: the compiler cannot inspect their representation, so they are not shareable unless their implementation explicitly promises that concurrent holders are safe:

```khora
pub type SafeHandle;

impl Share for SafeHandle {}
```

Writing that implementation is an assertion about the opaque implementation. Do not add it merely to silence a sharing diagnostic.

Mutable fiber-local containers are not made safe by wrapping their type name in a shareable record. If the value itself permits unsynchronized writes, it must remain local or be represented through a synchronization primitive designed for that use.

## `Shared<A>`

`Shared<A>` is a synchronized cell containing one shareable value:

```khora
pub type Shared<A>;

impl<A> Share for Shared<A> {}

impl<A: Share> Shared<A> {
  pub fn of(value: A) -> Shared<A>;
  pub fn get(self) -> A;
  pub fn set(self, value: A) -> ();
  pub fn update(self, change: (A) -> A) -> A;
  pub fn modify<B>(self, change: (A) -> Changed<A, B>) -> B;
}
```

Create, read, and replace a value as follows:

```khora
let count = Shared::of(0);

let before = Shared::get(count);
Shared::set(count, before + 1);
```

Use `update` when the read and write must be one serialized transition:

```khora
let after = Shared::update(count, fn n => n + 1);
```

The `change` closure runs once while the cell is locked. Its type has no `raises` row and the operation must not suspend. Fallible, blocking, or otherwise suspension-capable work belongs outside the critical section:

```khora
let refreshed = fetch_value()!;
Shared::set(cache, refreshed);
```

Calling `update` or `modify` recursively on the **same cell** from inside its own change function would deadlock. Khora detects that case and traps instead of waiting forever.

## `Changed<A, B>` and `modify`

`modify` is the atomic operation to use when a state transition also needs to return a value other than the new state.

Its result record is:

```khora
pub type Changed<A, B> = {
  state: A,
  result: B,
};
```

Example:

```khora
let issued = Shared::modify(next_id, fn current => {
  {
    state: current + 1,
    result: current,
  }
});
```

Both the state replacement and the returned result belong to the same locked transition.

## `Channel<A>`

A channel is a bounded hand-off queue, not another kind of shared cell:

```khora
pub type Channel<A>;

impl<A> Share for Channel<A> {}

impl<A: Share> Channel<A> {
  pub fn bounded(capacity: Int) -> Channel<A>;
  pub fn dropping(capacity: Int) -> Channel<A>;
  pub fn sliding(capacity: Int) -> Channel<A>;
  pub fn send(self, value: A) -> Bool;
  pub fn receive(self) -> Option<A>;
  pub fn poll(self) -> Option<A>;
  pub fn close(self) -> ();
  pub fn depth(self) -> Int;
}
```

Typical use:

```khora
let jobs = Channel::bounded(64);

if Channel::send(jobs, job)! {
  ()
} else {
  handle_closed_queue(job)
}

match Channel::receive(jobs)! {
  Option::Some(next) => process(next),
  Option::None => (),
}
```

A send to a full channel suspends until space becomes available. A receive from an empty open channel suspends until a value arrives. Suspension gives the scheduler worker back; it is not a busy wait or a blocked worker thread.

Both take a `!`, because both are cancellation points. These are the only two operations in `std::core` where a fiber can wait indefinitely, so a fiber parked on one has to be reachable by `Fiber::cancel` -- and a cancellation travels out of a call on a `raises` row or not at all. The row is a variable: neither operation raises an error of its own, and it carries whatever the caller's does.

A cancelled channel operation is always cancelled **empty-handed**. The runtime looks at the cancellation flag only once it has established there is nothing to take and no room to send, so a value arriving at the same moment as the cancellation is still delivered rather than dropped. A send that gives up releases its value, the same as a send to a closed channel.

`poll` never waits, so it is not a cancellation point and carries no row. That is the distinction between the two: the ones that can block are marked.

`send` returns `false` when the channel is closed. Closing wakes waiters. Receivers drain values already queued before `receive` begins returning `Option::None`.

A capacity less than one is treated as one. `Channel::bounded(0)` is therefore **not** a zero-capacity rendezvous channel.

### What a full channel does

The behavior belongs to the channel, not to the send. A queue is lossy or it is not, and two senders disagreeing about which is not a state a queue can be in.

| Constructor | A send into a full one | `send` answers | What is left |
| --- | --- | --- | --- |
| `bounded` | waits | `true` | everything |
| `dropping` | refuses the new value | `false` | the oldest |
| `sliding` | evicts the oldest | `true` | the newest |

`bounded` is the default because backpressure is: a queue nobody is draining is a producer that should slow down.

`dropping` is for a producer that must not stall — the request path writing an audit event, a handler emitting a metric. The `false` makes the loss a value the caller can count and report.

`sliding` is for a feed where only the newest value matters: a gauge, a progress indicator, a last-known position. It answers `true` because nothing was refused, so the loss is invisible at the call site. That is deliberate — nobody was going to act on it. Use `dropping` where somebody would.

### `poll`

`poll` takes a value if one is already there and never waits:

```khora
match Channel::poll(jobs) {
  Option::Some(next) => process(next),
  Option::None => do_something_else(),
}
```

`None` means "not right now", not "not ever": a closed and drained channel and a live empty one both answer `None`, and telling them apart is what `receive` is for. Use `poll` in a loop that has other work between looks. A loop that only polls is a loop that spins.

`depth` is observational: concurrent activity can make the returned count stale immediately, so it is appropriate for metrics and tests rather than synchronization decisions.

## `SharedFn`

A closure's function type does not reveal what the closure captured. `SharedFn` records the fact that a closure was checked for safe sharing at the point where its captures were visible:

```khora
pub type SharedFn<A, B, 'er>;

impl<A, B, 'er> Share for SharedFn<A, B, 'er> {}

impl<A, B, 'er> SharedFn<A, B, 'er> {
  pub fn of(f: (A) -> B raises 'er) -> SharedFn<A, B, 'er>;
  pub fn call(self, argument: A) -> B raises 'er;
}
```

Construct one from a closure literal or named function:

```khora
let callback = SharedFn::of(fn request => handle(request));
let response = SharedFn::call(callback, request);
```

Use `SharedFn` when a callback must be stored inside another shareable value, such as a router or callback table.

## Choosing the boundary

Use `Shared<A>` when several fibers coordinate around **one evolving value**. Use `Channel<A>` when a value or unit of work is **handed to one receiver**, especially when backpressure matters. Use `SharedFn` when a **callback itself** must cross the sharing boundary.

See [Concurrency](/docs/reference/concurrency/) for fiber and nursery lifetime rules and [Memory and resources](/docs/reference/memory-and-resources/) for structured cleanup.
