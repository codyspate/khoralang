# Cancelling a fiber whose body is a nursery never returns

Status: **diagnosed, not fixed.** The fix is a design change and wants your
decision first.

## What happens

```khora
let hand = Fiber::spawn(fn () => body(seen)!);   // body opens a nursery
clock.sleep(100);
Fiber::cancel(hand);      // returns immediately
Fiber::wait(hand);        // never returns
```

```
cancelling...
cancel returned; waiting...
                            <- forever; killed by `timeout 20`
```

Swap the body for the same worker spawned directly and `wait` returns in
about a millisecond. The nursery is the whole difference. Reproduces on the
default thread backend and under `KHORA_FIBERS=scheduler`; repro kept at
`/general/khora-agents/runs/rc2-pipeline/probe`, verified again on
`a99fbe7`.

## Why

Four steps, each correct on its own:

1. `khora_fiber_cancel` calls `scheduler::cancel_fiber(id)`, which flags
   **that one fiber** — `state.cancel()`, forget its timers, forget its
   reactor registration, wake it. Nothing walks anywhere.
2. The cancelled fiber's body is `nursery(...)`. A nursery's exit waits for
   every child, oldest first. That is the promise of structured concurrency
   and is exactly what `std/core.kh:5501` documents.
3. The adopted worker was never flagged, so it never reaches a cancellation
   point that fires. It keeps looping.
4. The nursery therefore never returns, so the outer fiber's completion latch
   is never signalled, so `wait_for` blocks forever.

**The runtime tracks no parentage.** `grep -n parent crates/khora-rt/src/fiber.rs`
finds three comments and no field. Cancellation cannot walk a tree that is not
recorded.

`khora_fiber_detach`'s own doc comment names this tension already:

> That is right, and it is also how a program hangs -- one finalizer that
> never returns holds its nursery, which holds its parent, up to `main`.
> `docs/design/scheduler.md` promises both bounded cancellation latency and
> that a nursery exit leaves every child stopped or joined, and those two are
> in tension exactly here.

So this is a known-shaped problem that nobody had connected to the hang.

## The three options

### A. Cancellation walks the fiber tree

Record a parent on each fiber at spawn; `cancel_fiber` walks descendants and
flags each.

- Makes `Fiber::cancel` mean what the docs say for every body, not just the
  ones without a nursery.
- Costs a field per fiber and a lock discipline for the tree. The walk has to
  tolerate children finishing mid-walk, which is the fiddly part.
- **This is the one I would choose**, because it makes the documented
  behaviour true rather than narrowing the documentation.

### B. A nursery observes its own cancellation

Leave the runtime alone; have the nursery's wait loop notice that the fiber it
is running on has been cancelled and cancel its children before waiting.

- Much smaller, and local to `std/core.kh` plus whatever intrinsic exposes
  "am I cancelled".
- Fixes the nursery case and nothing else. A fiber blocked on any other
  uninterruptible wait still hangs, so the same report comes back in a
  different shape.

### C. Document the limitation and move on

`Fiber::cancel` is honest about single fibers; say plainly that a body holding
a nursery is not cancellable today, and point at `Fiber::detach`.

- Cheap and true, and consistent with how the FFI linking gap was handled.
- Leaves a documented feature that hangs the process, which is the worst
  failure mode there is. I would not ship 0.3.0 this way.

## What I did not do

No code changed. `Fiber::cancel` is a documented feature and the fix touches
the scheduler's model of what a fiber is, which is past the line where I
should be asking rather than deciding.
