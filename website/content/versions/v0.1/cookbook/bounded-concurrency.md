---
title: Bounded concurrency
sidebar:
  order: 3
---

Structured concurrency tells you who owns concurrent work. `bounded_nursery` adds an admission limit: once the nursery has `limit` live children, adopting another child waits until capacity is available.

Use this when the amount of work is controlled by the outside world or can otherwise grow beyond the capacity of a database, remote service, filesystem, or memory budget.

## Complete example

This program has 1,000 jobs available but allows only 64 of them to be live children at once:

```khora
module main;

import std::core::{ChildFailed, Fiber, Nursery, bounded_nursery, print};

fn handle(job: Int) -> () {
  print("processing job ${job}");
}

fn launch_jobs() -> ()
  with { nursery: Nursery }
{
  let mut next = 0;

  while next < 1000 {
    let job = next;

    nursery.adopt(
      Fiber::spawn(fn () => handle(job))
    );

    next = next + 1;
  }
}

pub fn main() raises ChildFailed {
  bounded_nursery(64, launch_jobs)!
}
```

`launch_jobs` requires a `Nursery` capability because it adopts children. `bounded_nursery(64, launch_jobs)!` supplies that capability and does not return until all adopted children have finished.

The `!` is there because a nursery raises [`ChildFailed`](/docs/reference/concurrency/#a-child-that-failed) when a child fails: the first failure cancels the siblings and the block's answer does not arrive.

The important line is not the `Fiber::spawn`; it is the adoption:

```khora
nursery.adopt(Fiber::spawn(fn () => handle(job)));
```

When 64 children are already live, the next adoption waits. The producer therefore slows down at the same boundary where it creates more work instead of filling an unbounded queue somewhere else.

`adopt` takes a `Fiber<(), 'er>`. The answer is fixed at `()` — a nursery has nothing to do with a result it cannot hand back — but the failure row is free, so a job that fails needs no `catch` at the adoption site:

```khora
nursery.adopt(Fiber::spawn(fn () => handle(job)!));
```

That row is what leaves the child cancellable: a cancellation travels out on the same tagged return an error does, so a child with an empty row would have no channel to be stopped on. The cost is that a child's failure is reported at runtime rather than caught at compile time, and that is the price of children that can be stopped.

Keep a job's answer by holding its handle instead of adopting it. `Fiber::join` gives back what the body computed, and re-raises what it raised.

## Keep unrelated limits separate

A service can legitimately have different limits for different resources. For example, it might accept many mostly-idle HTTP connections while allowing only a smaller number of database operations to compete at once. Put the bounded nursery around the work controlled by the constrained resource rather than inventing one global fiber limit.

For a known, already-bounded handful of tasks, use an ordinary `nursery` instead. If you have three independent lookups, the collection itself already bounds the fan-out; adding another limit usually adds machinery without changing behavior.

## Failure and cancellation

The nursery owns its adopted children. On normal return it waits for them. If the nursery body leaves through failure or cancellation, children that are still running are cancelled and joined before the nursery is released.

That ownership rule is why bounded concurrency remains structured rather than becoming a semaphore wrapped around detached tasks.

See [Fibers and nurseries](/docs/reference/concurrency/) for the underlying concurrency model and the [Concurrency reference](/docs/reference/concurrency/) for exact signatures.
