---
title: Fibers
sidebar:
  order: 3
---

A fiber is a Khora computation that can suspend — waiting on a socket, a timer,
a channel — and continue later, while other fibers run.

## What a fiber is made of

Khora ships two implementations of the same interface, and picks one at run
time.

**Threads, by default.** Each fiber is an operating-system thread. Suspending
is the thread blocking; waking is the operating system scheduling it. The
kernel does the work, the stacks are the kernel's, and a blocking foreign call
blocks only the fiber that made it.

**A scheduler, with `KHORA_FIBERS=scheduler`.** Each fiber is a stackful
coroutine on a pool of worker threads, with work stealing. Suspending is a
stack switch in user space, and a fiber can move between workers.

They behave identically except under cancellation: a fiber inside
`clock.sleep` is woken by a cancellation under the scheduler, but runs the
sleep to completion under threads. [Known
limitations](/docs/limitations/#the-two-fiber-backends-are-distinguishable)
has the measurements.

Threads are the default because they are faster at the connection counts a
service actually runs at; the scheduler exists for programs that need far more
concurrent fibers than a machine has threads. [Known
limitations](/docs/limitations/#the-fiber-scheduler) has the measurements.

The remaining difference is cost and density:

| | thread | coroutine |
| --- | --- | --- |
| stack | 1–2 MB, the operating system's | 1 MB with a guard page |
| suspend | a kernel transition | a stack switch |
| how many | thousands | hundreds of thousands |

### What the scheduler does

Each worker has its own queue and takes from the front. A worker with nothing
to do steals half of another worker's queue from the back, so the owner and the
thief contend for the lock but never for the same task. Every so often a worker
checks a shared queue first, so a fiber woken from outside the pool — by the
reactor, or by another thread — is not left waiting behind a worker's own work.

A fiber woken this way can resume on a different worker than it started on.
Nothing a fiber owns is tied to a thread: its identity, its cancellation state
and its current span travel with it. The one exception is foreign code that is
itself thread-affine, which [FFI](/docs/reference/ffi/) covers.

## A nursery is a region

What makes Khora's concurrency *structured* is that a fiber cannot outlive the
block that started it. That is not a separate mechanism — it is
[regions](/docs/reference/memory-and-resources/#region-syntax), which
already run their finalisers on every way out of a block.

A nursery opens a region and installs a `Nursery` capability whose `spawn`
registers each fiber with it. Every path out of the block runs the finalisers,
and those wait for the children: running off the end, an early `return`, a
raise passing through, a cancellation.

There is nothing extra to enforce, and no way to write a fiber that escapes:
the `Nursery` capability is in scope only inside the block, and the block
cannot end while a child is still running.

```khora
nursery(fn () => {
  nursery.spawn(fn () => serve(first));
  nursery.spawn(fn () => serve(second));
})   // both have finished by here, whichever way the block ended
```

### Ending normally and ending badly

A nursery waits for its children on the way out, and cancels them first if it
is leaving because something failed. The distinction is not something the
program asks about: cancellation is idempotent, so "cancel then wait" is
correct on both paths and the normal one simply has nothing to cancel.

## Cancellation is cooperative, at cancellation points

Cancelling a fiber does not stop it where it stands. It sets a flag, and the
fiber notices at its next cancellation point.

There are two:

- a `!` site, which is also where propagation and suspension are marked; and
- a loop back-edge — the point where `loop` or `while` goes round again.

Both exist only in a function that can raise, because the `raises` row is what
a cancellation travels out on. A cancelled fiber unwinds from one of them the
same way a raise does, running each frame's releases and each region's
finalisers as it goes.

This is why a cancelled program closes its files. The back-edge is why the
ordinary shape of a periodic job can be stopped at all:

```khora
fn ticker() with { clock: Clock } raises Stop {
  loop { clock.sleep(200); }
}
```

There is no `!` in that body. Without the back-edge, a nursery that had to
unwind past it would wait for ever.

What neither kind widens is *which* functions have a cancellation point. A
function declared without `raises` has no tagged return for a cancellation to
travel on, so it runs to its end.

## Suspending is not a handler's job

A handler runs and returns; it cannot capture the rest of the computation. All
suspension belongs to fibers, which is what lets handlers be a function call
rather than a stack switch — see
[Effects and handlers](/docs/internals/effects/).

The practical consequence is that `with` blocks and nurseries compose without
either knowing about the other, and a capability handed to a fiber is just a
value it captured.
