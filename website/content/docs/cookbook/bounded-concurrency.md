---
title: Bound concurrent work
sidebar:
  order: 5
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

The `!` is there because a nursery raises [`ChildFailed`](/docs/reference/concurrency/#a-child-that-failed) when a child fails: every child is waited for, the failure is reported, and the block's answer does not arrive. Whether it stops the siblings depends on where the failing child was adopted — see [Failure and cancellation](#failure-and-cancellation) below before you rely on it.

The important line is not the `Fiber::spawn`; it is the adoption:

```khora
nursery.adopt(Fiber::spawn(fn () => handle(job)));
```

When 64 children are already live, the next adoption waits. The producer
therefore slows down at the same boundary where it creates more work instead of
filling an unbounded queue somewhere else.

**Subtract one when the limit stands for a real resource.** `bounded_nursery(64, ...)`
admits **65** live children, because `Fiber::spawn` starts the child before
`adopt` blocks on the limit — the fiber the producer is currently handing over
is already running. It does not matter when the limit is a rate you picked; it
matters a great deal when it is a connection pool of exactly 64, where the
sixty-fifth child is the one that waits on a connection that will never come
free. Write `bounded_nursery(63, ...)` for a pool of 64.

**A limit of zero or less is not a bound at all** — it is how the unbounded
[`nursery`](/docs/stdlib/api/core/#bounded_nursery) is built — so check a limit
that was computed from configuration before you pass it.

`adopt` takes a `Fiber<(), 'er>`. The answer is fixed at `()` — a nursery has nothing to do with a result it cannot hand back — but the failure row is free, so a job that fails needs no `catch` at the adoption site:

```khora
nursery.adopt(Fiber::spawn(fn () => handle(job)!));
```

The row is for failures only. Every child can be cancelled whatever its row; the row is what lets a child that can fail be adopted, with its failure reported to the nursery at runtime as `ChildFailed` rather than caught at compile time.

Keep a job's answer by holding its handle instead of adopting it. `Fiber::join` gives back what the body computed, and re-raises what it raised.

## Keep unrelated limits separate

A service can legitimately have different limits for different resources. For example, it might accept many mostly-idle HTTP connections while allowing only a smaller number of database operations to compete at once. Put the bounded nursery around the work controlled by the constrained resource rather than inventing one global fiber limit.

For a known, already-bounded handful of tasks, use an ordinary `nursery` instead. If you have three independent lookups, the collection itself already bounds the fan-out; adding another limit usually adds machinery without changing behavior.

## Failure and cancellation

The nursery owns its adopted children. On normal return it waits for them. If the nursery body leaves through failure or cancellation, children that are still running are cancelled and joined before the nursery is released.

**A child's own failure stops only the siblings still running when the
nursery sees it.** A nursery reaps handles oldest-first, so a failure is not
seen until every child adopted before it has finished. A failing child adopted
first cancels all its siblings within milliseconds; one adopted last is seen
after they have all finished, and cancels none.

A bounded nursery also goes on admitting and starting new children after a
failure has been recorded, so a limit of 64 over 1,000 jobs does not mean the
run stops near the failure.

What does hold is the other half: every child is waited for and `ChildFailed`
is reported, never lost.

So where a job is expensive, holds a connection, or has an effect outside the
process, have the job check a `Shared` flag itself rather than expecting the
group to collapse. [Known
limitations](/docs/limitations/#what-a-nursery-actually-does) has the
measurements.

That ownership rule is why bounded concurrency remains structured rather than becoming a semaphore wrapped around detached tasks.

See [Concurrency](/docs/reference/concurrency/) for the model underneath this and for the exact signatures, and [`Nursery` in `std::core`](/docs/stdlib/api/core/#nursery) for the declarations.
