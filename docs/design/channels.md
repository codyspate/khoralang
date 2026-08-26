# Channels — how a fiber hands something to another fiber

`Shared<A>` is a cell two fibers may both change. This is the other half, and
it exists because writing the first real resource-owning package found that the
half was missing.

## The gap

`docs/design/sharing.md` states the rule an effect handler lives under: **a
handler must be safe to hand to another fiber**, so it may not capture anything
writable. That rule is right and `sharing.md` argues it well. What nothing
noticed until `packages/postgres` is what it forbids.

A PostgreSQL connection is *writable* — it buffers the bytes that arrived and
were not yet a whole message — and it is *strictly serial*: two fibers writing
one socket interleave their frames and desynchronise the stream. So `std::db`'s
`Db` capability over a connection could not be written at all:

- the handler cannot capture the connection, because `Connection` is not
  `Share`;
- `Shared<Connection>` cannot hold it, because `Shared<A>` requires `A: Share`;
- and doing the query inside `Shared::update` is refused **by design** — a
  change function has no error row, so it cannot fail and cannot do I/O.

Three doors, all correctly locked. And making `Connection` shareable would not
have helped: two fibers on one socket corrupt the stream whatever the checker
says, so the serialisation has to exist somewhere real.

`sharing.md`'s "what is still open" did not list this. It is listed now.

## Decided

**A bounded channel.** One fiber owns the resource; the others send it
requests.

```khora
export type Channel<A>;
impl<A> Share for Channel<A> {}

impl<A: Share> Channel<A> {
  fn bounded(capacity: Int) -> Channel<A>;
  fn send(self, value: A) -> Bool;
  fn receive(self) -> Option<A>;
  fn close(self) -> ();
  fn depth(self) -> Int;
}
```

A fiber waiting at either end gives its worker back. A thread that is not a
fiber blocks on a condition variable instead, because it has nothing to give
back — the same accommodation `fiber::Done` makes, and for the same reason:
`main` is not a fiber.

## Why not a mutex

A mutex would have worked and would have been smaller. Two reasons against, and
they are the same reason twice.

**A lock held across a network round trip is a lock held across code the lock's
author did not write.** That is precisely the hazard `shared.rs` calls out
about its own critical section, and the reason `Shared::update`'s change
function is forbidden from failing. Introducing the same shape deliberately,
one module later, would be hard to defend.

**A bounded channel was needed anyway.** Roadmap 13.2 wants bounded queues and
backpressure; a pool of connections is a channel of the idle ones; a work queue
is a channel. One primitive, three uses — where a mutex would have served one
and left the others still to build.

The two are equal in power — a channel of one token is a mutex, a mutex plus a
condition is a channel — so the question was only which to make primitive. The
one that does not hold a lock across user code wins.

## What the shape buys

**Backpressure is the default, not a feature.** A full channel stops the
sender. A service under more load than it can serve declines work at the edge
instead of queueing it until memory runs out, and nobody had to remember to
write that.

**A pool is not a new thing.** Taking a worker is a receive, giving it back is
a send, and waiting for one is what the channel already does.

**Ownership is a fiber.** `packages/postgres`'s `serve` owns its connection
from the moment it opens it to the moment it closes it, and nothing else can
reach it. That is stronger than a lock, which only says *not at the same time*.

## Two decisions inside it

**A closed channel drains before it ends.** `receive` answers `None` only when
the channel is closed *and* empty. Values already sent are still worth having,
and a reader that stopped at the close would lose them.

**Capacity is at least one.** `bounded(0)` gets one rather than a rendezvous. A
rendezvous — where a send does not complete until a receive begins — is a
useful and different thing; building it out of these parts would mean a sender
waiting for a receiver that is waiting for a sender, and getting that wrong is
a hang rather than an error.

## What crosses, and who owns it

One word, as everywhere else in the runtime, plus — recorded once when the
channel is opened — whether it is a pointer and how to release it, since the
runtime cannot know `A`.

A value in the queue is **owned by the queue**. `send` takes the caller's
reference and `receive` gives it back, so nothing is duplicated; a value
abandoned in a channel nobody drains is released when the channel is; and a
send to a *closed* channel releases the value rather than keeping it, because a
send with nowhere to put its value must not be the quietest possible leak.

The tests assert on `khora_live_count` rather than only on output, because this
is the class of mistake that does not produce a wrong answer — it produces a
program that slowly runs out of memory.

## What it does not do

| Missing | Why, and what would force it |
| --- | --- |
| **Select** — waiting on the first of several | Wants a primitive of its own. Nothing has needed one |
| **Rendezvous** (capacity zero) | See above. A separate type, when something wants it |
| **A move-in spawn** | Still open, and `sharing.md` already lists it. It is why `postgres::db::serve` takes connection *settings* rather than a connection: a spawned fiber's captures are copied, so handing over a value that must not be copied is not expressible. Opening the resource inside the owning fiber turns out to be better anyway — the lifetime becomes exactly the fiber's |
| **Priorities, or a deadline on a send** | 13.2 may want the second |

## The bug found underneath it

Making `Channel<Cell>` work exposed something older. **Whether a type is
shareable was being answered differently depending on who asked.**

`std::db`'s `Cell` holds a `Decimal`. Answering "may two fibers hold a `Cell`"
means looking inside `Decimal` — and a module that imported `Cell` without also
importing `std::decimal` could not look, so it got `false`. The same type, the
same question, two answers, decided by an unrelated line at the top of the
file.

The fix carries the bodies a type's fields reach across the import boundary,
kept in a list of their own so those names are still **not in scope** — a
record literal must not infer as a type the file cannot name. `TypeMap::
reachable` and `reachable_adts`; the tests are in
`crates/khora-types/tests/visibility.rs`.
