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
  pub fn wait(self) -> () raises 'er;
}
```

`A` must be `Share`: the value is computed on one fiber and read on another.
So must every error in `'er`, for the same reason: `join` hands the error the
body raised to whoever joins, and a handle can be joined from more than one
fiber. A record with a `mut` field, an `Array` or a `Map` in a spawned body's
`raises` row is refused at the spawn, and a `Region` or `Scope` there is
refused with the message that names `scoped`. `nursery.spawn` checks its body
by the same rules, at the call that starts it.
The error of a `test` block is not held to this: the test runner reports it and
releases it, and it reaches no other fiber of the program.

`wait` waits without taking the answer, which is what you need after `cancel`: a
canceled fiber has no answer, so `join` on one unwinds the joiner along with it,
and `wait` on one returns. A child that *failed* is different: `wait` raises its
error, as `join` does, so `Fiber::wait(child)!` passes it on and a `catch`
around the wait names it. That includes a child that failed before a `cancel`
reached it: the cancellation came too late to count, and the failure stands.

`join` waits and answers what the body answered. If the body raised, `join` re-raises with the same type, so the caller catches by name:

```khora
module example;

import std::core::{Fiber, print};

type LoadError = | Timeout | Missing(id: Int);

fn load(id: Int) -> Int raises LoadError {
  if id < 0 { raise LoadError::Missing(id) } else { id * 10 }
}

fn run(id: Int) -> () {
  let child = Fiber::spawn(fn () => load(id)!);
  let total = Fiber::join(child)! catch {
    LoadError::Timeout => 0,
    LoadError::Missing(_id) => 0,
  };
  print(Int::to_string(total))
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

Releasing the final `Fiber` handle also waits for the child. This means a fiber cannot silently outlive the scope that still owns its handle. If the child raised and nothing took the error — no `join`, no `outcome`, no `wait` that returned the error — the release also prints `khora: a fiber ended with an error nobody was waiting for` on standard error, once; a `join` or `wait` that hands the error back prints nothing. [Known limitations](/docs/limitations/#concurrency-combinators) lists every case.

`Fiber::detach` is the exception, and the only one: it stops waiting and asks the fiber to stop. Both halves are asynchronous — `detach` signals and returns at once, so the fiber keeps running until it reaches its next cancellation point rather than stopping where it is. Its answer is discarded when it arrives, and a later failure is silent — the program said it was no longer listening. It cancels as well as detaching, because a detached fiber nobody asked to stop is a leak with a nicer name.

**Canceling a fiber running `Router::listen` works.** It was a known gap until
the runtime learned to check for cancellation in the poll loop a parked
`accept` sits in: a canceled listener now unwinds and the process exits.
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
  spawn: (() -> () raises 'er) -> (),
}
```

`nursery.spawn(body)` starts `body` on a fiber of its own, as a child of the nursery. The nursery admits the child **before** it starts, which is what lets a bounded nursery's limit be exact (below). The body is checked the way `Fiber::spawn`'s is: it is written at the call, as a lambda or a named function, and it may close over nothing another fiber cannot hold.

The answer is fixed at `()` and the row is not, and each half has its own reason.

The answer is fixed because a nursery has nothing to do with a result it cannot hand back: it holds children as bare handles and waits for them. A fiber whose result matters is one whose handle you keep and `join`.

The row stays for failures: it is how a child that can fail is spawned, and how its failure reaches the nursery. It has nothing to do with stopping a child. A cancellation leaves every function on a return of its own, whatever its row, so a child whose row is empty is stopped exactly like one whose row is not.

`'er` is quantified per call rather than per handler, so children raising unrelated failures are spawned by one nursery. A body that starts children declares the requirement and spawns each one; the child's body may raise, and no `catch` is needed where it is spawned:

```khora
fn fan_out() -> ()
  with { nursery: Nursery }
{
  nursery.spawn(fn () => first());
  nursery.spawn(fn () => second()!);
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

When the body completes normally, `nursery` waits until every child is finished. If the body leaves by failure or cancellation, releasing the nursery cancels children that are still running and waits for them before the scope is gone.

A child that acquires something is given a scope of its own, because a `Scope` stays on the fiber that opened it and a child cannot be handed its parent's. Open it in the child:

```khora
fn accept_loop(connection: Int) -> ()
  with { scope: Scope, nursery: Nursery }
{
  nursery.spawn(fn () => scoped(fn () => serve(connection)));
}
```

What the child acquires is released when its `scoped` ends, on the child, on every way out including its cancellation. `serve` uses the scope `scoped` hands it, not `accept_loop`'s own. [Sharing](/docs/reference/sharing/#regions-stay-home) has the rule.

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

A fiber gets an eight-megabyte stack on either backend — what `main` gets
from a default `ulimit -s` on Linux and macOS — so a recursion that fits in
`main` fits in a fiber, and the backend does not change how deep a program may
go. The stack is address space reserved up front and memory committed a page
at a time as it is touched, so a fiber that never recurses deeply is resident
for the few kilobytes it used. Running out is reported as in
[Traps](/docs/reference/traps/#running-out-of-stack); there is no setting that
makes a fiber's stack larger.

[Fibers](/docs/internals/fibers/) describes how each is scheduled.

### A child that failed

A nursery is a unit: the block asked for these fibers together, so one failing means the group's answer is not coming. Each failure cancels the siblings still running at the moment it happens; every child is still waited for, and the nursery raises

```khora
pub type ChildFailed = { children: Int };
```

A count rather than the child's own error, because `spawn` binds the row per call — two children may fail with two unrelated types and there is no one value to hand back. A child the nursery *canceled* is not counted: that is what a nursery does to its children, not something that went wrong.

**A failure is acted on when it happens, not when the nursery gets round to that child.** The failing child cancels its siblings itself, at the moment it fails, so a child spawned last stops a sibling spawned first and asleep for a second within milliseconds. Each sibling stops at its next cancellation point and runs its finalizers on the way out; one with no cancellation point runs to its end, and is still waited for. A body that keeps calling `spawn` after a failure starts those children: a failure cancels what is running, not what comes next, and the next failure cancels those. [Known limitations](/docs/limitations/#nursery-cancellation-is-cooperative) has the rest.

**A child's failure is reported once, as `ChildFailed`**, not also as the `nobody was waiting for` line a fiber's unjoined failure prints. If the body raises something else first, or is canceled, there is no `ChildFailed` to carry it, and the nursery prints that line once per failed child as it ends.

The body may be a named function or a lambda. A lambda resolves its capabilities where it is written, and as the argument to `nursery` that is inside the row `nursery` installs, so `nursery(fan_out)` and `nursery(fn () => fan_out())` mean the same thing.

### An operation can be generic in a row, but not in a type

`spawn` binds `'er` and cannot bind an answer type, which is why its body's type, `() -> () raises 'er`, fixes the answer and leaves the failure row free. That asymmetry is a rule about every effect operation rather than about nurseries: [Effects and rows](/docs/reference/effects/#an-operation-may-be-generic-in-a-row-but-not-in-a-type) has the reason.

### Why `spawn` takes the body and not a running fiber

A running fiber can only be counted after it has started, so a bounded nursery handed one could apply its limit only to the next: while the producer waited for room, the child it was holding was already running. Taking the body lets the nursery wait for room first and start the child after.

The body is still checked where it is written. `nursery.spawn(fn () => ..)` checks its lambda as `Fiber::spawn` would; the handler then forwards a closure that has already been checked, and no other closure value can be handed to `Fibers::spawn`.

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
    nursery.spawn(fn () => handle(request)!);
  }
}

bounded_nursery(128, serve)!
```

**At most `limit` children run at once, exactly.** Spawning into a full bounded nursery waits for its oldest running child to finish, and only then starts the new one; `bounded_nursery(128, serve)` above never has a 129th request in flight. Use this for work whose arrival rate is controlled externally so overload becomes backpressure instead of unbounded growth.

A fiber canceled while it waits for room stops there, and the body it was spawning never starts. The children already running are the nursery's: they are canceled when the nursery is released on the way out.

**The limit must be at least 1.** An integer literal below 1 is a compile error. A limit computed at run time that comes out below 1 stops the program with a trap that names the value (`bounded_nursery` needs a limit of at least 1, and this one is 0), before the body runs. The unbounded nursery is `nursery`.

Use unbounded `nursery` when the fan-out is already bounded by data the program holds, such as a known handful of independent tasks.

## Cancellation

Cancellation belongs to the target fiber:

```khora
let child = Fiber::spawn(fn () => work()!);
Fiber::cancel(child);
Fiber::wait(child)! catch { _ => () };
continue_parent();
```

`wait` rather than `join`, because a canceled fiber has no answer: `join` on
one unwinds the joiner along with it, and `continue_parent()` would never run.
The `catch` is there for a child that failed *before* the cancellation reached
it. A cancellation that arrives after the failure changes nothing —
`Fiber::canceled` answers `false` — so `wait` raises the child's error, and
with a bare `!` the parent would leave at the wait. Name the child's errors in
the `catch` instead of `_` to handle them rather than drop them.
`Fiber::outcome` is the third choice, when the answer is wanted if there is
one: it returns `Outcome::Answered(value)` or `Outcome::Stopped` without
unwinding the caller, and it too raises a child's failure.

`wait` is itself a cancellation point: a waiter canceled while it is parked
stops there. It needs no `raises` row for that.

Canceling a child does not cancel its parent.

### When cleanup does not finish

A canceled fiber runs its cleanup — its regions' finalizers — to completion,
and a second `Fiber::cancel` does not cut it short. A finalizer that blocks for
ever therefore holds its fiber, and anything waiting on it, for ever. Two
operations end that:

```khora
Fiber::abort(child);              // stop now, even inside cleanup
Fiber::cancel_within(child, 5000); // cancel now; abort if still running in 5 s
Fiber::wait(child)!;
```

`abort` stops the fiber at its next cancellation point **including inside a
finalizer**, and the children of any nursery it holds with it. Every
finalizer registered on the way out still starts, and runs until the first
loop trip, `!` or blocking call it reaches, in its own body or in a function
it calls; a blocking call stops it there rather than handing it a made-up
answer. Calling a function does not stop it before then. An `abort` that
arrives while a finalizer is already running stops it at its next
cancellation point of any kind.
What it costs is cleanup cut off part-way: a `ROLLBACK` not sent, a
connection dropped rather than returned. Khora has no default grace period, so nothing aborts a
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

### `timeout`, `race` and `Select`

Three calls in `std::core` stop work for you, and each is a cancel underneath
it, so each reaches what a cancel reaches: a `!`, a loop's back-edge, and
every blocking operation listed below. What one cannot reach — a loop with
none of those, a foreign call in progress — runs to its end first.

**`timeout(millis, body)`** runs `body` on the calling fiber and gives up on
it after `millis` milliseconds:

```khora
match timeout(500, fn () => fetch(url)!)! {
  Option::Some(page) => page,
  Option::None => cached(url),
}
```

`Some(answer)` when `body` finishes in time. `None` when the deadline comes
first: `body` is canceled where it is waiting — a sleep, a receive, a read, a
connect — the children of every nursery it opened are stopped, and its
finalizers run before `timeout` returns. A failure raised before the deadline
is raised as itself. Because `body` runs on the calling fiber, its answer
needs no `Share`, and it can use the caller's regions, capabilities and
mutable values.

- **Nested, the earlier deadline wins, and each `None` is its own.** An outer
  deadline that passes while an inner `timeout` is running stops the inner
  body and is the *outer* call's `None`.
- **A cancel of the whole fiber is never a `None`.** `Fiber::cancel`, a
  nursery stopping its children, or a shutdown goes on past every `timeout`
  to where the fiber stops.
- **Fibers it did not open a nursery for go on.** A child that `body`
  spawns into a nursery opened outside the `timeout` belongs to that nursery
  and outlives the deadline, as does a fiber started with `Fiber::spawn`.
- **In cleanup it bounds its own body.** A `timeout` in a finalizer, a
  `claim` abandon handler or a `Shared::update` change function stops its
  body at the deadline and answers `None`, and the rest of that cleanup
  runs. It never cuts short cleanup it did not open; bounding the cleanup
  itself is `Fiber::cancel_within`.
- **`millis` below 1 is `None` without running `body`.**
- The deadline is the runtime's monotonic clock, not the `Clock` capability,
  so a test handler's `sleep` does not move it.

**`race(branches)`** runs each branch on a fiber of its own and answers with
the first to finish:

```khora
let page = race([
  SharedFn::of(fn _ => fetch(primary)!),
  SharedFn::of(fn _ => fetch(mirror)!),
])!;
```

The others are canceled and `race` waits for them, so no branch outlives the
call; a loser in a sleep, a receive, a read or a connect is stopped within
milliseconds of the winner. A branch is a `SharedFn` because it runs on
another fiber, so what it closes over is checked where `SharedFn::of` is
written, as it is for `Fiber::spawn`, and the answer is `Share`. **A failure
is fail-fast**, as a nursery's is: the first branch to finish decides the
race, whether it answered or failed. For the first *success* instead, catch
inside each branch with `attempt` and answer a `Result`. A literal `race([])`
is a compile error; a list that turns out empty at run time stops the program
with a message.

**`Select`** receives from whichever of several channels has a value first:

```khora
let next = Select::new()
  |> Select::on(jobs, fn job => Event::Work(job))
  |> Select::on(control, fn message => Event::Control(message))
  |> Select::within(500)
  |> Select::wait();
match next {
  Selected::Got(event) => handle(event),
  Selected::TimedOut => tick(),
  Selected::Closed => stop(),
}
```

- **One value from one channel per `wait`.** A value that arrives on another
  channel while the waiter is being woken stays in its channel for the next
  receive. Between fibers, waiting selectors and waiting `receive`s share a
  channel's line: the one that started waiting first is served first. A
  waiter on the thread backend has no place in that line; it is woken with
  the others and the first to look takes the value.
- **Fair.** When several channels have a value, each `wait` starts looking at
  a different one, so a channel that is always ready does not starve one
  listed after it.
- **A cancellation point, canceled empty-handed**, as `receive` is: a value
  that was found reaches its branch, and the fiber stops at the first
  cancellation point inside the branch, a call the branch makes included.
- **A closed channel is waited past.** `wait` answers `Closed` only when every
  channel is closed and drained.
- **Receives only.** There is no branch for a send.

A `Select` is rebuilt for each `wait`, which allocates; for one channel,
`receive` is cheaper.

What does work beyond these is waiting on handles: [Take work off a queue safely](/docs/cookbook/taking-work-off-a-queue/) and [Bound concurrent work](/docs/cookbook/bounded-concurrency/) are the nearest recipes, and [known limitations](/docs/limitations/) is the page to check before assuming an operation exists.

Cancellation is observed at cancellation points rather than between arbitrary source instructions. They are:

- a `!` site, which is also where propagation and suspension are marked;
- a **loop back-edge** — the point where `loop` or `while` goes round again;
- a **call** to a function that can itself reach a cancellation point; and
- a **blocking operation**: `Channel::send`, `Channel::receive`,
  `Fiber::wait`, `Fiber::join`, `clock.sleep`, and opening, accepting,
  reading and writing on a network connection, TLS included. A canceled
  connect, read, write or accept gives up and the fiber stops there; it does
  not come back to the caller as a failed one. A connect that is resolving a
  name stops waiting for the lookup, which finishes on its own in the
  background and is thrown away. Waiting for a child process is not a
  cancellation point: a fiber stops after the call returns.

Every function has them, whatever its `raises` row says. A cancellation leaves
a function on a return of its own, not on the error channel.

A `!` observes a pending cancellation *before* the call it marks, so a computation already asked to stop does not do work it is about to throw away, and the arguments are not evaluated.

**A value the fiber is already holding is discarded with it.** Between the point a value becomes the fiber's responsibility and the next cancellation point after that, nobody else knows the fiber has it — so a cancellation there drops it, cleanly and silently. The shape that meets this is a worker taking a job off a channel and then calling something with it. For a channel, take the job with `Channel::claim` (below); for anything else, register the value with a region before that call and the region's finalizer runs on the unwind. [Take work off a queue safely](/docs/cookbook/taking-work-off-a-queue/) is the recipe.

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

The check on the two channel operations comes *after* the call rather than before it, and only when the call comes back **empty-handed**. The runtime looks at the cancellation flag only once it has established there is nothing to take and no room to send, so a value arriving at the same moment as the cancellation is delivered rather than discarded — a canceled receive is never holding a value nobody will see again. A send that gives up releases its value, the same as a send to a closed channel.

### Claiming work: `Channel::claim` and `Channel::offer`

```khora
impl<A: Share> Channel<A> {
  pub fn claim(self, abandon: (A) -> ()) -> Option<Claim<A>>;
  pub fn offer(self, value: A) -> Bool;
}

impl<A: Share> Claim<A> {
  pub fn done(self) -> ();
  pub fn give_back(self, to: Channel<A>) -> Bool;
}
```

`claim` is `receive` for a value that is *work*. It takes a value the same way, waiting while the channel is empty, and registers `abandon` before anything after the take can stop. The value is `claim.value`. The claim is settled by `done`, or by a `give_back` that answered `true`; any other way out of the code holding an unsettled claim (returning, raising, `break`, discarding it, a cancellation, SIGTERM) runs `abandon(value)` once, on this fiber, when the claim's last reference goes.

`offer` is a send that never waits: `false` when the channel is full or closed, and the value is released. It is not a cancellation point. `give_back` is `offer` back onto a channel, so it never waits either; when it answers `false` the claim is still unsettled.

`abandon` runs as cleanup, so it cannot be canceled and should not wait: a `send` to a full channel inside it waits forever, which is why `offer` exists. Under `Fiber::abort` the handler starts and runs until the first loop trip, `!` or blocking call it reaches, so a handler that only records (`Shared::set`) or requeues (`offer`) runs to its end, and a longer one can be cut short.

A claim holds a `Region`, so it stays on the fiber that took it: sending it on a `Channel` or a `Handoff`, or capturing it in a spawned fiber, is refused at compile time. A claim gives at-least-once delivery, not exactly-once, and nothing survives a process that is killed or traps. [Take work off a queue safely](/docs/cookbook/taking-work-off-a-queue/) has the whole list.

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
