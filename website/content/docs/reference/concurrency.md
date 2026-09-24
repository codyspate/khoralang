---
title: Concurrency
sidebar:
  order: 15
---

Start concurrent work with `Fiber::spawn`. A fiber cannot outlive the block
that started it, so nothing leaks when you leave.

**There is no `async` keyword and no `await`.** A function that suspends looks
like one that does not, so any function can call any function.

```khora
let child = Fiber::spawn(fn () => load(id)!);
let row = Fiber::join(child)!;
```

## Fibers

A fiber handle carries both the answer type and the failure row:

```khora
pub type Fiber<A, 'er>;

impl<A: Share, 'er> Fiber<A, 'er> {
  pub fn spawn(body: () -> A raises 'er) -> Fiber<A, 'er>;
  pub fn join(self) -> A raises 'er;
  pub fn cancel(self) -> ();
  pub fn detach(self) -> ();
  pub fn wait(self) -> ();
}
```

`A` must be `Share`: the value is computed on one fiber and read on another.

`wait` waits without taking the answer, which is what you need after `cancel`: a
cancelled fiber has no answer, so `join` on one unwinds the joiner along with it.

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

`Fiber::detach` is the exception, and the only one: it stops waiting and asks the fiber to stop. Both halves are asynchronous — `detach` signals and returns at once, so the fiber keeps running until it reaches its next cancellation point rather than stopping where it is. Its answer is discarded when it arrives, and a later failure is silent — the program said it was no longer listening. It cancels as well as detaching, because a detached fiber nobody asked to stop is a leak with a nicer name.

**Cancelling a fiber running `Router::listen` works.** It was a known gap until
the runtime learned to check for cancellation in the poll loop a parked
`accept` sits in: a cancelled listener now unwinds and the process exits.
Cancel it, wait for it, and the port is released.

The shape still worth keeping is **drain first, stop last**: stop accepting
work, wait for what is in flight, and only then cancel the listener — otherwise
connections mid-request are dropped rather than finished.
[Serve HTTP](/docs/cookbook/http-service/#stopping-a-service) has the code.

Without it, a bounded wait over a body with an uninterruptible tail could not be honored. That is the failure it exists for: every other way out of a handle waits, letting the binding go included, so one finalizer that never returns holds its nursery, which holds its parent, up to `main`. Reach for it when a bounded wait matters more than a clean one, and not otherwise.

## Nurseries

A nursery owns a set of fibers. The capability installed in a nursery body is:

```khora
pub effect Nursery {
  adopt: (Fiber<(), 'er>) -> (),
}
```

The answer is fixed at `()` and the row is not, and each half has its own reason.

The answer is fixed because a nursery has nothing to do with a result it cannot hand back: it holds children as bare handles and waits for them. A fiber whose result matters is one whose handle you keep and `join`.

The row stays for failures: it is how a child that can fail is adopted, and how its failure reaches the nursery. It has nothing to do with stopping a child. A cancellation leaves every function on a return of its own, whatever its row, so a child whose row is empty is stopped exactly like one whose row is not.

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
  raises 'er + ChildFailed
```

Example:

```khora
fn run() -> () raises ChildFailed {
  nursery(fan_out)!
}
```

When the body completes normally, `nursery` waits until every adopted child is finished. If the body leaves by failure or cancellation, releasing the nursery cancels children that are still running and waits for them before the scope is gone.

### What a fiber is made of

A fiber is an operating-system thread. There is a second implementation —
stackful coroutines on a pool of workers — behind an environment variable:

```bash
KHORA_FIBERS=scheduler ./build/myapp
```

Threads are the default because they are faster at the connection counts a
service actually runs at. The coroutine's advantage is *density*: a suspended
fiber costs roughly 4 KB against a thread's 33 KB, which matters when tens of
thousands are waiting rather than working.

**The two answer cancellation alike** — a fiber in `clock.sleep`, on a channel
or on another fiber is woken on either — but they schedule differently, so a
program that depends on an order the language does not promise can tell them
apart. [Known
limitations](/docs/limitations/#the-two-fiber-backends-are-distinguishable) has
the detail.

A thread gets the operating system's stack — two megabytes on Linux, one on
Windows — and a coroutine gets one megabyte with a guard page, so deep
recursion near the old limit may be over the new one. The failure is a clean
fault rather than corruption.

[Fibers](/docs/internals/fibers/) describes how each is scheduled.

### A child that failed

A nursery is a unit: the block asked for these fibers together, so one failing means the group's answer is not coming. The first failure cancels the siblings still running when the nursery sees it; every child is still waited for, and the nursery raises

```khora
pub type ChildFailed = { children: Int };
```

A count rather than the child's own error, because `adopt` binds the row per adoption — two children may fail with two unrelated types and there is no one value to hand back. A child the nursery *cancelled* is not counted: that is what a nursery does to its children, not something that went wrong.

**A failure is seen in adoption order.** A nursery reaps handles oldest-first, so a child's failure is not seen until every child adopted before it has finished, and it cancels only the siblings still running at that moment. A failing child adopted first cancels all its siblings within milliseconds; one adopted in the middle of twelve cancelled between two and six of the eleven in measurement; one adopted last is seen after every sibling has finished and cancels none. Every child is waited for and the failure is always reported. When a sibling's failure has to stop work that is expensive, holds a resource, or has an effect outside the process, adopt the child that can fail first, or have that work check a `Shared` flag itself. [Known limitations](/docs/limitations/#a-childs-failure-is-seen-late-unless-it-was-adopted-early) has the numbers.

The body may be a named function or a lambda. A lambda resolves its capabilities where it is written, and as the argument to `nursery` that is inside the row `nursery` installs, so `nursery(fan_out)` and `nursery(fn () => fan_out())` mean the same thing.

### An operation can be generic in a row, but not in a type

`adopt` binds `'er` and cannot bind an answer type, which is why `Fiber<(), 'er>` fixes the answer and leaves the failure row free. That asymmetry is a rule about every effect operation rather than about nurseries: [Effects and rows](/docs/reference/effects/#an-operation-may-be-generic-in-a-row-but-not-in-a-type) has the reason, with `adopt` as its example.

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
  raises 'er + ChildFailed
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

Adopting past a bounded nursery's limit waits for older work to finish. Use this for work whose arrival rate is controlled externally so overload becomes backpressure instead of unbounded growth. A fiber cancelled while it waits for room passes the cancel to the child it is waiting on, and stops as soon as that child has.

Two riders on the number, both measured rather than intended:

- **The limit admits `limit + 1` live children.** `Fiber::spawn` *starts* the child and `nursery.adopt` is what blocks, so by the time the bound is applied the extra work is already running. `bounded_nursery(128, serve)` above is 129 live children. Subtract one where the limit stands for a real resource such as a connection pool.
- **A limit of zero or less means no limit**, because that is how the unbounded `nursery` is built. A limit computed from configuration that comes out zero removes the bound rather than clamping to one; check it before you pass it.

Use unbounded `nursery` when the fan-out is already bounded by data the program holds, such as a known handful of independent tasks.

## Cancellation

Cancellation belongs to the target fiber:

```khora
let child = Fiber::spawn(fn () => work());
Fiber::cancel(child);
Fiber::wait(child)!;
continue_parent();
```

`wait` rather than `join`, because a cancelled fiber has no answer: `join` on
one unwinds the joiner along with it, and `continue_parent()` would never run.
`Fiber::outcome` is the third choice, when the answer is wanted if there is
one: it returns `Outcome::Answered(value)` or `Outcome::Stopped` without
unwinding the caller.

`wait` is itself a cancellation point: a waiter cancelled while it is parked
stops there. It needs no `raises` row for that.

Cancelling a child does not cancel its parent.

### When cleanup does not finish

A cancelled fiber runs its cleanup — its regions' finalizers — to completion,
and a second `Fiber::cancel` does not cut it short. A finalizer that blocks for
ever therefore holds its fiber, and anything waiting on it, for ever. Two
operations end that:

```khora
Fiber::abort(child);              // stop now, even inside cleanup
Fiber::cancel_within(child, 5000); // cancel now; abort if still running in 5 s
Fiber::wait(child);
```

`abort` stops the fiber at its next cancellation point **including inside a
finalizer**, and the children of any nursery it holds with it. What it costs
is cleanup cut off part-way: a `ROLLBACK` not sent, a connection dropped
rather than returned. Khora has no default grace period, so nothing aborts a
fiber unless the program asks. `cancel_within` is the usual way to ask: it
bounds how long cleanup may take, and a fiber that finishes before the
deadline is not aborted.

Two things `abort` does not interrupt: a single foreign (C) or file-system
call already in progress, which finishes first; and a change function running
under `Shared::update` or `Shared::modify`, which holds the cell's lock and
runs to its end. A blocking call inside one gives up at once instead; a
`Fiber::join` or `wait` inside one that comes back stopped ends it with the
cell unchanged. Inside a finalizer, a plain cancel does not end such a
`join` or `wait` — it waits for the child, and the rest of the finalizer runs —
and `abort` does. [Sharing](/docs/reference/sharing/) has the detail.

**There is no `timeout`, no `race` and no `select`.** A deadline can be built by
hand — [Timeouts and cancellation](/docs/cookbook/timeouts-and-cancellation/)
has the shape, and it works because cancelling a fiber cancels the children of
any nursery it holds. A race is harder: a parent cancelled while it waits stops
only once the child it waits on has stopped, so a hand-written race is bounded
by how quickly the losing branch reaches its next cancellation point.
[Known limitations](/docs/limitations/#concurrency-combinators) has the
measurements.

What does work is waiting on handles: [Take work off a queue safely](/docs/cookbook/taking-work-off-a-queue/) and [Bound concurrent work](/docs/cookbook/bounded-concurrency/) are the nearest recipes, and [known limitations](/docs/limitations/) is the page to check before assuming an operation exists.

Cancellation is observed at cancellation points rather than between arbitrary source instructions. They are:

- a `!` site, which is also where propagation and suspension are marked;
- a **loop back-edge** — the point where `loop` or `while` goes round again;
- a **call** to a function that can itself reach a cancellation point; and
- a **blocking operation**: `Channel::send`, `Channel::receive`,
  `Fiber::wait`, `Fiber::join`, `clock.sleep`, and accepting, reading and
  writing on a network connection. A cancelled read, write or accept gives up
  and the fiber stops there; it does not come back to the caller as a failed
  read. Opening a connection and waiting for a child process are not
  cancellation points: a fiber stops after the call returns.

Every function has them, whatever its `raises` row says. A cancellation leaves
a function on a return of its own, not on the error channel.

A `!` observes a pending cancellation *before* the call it marks, so a computation already asked to stop does not do work it is about to throw away, and the arguments are not evaluated.

**A value the fiber is already holding is discarded with it.** Between the point a value becomes the fiber's responsibility and the next cancellation point after that, nobody else knows the fiber has it — so a cancellation there drops it, cleanly and silently. The shape that meets this is a worker taking a job off a channel and then calling something with it. Register the value with a region before that call and the region's finalizer runs on the unwind; [Take work off a queue safely](/docs/cookbook/taking-work-off-a-queue/) is the recipe.

Reading the flag after the call instead would move the problem rather than remove it: the fiber would be holding the call's result at the same point. Work in flight across a cancellation boundary is at risk whichever side the boundary is read on.

The back-edge is why an ordinary background worker can be stopped:

```khora
fn reporter() -> () with { clock: Clock } {
  loop {
    clock.sleep(5000);
    report();
  }
}
```

There is no `!` and no `raises` row. A fiber running it stops at the loop, at
the sleep, or inside `report`, whichever it reaches first.

A blocked operation is woken by a cancellation on both backends, so a fiber
parked in `clock.sleep`, on an empty channel or on another fiber stops without
waiting out the wait. A single foreign (C) call or file-system call is not
interrupted: the fiber finishes it and stops at the next cancellation point
after it.

The check on the two channel operations comes *after* the call rather than before it, and only when the call comes back **empty-handed**. The runtime looks at the cancellation flag only once it has established there is nothing to take and no room to send, so a value arriving at the same moment as the cancellation is delivered rather than discarded — a cancelled receive is never holding a value nobody will see again. A send that gives up releases its value, the same as a send to a closed channel.

### What cancellation costs

A function that can reach a cancellation point returns a small tag alongside
its answer, and its caller checks it after the call. A function that reaches
none — straight-line arithmetic, a field read — is compiled without one and
cannot be stopped part-way, which is the point: there is nothing in it to stop
at. A function that recurses -- by name, through a closure or a
function-typed field, or through a function it hands to `attempt` or another
`std` operation that calls it -- checks for a cancellation when it is
entered, so deep recursion stops too.

Cancellation is **not** a member of a `raises` row. A `catch` that handles every declared failure, `_` included, does not see a cancellation: it passes through.

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

A value captured by or handed to concurrent work has to satisfy the sharing
rules, which are their own page: [Sharing](/docs/reference/sharing/).
