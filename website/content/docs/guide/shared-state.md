---
title: Shared state
sidebar:
  order: 10
---

Khora keeps ordinary mutation fiber-local. When several fibers genuinely need to coordinate around one logical value, `Shared<A>` is the explicit synchronized boundary.

Sharing and hand-off are different problems:

- `Shared<A>` is one value several fibers may read or replace;
- `Channel<A>` is a queue where each value is handed to one receiver;
- `SharedFn<A, B, 'e>` certifies a closure that may safely live in shareable data.

## Shareability is checked

`Share` is the marker for values two fibers may hold at once:

```khora
pub trait Share {}
```

Ordinary immutable records, variants, and tuples are shareable when everything inside them is shareable. Opaque types whose implementation the compiler cannot inspect default to **not** shareable; their author must explicitly assert `Share` when the implementation is safe.

That is why an ordinary mutable container is not automatically safe merely because its type contains no visible fields.

## Use `Shared<A>` for one evolving value

Create a cell with `Shared::of`:

```khora
let requests = Shared::of(0);
```

Read or replace it directly:

```khora
let current = Shared::get(requests);
Shared::set(requests, current + 1);
```

For a read-modify-write operation, use `update` so the whole transition is serialized:

```khora
let next = Shared::update(requests, fn count => count + 1);
```

The core API is:

```khora
pub type Shared<A>;

impl<A: Share> Shared<A> {
  pub fn of(value: A) -> Shared<A>;
  pub fn get(self) -> A;
  pub fn set(self, value: A) -> ();
  pub fn update(self, change: (A) -> A) -> A;
  pub fn modify<B>(self, change: (A) -> Changed<A, B>) -> B;
}
```

`update` runs `change` once while the cell is locked. The closure cannot fail or suspend. That restriction is deliberate: work that can leave the critical section belongs outside it.

Do not update the same `Shared` cell recursively from inside its own `update`; that would wait on the lock already held by the same operation, so Khora reports it as a trap rather than hanging forever.

## Return something other than the new state with `modify`

Sometimes the state transition also needs to produce a result. `Changed<A, B>` carries both:

```khora
pub type Changed<A, B> = {
  state: A,
  result: B,
};
```

For example, atomically allocate a sequence number while storing the next one:

```khora
let issued = Shared::modify(next_id, fn current => {
  {
    state: current + 1,
    result: current,
  }
});
```

The result belongs to the exact state transition that installed the new state; there is no second read in which another fiber can race ahead.

## Keep slow work outside the cell

Do not put network calls, filesystem work, sleeps, or fallible operations inside `update`/`modify`.

Compute first, then install the result:

```khora
let refreshed = fetch_remote_value()!;
Shared::set(cache, refreshed);
```

This keeps the critical section small and prevents external latency from becoming lock contention.

## Use a `Channel` for hand-off and backpressure

A channel is not shared mutable state. It is a bounded queue whose values are consumed by one receiver each:

```khora
let jobs = Channel::bounded(64);

Channel::send(jobs, job);

match Channel::receive(jobs) {
  Option::Some(next) => process(next),
  Option::None => (),
}
```

The API is:

```khora
pub type Channel<A>;

impl<A: Share> Channel<A> {
  pub fn bounded(capacity: Int) -> Channel<A>;
  pub fn send(self, value: A) -> Bool;
  pub fn receive(self) -> Option<A>;
  pub fn close(self) -> ();
  pub fn depth(self) -> Int;
}
```

A full channel suspends the sender until space is available; an empty channel suspends the receiver until a value arrives. Those waits give the scheduler worker back rather than blocking it.

`send` returns `false` when the channel has been closed. `receive` drains already queued values before returning `None` once the channel is both closed and empty. A requested capacity below one becomes one; Khora's channel is not a zero-capacity rendezvous channel.

Channels are the right primitive for work queues, ownership of a non-shareable resource by one fiber, and pools whose idle members can be handed out and returned.

## Use `SharedFn` for shareable callbacks

A closure's function type does not reveal what it captured. `SharedFn::of` certifies a closure at the point where those captures are visible to the compiler:

```khora
let callback = SharedFn::of(fn request => handle(request));
let response = SharedFn::call(callback, request);
```

Its surface is:

```khora
pub type SharedFn<A, B, 'e>;

impl<A, B, 'e> SharedFn<A, B, 'e> {
  pub fn of(f: (A) -> B raises 'e) -> SharedFn<A, B, 'e>;
  pub fn call(self, argument: A) -> B raises 'e;
}
```

Use this when a callback must be stored in something that itself crosses fiber boundaries, such as a router or callback table.

For the exact rules, see [Sharing](/docs/reference/sharing/). For ownership and cancellation of the fibers themselves, continue with [Fibers and nurseries](/docs/guide/fibers-and-nurseries/).
