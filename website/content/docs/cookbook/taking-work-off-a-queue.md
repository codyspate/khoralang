---
title: Take work off a queue safely
sidebar:
  order: 4
---

A worker pool is a fiber that reads a job from a channel and does it. The obvious loop loses jobs, silently, and exits 0 while doing it.

```khora
loop {
  match Channel::receive(jobs) {
    Option::None => break,
    Option::Some(job) => serve(job)!,   // the hole
  }
}
```

`Channel::receive` has already emptied that slot. The call to `serve` is a cancellation point, and a cancellation that arrived in between is taken *before* the call runs, so the job is in nobody's hands. It is not served and not in the queue. Nothing is printed and the fiber unwinds cleanly.

Measured over two hundred canceled rounds, on both fiber backends, that loop took between 196 and 200 jobs out of the channel that it never accounted for.

## Take a claim instead

```khora
loop {
  match Channel::claim(jobs, fn job => abandon(job)) {
    Option::None => break,
    Option::Some(c) => {
      serve(c.value)!;
      c.done();
    },
  }
}
```

`Channel::claim` takes a job the way `receive` does, and registers the function you give it before anything after the take can stop. The claim is yours until you settle it:

- `c.done()` says the job was handled. Call it after the last `!` of the work, because a cancel that lands between the work finishing and `done` makes the claim unsettled, and the job is handed to `abandon` again.
- `c.give_back(queue)` puts the job back on a channel without waiting. It answers `false` when that channel is full or closed; the claim is then still unsettled, and `abandon` runs when it is let go.
- Anything else runs `abandon(job)` once, on this fiber: returning, raising, `break`, discarding the claim with `let _ =`, a cancellation, and the first SIGTERM or Ctrl-C, which cancel the whole program.

The handler is a required argument, so a worker cannot take a job without saying what happens to it if the work does not finish. What it should do is the application's decision: put the job back, write it to a dead-letter queue, or record that it was dropped.

**Keep the handler to recording or requeuing, with `Channel::offer` and `Shared::set`.** It runs as cleanup, which has two consequences:

- A cleanup cannot be canceled, and that also means it cannot wait: a `Channel::send` to a full queue inside one waits forever. `Channel::offer` never waits. It answers `false` when there is no room, and the handler can then record the job somewhere else.
- `Fiber::abort`, and `cancel_within` once its deadline passes, start the handler, which runs until the first loop trip, `!` or blocking call it reaches. A handler made of `offer` and `Shared::set` has none and runs to the end; one with a loop or a wait may be cut off partway.

```khora
let requeue = fn job => {
  if !Channel::offer(jobs, job) { Shared::set(dropped, job); };
};
```

A claim gives *at-least-once* delivery. Work that finished but was canceled before `done` is abandoned, and if the handler requeues it, it runs again. Make the work idempotent.

## Complete example

```khora
module worker::main;

import std::core::{Channel, Claim, Fiber, Option, Shared, print};

pub type Halt = | Now;

fn serve(job: Int, stop: Channel<Int>) -> () raises Halt {
  // Stands in for real work: waits until it is told to stop.
  let _ = Channel::receive(stop);
}

fn worker(
  jobs: Channel<Int>,
  started: Channel<Int>,
  stop: Channel<Int>,
  served: Shared<Int>,
  requeued: Shared<Int>,
) -> () raises Halt {
  loop {
    match Channel::claim(jobs, fn job => {
      if Channel::offer(jobs, job) { Shared::set(requeued, Shared::get(requeued) + 1); };
    }) {
      Option::None => break,
      Option::Some(c) => {
        let _ = Channel::send(started, c.value);
        serve(c.value, stop)!;
        c.done();
        Shared::set(served, Shared::get(served) + 1);
      },
    }
  }
}

pub fn main() -> () {
  let jobs: Channel<Int> = Channel::bounded(4);
  let started: Channel<Int> = Channel::bounded(1);
  let stop: Channel<Int> = Channel::bounded(1);
  let served = Shared::of(0);
  let requeued = Shared::of(0);
  let _ = Channel::send(jobs, 1);
  let _ = Channel::send(jobs, 2);

  let hand = Fiber::spawn(fn () => worker(jobs, started, stop, served, requeued)!);
  // Cancel the worker in the middle of its first job.
  let _ = Channel::receive(started);
  Fiber::cancel(hand);
  Fiber::wait(hand)! catch { _ => () };

  print("served ${Shared::get(served)}, requeued ${Shared::get(requeued)}, queued ${Channel::depth(jobs)}");
}
```

It prints `served 0, requeued 1, queued 2`: the job that was in the worker's hands is back in the queue next to the one it never took.

**Assert the reconciliation in a real pool:** every job taken equals the jobs served plus the jobs abandoned. Count *taken* from the producer's side (sent, minus what is left in the queue), not in the worker, because a worker that loses a job is also the code that would fail to count it. Over the same two hundred canceled rounds as above, `claim` took 351 to 396 jobs per run and accounted for every one, on both backends. A thousand rounds of a cancel delivered right after the take lost none either, where the plain loop lost all of them on one backend and two thirds on the other.

## What a claim does not cover

- **SIGKILL, running out of memory, power loss.** No cleanup runs, and a job in an in-memory channel is gone. Only an external queue with leases survives this.
- **A trap** (overflow, `todo()`, an index out of range). It ends the process at status 134 without running cleanup.
- **A long handler under `abort`.** See above: the handler starts, and anything past its first loop trip, `!` or wait may not run.
- **A foreign call that never returns.** It is not a cancellation point. The claim is abandoned at the next one after the call returns, so if it never does, nothing runs.
- **A claim kept somewhere long-lived.** One stored in a list or a record is abandoned when its last reference goes, which can be much later. One held by a reference cycle is never abandoned.
- **A row claimed in a database.** That needs a lease in the database itself: an expiry, and an owner checked when the work is acknowledged. A claim can release such a lease promptly on a cancel, but it cannot replace one, because the hazard that matters is the process dying.

A claim cannot be sent to another fiber, through a `Channel` or a `Handoff`, or captured by a spawned fiber: it holds a `Region`, and a region stays on the fiber that opened it. The compiler refuses all three.

## If you cannot use `claim`

The same guarantee can be written by hand, for a value that does not come from a channel's `receive`, or one that needs more than the job in its handler. The registration has to come **immediately** after the take, with nothing in between:

```khora
Option::Some(job) => {
  let settled = Shared::of(false);
  let region = Region::open();
  Region::defer(region, fn () => {
    if !Shared::get(settled) { abandon(job); };
  });
  serve(job)!;
  Shared::set(settled, true);
}
```

`Region::open`, `Shared::of` and `Region::defer` cannot stop, so there is no cancellation point between the receive and the registration. **Nothing else may go there**, not even a counter: `Shared::update` hands a function to the runtime and is a cancellation point. An earlier version of this page bumped a `took` counter with `Shared::update` before `Region::open`, and in a design round's measurement that version lost 5 jobs in 200 rounds on threads. Calling a helper that does the registration has the same problem, because a function that takes a function value can stop as it is entered; that lost 13 in 200.

Over two hundred canceled rounds this pattern, written exactly as above, lost no job on either backend.

## Why the check is before the call and not after

A `!` reads the cancellation flag before the call it marks, so a computation already asked to stop does not evaluate arguments or do work it is about to throw away. The cost is exactly the case above.

Checking afterwards instead would not remove the problem, only move it: the fiber would then be holding the call's *result* when it unwound. Work in flight across a cancellation boundary is at risk whichever side the boundary is read on, and a region is what makes it recoverable.

`Channel::send` and `Channel::receive` are the exception, and are safe: they are cancellation points themselves, and the runtime looks at the flag only once it has established there is nothing to take and no room to send. A canceled receive is never holding a value. That is what lets `claim` register its handler in time. See [Concurrency](/docs/reference/concurrency/).

## See also

- [Make resource cleanup cancellation-safe](/docs/cookbook/cancellation-safe-resources/): the same mechanism for a resource rather than a job.
- [Bound concurrent work](/docs/cookbook/bounded-concurrency/): how many workers, and what happens when they are all busy.
