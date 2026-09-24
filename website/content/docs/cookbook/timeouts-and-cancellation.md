---
title: Give work a deadline (timeouts and cancellation)
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
  Fiber::wait(hand)! catch { ChildFailed => () };
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

### When the work finishing early has to be fast

**That recipe always takes the whole deadline.** The timing fiber sleeps the
full interval whether the work finished in 2 ms or not at all, which is right
when the deadline is a budget you intend to spend, and wrong for a call that
usually returns immediately — a five-second guard on a health check would make
every healthy check take five seconds.

Poll instead, when the common case is early completion:

```khora
let hand = Fiber::spawn(fn () => work());
let started = clock.monotonic_millis();
let mut done = false;
let mut expired = false;
while !done && !expired {
  if Fiber::finished(hand) {
    done = true;
  } else if clock.monotonic_millis() - started > millis {
    expired = true;
  } else {
    clock.sleep(25)!;
  }
};
if expired {
  Fiber::cancel(hand);
  // **Detach as well as cancel.** A fiber blocked in a call with no
  // cancellation point -- a connect to an unroutable host, say -- is not
  // freed by the cancel, and releasing its handle waits for it. `detach`
  // says the program is no longer interested, so its own exit is not held
  // up by work it has already given up on.
  Fiber::detach(hand);
  raise Timeout
} else {
  Fiber::join(hand)!
}
```

The poll interval is the cost: 25 ms means up to 25 ms of latency added to a
fast answer, against a deadline that would otherwise cost its whole length.

## Any work can be interrupted

A cancelled fiber stops at its next cancellation point: a loop going round, a
call to a function that has one, or a blocking operation such as a sleep, a
channel receive or a wait. Every function has them, whatever its `raises` row,
so a lambda with no row is stopped as promptly as a named function:

```khora
nursery.adopt(Fiber::spawn(fn () => {
  let mut n = 0;
  loop { n = n + 1; }
}));
```

Cancelling the fiber holding that nursery stops this loop at its next trip.

What is not interrupted is a single call into foreign (C) code or a
file-system call already in progress, and a blocking connect or a wait for a
child process: the fiber finishes the call and stops at the next cancellation
point after it. A connect to an unroutable host is the common case, which is
why the polling recipe above detaches as well as cancels.

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
- **`Router::listen` can be cancelled**, and the process exits when it is. This
  was a known gap and is fixed; see
  [Concurrency](/docs/reference/concurrency/) — drain first, stop last.
