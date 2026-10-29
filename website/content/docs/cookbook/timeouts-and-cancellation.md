---
title: Give work a deadline (timeouts and cancellation)
sidebar:
  order: 13
---

`timeout` gives a call a deadline. The body runs on the calling fiber; if it
has not answered when the deadline passes, it is canceled where it is
waiting, the children of every nursery it opened are stopped, its finalizers
run, and `timeout` answers `None`.

```khora
module deadline::main;

import std::core::{ChildFailed, Nursery, Option, nursery, print, timeout};
import std::clock::{Clock};

/// The work. It never finishes on its own.
fn worker() -> () with { clock: Clock } {
  loop {
    clock.sleep(50);
  }
}

fn fan_out() -> () with { nursery: Nursery, clock: Clock } {
  nursery.spawn(fn () => with { clock: Clock::real() } { worker() });
  print("  worker spawned");
}

pub fn main() -> Int raises ChildFailed {
  with { clock: Clock::real() } {
    print("starting work with a 300ms deadline");
    let done: Option<()> = timeout(300, fn () => nursery(fn () => fan_out())!)!;
    match done {
      Option::Some(_) => print("finished in time"),
      Option::None => print("gave up: took too long"),
    };
  };
  0
}
```

```text
starting work with a 300ms deadline
  worker spawned
gave up: took too long
```

The worker inside the nursery is stopped too: a deadline reaches the
children of a nursery opened inside the `timeout`, and the nursery waits for
them before `timeout` returns. A body that finishes early returns at once;
the deadline costs nothing when it is not reached.

### A deadline on work that is already running

`timeout` wraps a call. A fiber somebody else started is given a deadline by
its handle: `Fiber::cancel` it when the time is up and `Fiber::wait` for it.

```khora
let hand = Fiber::spawn(fn () => work());
clock.sleep(millis);
Fiber::cancel(hand);
Fiber::wait(hand)! catch { _ => () };
```

`wait` rather than `join`, because a canceled fiber has no answer to take.
`Fiber::cancel_within(hand, millis)` goes one step further and aborts the
fiber's cleanup if it is still running after `millis`.

## Any work can be interrupted

A canceled fiber stops at its next cancellation point: a loop going round, a
call to a function that has one, or a blocking operation such as a sleep, a
channel receive or a wait. Every function has them, whatever its `raises` row,
so a lambda with no row is stopped as promptly as a named function:

```khora
nursery.spawn(fn () => {
  let mut n = 0;
  loop { n = n + 1; }
});
```

Canceling the fiber holding that nursery stops this loop at its next trip.

What is not interrupted is a single call into foreign (C) code or a
file-system call already in progress, or a wait for a child process: the
fiber finishes the call and stops at the next cancellation point after it.

## Canceling a nursery cancels its children

A fiber whose body is a `nursery` takes its children with it. Canceling the
parent cancels every child it is holding, including the ones it is currently
waiting on, so a fan-out under a deadline stops rather than waiting for workers
nobody told.

This is what stops the worker in the first example: the deadline cancels the
fiber running the nursery, and the nursery cancels its children.

Canceling through a nursery costs what canceling without one costs —
measured at 106 ms against 107 ms for the same worker spawned directly.

## What a canceled fiber does on the way out

Cancellation is not a kill. The fiber unwinds the way a failure does: `scoped`
resources are released, `Region` finalizers run, and a nursery in the unwind
path stops its own children first. [Cancellation-safe
resources](/docs/cookbook/cancellation-safe-resources/) has the resource rules.

A canceled fiber's answer is discarded. `Fiber::join` on one unwinds the
joiner along with it, which is why every example here uses `wait`.

## What this does not give you

- **The first of several is `race` or `Select`, not `timeout`.** `race` runs
  branches on fibers of their own and answers with the first to finish;
  `Select` waits for the first of several channels.
  [Concurrency](/docs/reference/concurrency/) has both.
- **A canceled fiber is not stopped at an arbitrary instruction.** It stops at
  its next cancellation point, which is why a worker that spends 200 ms inside
  one infallible call takes up to 200 ms to notice.
- **`Router::listen` can be canceled**, and the process exits when it is. This
  was a known gap and is fixed; see
  [Concurrency](/docs/reference/concurrency/) — drain first, stop last.
