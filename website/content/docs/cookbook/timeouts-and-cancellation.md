---
title: Timeouts and cancellation
sidebar:
  order: 13
---

There is no `timeout` function in the standard library. A deadline is built from
two things you already have: a fiber, and a clock.

```khora
module deadline::main;

import std::core::{print, Fiber, Nursery, nursery};
import std::clock::{Clock};

pub type Timeout = | TookTooLong;

/// The work. It never finishes on its own.
fn worker() -> () with { clock: Clock } raises Timeout {
  loop {
    clock.sleep(50)!;
  }
}

fn fan_out() -> () with { nursery: Nursery, clock: Clock } {
  nursery.adopt(Fiber::spawn(fn () => worker()!));
  print("  worker adopted");
}

/// Runs the fan-out in its own fiber, and gives up on it after `millis`.
fn with_deadline(millis: Int) -> () with { clock: Clock } raises Timeout {
  let hand = Fiber::spawn(fn () => {
    nursery(fn () => fan_out())!;
    ()
  });

  clock.sleep(millis)!;
  print("  deadline reached; cancelling");
  Fiber::cancel(hand);
  Fiber::wait(hand);
  print("  cancelled, and the wait returned");
  raise Timeout::TookTooLong
}

pub fn main() -> Int {
  with { clock: Clock::real() } {
    print("starting work with a 300ms deadline");
    with_deadline(300)! catch { Timeout::TookTooLong => print("gave up: took too long") };
  };
  0
}
```

```text
starting work with a 300ms deadline
  worker adopted
  deadline reached; cancelling
  cancelled, and the wait returned
gave up: took too long
```

The whole program takes 308 ms for a 300 ms deadline.

The shape is: put the work in a fiber, sleep for the deadline, cancel the
handle, `wait` for it, then raise. `wait` rather than `join`, because a
cancelled fiber has no answer to take.

## The work must have somewhere to be interrupted

**A cancellation travels on a `raises` row.** It is observed at a `!`, at a loop
back-edge in a function that can fail, or inside a wait. Work that does none of
those is not interrupted — not because cancellation is unreliable, but because
there is nowhere in it to look.

This is the one that costs people an afternoon, because the failure is a hang
with no message. The worker above is a named function with `raises Timeout`.
Written as an inline lambda instead:

```khora
// Cannot be cancelled: the loop has no failure channel.
nursery.adopt(Fiber::spawn(fn () => {
  let mut n = 0;
  loop { n = n + 1; }
}));
```

`Fiber::cancel` returns, and `Fiber::wait` never does. The fiber was asked to
stop and had no cancellation point at which to notice.

A lambda cannot declare a `with` row or a `raises` row of its own, so the fix is
the shape used above: write the work as a named function that declares
`raises`, and let the lambda call it with `!`.

If the work genuinely cannot fail and must still be stoppable, give it a flag to
read — `Shared<Bool>` — and check it in the loop.

## Cancelling a nursery cancels its children

A fiber whose body is a `nursery` takes its children with it. Cancelling the
parent cancels every child it is holding, including the ones it is currently
waiting on, so a fan-out under a deadline stops rather than waiting for workers
nobody told.

This is what makes the example above work: `with_deadline` cancels one handle,
and the worker inside the nursery stops.

Cancelling through a nursery costs what cancelling without one costs —
measured at 106 ms against 107 ms for the same worker spawned directly.

## What a cancelled fiber does on the way out

Cancellation is not a kill. The fiber unwinds the way a failure does: `scoped`
resources are released, `Region` finalizers run, and a nursery in the unwind
path stops its own children first. [Cancellation-safe
resources](/docs/cookbook/cancellation-safe-resources/) has the resource rules.

A cancelled fiber's answer is discarded. `Fiber::join` on one unwinds the
joiner along with it, which is why every example here uses `wait`.

## What this does not give you

- **No `race` and no `select`.** Waiting on the first of several fibers has to
  be built from a channel the workers send to.
- **A deadline is a sleep, not a scheduler deadline.** The timing fiber sleeps
  the whole interval; it does not wake early because the work finished. For a
  deadline that ends as soon as either side does, have the work signal a channel
  and read the channel with its own timeout fiber.
- **A cancelled fiber is not stopped at an arbitrary instruction.** It stops at
  its next cancellation point, which is why a worker that spends 200 ms inside
  one infallible call takes up to 200 ms to notice.
- **`Router::listen` cannot be cancelled while serving.** See
  [Concurrency](/docs/reference/concurrency/) — drain first, detach last.
