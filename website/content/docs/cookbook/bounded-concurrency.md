---
title: Bound concurrent work
sidebar:
  order: 5
---

Structured concurrency tells you who owns concurrent work. `bounded_nursery` adds an admission limit: once the nursery has `limit` running children, spawning another waits until one of them finishes, and the new child starts only then.

Use this when the amount of work is controlled by the outside world or can otherwise grow beyond the capacity of a database, remote service, filesystem, or memory budget.

## Complete example

This program has 1,000 jobs available but allows only 64 of them to be live children at once:

```khora
module main;

import std::core::{ChildFailed, Nursery, bounded_nursery, print};

fn handle(job: Int) -> () {
  print("processing job ${job}");
}

fn launch_jobs() -> ()
  with { nursery: Nursery }
{
  let mut next = 0;

  while next < 1000 {
    let job = next;

    nursery.spawn(fn () => handle(job));

    next = next + 1;
  }
}

pub fn main() raises ChildFailed {
  bounded_nursery(64, launch_jobs)!
}
```

`launch_jobs` requires a `Nursery` capability because it spawns children. `bounded_nursery(64, launch_jobs)!` supplies that capability and does not return until all of its children have finished.

The `!` is there because a nursery raises [`ChildFailed`](/docs/reference/concurrency/#a-child-that-failed) when a child fails: every child is waited for, the failure is reported, and the block's answer does not arrive. The failing child cancels its siblings as it fails — see [Failure and cancellation](#failure-and-cancellation) below for what that does and does not stop.

The line that applies the limit is the spawn:

```khora
nursery.spawn(fn () => handle(job));
```

When 64 children are already running, the next `spawn` waits, and the job does
not start until there is room. The producer therefore slows down at the same
boundary where it creates more work instead of filling an unbounded queue
somewhere else.

**The limit is exact.** `bounded_nursery(64, ...)` never runs a sixty-fifth
child, so a nursery in front of a connection pool of 64 is
`bounded_nursery(64, ...)`.

**The limit must be at least 1.** `bounded_nursery(0, ...)` is a compile
error, and a limit computed at run time that comes out below 1 stops the
program with a message naming the value. For no limit at all, use
[`nursery`](/docs/stdlib/api/core/#nursery).

The body passed to `spawn` returns `()`, because a nursery has nothing to do with a result it cannot hand back, but it may raise, so a job that fails needs no `catch` where it is spawned:

```khora
nursery.spawn(fn () => handle(job)!);
```

The row is for failures only. Every child can be canceled whatever its row; the row is what lets a child that can fail be spawned, with its failure reported to the nursery at runtime as `ChildFailed` rather than caught at compile time.

The body is checked the way `Fiber::spawn`'s is: write it at the call, as a lambda or a named function, and it may not close over anything another fiber cannot hold.

Keep a job's answer with `Fiber::spawn` and its handle instead. `Fiber::join` gives back what the body computed, and re-raises what it raised.

## Keep unrelated limits separate

A service can legitimately have different limits for different resources. For example, it might accept many mostly-idle HTTP connections while allowing only a smaller number of database operations to compete at once. Put the bounded nursery around the work controlled by the constrained resource rather than inventing one global fiber limit.

For a known, already-bounded handful of tasks, use an ordinary `nursery` instead. If you have three independent lookups, the collection itself already bounds the fan-out; adding another limit usually adds machinery without changing behavior.

## Failure and cancellation

The nursery owns its children. On normal return it waits for them. If the nursery body leaves through failure or cancellation, children that are still running are canceled and joined before the nursery is released.

**A child that fails cancels its siblings when it fails**, whichever order
they were spawned in. Cancellation is cooperative: a sibling stops at its next
cancellation point, and one without any keeps working until it ends, and is
still waited for.

A failure cancels the children running at that moment, and every failure
does; it does not stop the body. A body that keeps calling `spawn` after a
failure starts those children, and a bounded nursery admits them as room
frees up, so have the producer stop on its own when a fault means the rest
of the work is wasted.
Every admitted child is waited for, and `ChildFailed` reports how many failed.

[Known limitations](/docs/limitations/#nursery-cancellation-is-cooperative)
describes the remaining constraints.

That ownership rule is why bounded concurrency remains structured rather than becoming a semaphore wrapped around detached tasks.

See [Concurrency](/docs/reference/concurrency/) for the model underneath this and for the exact signatures, and [`Nursery` in `std::core`](/docs/stdlib/api/core/#nursery) for the declarations.
