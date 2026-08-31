---
title: Take work off a queue safely
sidebar:
  order: 10
---

A worker pool is a fiber that reads a job from a channel and does it. The obvious loop loses jobs, silently, and exits 0 while doing it.

```khora
loop {
  match Channel::receive(jobs)! {
    Option::None => break,
    Option::Some(job) => serve(job)!,   // the hole
  }
}
```

`Channel::receive` has already emptied that slot. The `!` on `serve` is a cancellation point, and a cancellation that arrived in between is taken *before* the call runs — so the job is in nobody's hands. Not served, not in the queue, gone. Nothing is printed and the fiber unwinds cleanly.

Measured over two hundred cancelled rounds, the loop above took 198 jobs out of the channel and accounted for none of them.

## The rule

**A value stops being the queue's and starts being yours at the moment it is received.** Between that moment and the first `!` after it, the fiber owns something nobody else knows about, and a cancellation there discards it.

So register it before the first `!`:

```khora
Option::Some(job) => {
  let settled = Shared::of(false);
  let region = Region::open();
  Region::defer(region, fn () => {
    if !Shared::get(settled) { abandon(board, job.id); };
  });
  serve(board, job)!;
  Shared::set(settled, true);
}
```

`Region::defer` runs on **every** way out of the enclosing block — a normal return, a typed failure, and an unwind from cancellation — which is why the flag is there. A finalizer that counted every job would be counting successes as losses; the flag is what tells it which of the three it is cleaning up after. Set it after the last `!`, where there is no longer a cancellation point between the work finishing and the record of it.

What `abandon` should do is the application's decision — put the job back, mark it for redelivery, write it to a dead-letter queue, or simply record that it was dropped. The point is that something happens, and that the decision is made where the job is taken rather than left to nobody.

## Complete example

```khora
module worker::main;

import std::clock::{Clock};
import std::core::{Channel, Fiber, Option, Region, Shared, attempt, print};

pub type Halt = | Now;

fn serve(done: Shared<Int>) -> () raises Halt {
  let _ = Shared::update(done, fn n => n + 1);
}

fn worker(
  jobs: Channel<Int>,
  took: Shared<Int>,
  done: Shared<Int>,
  abandoned: Shared<Int>,
) -> () raises Halt {
  loop {
    match Channel::receive(jobs)! {
      Option::None => break,
      Option::Some(_job) => {
        let _ = Shared::update(took, fn n => n + 1);
        // Registered before the first `!` below, because after that line this
        // fiber may never run again. The flag is what tells the finalizer
        // whether it is cleaning up after a cancellation or after success.
        let settled = Shared::of(false);
        let region = Region::open();
        Region::defer(region, fn () => {
          if !Shared::get(settled) {
            let _ = Shared::update(abandoned, fn n => n + 1);
          };
        });
        serve(done)!;
        Shared::set(settled, true);
      },
    }
  }
}

pub fn main() -> Int {
  let _ = attempt(fn () => {
    let jobs: Channel<Int> = Channel::bounded(4);
    let took = Shared::of(0);
    let done = Shared::of(0);
    let abandoned = Shared::of(0);

    let hand = Fiber::spawn(fn () => worker(jobs, took, done, abandoned)!);
    Channel::send(jobs, 1)!;
    Channel::send(jobs, 2)!;
    clock.sleep(40);
    Fiber::cancel(hand);
    Fiber::wait(hand);

    print("took ${Shared::get(took)}, served ${Shared::get(done)}, abandoned ${Shared::get(abandoned)}");
  } with { clock: Clock::real() });
  0
}
```

Every job that came off the queue is in exactly one of the last two counts. That reconciliation — *taken equals served plus abandoned* — is the invariant worth asserting in a real pool, because it is the one that catches this class of bug. Over two hundred rounds cut at a jittered moment it held every time; the same two hundred rounds without the region took 198 jobs and accounted for none of them. Without the reconciliation the loss is invisible: the program exits 0 either way.

## Why the check is before the call and not after

A `!` reads the cancellation flag before the call it marks, so a computation already asked to stop does not evaluate arguments or do work it is about to throw away. The cost is exactly the case above.

Checking afterwards instead would not remove the problem, only move it: the fiber would then be holding the call's *result* when it unwound. Work in flight across a cancellation boundary is at risk whichever side the boundary is read on, and a region is what makes it recoverable. That is what regions are for.

`Channel::send` and `Channel::receive` are the exception, and are safe: they are cancellation points themselves, and the runtime looks at the flag only once it has established there is nothing to take and no room to send. A cancelled receive is never holding a value. See [Concurrency](/docs/reference/concurrency/).

## See also

- [Make resource cleanup cancellation-safe](/docs/cookbook/cancellation-safe-resources/) — the same mechanism for a resource rather than a job.
- [Bound concurrent work](/docs/cookbook/bounded-concurrency/) — how many workers, and what happens when they are all busy.
